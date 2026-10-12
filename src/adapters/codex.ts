import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import type { NativeSessionRef, PermissionMode, TurnUsage } from "../types.ts";
import type { BatonAdapter, TurnEvent, TurnEventEmitter, TurnStartInput } from "./types.ts";
import { authSignature } from "../config.ts";

export interface CodexConnection {
  write(line: string): void;
  setOnLine(listener: ((line: string) => void) | null): void;
  setOnClose(listener: ((code: number | null) => void) | null): void;
  kill(): void;
}

export function spawnCodexAppServer(authEnv: Record<string, string>): CodexConnection {
  const proc: ChildProcessWithoutNullStreams = spawn("codex", ["app-server"], {
    stdio: ["pipe", "pipe", "pipe"],
    env: { ...process.env, ...authEnv },
  });
  let buffer = "";
  let lineListener: ((line: string) => void) | null = null;
  let closeListener: ((code: number | null) => void) | null = null;
  proc.stdout.setEncoding("utf8");
  proc.stdout.on("data", (chunk: string) => {
    buffer += chunk;
    let newlineIndex = buffer.indexOf("\n");
    while (newlineIndex !== -1) {
      const line = buffer.slice(0, newlineIndex).replace(/\r$/, "");
      buffer = buffer.slice(newlineIndex + 1);
      if (line.trim().length > 0) lineListener?.(line);
      newlineIndex = buffer.indexOf("\n");
    }
  });
  proc.on("close", (code) => closeListener?.(code));
  return {
    write(line) {
      proc.stdin.write(`${line}\n`);
    },
    setOnLine(listener) {
      lineListener = listener;
    },
    setOnClose(listener) {
      closeListener = listener;
    },
    kill() {
      proc.kill();
    },
  };
}

type ServerRequestHandler = (payload: Record<string, unknown>) => Promise<Record<string, unknown>>;

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export class CodexAppServerClient {
  private nextId = 1;
  private readonly pending = new Map<number, PendingRequest>();
  private readonly serverRequestHandlers = new Map<string, ServerRequestHandler>();
  private closed = false;

  constructor(
    private readonly connection: CodexConnection,
    private readonly requestTimeoutMs = 60000,
  ) {
    connection.setOnLine((line) => this.handleLine(line));
    connection.setOnClose((code) => this.handleClose(code));
  }

  onNotification: ((method: string, params: Record<string, unknown>) => void) | null = null;
  onExit: ((error: Error) => void) | null = null;

  request(method: string, params?: unknown): Promise<unknown> {
    if (this.closed) return Promise.reject(new Error("codex app-server connection closed"));
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`codex app-server request timed out: ${method}`));
      }, this.requestTimeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.connection.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    });
  }

  async requestAs<T>(method: string, params?: unknown): Promise<T> {
    return (await this.request(method, params)) as T;
  }

  notify(method: string, params?: unknown): void {
    if (this.closed) return;
    this.connection.write(JSON.stringify({ jsonrpc: "2.0", method, params }));
  }

  handleServerRequest(method: string, handler: ServerRequestHandler): void {
    this.serverRequestHandlers.set(method, handler);
  }

  dispose(): void {
    if (this.closed) return;
    this.closed = true;
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(new Error("codex app-server disposed"));
    }
    this.pending.clear();
    this.connection.setOnLine(null);
    this.connection.setOnClose(null);
    this.connection.kill();
  }

  private handleClose(code: number | null): void {
    if (this.closed) return;
    this.closed = true;
    const error = new Error(`codex app-server exited with code ${code ?? "unknown"}`);
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
    this.onExit?.(error);
  }

  private handleLine(line: string): void {
    let message: unknown;
    try {
      message = JSON.parse(line);
    } catch {
      return;
    }
    if (typeof message !== "object" || message === null) return;
    const frame = message as Record<string, unknown>;
    if (typeof frame.method === "string") {
      const params = (typeof frame.params === "object" && frame.params !== null
        ? frame.params
        : {}) as Record<string, unknown>;
      if (typeof frame.id === "string" || typeof frame.id === "number") {
        void this.handleServerRequestFrame(frame.id, frame.method, params);
      } else {
        this.onNotification?.(frame.method, params);
      }
      return;
    }
    if (frame.id !== undefined) {
      const id = typeof frame.id === "number" ? frame.id : Number(frame.id);
      const pending = this.pending.get(id);
      if (!pending) return;
      this.pending.delete(id);
      clearTimeout(pending.timer);
      if (frame.error !== undefined) {
        const error = frame.error as { code?: number; message?: string };
        const rejection = new Error(
          `codex app-server request error (${error.code ?? "unknown"}): ${error.message ?? "unknown error"}`,
        );
        (rejection as Error & { code?: number }).code = error.code;
        (rejection as Error & { errorMessage?: string }).errorMessage = error.message;
        pending.reject(rejection);
      } else {
        pending.resolve(frame.result);
      }
    }
  }

  private async handleServerRequestFrame(
    id: string | number,
    method: string,
    params: Record<string, unknown>,
  ): Promise<void> {
    const handler = this.serverRequestHandlers.get(method);
    if (!handler) {
      this.connection.write(
        JSON.stringify({
          jsonrpc: "2.0",
          id,
          error: { code: -32601, message: `baton has no handler for ${method}` },
        }),
      );
      return;
    }
    try {
      const result = await handler(params);
      this.connection.write(JSON.stringify({ jsonrpc: "2.0", id, result }));
    } catch (error) {
      this.connection.write(
        JSON.stringify({
          jsonrpc: "2.0",
          id,
          error: { code: -32000, message: error instanceof Error ? error.message : "baton handler failed" },
        }),
      );
    }
  }
}

