import { describe, expect, it } from "vitest";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { BatonStore } from "../src/store.ts";

describe("BatonStore", () => {
  it("creates, lists, and updates threads", () => {
    const store = BatonStore.memory();
    const thread = store.createThread({
      title: "New thread",
      cwd: "/tmp/project",
      currentProvider: "claude",
      currentModel: "claude-opus-5-5",
    });
    expect(store.getThread(thread.id)?.title).toBe("New thread");
    expect(store.listThreads("/tmp/project")).toHaveLength(1);
    expect(store.listThreads("/tmp/other")).toHaveLength(0);

    store.setThreadProvider(thread.id, "codex", "gpt-6.1-sol");
    expect(store.getThread(thread.id)?.currentProvider).toBe("codex");
    expect(store.getThread(thread.id)?.currentModel).toBe("gpt-6.1-sol");

    const second = store.createThread({
      title: "another",
      cwd: "/tmp/project",
      currentProvider: "codex",
      currentModel: "gpt-6.1-sol",
    });
    store.deleteThread(second.id);
    expect(store.listThreads("/tmp/project")).toHaveLength(1);
  });

  it("creates threads with an explicit id and drafts unpersisted threads", () => {
    const store = BatonStore.memory();
    const draft = store.draftThread({
      title: "New thread",
      cwd: "/tmp/x",
      currentProvider: "claude",
      currentModel: null,
    });
    expect(store.getThread(draft.id)).toBeNull();
    const persisted = store.createThread(
      { title: "New thread", cwd: "/tmp/x", currentProvider: "claude", currentModel: null },
      draft.id,
    );
    expect(persisted.id).toBe(draft.id);
    expect(store.getThread(draft.id)?.title).toBe("New thread");
  });

  it("tracks messages and turn numbers", () => {
    const store = BatonStore.memory();
    const thread = store.createThread({
      title: "t",
      cwd: "/tmp/x",
      currentProvider: "claude",
      currentModel: "m",
    });
    expect(store.nextTurnNumber(thread.id)).toBe(1);
    store.addMessage({ threadId: thread.id, turn: 1, role: "user", text: "hello" });
    store.addMessage({ threadId: thread.id, turn: 1, role: "assistant", provider: "claude", model: "m", text: "hi" });
    expect(store.nextTurnNumber(thread.id)).toBe(2);
    const messages = store.listMessages(thread.id);
    expect(messages).toHaveLength(2);
    expect(messages[0]?.role).toBe("user");
    expect(messages[1]?.role).toBe("assistant");
  });

  it("upserts provider sessions per provider", () => {
    const store = BatonStore.memory();
    const thread = store.createThread({
      title: "t",
      cwd: "/tmp/x",
      currentProvider: "claude",
      currentModel: "m",
    });
    expect(store.getProviderSession(thread.id, "codex")).toBeNull();
    store.upsertProviderSession(thread.id, "codex", "native-1", "gpt-6.1-sol", "/path/rollout.jsonl");
    store.upsertProviderSession(thread.id, "claude", "session-a", null, null);
    expect(store.getProviderSession(thread.id, "codex")?.nativeSessionId).toBe("native-1");
    expect(store.getProviderSession(thread.id, "claude")?.nativeSessionId).toBe("session-a");

    store.upsertProviderSession(thread.id, "codex", "native-2", null, null);
    const codex = store.getProviderSession(thread.id, "codex");
    expect(codex?.nativeSessionId).toBe("native-2");
    expect(codex?.rolloutPath).toBe("/path/rollout.jsonl");

    expect(store.findThreadByNativeSession("codex", "native-2")?.id).toBe(thread.id);
    expect(store.findThreadByNativeSession("claude", "session-a")?.id).toBe(thread.id);
  });

  it("imports a thread with messages transactionally", () => {
    const store = BatonStore.memory();
    const thread = store.importThread({
      title: "imported",
      cwd: "/tmp/repo",
      currentProvider: "claude",
      currentModel: "claude-opus-5-5",
      nativeSessionId: "native-import",
      rolloutPath: null,
      messages: [
        { role: "user", text: "fix the bug" },
        { role: "assistant", provider: "claude", model: "claude-opus-5-5", text: "done" },
        { role: "user", text: "thanks" },
      ],
    });
    expect(store.getProviderSession(thread.id, "claude")?.nativeSessionId).toBe("native-import");
    const messages = store.listMessages(thread.id);
    expect(messages).toHaveLength(3);
    expect(messages.map((message) => message.role)).toEqual(["user", "assistant", "user"]);
    expect(store.nextTurnNumber(thread.id)).toBe(3);
  });

  it("records checkpoints and usage", () => {
    const store = BatonStore.memory();
    const thread = store.createThread({
      title: "t",
      cwd: "/tmp/x",
      currentProvider: "codex",
      currentModel: "m",
    });
    store.addCheckpoint(thread.id, 1, "refs/baton/checkpoints/x/1");
    store.addUsage({
      threadId: thread.id,
      turn: 1,
      provider: "codex",
      model: "m",
      inputTokens: 100,
      outputTokens: 50,
      cacheReadTokens: 800,
      cacheCreationTokens: 100,
      costUsd: null,
    });
    expect(store.listCheckpoints(thread.id)[0]?.gitRef).toBe("refs/baton/checkpoints/x/1");
    expect(store.getCheckpoint(thread.id, 1)?.turn).toBe(1);
    expect(store.listUsage(thread.id)[0]?.cacheReadTokens).toBe(800);
    expect(store.listUsage()).toHaveLength(1);
  });

  it("supports null models that follow harness config", () => {
    const store = BatonStore.memory();
    const thread = store.createThread({
      title: "t",
      cwd: "/tmp/x",
      currentProvider: "codex",
      currentModel: null,
    });
    expect(store.getThread(thread.id)?.currentModel).toBeNull();
    store.setThreadProvider(thread.id, "claude", null);
    expect(store.getThread(thread.id)?.currentModel).toBeNull();
    store.setThreadModel(thread.id, "claude-opus-5-5");
    expect(store.getThread(thread.id)?.currentModel).toBe("claude-opus-5-5");
  });

  it("migrates old NOT NULL schema, resetting stale baked-in default models", () => {
    const dir = mkdtempSync(join(tmpdir(), "baton-store-"));
    try {
      const path = join(dir, "state.sqlite");
      const legacy = new DatabaseSync(path);
      legacy.exec(`
        CREATE TABLE threads (
          id TEXT PRIMARY KEY,
          title TEXT NOT NULL,
          cwd TEXT NOT NULL,
          current_provider TEXT NOT NULL,
          current_model TEXT NOT NULL,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );
      `);
      const insert = legacy.prepare(
        "INSERT INTO threads (id, title, cwd, current_provider, current_model, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
      );
      insert.run("stale-id", "stale", "/tmp/x", "codex", "gpt-6.1-sol", "2026-10-12T00:00:00Z", "2026-10-12T00:00:00Z");
      insert.run("fresh-id", "fresh", "/tmp/x", "claude", "claude-opus-5-5", "2026-10-12T00:00:00Z", "2026-10-12T00:00:00Z");
      legacy.close();

      const store = BatonStore.open(path);
      expect(store.getThread("stale-id")?.currentModel).toBeNull();
      expect(store.getThread("fresh-id")?.currentModel).toBe("claude-opus-5-5");
      store.setThreadProvider("stale-id", "codex", null);
      expect(store.getThread("stale-id")?.currentModel).toBeNull();
      store.close();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("keeps explicit model choices on reopened nullable-schema databases", () => {
    const dir = mkdtempSync(join(tmpdir(), "baton-store-"));
    try {
      const path = join(dir, "state.sqlite");
      const store = BatonStore.open(path);
      const thread = store.createThread({
        title: "explicit",
        cwd: "/tmp/x",
        currentProvider: "codex",
        currentModel: "gpt-6.1-sol",
      });
      store.close();

      const reopened = BatonStore.open(path);
      expect(reopened.getThread(thread.id)?.currentModel).toBe("gpt-6.1-sol");
      reopened.close();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
