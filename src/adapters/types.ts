import type { ApprovalDecision, NativeSessionRef, PermissionMode, ProviderId, TurnUsage } from "../types.ts";

export interface TurnStartInput {
  threadId: string;
  cwd: string;
  model: string;
  prompt: string;
  permissionMode: PermissionMode;
  authEnv: Record<string, string>;
}

export type TurnEvent =
  | { type: "text-delta"; text: string }
  | { type: "assistant-text"; text: string }
  | { type: "tool-started"; tool: string; detail: string | null }
  | { type: "tool-completed"; tool: string; detail: string | null; exitCode: number | null }
  | {
      type: "approval-request";
      id: string;
      kind: "command" | "fileChange" | "permissions";
      summary: string;
      respond: (decision: ApprovalDecision) => Promise<void>;
    }
  | { type: "usage"; usage: TurnUsage }
  | { type: "turn-completed"; finalText: string; usage: TurnUsage | null; nativeSessionId: string | null; rolloutPath: string | null }
  | { type: "turn-failed"; error: string };

export type TurnEventEmitter = (event: TurnEvent) => void;

export interface BatonAdapter {
  readonly id: ProviderId;
  startTurn(input: TurnStartInput, session: NativeSessionRef | null, emit: TurnEventEmitter): Promise<void>;
  interrupt(): Promise<void>;
  dispose(): Promise<void>;
}
