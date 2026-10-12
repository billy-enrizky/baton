import { isProviderId, type ProviderId } from "./types.ts";

export type ParsedCommand =
  | { kind: "chat"; cwd: string; threadId: string | null; provider: ProviderId | null; model: string | null }
  | { kind: "threads"; cwd: string }
  | { kind: "new"; cwd: string; title: string; provider: ProviderId | null; model: string | null }
  | { kind: "import"; cwd: string; claudeHome: string | null; codexHome: string | null; limit: number; dryRun: boolean }
  | { kind: "checkpoints"; threadId: string }
  | { kind: "diff"; threadId: string; turn: number | null }
  | { kind: "restore"; threadId: string; turn: number }
  | { kind: "usage"; threadId: string | null }
  | { kind: "auth"; set: { provider: ProviderId; mode: "subscription" | "api" } | null }
  | { kind: "help" }
  | { kind: "version" };

export function parseCli(argv: ReadonlyArray<string>): ParsedCommand {
  const args = [...argv];
  let cwd = process.cwd();
  let threadId: string | null = null;
  let provider: ProviderId | null = null;
  let model: string | null = null;
  let claudeHome: string | null = null;
  let codexHome: string | null = null;
  let limit = 50;
  let dryRun = false;

  const positional: string[] = [];
  while (args.length > 0) {
    const arg = args.shift();
    if (arg === undefined) break;
    if (arg === "-C" || arg === "--cwd") {
      cwd = expectValue(args, arg);
    } else if (arg.startsWith("--cwd=")) {
      cwd = arg.slice("--cwd=".length);
    } else if (arg === "-t" || arg === "--thread") {
      threadId = expectValue(args, arg);
    } else if (arg.startsWith("--thread=")) {
      threadId = arg.slice("--thread=".length);
    } else if (arg === "-p" || arg === "--provider") {
      provider = parseProvider(expectValue(args, arg), arg);
    } else if (arg.startsWith("--provider=")) {
      provider = parseProvider(arg.slice("--provider=".length), arg);
    } else if (arg === "-m" || arg === "--model") {
      model = expectValue(args, arg);
    } else if (arg.startsWith("--model=")) {
      model = arg.slice("--model=".length);
    } else if (arg === "--claude-home") {
      claudeHome = expectValue(args, arg);
    } else if (arg.startsWith("--claude-home=")) {
      claudeHome = arg.slice("--claude-home=".length);
    } else if (arg === "--codex-home") {
      codexHome = expectValue(args, arg);
    } else if (arg.startsWith("--codex-home=")) {
      codexHome = arg.slice("--codex-home=".length);
    } else if (arg === "--limit") {
      limit = Number(expectValue(args, arg));
    } else if (arg.startsWith("--limit=")) {
      limit = Number(arg.slice("--limit=".length));
    } else if (arg === "--dry-run") {
      dryRun = true;
    } else if (arg === "-h" || arg === "--help") {
      return { kind: "help" };
    } else if (arg === "-v" || arg === "--version") {
      return { kind: "version" };
    } else {
      positional.push(arg);
    }
  }

  const command = positional[0] ?? "chat";
  const rest = positional.slice(1);
  switch (command) {
    case "chat":
      return { kind: "chat", cwd, threadId, provider, model };
    case "threads":
    case "list":
      return { kind: "threads", cwd };
    case "new": {
      const title = rest.join(" ").trim() || "New thread";
      return { kind: "new", cwd, title, provider, model };
    }
    case "import": {
      const parsedLimit = Number.isFinite(limit) && limit > 0 ? Math.floor(limit) : 50;
      return { kind: "import", cwd, claudeHome, codexHome, limit: parsedLimit, dryRun };
    }
    case "checkpoints": {
      const id = requireThreadId(rest);
      return { kind: "checkpoints", threadId: id };
    }
    case "diff": {
      const id = requireThreadId(rest);
      const turn = rest[1] !== undefined ? Number(rest[1]) : null;
      return { kind: "diff", threadId: id, turn: turn !== null && Number.isFinite(turn) ? turn : null };
    }
    case "restore": {
      const id = requireThreadId(rest);
      const turn = Number(rest[1]);
      if (!Number.isFinite(turn)) {
        throw new Error("restore requires a turn number: baton restore <threadId> <turn>");
      }
      return { kind: "restore", threadId: id, turn };
    }
    case "usage": {
      return { kind: "usage", threadId: rest[0] ?? null };
    }
    case "auth": {
      const target = rest[0];
      const mode = rest[1];
      if (target === undefined) return { kind: "auth", set: null };
      if (!isProviderId(target)) {
        throw new Error(`unknown provider "${target}", expected claude or codex`);
      }
      if (mode !== "subscription" && mode !== "api") {
        throw new Error(`unknown auth mode "${mode ?? ""}", expected subscription or api`);
      }
      return { kind: "auth", set: { provider: target, mode } };
    }
    case "help":
      return { kind: "help" };
    case "version":
      return { kind: "version" };
    default:
      throw new Error(`unknown command "${command}". Run baton --help.`);
  }
}

function expectValue(args: string[], flag: string): string {
  const value = args.shift();
  if (value === undefined || value.startsWith("-")) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
}

function parseProvider(value: string, flag: string): ProviderId {
  if (!isProviderId(value)) {
    throw new Error(`${flag}: unknown provider "${value}", expected claude or codex`);
  }
  return value;
}

function requireThreadId(rest: string[]): string {
  const id = rest[0];
  if (!id) {
    throw new Error("this command requires a thread id. Run baton threads to list them.");
  }
  return id;
}

export const HELP_TEXT = `baton: one terminal CLI for every coding agent harness

Usage: baton [command] [options]

Commands:
  baton                     start the TUI with a fresh thread (unsaved until you send a message)
  baton chat                same as bare baton
  baton new [title]         create a thread
  baton threads             list threads for this directory
  baton -t <id>             open an existing thread in the TUI
  baton import              import recent native sessions (claude jsonl, codex rollouts) as threads
  baton checkpoints <id>    list checkpoints for a thread
  baton diff <id> [turn]    diff a checkpoint (default: latest) against the working tree
  baton restore <id> <turn> restore the working tree to a checkpoint
  baton usage [id]          show token usage and cache hit rates
  baton auth [provider] [subscription|api]
                            show or set billing mode per provider

Options:
  -C, --cwd <dir>           working directory (default: current)
  -t, --thread <id>         open a specific thread
  -p, --provider <id>       claude or codex
  -m, --model <model>       model id for the provider
  --claude-home <dir>       override ~/.claude for import
  --codex-home <dir>        override ~/.codex for import
  --limit <n>               max sessions to import (default 50)
  --dry-run                 list importable sessions without importing

In-chat commands: /help /switch /model /mode /auth /new /threads /open /usage /checkpoints /diff /quit`;
