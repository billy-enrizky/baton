import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import type { ProviderId } from "./types.ts";

export interface DiscoveredMessage {
  role: "user" | "assistant";
  text: string;
  createdAt: string;
}

export interface DiscoveredSession {
  provider: ProviderId;
  nativeSessionId: string;
  title: string;
  model: string | null;
  cwd: string | null;
  path: string;
  lastActiveAtMs: number;
  messages: DiscoveredMessage[];
}

const MAX_RECORDS = 4000;
const MAX_MESSAGES = 200;
const MAX_SCAN_FILES = 500;

interface TranscriptRecord {
  type?: unknown;
  isSidechain?: unknown;
  isMeta?: unknown;
  isCompactSummary?: unknown;
  sessionId?: unknown;
  aiTitle?: unknown;
  cwd?: unknown;
  timestamp?: unknown;
  message?: {
    model?: unknown;
    role?: unknown;
    content?: unknown;
  };
  payload?: {
    type?: unknown;
    id?: unknown;
    session_id?: unknown;
    cwd?: unknown;
    model?: unknown;
    role?: unknown;
    message?: unknown;
    content?: unknown;
    internal_chat_message_metadata_passthrough?: unknown;
  };
}

interface ContentBlock {
  type?: unknown;
  text?: unknown;
}

function asRecord(value: unknown): TranscriptRecord | null {
  if (typeof value !== "object" || value === null) return null;
  return value as TranscriptRecord;
}

function extractText(content: unknown): string {
  if (typeof content === "string") return content.trim();
  if (!Array.isArray(content)) return "";
  return (content as ContentBlock[])
    .filter(
      (block) =>
        block?.type === "text" || block?.type === "input_text" || block?.type === "output_text",
    )
    .map((block) => (typeof block.text === "string" ? block.text.trim() : ""))
    .filter((text) => text.length > 0)
    .join("\n");
}

function normalizeTimestamp(value: unknown, fallback: string): string {
  if (typeof value !== "string" || value.length === 0) return fallback;
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? fallback : parsed.toISOString();
}

function codexTurnId(metadata: unknown): string | null {
  if (typeof metadata !== "object" || metadata === null) return null;
  const turnId = (metadata as { turn_id?: unknown }).turn_id;
  return typeof turnId === "string" && turnId.trim().length > 0 ? turnId : null;
}

interface RetainedMessage extends DiscoveredMessage {
  codexResponseUser: boolean;
}

export function parseClaudeTranscript(
  contents: string,
  meta: { path: string; lastActiveAtMs: number; fallbackSessionId: string },
): DiscoveredSession | null {
  const lines = contents.split("\n").filter((line) => line.trim().length > 0);
  if (lines.length === 0 || lines.length > MAX_RECORDS) return null;
  const fallbackTimestamp = new Date(meta.lastActiveAtMs).toISOString();
  let providerSessionId = meta.fallbackSessionId;
  let title: string | null = null;
  let model: string | null = null;
  let cwd: string | null = null;
  const messages: RetainedMessage[] = [];
  let firstUserMessage: RetainedMessage | undefined;

  for (const line of lines) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }
    const record = asRecord(parsed);
    if (!record) continue;
    if (record.isSidechain === true || record.isMeta === true || record.isCompactSummary === true) {
      continue;
    }
    if (typeof record.sessionId === "string" && record.sessionId.trim().length > 0) {
      providerSessionId = record.sessionId.trim();
    }
    if (typeof record.aiTitle === "string" && record.aiTitle.trim().length > 0) {
      title = record.aiTitle.trim();
    }
    if (typeof record.cwd === "string" && record.cwd.trim().length > 0) {
      cwd = record.cwd.trim();
    }
    const messageModel = typeof record.message?.model === "string" ? record.message.model.trim() : "";
    if (messageModel && messageModel !== "<synthetic>") model = messageModel;
    if (record.type !== "user" && record.type !== "assistant") continue;
    const text = extractText(record.message?.content);
    if (text.length === 0) continue;
    const retained: RetainedMessage = {
      role: record.type === "assistant" ? "assistant" : "user",
      text,
      createdAt: normalizeTimestamp(record.timestamp, fallbackTimestamp),
      codexResponseUser: false,
    };
    if (firstUserMessage === undefined && retained.role === "user") firstUserMessage = retained;
    messages.push(retained);
    if (messages.length > MAX_MESSAGES) messages.shift();
  }

  return finishSession({
    provider: "claude",
    providerSessionId,
    title,
    model,
    cwd,
    messages,
    firstUserMessage,
    fallbackTimestamp,
    path: meta.path,
    lastActiveAtMs: meta.lastActiveAtMs,
  });
}

