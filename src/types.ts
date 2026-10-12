export type ProviderId = "claude" | "codex";

export const PROVIDER_IDS: readonly ProviderId[] = ["claude", "codex"];

export function isProviderId(value: string): value is ProviderId {
  return (PROVIDER_IDS as readonly string[]).includes(value);
}

export type PermissionMode = "default" | "acceptEdits" | "bypassPermissions" | "plan";

export const PERMISSION_MODES: readonly PermissionMode[] = [
  "default",
  "acceptEdits",
  "bypassPermissions",
  "plan",
];

export function isPermissionMode(value: string): value is PermissionMode {
  return (PERMISSION_MODES as readonly string[]).includes(value);
}

export type ApprovalDecision = "accept" | "acceptAlways" | "deny";

export interface TurnUsage {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number | null;
}

export interface NativeSessionRef {
  nativeSessionId: string;
  rolloutPath: string | null;
}
