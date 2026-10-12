#!/usr/bin/env node
import { parseCli, HELP_TEXT } from "./cli.ts";
import { BatonStore } from "./store.ts";
import { batonHome, batonDbPath, claudeProjectsDir, codexSessionsDir } from "./paths.ts";
import { scanClaudeSessions, scanCodexSessions } from "./scanner.ts";
import {
  describeAuth,
  loadConfigFile,
  resolveConfig,
  saveConfigFile,
} from "./config.ts";
import { isProviderId, type ProviderId } from "./types.ts";
import { diffCheckpointAgainstWorktree, restoreCheckpoint } from "./checkpoints.ts";
import { cacheHitRate, formatUsageLine, sumUsage } from "./usage.ts";

const VERSION = "0.1.0";

async function main(): Promise<void> {
  const command = parseCli(process.argv.slice(2));
  const home = batonHome();

  switch (command.kind) {
    case "help":
      console.log(HELP_TEXT);
      return;
    case "version":
      console.log(VERSION);
      return;
    case "auth": {
      const resolved = resolveConfig({ home, cwd: process.cwd() });
      if (!command.set) {
        console.log(`claude: ${describeAuth(resolved.claude)}`);
        console.log(`codex:  ${describeAuth(resolved.codex)}`);
        return;
      }
      const { provider, mode } = command.set;
      if (mode === "api" && !resolved[provider].apiKey) {
        throw new Error(
          `no API key configured for ${provider}. Set ${resolved[provider].apiKeyEnvName} in the environment, the harness config, or ${home}/config.json first.`,
        );
      }
      const file = loadConfigFile(home) ?? {};
      file[provider] = { ...(file[provider] ?? {}), mode };
      saveConfigFile(home, file);
      console.log(`${provider} billing set to ${mode}`);
      return;
    }
    case "threads": {
      const store = BatonStore.open(batonDbPath());
      const resolved = resolveConfig({ home, cwd: command.cwd });
      const threads = store.listThreads(command.cwd);
      if (threads.length === 0) {
        console.log("no threads in this directory yet. Run baton to start one.");
        return;
      }
      for (const thread of threads) {
        const model = thread.currentModel ?? resolved[thread.currentProvider].model;
        console.log(`${thread.id.slice(0, 8)}  ${thread.currentProvider}/${model}  ${thread.title}`);
      }
      return;
    }
    case "new": {
      const store = BatonStore.open(batonDbPath());
      const resolved = resolveConfig({ home, cwd: command.cwd });
      const provider: ProviderId = command.provider ?? "claude";
      const model = command.model ?? resolved[provider].model;
      const thread = store.createThread({
        title: command.title,
        cwd: command.cwd,
        currentProvider: provider,
        currentModel: command.model ?? null,
      });
      console.log(`created thread ${thread.id.slice(0, 8)} (${provider}/${model})`);
      return;
    }
    case "import": {
      const store = BatonStore.open(batonDbPath());
      const resolved = resolveConfig({ home, cwd: command.cwd });
      const claudeSessions = scanClaudeSessions(command.claudeHome ?? claudeProjectsDir(), command.limit);
      const codexSessions = scanCodexSessions(command.codexHome ?? codexSessionsDir(), command.limit);
      let imported = 0;
      let skipped = 0;
      for (const session of [...claudeSessions, ...codexSessions]) {
        const existing = store.findThreadByNativeSession(session.provider, session.nativeSessionId);
        if (existing) {
          skipped += 1;
          continue;
        }
        if (command.dryRun) {
          console.log(`[dry-run] ${session.provider} ${session.nativeSessionId.slice(0, 8)}  ${session.title}`);
          continue;
        }
        const thread = store.importThread({
          title: session.title,
          cwd: session.cwd ?? command.cwd,
          currentProvider: session.provider,
          currentModel: session.model ?? null,
          nativeSessionId: session.nativeSessionId,
          rolloutPath: session.provider === "codex" ? session.path : null,
          messages: session.messages,
        });
        imported += 1;
        console.log(`imported ${session.provider} ${session.nativeSessionId.slice(0, 8)} as ${thread.id.slice(0, 8)}  ${thread.title}`);
      }
      if (!command.dryRun) {
        console.log(`imported ${imported}, skipped ${skipped} already present`);
      }
      return;
    }
    case "checkpoints": {
      const store = BatonStore.open(batonDbPath());
      const thread = store.getThread(command.threadId);
      if (!thread) throw new Error(`no thread matches ${command.threadId}`);
      const checkpoints = store.listCheckpoints(thread.id);
      if (checkpoints.length === 0) {
        console.log("no checkpoints yet");
        return;
      }
      for (const checkpoint of checkpoints) {
        console.log(`turn ${String(checkpoint.turn).padStart(3)}  ${checkpoint.gitRef}`);
      }
      return;
    }
    case "diff": {
      const store = BatonStore.open(batonDbPath());
      const thread = store.getThread(command.threadId);
      if (!thread) throw new Error(`no thread matches ${command.threadId}`);
      const checkpoints = store.listCheckpoints(thread.id);
      if (checkpoints.length === 0) throw new Error("no checkpoints yet");
      const checkpoint =
        command.turn === null
          ? checkpoints[checkpoints.length - 1]
          : checkpoints.find((candidate) => candidate.turn === command.turn);
      if (!checkpoint) throw new Error(`no checkpoint for turn ${command.turn}`);
      const diff = await diffCheckpointAgainstWorktree(thread.cwd, checkpoint.gitRef);
      console.log(diff.length > 0 ? diff : "(no differences)");
      return;
    }
    case "restore": {
      const store = BatonStore.open(batonDbPath());
      const thread = store.getThread(command.threadId);
      if (!thread) throw new Error(`no thread matches ${command.threadId}`);
      const checkpoint = store.getCheckpoint(thread.id, command.turn);
      if (!checkpoint) throw new Error(`no checkpoint for turn ${command.turn}`);
      await restoreCheckpoint(thread.cwd, checkpoint.gitRef);
      console.log(`restored ${thread.cwd} to ${checkpoint.gitRef}`);
      return;
    }
    case "usage": {
      const store = BatonStore.open(batonDbPath());
      const resolved = resolveConfig({ home, cwd: process.cwd() });
      const threads = command.threadId
        ? [store.getThread(command.threadId)].filter((thread): thread is NonNullable<typeof thread> => thread !== null)
        : store.listThreads();
      if (threads.length === 0) throw new Error("no threads found");
      for (const thread of threads) {
        const rows = store.listUsage(thread.id);
        if (rows.length === 0) continue;
        const totals = sumUsage(rows);
        const rate = cacheHitRate(totals);
        const rateLabel = rate === null ? "n/a" : `${(rate * 100).toFixed(0)}%`;
        const model = thread.currentModel ?? resolved[thread.currentProvider].model;
        console.log(
          `${thread.id.slice(0, 8)}  ${thread.currentProvider}/${model}  ${thread.title}  ${formatUsageLine(totals)}  hit ${rateLabel}`,
        );
      }
      return;
    }
    case "chat": {
      const store = BatonStore.open(batonDbPath());
      const resolved = resolveConfig({ home, cwd: command.cwd });
      let thread = command.threadId ? store.getThread(command.threadId) : null;
      if (command.threadId && !thread) throw new Error(`no thread matches ${command.threadId}`);
      if (!thread) thread = store.latestThreadInCwd(command.cwd);
      const provider: ProviderId = command.provider ?? thread?.currentProvider ?? "claude";
      if (!isProviderId(provider)) throw new Error(`unknown provider ${provider}`);
      if (!thread) {
        thread = store.createThread({
          title: "New thread",
          cwd: command.cwd,
          currentProvider: provider,
          currentModel: command.model ?? null,
        });
      } else if (thread.currentProvider !== provider) {
        store.setThreadProvider(thread.id, provider, command.model ?? null);
      } else if (command.model !== null && command.model !== thread.currentModel) {
        store.setThreadModel(thread.id, command.model);
      }
      const { ClaudeAdapter } = await import("./adapters/claude.ts");
      const { CodexAdapter } = await import("./adapters/codex.ts");
      const { runChatTui } = await import("./tui/App.tsx");
      const adapters: Record<ProviderId, import("./adapters/types.ts").BatonAdapter> = {
        claude: new ClaudeAdapter(),
        codex: new CodexAdapter(),
      };
      try {
        await runChatTui({ store, adapters, home, thread, version: VERSION });
      } finally {
        await adapters.claude.dispose();
        await adapters.codex.dispose();
        store.close();
      }
      return;
    }
  }
}

main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
});
