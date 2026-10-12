import { randomUUID } from "node:crypto";
import { query as claudeQuery, type Query } from "@anthropic-ai/claude-agent-sdk";
import type { NativeSessionRef, TurnUsage } from "../types.ts";
import type { BatonAdapter, TurnEvent, TurnEventEmitter, TurnStartInput } from "./types.ts";

type AssistantContentBlock = {
  type?: string;
  text?: string;
  name?: string;
  input?: unknown;
  thinking?: string;
};

type AssistantMessage = {
  type?: string;
  message?: {
    model?: string;
    usage?: {
      input_tokens?: number;
      output_tokens?: number;
      cache_read_input_tokens?: number;
      cache_creation_input_tokens?: number;
    };
    content?: AssistantContentBlock[];
  };
};

type ResultMessage = {
  type?: string;
  subtype?: string;
  is_error?: boolean;
  result?: string;
  session_id?: string;
  total_cost_usd?: number;
  usage?: {
    input_tokens?: number;
    output_tokens?: number;
    cache_read_input_tokens?: number;
    cache_creation_input_tokens?: number;
  };
};

type StreamEventMessage = {
  type?: string;
  parent_tool_use_id?: string | null;
  event?: {
    type?: string;
    delta?: {
      type?: string;
      text?: string;
    };
  };
};

export class ClaudeAdapter implements BatonAdapter {
  readonly id = "claude" as const;

  private activeQuery: Query | null = null;

  constructor(
    private readonly queryFn: (options: unknown) => Query = (options) => claudeQuery(options as Parameters<typeof claudeQuery>[0]),
  ) {}

  async startTurn(input: TurnStartInput, session: NativeSessionRef | null, emit: TurnEventEmitter): Promise<void> {
    const resolvedQuery = this.queryFn;
    const mintSessionId = randomUUID();
    const options: Record<string, unknown> = {
      model: input.model,
      cwd: input.cwd,
      permissionMode: input.permissionMode,
      env: { ...input.authEnv },
      includePartialMessages: true,
      canUseTool: async (toolName: string, toolInput: Record<string, unknown>) => {
        const decision = await new Promise<string>((resolve) => {
          emit({
            type: "approval-request",
            id: `claude:${toolName}:${randomUUID()}`,
            kind: "command",
            summary: approvalSummaryForTool(toolName, toolInput),
            respond: async (choice) => {
              resolve(choice);
            },
          });
        });
        if (decision === "deny") {
          return { behavior: "deny", message: "Denied by user in baton", updatedInput: toolInput };
        }
        return { behavior: "allow", updatedInput: toolInput };
      },
    };
    if (session) {
      options.resume = session.nativeSessionId;
    } else {
      options.sessionId = mintSessionId;
    }

    const conversation = resolvedQuery({ prompt: input.prompt, options });
    this.activeQuery = conversation;
    let finalText = "";
    let usage: TurnUsage | null = null;
    let sessionId = session?.nativeSessionId ?? mintSessionId;
    let sawStreamEvents = false;
    try {
      for await (const message of conversation) {
        const typed = message as AssistantMessage & ResultMessage & StreamEventMessage;
        if (typed.type === "stream_event") {
          const delta = typed.event?.delta;
          if (typed.event?.type === "content_block_delta" && delta?.type === "text_delta" && typeof delta.text === "string") {
            sawStreamEvents = true;
            if (!typed.parent_tool_use_id && delta.text.length > 0) {
              emit({ type: "text-delta", text: delta.text });
            }
          }
          continue;
        }
        if (typed.type === "assistant" && typed.message) {
          for (const block of typed.message.content ?? []) {
            if (
              block.type === "text" &&
              typeof block.text === "string" &&
              block.text.length > 0 &&
              !sawStreamEvents
            ) {
              emit({ type: "text-delta", text: block.text });
            } else if (block.type === "tool_use" && typeof block.name === "string") {
              emit({ type: "tool-started", tool: block.name, detail: toolInputSummary(block.input) });
            }
          }
          const messageUsage = typed.message.usage;
          if (messageUsage) {
            usage = {
              inputTokens: messageUsage.input_tokens ?? 0,
              outputTokens: messageUsage.output_tokens ?? 0,
              cacheReadTokens: messageUsage.cache_read_input_tokens ?? 0,
              cacheCreationTokens: messageUsage.cache_creation_input_tokens ?? 0,
              costUsd: null,
            };
            emit({ type: "usage", usage });
          }
        } else if (typed.type === "result") {
          if (typeof typed.result === "string") finalText = typed.result;
          if (typeof typed.session_id === "string" && typed.session_id.length > 0) sessionId = typed.session_id;
          const resultUsage = typed.usage;
          usage = {
            inputTokens: resultUsage?.input_tokens ?? 0,
            outputTokens: resultUsage?.output_tokens ?? 0,
            cacheReadTokens: resultUsage?.cache_read_input_tokens ?? 0,
            cacheCreationTokens: resultUsage?.cache_creation_input_tokens ?? 0,
            costUsd: typeof typed.total_cost_usd === "number" ? typed.total_cost_usd : null,
          };
          if (typed.is_error) {
            emit({ type: "turn-failed", error: finalText || "claude turn ended with an error" });
            return;
          }
        }
      }
    } finally {
      this.activeQuery = null;
    }
    emit({ type: "turn-completed", finalText, usage, nativeSessionId: sessionId, rolloutPath: null });
  }

  async interrupt(): Promise<void> {
    const conversation = this.activeQuery;
    if (!conversation) return;
    try {
      await conversation.interrupt();
    } catch {
      this.activeQuery = null;
    }
  }

  async dispose(): Promise<void> {
    this.activeQuery = null;
  }
}

function approvalSummaryForTool(toolName: string, input: Record<string, unknown>): string {
  const command = typeof input.command === "string" ? input.command : null;
  const filePath = typeof input.file_path === "string" ? input.file_path : null;
  if (command) return command;
  if (filePath) return filePath;
  return toolName;
}

function toolInputSummary(input: unknown): string | null {
  if (typeof input !== "object" || input === null) return null;
  const record = input as Record<string, unknown>;
  if (typeof record.command === "string") return record.command;
  if (typeof record.file_path === "string") return record.file_path;
  if (typeof record.path === "string") return record.path;
  if (typeof record.pattern === "string") return record.pattern;
  return null;
}
