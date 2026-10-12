import { afterEach, beforeEach, describe, expect, it } from "vitest";
import React from "react";
import { render } from "ink-testing-library";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { BatonApp } from "../src/tui/App.tsx";
import { BatonStore } from "../src/store.ts";
import type { BatonAdapter, TurnEvent, TurnEventEmitter, TurnStartInput } from "../src/adapters/types.ts";
import type { NativeSessionRef, ProviderId } from "../src/types.ts";

class HangingAdapter implements BatonAdapter {
  readonly id: ProviderId;
  constructor(id: ProviderId) {
    this.id = id;
  }
  async startTurn(_input: TurnStartInput, _session: NativeSessionRef | null, _emit: TurnEventEmitter): Promise<void> {
    await new Promise(() => undefined);
  }
  async interrupt(): Promise<void> {}
  async dispose(): Promise<void> {}
}

class EchoAdapter implements BatonAdapter {
  readonly id: ProviderId;
  constructor(id: ProviderId) {
    this.id = id;
  }
  async startTurn(input: TurnStartInput, _session: NativeSessionRef | null, emit: TurnEventEmitter): Promise<void> {
    emit({ type: "text-delta", text: "the reply" });
    emit({
      type: "turn-completed",
      finalText: "the reply",
      usage: { inputTokens: 10, outputTokens: 5, cacheReadTokens: 100, cacheCreationTokens: 0, costUsd: null },
      nativeSessionId: "native-1",
      rolloutPath: null,
    });
    void input;
  }
  async interrupt(): Promise<void> {}
  async dispose(): Promise<void> {}
}

let store: BatonStore;
let home: string;

beforeEach(() => {
  home = mkdtempSync(join(tmpdir(), "baton-app-"));
  store = BatonStore.open(join(home, "state.sqlite"));
});

afterEach(() => {
  store.close();
  rmSync(home, { recursive: true, force: true });
});

function renderApp(adapters: Record<ProviderId, BatonAdapter>, options?: { draft?: boolean }) {
  const thread = options?.draft
    ? store.draftThread({ title: "test thread", cwd: process.cwd(), currentProvider: "claude", currentModel: null })
    : store.createThread({
        title: "test thread",
        cwd: process.cwd(),
        currentProvider: "claude",
        currentModel: null,
      });
  const instance = render(
    <BatonApp
      store={store}
      adapters={adapters}
      home={home}
      initialThread={thread}
      initialMessages={[]}
      initialUsage={[]}
      version="test"
    />,
    { exitOnCtrlC: false },
  );
  return { instance, thread };
}

function type(instance: { stdin: { write: (data: string) => void } }, text: string): void {
  for (const char of text) {
    instance.stdin.write(char);
  }
}

async function settle(ms = 100): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

describe("BatonApp message visibility", () => {
  it("echoes the user message immediately while the turn is still running", async () => {
    const { instance } = renderApp({ claude: new HangingAdapter("claude"), codex: new HangingAdapter("codex") });
    await settle();
    type(instance, "hello baton");
    instance.stdin.write("\r");
    await settle();
    const frame = instance.lastFrame() ?? "";
    expect(frame).toContain("hello baton");
    expect(frame).toContain("Thinking");
    instance.unmount();
  });

  it("keeps the user message and shows the assistant reply after the turn", async () => {
    const { instance, thread } = renderApp({ claude: new EchoAdapter("claude"), codex: new HangingAdapter("codex") });
    await settle();
    type(instance, "say hi");
    instance.stdin.write("\r");
    await settle(200);
    const frame = instance.lastFrame() ?? "";
    expect(frame).toContain("say hi");
    expect(frame).toContain("the reply");
    expect(store.listMessages(thread.id)).toHaveLength(2);
    expect(store.listMessages(thread.id)[0]?.text).toBe("say hi");
    expect(store.listMessages(thread.id)[1]?.text).toBe("the reply");
    instance.unmount();
  });

  it("persists a draft thread only when the first message is sent", async () => {
    const { instance, thread } = renderApp(
      { claude: new EchoAdapter("claude"), codex: new HangingAdapter("codex") },
      { draft: true },
    );
    await settle();
    expect(store.getThread(thread.id)).toBeNull();
    type(instance, "first message");
    instance.stdin.write("\r");
    await settle(200);
    expect(store.getThread(thread.id)).not.toBeNull();
    expect(store.listMessages(thread.id)).toHaveLength(2);
    expect(store.listMessages(thread.id)[0]?.text).toBe("first message");
    instance.unmount();
  });

  it("echoes the user message even when the turn fails", async () => {
    class FailingAdapter extends HangingAdapter {
      override async startTurn(): Promise<void> {
        throw new Error("boom");
      }
    }
    const { instance, thread } = renderApp({ claude: new FailingAdapter("claude"), codex: new HangingAdapter("codex") });
    await settle();
    type(instance, "this will fail");
    instance.stdin.write("\r");
    await settle(200);
    const frame = instance.lastFrame() ?? "";
    expect(frame).toContain("this will fail");
    expect(frame).toContain("boom");
    expect(store.listMessages(thread.id)[0]?.text).toBe("this will fail");
    instance.unmount();
  });
});