export function parseCodexTranscript(
  contents: string,
  meta: { path: string; lastActiveAtMs: number },
): DiscoveredSession | null {
  const lines = contents.split("\n").filter((line) => line.trim().length > 0);
  if (lines.length === 0 || lines.length > MAX_RECORDS) return null;
  const fallbackTimestamp = new Date(meta.lastActiveAtMs).toISOString();
  let providerSessionId = "";
  let hasCodexSessionId = false;
  let model: string | null = null;
  let cwd: string | null = null;
  const messages: RetainedMessage[] = [];
  let firstUserMessage: RetainedMessage | undefined;

  const records: Array<{ record: TranscriptRecord; index: number }> = [];
  for (const line of lines) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }
    const record = asRecord(parsed);
    if (record) records.push({ record, index: records.length });
  }

  const canonicalCodexResponseUserIndices = new Set<number>();
  let canonicalUserTextsInTurn = new Set<string>();
  let responseUsersInTurn: Array<{ index: number; turnId: string; text: string }> = [];
  const finishCodexTurn = () => {
    const canonicalTurnIds = new Set(
      responseUsersInTurn.flatMap((responseUser) =>
        canonicalUserTextsInTurn.has(responseUser.text) ? [responseUser.turnId] : [],
      ),
    );
    for (const responseUser of responseUsersInTurn) {
      if (canonicalTurnIds.has(responseUser.turnId)) {
        canonicalCodexResponseUserIndices.add(responseUser.index);
      }
    }
    canonicalUserTextsInTurn = new Set();
    responseUsersInTurn = [];
  };
  for (const { record } of records) {
    if (
      record.type === "response_item" &&
      record.payload?.type === "message" &&
      record.payload.role === "assistant"
    ) {
      finishCodexTurn();
      continue;
    }
    if (record.type === "event_msg" && record.payload?.type === "user_message") {
      const text = typeof record.payload.message === "string" ? record.payload.message.trim() : "";
      if (text.length > 0) canonicalUserTextsInTurn.add(text);
      continue;
    }
    if (
      record.type === "response_item" &&
      record.payload?.type === "message" &&
      record.payload.role === "user"
    ) {
      const turnId = codexTurnId(record.payload.internal_chat_message_metadata_passthrough);
      const text = extractText(record.payload.content);
      if (turnId !== null && text.length > 0) {
        responseUsersInTurn.push({ index: -1, turnId, text });
      }
    }
  }
  finishCodexTurn();
  const responseUserTextsByTurn = responseUsersInTurn;

  for (const { record, index } of records) {
    if (record.type === "session_meta") {
      const sessionId =
        (typeof record.payload?.id === "string" ? record.payload.id.trim() : "") ||
        (typeof record.payload?.session_id === "string" ? record.payload.session_id.trim() : "");
      if (!hasCodexSessionId && sessionId) {
        providerSessionId = sessionId;
        hasCodexSessionId = true;
      }
      if (typeof record.payload?.cwd === "string" && record.payload.cwd.trim().length > 0) {
        cwd = record.payload.cwd.trim();
      }
      continue;
    }
    if (record.type === "turn_context") {
      const contextModel = typeof record.payload?.model === "string" ? record.payload.model.trim() : "";
      if (contextModel) model = contextModel;
      if (typeof record.payload?.cwd === "string" && record.payload.cwd.trim().length > 0) {
        cwd = record.payload.cwd.trim();
      }
      continue;
    }
    if (record.type === "event_msg" && record.payload?.type === "user_message") {
      const text = typeof record.payload.message === "string" ? record.payload.message : "";
      if (text.trim().length === 0) continue;
      for (let i = messages.length - 1; i >= 0; i--) {
        const message = messages[i];
        if (!message) break;
        if (message.role === "assistant") break;
        if (message.codexResponseUser && message.text.trim() === text.trim()) {
          if (firstUserMessage === message) firstUserMessage = undefined;
          messages.splice(i, 1);
          break;
        }
      }
      const retained: RetainedMessage = {
        role: "user",
        text,
        createdAt: normalizeTimestamp(record.timestamp, fallbackTimestamp),
        codexResponseUser: false,
      };
      if (firstUserMessage === undefined) firstUserMessage = retained;
      messages.push(retained);
      if (messages.length > MAX_MESSAGES) messages.shift();
      continue;
    }
    if (
      record.type !== "response_item" ||
      record.payload?.type !== "message" ||
      (record.payload.role !== "user" && record.payload.role !== "assistant")
    ) {
      continue;
    }
    const extractedText = extractText(record.payload.content);
    if (extractedText.length === 0) continue;
    const role = record.payload.role;
    if (role === "user") {
      const turnId = codexTurnId(record.payload.internal_chat_message_metadata_passthrough);
      const duplicated =
        (turnId !== null &&
          responseUserTextsByTurn.some(
            (candidate) => candidate.turnId === turnId && candidate.text === extractedText,
          )) ||
        hasMatchingCodexEventInTurn(messages, extractedText);
      if (duplicated) continue;
    }
    const retained: RetainedMessage = {
      role: role === "assistant" ? "assistant" : "user",
      text: extractedText,
      createdAt: normalizeTimestamp(record.timestamp, fallbackTimestamp),
      codexResponseUser: role === "user",
    };
    if (firstUserMessage === undefined && retained.role === "user") firstUserMessage = retained;
    messages.push(retained);
    if (messages.length > MAX_MESSAGES) messages.shift();
  }

  return finishSession({
    provider: "codex",
    providerSessionId,
    title: null,
    model,
    cwd,
    messages,
    firstUserMessage,
    fallbackTimestamp,
    path: meta.path,
    lastActiveAtMs: meta.lastActiveAtMs,
  });
}

