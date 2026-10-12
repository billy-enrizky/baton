import type { ProviderId } from "./types.ts";
import type { BatonAdapter, TurnEventEmitter } from "./adapters/types.ts";
import type { BatonStore, MessageRow, ThreadRow } from "./store.ts";
import { buildHandoff, gitStatusSummary, wrapPromptWithHandoff } from "./handoff.ts";
import { captureCheckpoint } from "./checkpoints.ts";
import { providerAuthEnv, resolveConfig } from "./config.ts";

export interface BatonRuntimeDeps {
  store: BatonStore;
  adapters: Partial<Record<ProviderId, BatonAdapter>>;
  home: string;
}

export interface TurnOutcome {
  threadId: string;
  turn: number;
  ok: boolean;
  finalText: string;
  error: string | null;
}

export interface TurnOptions {
  permissionMode: "default" | "acceptEdits" | "bypassPermissions" | "plan";
}

const DEFAULT_TURN_OPTIONS: TurnOptions = { permissionMode: "default" };

export function deriveTitle(text: string): string {
  const firstLine = text.trim().split("\n")[0] ?? "";
  const title = firstLine.slice(0, 80).trim();
  return title.length > 0 ? title : "New thread";
}

export async function runTurn(
  deps: BatonRuntimeDeps,
  thread: ThreadRow,
  userText: string,
  emit: TurnEventEmitter,
  options: TurnOptions = DEFAULT_TURN_OPTIONS,
): Promise<TurnOutcome> {
  const { store, adapters, home } = deps;
  const provider = thread.currentProvider;
  const adapter = adapters[provider];
  if (!adapter) {
    return {
      threadId: thread.id,
      turn: store.nextTurnNumber(thread.id),
      ok: false,
      finalText: "",
      error: `no adapter available for provider "${provider}"`,
    };
  }

  const config = resolveConfig({ home, cwd: thread.cwd });
  const model = thread.currentModel ?? config[provider].model;
  const authEnv = providerAuthEnv(config[provider], provider);

  const priorMessages = store.listMessages(thread.id);
  const turn = store.nextTurnNumber(thread.id);
  store.addMessage({ threadId: thread.id, turn, role: "user", provider, model, text: userText });

  const session = store.getProviderSession(thread.id, provider);
  let prompt = userText;
  if (!session && priorMessages.length > 0) {
    const handoff = buildHandoff({
      cwd: thread.cwd,
      threadTitle: thread.title,
      messages: [
        ...priorMessages,
        { role: "user", provider, model, text: userText },
      ],
      gitStatus: await gitStatusSummary(thread.cwd),
    });
    prompt = wrapPromptWithHandoff(handoff, userText);
  }

  if (thread.title === "New thread") {
    store.renameThread(thread.id, deriveTitle(userText));
  }

  return await new Promise<TurnOutcome>((resolve) => {
    const finish = (ok: boolean, finalText: string, error: string | null, nativeSessionId: string | null, rolloutPath: string | null) => {
      if (nativeSessionId) {
        store.upsertProviderSession(thread.id, provider, nativeSessionId, model, rolloutPath);
      }
      resolve({ threadId: thread.id, turn, ok, finalText, error });
      void captureCheckpoint(thread.cwd, thread.id, turn)
        .then((ref) => store.addCheckpoint(thread.id, turn, ref))
        .catch(() => undefined);
    };

    void adapter
      .startTurn(
        {
          threadId: thread.id,
          cwd: thread.cwd,
          model: model,
          prompt,
          permissionMode: options.permissionMode,
          authEnv,
        },
        session ? { nativeSessionId: session.nativeSessionId, rolloutPath: session.rolloutPath } : null,
        (event) => {
          emit(event);
          if (event.type === "turn-completed") {
            store.addMessage({
              threadId: thread.id,
              turn,
              role: "assistant",
              provider,
              model: model,
              text: event.finalText,
            });
            if (event.usage) {
              store.addUsage({
                threadId: thread.id,
                turn,
                provider,
                model: model,
                inputTokens: event.usage.inputTokens,
                outputTokens: event.usage.outputTokens,
                cacheReadTokens: event.usage.cacheReadTokens,
                cacheCreationTokens: event.usage.cacheCreationTokens,
                costUsd: event.usage.costUsd,
              });
            }
            finish(true, event.finalText, null, event.nativeSessionId, event.rolloutPath);
          } else if (event.type === "turn-failed") {
            finish(false, "", event.error, null, null);
          }
        },
      )
      .catch((error: unknown) => {
        const message = error instanceof Error ? error.message : String(error);
        emit({ type: "turn-failed", error: message });
        finish(false, "", message, null, null);
      });
  });
}

export function listThreadSummaries(store: BatonStore, cwd?: string): Array<ThreadRow & { messageCount: number }> {
  return store.listThreads(cwd).map((thread) => ({
    ...thread,
    messageCount: store.listMessages(thread.id).length,
  }));
}

export function threadMessages(store: BatonStore, threadId: string): MessageRow[] {
  return store.listMessages(threadId);
}
