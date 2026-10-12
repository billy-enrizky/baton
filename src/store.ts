import { randomUUID } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { mkdirSync } from "node:fs";
import { dirname } from "node:path";
import type { ProviderId } from "./types.ts";

export interface ThreadRow {
  id: string;
  title: string;
  cwd: string;
  currentProvider: ProviderId;
  currentModel: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface ProviderSessionRow {
  threadId: string;
  provider: ProviderId;
  nativeSessionId: string;
  model: string | null;
  rolloutPath: string | null;
  updatedAt: string;
}

export interface MessageRow {
  id: number;
  threadId: string;
  turn: number;
  role: "user" | "assistant";
  provider: string | null;
  model: string | null;
  text: string;
  createdAt: string;
}

export interface CheckpointRow {
  threadId: string;
  turn: number;
  gitRef: string;
  createdAt: string;
}

export interface UsageRow {
  id: number;
  threadId: string;
  turn: number;
  provider: ProviderId;
  model: string | null;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number | null;
  createdAt: string;
}

export interface NewThread {
  title: string;
  cwd: string;
  currentProvider: ProviderId;
  currentModel: string | null;
}

export interface ImportThread {
  title: string;
  cwd: string;
  currentProvider: ProviderId;
  currentModel: string | null;
  nativeSessionId: string;
  rolloutPath: string | null;
  messages: ReadonlyArray<{
    role: "user" | "assistant";
    provider?: string | null;
    model?: string | null;
    text: string;
    createdAt?: string;
  }>;
}

const MIGRATIONS = `
CREATE TABLE IF NOT EXISTS threads (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  cwd TEXT NOT NULL,
  current_provider TEXT NOT NULL,
  current_model TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS provider_sessions (
  thread_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  native_session_id TEXT NOT NULL,
  model TEXT,
  rollout_path TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (thread_id, provider)
);
CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  thread_id TEXT NOT NULL,
  turn INTEGER NOT NULL,
  role TEXT NOT NULL,
  provider TEXT,
  model TEXT,
  text TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_thread_idx ON messages (thread_id, id);
CREATE TABLE IF NOT EXISTS checkpoints (
  thread_id TEXT NOT NULL,
  turn INTEGER NOT NULL,
  git_ref TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (thread_id, turn)
);
CREATE TABLE IF NOT EXISTS usage (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  thread_id TEXT NOT NULL,
  turn INTEGER NOT NULL,
  provider TEXT NOT NULL,
  model TEXT,
  input_tokens INTEGER NOT NULL,
  output_tokens INTEGER NOT NULL,
  cache_read_tokens INTEGER NOT NULL,
  cache_creation_tokens INTEGER NOT NULL,
  cost_usd REAL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS usage_thread_idx ON usage (thread_id, id);
`;

type SqlRow = Record<string, unknown>;

function str(value: unknown): string {
  return typeof value === "string" ? value : String(value ?? "");
}

function nullableStr(value: unknown): string | null {
  if (value === null || value === undefined) return null;
  return typeof value === "string" ? value : String(value);
}

function num(value: unknown): number {
  const parsed = typeof value === "number" ? value : Number(value ?? 0);
  return Number.isFinite(parsed) ? parsed : 0;
}

function nullableNum(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  const parsed = typeof value === "number" ? value : Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function mapThread(row: SqlRow): ThreadRow {
  return {
    id: str(row.id),
    title: str(row.title),
    cwd: str(row.cwd),
    currentProvider: str(row.current_provider) as ProviderId,
    currentModel: nullableStr(row.current_model),
    createdAt: str(row.created_at),
    updatedAt: str(row.updated_at),
  };
}

function mapSession(row: SqlRow): ProviderSessionRow {
  return {
    threadId: str(row.thread_id),
    provider: str(row.provider) as ProviderId,
    nativeSessionId: str(row.native_session_id),
    model: nullableStr(row.model),
    rolloutPath: nullableStr(row.rollout_path),
    updatedAt: str(row.updated_at),
  };
}

function mapMessage(row: SqlRow): MessageRow {
  const role = str(row.role);
  return {
    id: num(row.id),
    threadId: str(row.thread_id),
    turn: num(row.turn),
    role: role === "assistant" ? "assistant" : "user",
    provider: nullableStr(row.provider),
    model: nullableStr(row.model),
    text: str(row.text),
    createdAt: str(row.created_at),
  };
}

function mapCheckpoint(row: SqlRow): CheckpointRow {
  return {
    threadId: str(row.thread_id),
    turn: num(row.turn),
    gitRef: str(row.git_ref),
    createdAt: str(row.created_at),
  };
}

function mapUsage(row: SqlRow): UsageRow {
  return {
    id: num(row.id),
    threadId: str(row.thread_id),
    turn: num(row.turn),
    provider: str(row.provider) as ProviderId,
    model: nullableStr(row.model),
    inputTokens: num(row.input_tokens),
    outputTokens: num(row.output_tokens),
    cacheReadTokens: num(row.cache_read_tokens),
    cacheCreationTokens: num(row.cache_creation_tokens),
    costUsd: nullableNum(row.cost_usd),
    createdAt: str(row.created_at),
  };
}

function migrateThreadModelNullable(db: DatabaseSync): void {
  const column = db
    .prepare(`SELECT "notnull" AS is_notnull FROM pragma_table_info('threads') WHERE name = 'current_model'`)
    .get() as { is_notnull: number } | undefined;
  if (!column || column.is_notnull === 0) return;
  db.exec(`
    CREATE TABLE threads_migrated (
      id TEXT PRIMARY KEY,
      title TEXT NOT NULL,
      cwd TEXT NOT NULL,
      current_provider TEXT NOT NULL,
      current_model TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL
    );
    INSERT INTO threads_migrated (id, title, cwd, current_provider, current_model, created_at, updated_at)
      SELECT id, title, cwd, current_provider,
        CASE WHEN current_model IN ('gpt-6.1-sol', 'claude-sonnet-4-5') THEN NULL ELSE current_model END,
        created_at, updated_at
      FROM threads;
    DROP TABLE threads;
    ALTER TABLE threads_migrated RENAME TO threads;
  `);
}

export class BatonStore {
  private constructor(private readonly db: DatabaseSync) {}
  static open(path: string): BatonStore {
    mkdirSync(dirname(path), { recursive: true });
    const db = new DatabaseSync(path);
    db.exec(MIGRATIONS);
    migrateThreadModelNullable(db);
    return new BatonStore(db);
  }

  static memory(): BatonStore {
    const db = new DatabaseSync(":memory:");
    db.exec(MIGRATIONS);
    migrateThreadModelNullable(db);
    return new BatonStore(db);
  }

  close(): void {
    this.db.close();
  }

  createThread(input: NewThread, id: string = randomUUID()): ThreadRow {
    const now = new Date().toISOString();
    const thread: ThreadRow = {
      id,
      title: input.title,
      cwd: input.cwd,
      currentProvider: input.currentProvider,
      currentModel: input.currentModel,
      createdAt: now,
      updatedAt: now,
    };
    this.db
      .prepare(
        "INSERT INTO threads (id, title, cwd, current_provider, current_model, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
      )
      .run(
        thread.id,
        thread.title,
        thread.cwd,
        thread.currentProvider,
        thread.currentModel,
        thread.createdAt,
        thread.updatedAt,
      );
    return thread;
  }

  importThread(input: ImportThread): ThreadRow {
    const now = new Date().toISOString();
    const thread: ThreadRow = {
      id: randomUUID(),
      title: input.title,
      cwd: input.cwd,
      currentProvider: input.currentProvider,
      currentModel: input.currentModel,
      createdAt: now,
      updatedAt: now,
    };
    this.db.exec("BEGIN");
    try {
      this.db
        .prepare(
          "INSERT INTO threads (id, title, cwd, current_provider, current_model, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .run(
          thread.id,
          thread.title,
          thread.cwd,
          thread.currentProvider,
          thread.currentModel,
          thread.createdAt,
          thread.updatedAt,
        );
      this.db
        .prepare(
          "INSERT INTO provider_sessions (thread_id, provider, native_session_id, model, rollout_path, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .run(thread.id, input.currentProvider, input.nativeSessionId, input.currentModel, input.rolloutPath, now);
      const insertMessage = this.db.prepare(
        "INSERT INTO messages (thread_id, turn, role, provider, model, text, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
      );
      let turn = 0;
      for (const message of input.messages) {
        if (message.role === "user") turn += 1;
        insertMessage.run(
          thread.id,
          turn,
          message.role,
          message.provider ?? null,
          message.model ?? null,
          message.text,
          message.createdAt ?? now,
        );
      }
      this.db.exec("COMMIT");
    } catch (error) {
      this.db.exec("ROLLBACK");
      throw error;
    }
    return thread;
  }

  listThreads(cwd?: string): ThreadRow[] {
    const rows = cwd
      ? this.db
          .prepare("SELECT * FROM threads WHERE cwd = ? ORDER BY updated_at DESC, rowid DESC")
          .all(cwd)
      : this.db.prepare("SELECT * FROM threads ORDER BY updated_at DESC, rowid DESC").all();
    return rows.map((row) => mapThread(row as SqlRow));
  }

  getThread(id: string): ThreadRow | null {
    const row = this.db.prepare("SELECT * FROM threads WHERE id = ?").get(id) as SqlRow | undefined;
    return row ? mapThread(row) : null;
  }

  draftThread(input: NewThread): ThreadRow {
    const now = new Date().toISOString();
    return {
      id: randomUUID(),
      title: input.title,
      cwd: input.cwd,
      currentProvider: input.currentProvider,
      currentModel: input.currentModel,
      createdAt: now,
      updatedAt: now,
    };
  }

  rewindThread(threadId: string, fromTurn: number): void {
    this.db.exec("BEGIN");
    try {
      this.db.prepare("DELETE FROM messages WHERE thread_id = ? AND turn >= ?").run(threadId, fromTurn);
      this.db.prepare("DELETE FROM usage WHERE thread_id = ? AND turn >= ?").run(threadId, fromTurn);
      this.db.prepare("DELETE FROM provider_sessions WHERE thread_id = ?").run(threadId);
      this.db.prepare("UPDATE threads SET updated_at = ? WHERE id = ?").run(new Date().toISOString(), threadId);
      this.db.exec("COMMIT");
    } catch (error) {
      this.db.exec("ROLLBACK");
      throw error;
    }
  }

  deleteThread(id: string): void {
    this.db.exec("BEGIN");
    try {
      this.db.prepare("DELETE FROM threads WHERE id = ?").run(id);
      this.db.prepare("DELETE FROM provider_sessions WHERE thread_id = ?").run(id);
      this.db.prepare("DELETE FROM messages WHERE thread_id = ?").run(id);
      this.db.prepare("DELETE FROM checkpoints WHERE thread_id = ?").run(id);
      this.db.prepare("DELETE FROM usage WHERE thread_id = ?").run(id);
      this.db.exec("COMMIT");
    } catch (error) {
      this.db.exec("ROLLBACK");
      throw error;
    }
  }

  renameThread(id: string, title: string): void {
    this.db
      .prepare("UPDATE threads SET title = ?, updated_at = ? WHERE id = ?")
      .run(title, new Date().toISOString(), id);
  }

  setThreadProvider(id: string, provider: ProviderId, model: string | null): void {
    this.db
      .prepare("UPDATE threads SET current_provider = ?, current_model = ?, updated_at = ? WHERE id = ?")
      .run(provider, model, new Date().toISOString(), id);
  }

  setThreadModel(id: string, model: string): void {
    this.db
      .prepare("UPDATE threads SET current_model = ?, updated_at = ? WHERE id = ?")
      .run(model, new Date().toISOString(), id);
  }

  touchThread(id: string): void {
    this.db.prepare("UPDATE threads SET updated_at = ? WHERE id = ?").run(new Date().toISOString(), id);
  }

  upsertProviderSession(
    threadId: string,
    provider: ProviderId,
    nativeSessionId: string,
    model: string | null,
    rolloutPath: string | null,
  ): void {
    this.db
      .prepare(
        `INSERT INTO provider_sessions (thread_id, provider, native_session_id, model, rollout_path, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (thread_id, provider) DO UPDATE SET
           native_session_id = excluded.native_session_id,
           model = COALESCE(excluded.model, model),
           rollout_path = COALESCE(excluded.rollout_path, rollout_path),
           updated_at = excluded.updated_at`,
      )
      .run(threadId, provider, nativeSessionId, model, rolloutPath, new Date().toISOString());
  }

  getProviderSession(threadId: string, provider: ProviderId): ProviderSessionRow | null {
    const row = this.db
      .prepare("SELECT * FROM provider_sessions WHERE thread_id = ? AND provider = ?")
      .get(threadId, provider) as SqlRow | undefined;
    return row ? mapSession(row) : null;
  }

  listProviderSessions(threadId: string): ProviderSessionRow[] {
    const rows = this.db
      .prepare("SELECT * FROM provider_sessions WHERE thread_id = ? ORDER BY provider")
      .all(threadId);
    return rows.map((row) => mapSession(row as SqlRow));
  }

  findThreadByNativeSession(provider: ProviderId, nativeSessionId: string): ThreadRow | null {
    const row = this.db
      .prepare(
        `SELECT t.* FROM threads t
         JOIN provider_sessions s ON s.thread_id = t.id
         WHERE s.provider = ? AND s.native_session_id = ?`,
      )
      .get(provider, nativeSessionId) as SqlRow | undefined;
    return row ? mapThread(row) : null;
  }

  nextTurnNumber(threadId: string): number {
    const row = this.db
      .prepare("SELECT COALESCE(MAX(turn), 0) AS max_turn FROM messages WHERE thread_id = ?")
      .get(threadId) as SqlRow | undefined;
    return num(row?.max_turn) + 1;
  }

  addMessage(input: {
    threadId: string;
    turn: number;
    role: "user" | "assistant";
    provider?: string | null;
    model?: string | null;
    text: string;
  }): MessageRow {
    const now = new Date().toISOString();
    const result = this.db
      .prepare(
        "INSERT INTO messages (thread_id, turn, role, provider, model, text, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
      )
      .run(input.threadId, input.turn, input.role, input.provider ?? null, input.model ?? null, input.text, now);
    const id = Number(result.lastInsertRowid ?? 0);
    this.touchThread(input.threadId);
    return {
      id,
      threadId: input.threadId,
      turn: input.turn,
      role: input.role,
      provider: input.provider ?? null,
      model: input.model ?? null,
      text: input.text,
      createdAt: now,
    };
  }

  listMessages(threadId: string): MessageRow[] {
    const rows = this.db
      .prepare("SELECT * FROM messages WHERE thread_id = ? ORDER BY id")
      .all(threadId);
    return rows.map((row) => mapMessage(row as SqlRow));
  }

  addCheckpoint(threadId: string, turn: number, gitRef: string): void {
    this.db
      .prepare(
        "INSERT INTO checkpoints (thread_id, turn, git_ref, created_at) VALUES (?, ?, ?, ?) ON CONFLICT (thread_id, turn) DO UPDATE SET git_ref = excluded.git_ref",
      )
      .run(threadId, turn, gitRef, new Date().toISOString());
  }

  listCheckpoints(threadId: string): CheckpointRow[] {
    const rows = this.db
      .prepare("SELECT * FROM checkpoints WHERE thread_id = ? ORDER BY turn")
      .all(threadId);
    return rows.map((row) => mapCheckpoint(row as SqlRow));
  }

  getCheckpoint(threadId: string, turn: number): CheckpointRow | null {
    const row = this.db
      .prepare("SELECT * FROM checkpoints WHERE thread_id = ? AND turn = ?")
      .get(threadId, turn) as SqlRow | undefined;
    return row ? mapCheckpoint(row) : null;
  }

  addUsage(input: {
    threadId: string;
    turn: number;
    provider: ProviderId;
    model?: string | null;
    inputTokens: number;
    outputTokens: number;
    cacheReadTokens: number;
    cacheCreationTokens: number;
    costUsd?: number | null;
  }): void {
    this.db
      .prepare(
        "INSERT INTO usage (thread_id, turn, provider, model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
      )
      .run(
        input.threadId,
        input.turn,
        input.provider,
        input.model ?? null,
        input.inputTokens,
        input.outputTokens,
        input.cacheReadTokens,
        input.cacheCreationTokens,
        input.costUsd ?? null,
        new Date().toISOString(),
      );
  }

  listUsage(threadId?: string): UsageRow[] {
    const rows = threadId
      ? this.db.prepare("SELECT * FROM usage WHERE thread_id = ? ORDER BY id").all(threadId)
      : this.db.prepare("SELECT * FROM usage ORDER BY id").all();
    return rows.map((row) => mapUsage(row as SqlRow));
  }
}