function hasMatchingCodexEventInTurn(messages: ReadonlyArray<RetainedMessage>, text: string): boolean {
  const comparisonText = text.trim();
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index];
    if (!message) continue;
    if (message.role === "assistant") return false;
    if (message.role === "user" && !message.codexResponseUser && message.text.trim() === comparisonText) {
      return true;
    }
  }
  return false;
}

function finishSession(input: {
  provider: ProviderId;
  providerSessionId: string;
  title: string | null;
  model: string | null;
  cwd: string | null;
  messages: RetainedMessage[];
  firstUserMessage: RetainedMessage | undefined;
  fallbackTimestamp: string;
  path: string;
  lastActiveAtMs: number;
}): DiscoveredSession | null {
  if (input.providerSessionId.trim().length === 0 || input.firstUserMessage === undefined) return null;
  const firstUserMessageRetained = input.messages.includes(input.firstUserMessage);
  const visibleMessages: DiscoveredMessage[] = input.messages.map(
    ({ codexResponseUser: _codexResponseUser, ...message }) => message,
  );
  const visibleFirstUserMessage: DiscoveredMessage = input.firstUserMessage;
  const retainedMessages = firstUserMessageRetained
    ? visibleMessages
    : [visibleFirstUserMessage, ...visibleMessages.slice(-(MAX_MESSAGES - 1))];
  const titleCandidate =
    input.title ??
    retainedMessages
      .filter((message) => message.role === "user" && !isContextInjection(message.text))
      .map((message) => message.text.trim().split("\n")[0]?.slice(0, 100).trim() ?? "")
      .find((line) => line.length > 0);
  return {
    provider: input.provider,
    nativeSessionId: input.providerSessionId,
    title: titleCandidate && titleCandidate.length > 0 ? titleCandidate : "Imported thread",
    model: input.model,
    cwd: input.cwd,
    path: input.path,
    lastActiveAtMs: input.lastActiveAtMs,
    messages: retainedMessages,
  };
}

function isContextInjection(text: string): boolean {
  const trimmed = text.trimStart();
  return trimmed.startsWith("# AGENTS.md") || trimmed.startsWith("<environment_context>");
}

function listTranscriptFiles(root: string, matches: (path: string) => boolean): string[] {
  let entries: string[];
  try {
    entries = readdirSync(root, { recursive: true, encoding: "utf8" }) as unknown as string[];
  } catch {
    return [];
  }
  return entries
    .filter((entry) => typeof entry === "string")
    .map((entry) => join(root, entry))
    .filter((path) => {
      if (!path.endsWith(".jsonl") || !matches(path)) return false;
      try {
        return statSync(path).isFile();
      } catch {
        return false;
      }
    });
}

export function scanClaudeSessions(projectsDir: string, limit = 50): DiscoveredSession[] {
  const files = listTranscriptFiles(projectsDir, () => true)
    .map((path) => ({ path, mtimeMs: statMtime(path) }))
    .sort((a, b) => b.mtimeMs - a.mtimeMs)
    .slice(0, Math.min(limit, MAX_SCAN_FILES));
  const sessions: DiscoveredSession[] = [];
  for (const file of files) {
    const fallbackSessionId = file.path.split("/").pop()?.replace(/\.jsonl$/, "") ?? "";
    const session = parseClaudeTranscript(readFileOrNull(file.path) ?? "", {
      path: file.path,
      lastActiveAtMs: file.mtimeMs,
      fallbackSessionId,
    });
    if (session) sessions.push(session);
  }
  return dedupeSessions(sessions).slice(0, limit);
}

export function scanCodexSessions(sessionsDir: string, limit = 50): DiscoveredSession[] {
  const files = listTranscriptFiles(sessionsDir, (path) => path.includes("rollout-"))
    .map((path) => ({ path, mtimeMs: statMtime(path) }))
    .sort((a, b) => b.mtimeMs - a.mtimeMs)
    .slice(0, Math.min(limit, MAX_SCAN_FILES));
  const sessions: DiscoveredSession[] = [];
  for (const file of files) {
    const session = parseCodexTranscript(readFileOrNull(file.path) ?? "", {
      path: file.path,
      lastActiveAtMs: file.mtimeMs,
    });
    if (session) sessions.push(session);
  }
  return dedupeSessions(sessions).slice(0, limit);
}

function dedupeSessions(sessions: DiscoveredSession[]): DiscoveredSession[] {
  const seen = new Set<string>();
  const unique: DiscoveredSession[] = [];
  for (const session of sessions) {
    const key = `${session.provider}:${session.nativeSessionId}`;
    if (seen.has(key)) continue;
    seen.add(key);
    unique.push(session);
  }
  return unique;
}

function statMtime(path: string): number {
  try {
    return statSync(path).mtimeMs;
  } catch {
    return 0;
  }
}

function readFileOrNull(path: string): string | null {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return null;
  }
}