interface CodexThreadInfo {
  thread: {
    id: string;
    path?: string | null;
  };
}

interface CodexTokenUsage {
  inputTokens?: number;
  cachedInputTokens?: number;
  cacheWriteInputTokens?: number;
  outputTokens?: number;
}

interface CumulativeUsage {
  inputTokens: number;
  cachedInputTokens: number;
  cacheWriteInputTokens: number;
  outputTokens: number;
}

function normalizeCumulative(usage: CodexTokenUsage): CumulativeUsage {
  return {
    inputTokens: usage.inputTokens ?? 0,
    cachedInputTokens: usage.cachedInputTokens ?? 0,
    cacheWriteInputTokens: usage.cacheWriteInputTokens ?? 0,
    outputTokens: usage.outputTokens ?? 0,
  };
}

function diffUsage(current: CumulativeUsage, baseline: CumulativeUsage | null): CumulativeUsage {
  if (baseline === null) return { ...current };
  return {
    inputTokens: Math.max(current.inputTokens - baseline.inputTokens, 0),
    cachedInputTokens: Math.max(current.cachedInputTokens - baseline.cachedInputTokens, 0),
    cacheWriteInputTokens: Math.max(current.cacheWriteInputTokens - baseline.cacheWriteInputTokens, 0),
    outputTokens: Math.max(current.outputTokens - baseline.outputTokens, 0),
  };
}

function sandboxPolicyFor(mode: PermissionMode): Record<string, unknown> {
  switch (mode) {
    case "bypassPermissions":
      return { type: "dangerFullAccess" };
    case "plan":
      return { type: "readOnly", networkAccess: false };
    default:
      return { type: "workspaceWrite", writableRoots: [], networkAccess: false };
  }
}

function approvalPolicyFor(mode: PermissionMode): string {
  return mode === "bypassPermissions" ? "never" : "on-request";
}

function approvalSummary(kind: string, payload: Record<string, unknown>): string {
  if (typeof payload.command === "string" && payload.command.length > 0) return payload.command;
  const changes = Array.isArray(payload.changes)
    ? payload.changes
        .map((change) =>
          typeof change === "object" && change !== null && typeof (change as { path?: unknown }).path === "string"
            ? (change as { path: string }).path
            : "",
        )
        .filter((path) => path.length > 0)
        .join(", ")
    : "";
  if (changes.length > 0) return changes;
  if (typeof payload.reason === "string" && payload.reason.length > 0) return payload.reason;
  return `codex requests ${kind} approval`;
}

interface ActiveCodexTurn {
  threadId: string;
  emit: TurnEventEmitter;
  resolve: () => void;
  reject: (error: Error) => void;
  finalText: string;
  rolloutPath: string | null;
  turnUsage: TurnUsage | null;
  baseline: CumulativeUsage | null;
}

export class CodexAdapter implements BatonAdapter {
  readonly id = "codex" as const;

  private client: CodexAppServerClient | null = null;
  private clientAuthSignature: string | null = null;
  private readonly lastCumulativeByThread = new Map<string, CumulativeUsage>();
  private activeTurn: ActiveCodexTurn | null = null;

  constructor(
    private readonly spawnConnection: (authEnv: Record<string, string>) => CodexConnection = spawnCodexAppServer,
    private readonly version = "0.1.0",
  ) {}

  async startTurn(input: TurnStartInput, session: NativeSessionRef | null, emit: TurnEventEmitter): Promise<void> {
    const client = await this.ensureClient(input.authEnv);
    let threadId = session?.nativeSessionId ?? null;
    let rolloutPath = session?.rolloutPath ?? null;
    if (threadId) {
      await client.request("thread/resume", { threadId, excludeTurns: true });
    } else {
      const started = await client.requestAs<CodexThreadInfo>("thread/start", {
        cwd: input.cwd,
        model: input.model,
      });
      threadId = started.thread.id;
      rolloutPath = started.thread.path ?? null;
    }
    if (!threadId) throw new Error("codex app-server did not return a thread id");

    const activeThread = threadId;
    await new Promise<void>((resolve, reject) => {
      const active: ActiveCodexTurn = {
        threadId: activeThread,
        emit,
        resolve,
        reject,
        finalText: "",
        rolloutPath,
        turnUsage: null,
        baseline: this.lastCumulativeByThread.get(activeThread) ?? null,
      };
      this.activeTurn = active;
      client
        .request("turn/start", {
          threadId: activeThread,
          cwd: input.cwd,
          model: input.model,
          input: [{ type: "text", text: input.prompt }],
          approvalPolicy: approvalPolicyFor(input.permissionMode),
          sandboxPolicy: sandboxPolicyFor(input.permissionMode),
          approvalsReviewer: "user",
          summary: "detailed",
        })
        .catch((error: unknown) => {
          if (this.activeTurn === active) this.activeTurn = null;
          reject(error instanceof Error ? error : new Error(String(error)));
        });
    });
  }

  async interrupt(): Promise<void> {
    const active = this.activeTurn;
    if (!active || !this.client) return;
    try {
      await this.client.request("turn/interrupt", { threadId: active.threadId });
    } catch {
      this.client.notify("turn/interrupt", { threadId: active.threadId });
    }
  }

  async dispose(): Promise<void> {
    this.client?.dispose();
    this.client = null;
    this.clientAuthSignature = null;
  }

  private async ensureClient(authEnv: Record<string, string>): Promise<CodexAppServerClient> {
    const signature = authSignature(authEnv);
    if (this.client && this.clientAuthSignature === signature) return this.client;
    if (this.client) {
      this.client.dispose();
      this.client = null;
      this.clientAuthSignature = null;
    }
    const client = new CodexAppServerClient(this.spawnConnection(authEnv));
    client.onNotification = (method, params) => this.handleNotification(method, params);
    client.onExit = (error) => {
      this.client = null;
      this.clientAuthSignature = null;
      this.activeTurn?.reject(error);
      this.activeTurn = null;
    };
    this.installApprovalHandlers(client);
    try {
      await client.request("initialize", {
        clientInfo: { name: "baton", title: "baton", version: this.version },
        capabilities: { experimentalApi: true },
      });
    } catch (error) {
      const isAlreadyInitialized =
        error instanceof Error &&
        (error as Error & { errorMessage?: string }).errorMessage === "Already initialized";
      if (!isAlreadyInitialized) {
        client.dispose();
        throw error;
      }
    }
    client.notify("initialized");
    this.client = client;
    this.clientAuthSignature = signature;
    return client;
  }

  private installApprovalHandlers(client: CodexAppServerClient): void {
    const kinds = [
      ["item/commandExecution/requestApproval", "command"],
      ["item/fileChange/requestApproval", "fileChange"],
      ["item/permissions/requestApproval", "permissions"],
    ] as const;
    for (const [method, kind] of kinds) {
      client.handleServerRequest(method, async (payload) => {
        const active = this.activeTurn;
        if (!active) throw new Error("approval requested with no active baton turn");
        const decision = await new Promise<string>((resolve) => {
          active.emit({
            type: "approval-request",
            id: `${method}:${String(payload.approvalId ?? payload.itemId ?? "")}`,
            kind,
            summary: approvalSummary(kind, payload),
            respond: async (choice) => {
              resolve(choice);
            },
          });
        });
        return { decision: decision === "acceptAlways" ? "acceptForSession" : decision };
      });
    }
  }

  private handleNotification(method: string, params: Record<string, unknown>): void {
    const threadId = typeof params.threadId === "string" ? params.threadId : null;
    if (method === "thread/tokenUsage/updated") {
      const usagePayload = (typeof params.tokenUsage === "object" && params.tokenUsage !== null
        ? params.tokenUsage
        : {}) as CodexTokenUsage;
      const cumulative = normalizeCumulative(usagePayload);
      if (threadId) this.lastCumulativeByThread.set(threadId, cumulative);
      const active = this.activeTurn;
      if (active && threadId === active.threadId) {
        const delta = diffUsage(cumulative, active.baseline);
        active.turnUsage = {
          inputTokens: delta.inputTokens,
          outputTokens: delta.outputTokens,
          cacheReadTokens: delta.cachedInputTokens,
          cacheCreationTokens: delta.cacheWriteInputTokens,
          costUsd: null,
        };
        active.emit({ type: "usage", usage: active.turnUsage });
      }
      return;
    }
    const active = this.activeTurn;
    if (!active || !threadId || threadId !== active.threadId) return;

    switch (method) {
      case "item/agentMessage/delta": {
        const delta = typeof params.delta === "string" ? params.delta : "";
        if (delta) active.emit({ type: "text-delta", text: delta });
        break;
      }
      case "item/started":
      case "item/completed": {
        this.handleItemNotification(method, params, active);
        break;
      }
      case "turn/completed": {
        const turn = (typeof params.turn === "object" && params.turn !== null ? params.turn : {}) as {
          items?: Array<{ type?: string; text?: string }>;
        };
        const finalMessage = [...(turn.items ?? [])].reverse().find((item) => item.type === "agentMessage");
        const finalText = typeof finalMessage?.text === "string" ? finalMessage.text : active.finalText;
        const usage = active.turnUsage;
        const nativeSessionId = active.threadId;
        const rolloutPath = active.rolloutPath;
        this.activeTurn = null;
        active.emit({ type: "turn-completed", finalText, usage, nativeSessionId, rolloutPath });
        active.resolve();
        break;
      }
      case "turn/failed": {
        const error = typeof params.error === "string" ? params.error : "codex turn failed";
        this.activeTurn = null;
        active.emit({ type: "turn-failed", error });
        active.reject(new Error(error));
        break;
      }
      default:
        break;
    }
  }

  private handleItemNotification(method: string, params: Record<string, unknown>, active: ActiveCodexTurn): void {
    const item = (typeof params.item === "object" && params.item !== null ? params.item : {}) as {
      type?: string;
      text?: string;
      command?: string;
      status?: string;
      exitCode?: number | null;
      changes?: Array<{ path?: string }>;
    };
    if (item.type === "agentMessage" && typeof item.text === "string" && item.text.length > 0) {
      active.finalText = item.text;
      if (method === "item/completed") active.emit({ type: "assistant-text", text: item.text });
      return;
    }
    if (item.type === "commandExecution") {
      const command = typeof item.command === "string" ? item.command : "";
      if (method === "item/started") {
        active.emit({ type: "tool-started", tool: "shell", detail: command });
      } else if (item.status === "completed" || item.status === "failed") {
        active.emit({
          type: "tool-completed",
          tool: "shell",
          detail: command,
          exitCode: typeof item.exitCode === "number" ? item.exitCode : null,
        });
      }
      return;
    }
    if (item.type === "fileChange") {
      const paths = (item.changes ?? [])
        .map((change) => change.path)
        .filter((path): path is string => typeof path === "string")
        .join(", ");
      if (method === "item/started") {
        active.emit({ type: "tool-started", tool: "edit", detail: paths });
      } else {
        active.emit({ type: "tool-completed", tool: "edit", detail: paths, exitCode: null });
      }
    }
  }
}
