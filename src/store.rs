use anyhow::Result;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{
    params, params_from_iter, Connection, OptionalExtension, Transaction, TransactionBehavior,
};
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

use crate::types::{PermissionMode, ProviderId, SessionMode, TurnPhase, TurnUsage};

#[derive(Debug, Clone)]
pub struct ThreadRow {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub current_provider: ProviderId,
    pub current_model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub permission_mode: PermissionMode,
    pub goal: Option<String>,
    pub goal_paused: bool,
    pub session_mode: SessionMode,
    pub pinned: bool,
    pub pin_order_key: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinMoveOutcome {
    Moved,
    AlreadyFirst,
    AlreadyLast,
    NotPinned,
    NotFound,
}

#[derive(Debug, Clone)]
pub struct TurnRow {
    pub thread_id: String,
    pub turn: i64,
    pub provider: ProviderId,
    pub model: Option<String>,
    pub phase: TurnPhase,
    pub error: Option<String>,
    pub provider_turn_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RetryableTurnRow {
    pub turn: i64,
    pub provider: crate::types::ProviderKey,
    pub model: Option<String>,
    pub phase: TurnPhase,
    pub error_preview: Option<String>,
    pub message_id: i64,
    pub prompt_preview: String,
    pub prompt_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadOpenWork {
    Turn,
    Queue,
    NativeRewind,
    Orchestration,
}

#[derive(Debug, Clone)]
pub struct ProviderSessionRow {
    pub thread_id: String,
    /// The stored name, including a configured agent Baton does not know yet.
    pub provider: crate::types::ProviderKey,
    pub native_session_id: String,
    pub model: Option<String>,
    pub rollout_path: Option<String>,
    pub updated_at: String,
    /// Highest turn this provider's native session has already seen.
    pub covered_turn: i64,
}

#[derive(Debug, Clone)]
pub struct OrchestrationRunRow {
    pub id: String,
    pub goal: String,
    pub coordinator_provider: ProviderId,
    pub coordinator_model: String,
    pub summary: Option<String>,
    pub plan_json: String,
    pub status: String,
    pub dirty_source: bool,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub archived_at: Option<String>,
    pub owner_pid: Option<u32>,
    pub owner_start: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OrchestrationTaskRow {
    pub id: String,
    pub run_id: String,
    pub position: i64,
    pub title: String,
    pub provider: ProviderId,
    pub model: String,
    pub prompt: String,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub thread_id: Option<String>,
    pub status: String,
    pub merged_at: Option<String>,
    pub pr_url: Option<String>,
    pub snapshot_base: Option<String>,
    pub snapshot_tree: Option<String>,
    pub reasoning_effort: Option<String>,
    pub text: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReviewNoteRow {
    pub id: i64,
    pub thread_id: String,
    pub turn: i64,
    pub path: String,
    pub line: usize,
    pub added: bool,
    pub body: String,
    pub diff_fingerprint: String,
    pub created_at: String,
    pub sent_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MessageSearchRow {
    pub message_id: i64,
    pub thread_id: String,
    pub title: String,
    pub turn: i64,
    pub role: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub text: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub thread_id: String,
    pub turn: i64,
    pub role: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub text: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct NativeRewindPending {
    pub thread_id: String,
    pub provider: ProviderId,
    pub turn: i64,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct CheckpointRow {
    pub thread_id: String,
    pub turn: i64,
    pub git_ref: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct MemoryRow {
    pub id: i64,
    /// `global` or `project`.
    pub scope: String,
    /// Empty for global memories.
    pub cwd: String,
    pub text: String,
    pub created_at: String,
    pub updated_at: String,
}

const MEMORY_TEXT_LIMIT: usize = 2000;
const MEMORY_SCOPE_LIMIT: i64 = 200;
pub const RECENT_FILE_LIMIT: usize = 200;

#[derive(Debug, Clone)]
pub struct ProviderProcessRow {
    pub pid: u32,
    pub provider: ProviderId,
    pub port: Option<u16>,
    pub command: String,
    pub process_start: Option<String>,
    pub cwd: String,
    pub owner_pid: u32,
    pub started_at: String,
}

#[derive(Debug, Clone)]
pub struct UsageRow {
    pub thread_id: String,
    pub turn: i64,
    pub provider: ProviderId,
    pub model: Option<String>,
    pub usage: TurnUsage,
    pub created_at: String,
}

pub struct NewThread {
    pub title: String,
    pub cwd: String,
    pub current_provider: ProviderId,
    pub current_model: Option<String>,
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub struct BatonStore {
    db: Connection,
}

const MIGRATIONS: &str = r#"
CREATE TABLE IF NOT EXISTS threads (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  cwd TEXT NOT NULL,
  current_provider TEXT NOT NULL,
  current_model TEXT,
  reasoning_effort TEXT,
  permission_mode TEXT NOT NULL DEFAULT 'default',
  goal TEXT,
  goal_paused INTEGER NOT NULL DEFAULT 0,
  session_mode TEXT NOT NULL DEFAULT 'single',
  pinned INTEGER NOT NULL DEFAULT 0,
  pin_order_key TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS turns (
  thread_id TEXT NOT NULL,
  turn INTEGER NOT NULL,
  provider TEXT NOT NULL,
  model TEXT,
  phase TEXT NOT NULL,
  error TEXT,
  provider_turn_id TEXT,
  started_at TEXT NOT NULL,
  ended_at TEXT,
  PRIMARY KEY (thread_id, turn)
);
CREATE TABLE IF NOT EXISTS provider_sessions (
  thread_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  native_session_id TEXT NOT NULL,
  model TEXT,
  rollout_path TEXT,
  updated_at TEXT NOT NULL,
  covered_turn INTEGER NOT NULL DEFAULT 0,
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
CREATE TABLE IF NOT EXISTS review_notes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  thread_id TEXT NOT NULL,
  turn INTEGER NOT NULL,
  path TEXT NOT NULL,
  line INTEGER NOT NULL,
  added INTEGER NOT NULL,
  body TEXT NOT NULL,
  diff_fingerprint TEXT NOT NULL,
  created_at TEXT NOT NULL,
  sent_at TEXT
);
CREATE INDEX IF NOT EXISTS review_notes_thread_turn_idx ON review_notes (thread_id, turn, sent_at);
CREATE TABLE IF NOT EXISTS queued_messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  thread_id TEXT NOT NULL,
  text TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS queued_messages_thread_idx ON queued_messages (thread_id, id);
CREATE TABLE IF NOT EXISTS prompt_history (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  cwd TEXT NOT NULL,
  text TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS prompt_history_cwd_idx ON prompt_history (cwd, id);
CREATE TABLE IF NOT EXISTS memories (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  scope TEXT NOT NULL,
  cwd TEXT NOT NULL,
  text TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(scope, cwd, text)
);
CREATE INDEX IF NOT EXISTS memories_scope_cwd_idx ON memories (scope, cwd, id);
CREATE TABLE IF NOT EXISTS recent_files (
  project_path TEXT NOT NULL,
  path TEXT NOT NULL,
  picked_at TEXT NOT NULL,
  seq INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (project_path, path)
);
CREATE INDEX IF NOT EXISTS recent_files_project_idx ON recent_files (project_path, seq DESC);
CREATE TABLE IF NOT EXISTS checkpoints (
  thread_id TEXT NOT NULL,
  turn INTEGER NOT NULL,
  git_ref TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (thread_id, turn)
);
CREATE TABLE IF NOT EXISTS native_rewinds (
  thread_id TEXT PRIMARY KEY,
  provider TEXT NOT NULL,
  turn INTEGER NOT NULL,
  created_at TEXT NOT NULL
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
CREATE TABLE IF NOT EXISTS provider_processes (
  pid INTEGER PRIMARY KEY,
  provider TEXT NOT NULL,
  port INTEGER,
  command TEXT NOT NULL,
  process_start TEXT,
  cwd TEXT NOT NULL,
  owner_pid INTEGER NOT NULL,
  started_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS orchestration_runs (
  id TEXT PRIMARY KEY,
  goal TEXT NOT NULL,
  coordinator_provider TEXT NOT NULL,
  coordinator_model TEXT NOT NULL,
  summary TEXT,
  plan_json TEXT NOT NULL,
  status TEXT NOT NULL,
  dirty_source INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  finished_at TEXT,
  archived_at TEXT,
  owner_pid INTEGER,
  owner_start TEXT
);
CREATE TABLE IF NOT EXISTS orchestration_tasks (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  position INTEGER NOT NULL DEFAULT 0,
  title TEXT NOT NULL,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  prompt TEXT NOT NULL,
  worktree_path TEXT,
  branch TEXT,
  commit_sha TEXT,
  thread_id TEXT,
  status TEXT NOT NULL,
  text TEXT,
  error TEXT,
  created_at TEXT NOT NULL,
  finished_at TEXT,
  merged_at TEXT,
  pr_url TEXT,
  snapshot_base TEXT,
  snapshot_tree TEXT,
  reasoning_effort TEXT
);
CREATE INDEX IF NOT EXISTS orchestration_tasks_run_idx ON orchestration_tasks (run_id, created_at);
"#;

/// One schema path for on-disk and in-memory stores. A test store that skips a
/// migration drifts from production, which hides exactly the bugs these tests
/// are meant to catch.
fn apply_schema(db: &Connection) -> Result<()> {
    db.execute_batch(MIGRATIONS)?;
    migrate_thread_model_nullable(db)?;
    ensure_thread_columns(db)?;
    ensure_column(
        db,
        "provider_sessions",
        "covered_turn",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(db, "provider_processes", "process_start", "TEXT")?;
    ensure_column(db, "recent_files", "seq", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(db, "orchestration_tasks", "commit_sha", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "merged_at", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "position", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(db, "orchestration_tasks", "pr_url", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "snapshot_base", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "snapshot_tree", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "reasoning_effort", "TEXT")?;
    ensure_column(db, "orchestration_runs", "dirty_source", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(db, "orchestration_runs", "archived_at", "TEXT")?;
    ensure_column(db, "orchestration_runs", "owner_pid", "INTEGER")?;
    ensure_column(db, "orchestration_runs", "owner_start", "TEXT")?;
    Ok(())
}

fn ensure_column(db: &Connection, table: &str, column: &str, definition: &str) -> Result<()> {
    let exists = db
        .query_row(
            "SELECT 1 FROM pragma_table_info(?) WHERE name = ?",
            params![table, column],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        db.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn ensure_thread_columns(db: &Connection) -> Result<()> {
    ensure_column(db, "threads", "permission_mode", "TEXT NOT NULL DEFAULT 'default'")?;
    ensure_column(db, "threads", "reasoning_effort", "TEXT")?;
    ensure_column(db, "threads", "goal", "TEXT")?;
    ensure_column(db, "threads", "goal_paused", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(
        db,
        "threads",
        "session_mode",
        "TEXT NOT NULL DEFAULT 'single'",
    )?;
    ensure_column(db, "threads", "pinned", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(db, "threads", "pin_order_key", "TEXT")?;
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS threads_pin_order_idx ON threads(pin_order_key DESC) WHERE pin_order_key IS NOT NULL;
         CREATE INDEX IF NOT EXISTS threads_pin_backfill_idx ON threads(updated_at) WHERE pinned = 1 AND (pin_order_key IS NULL OR pin_order_key = '');",
    )?;
    backfill_pin_order_keys(db)
}

fn path_candidates(cwd: &Path) -> Vec<std::path::PathBuf> {
    let mut candidates = vec![cwd.to_path_buf()];
    if let Ok(canonical) = cwd.canonicalize() {
        if canonical != cwd {
            candidates.push(canonical);
        }
    }
    if let Ok(target) = cwd.read_link() {
        let target = if target.is_absolute() {
            target
        } else {
            cwd.parent().unwrap_or(Path::new(".")).join(target)
        };
        if !candidates.contains(&target) {
            candidates.push(target);
        }
    }
    candidates
}

fn parse_pin_order_key(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn format_pin_order_key(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn truncate_to_microseconds(value: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(value.timestamp_micros()).unwrap_or(value)
}

fn next_pin_order_key_from(now: DateTime<Utc>, max: Option<DateTime<Utc>>) -> String {
    let now = truncate_to_microseconds(now);
    let next = match max {
        Some(max) if now <= max => max + Duration::microseconds(1),
        _ => now,
    };
    format_pin_order_key(next)
}

fn backfill_pin_order_keys(db: &Connection) -> Result<()> {
    let needs_repair: Option<i64> = db
        .query_row(
            "SELECT 1 FROM threads WHERE pinned = 1 AND (pin_order_key IS NULL OR pin_order_key = '') LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if needs_repair.is_none() {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    let max: Option<String> = transaction.query_row(
        "SELECT MAX(pin_order_key) FROM threads WHERE pin_order_key IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    let mut last = max.as_deref().and_then(parse_pin_order_key);
    let missing = {
        let mut statement = transaction.prepare(
            "SELECT id, updated_at, created_at FROM threads WHERE pinned = 1 AND (pin_order_key IS NULL OR pin_order_key = '') ORDER BY updated_at ASC, rowid ASC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (id, updated_at, created_at) in missing {
        let base = match parse_pin_order_key(&updated_at) {
            Some(value) => truncate_to_microseconds(value),
            None => last
                .map(truncate_to_microseconds)
                .or_else(|| parse_pin_order_key(&created_at).map(truncate_to_microseconds))
                .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap_or_else(Utc::now)),
        };
        let normalized_last = last.map(truncate_to_microseconds);
        let key = match normalized_last {
            Some(last) if base <= last => last + Duration::microseconds(1),
            _ => base,
        };
        let changed = transaction.execute(
            "UPDATE threads SET pin_order_key = ? WHERE id = ?",
            params![format_pin_order_key(key), id],
        )?;
        if changed != 1 {
            anyhow::bail!("thread {id} not found while backfilling pin order");
        }
        last = Some(key);
    }
    transaction.commit()?;
    Ok(())
}

fn migrate_thread_model_nullable(db: &Connection) -> Result<()> {
    let not_null: Option<i64> = db
        .query_row(
            r#"SELECT "notnull" FROM pragma_table_info('threads') WHERE name = 'current_model'"#,
            [],
            |row| row.get(0),
        )
        .optional()?;
    match not_null {
        Some(1) => {
            db.execute_batch(
                r#"
                CREATE TABLE threads_migrated (
                  id TEXT PRIMARY KEY,
                  title TEXT NOT NULL,
                  cwd TEXT NOT NULL,
                  current_provider TEXT NOT NULL,
                  current_model TEXT,
                  permission_mode TEXT NOT NULL DEFAULT 'default',
                  goal TEXT,
                  goal_paused INTEGER NOT NULL DEFAULT 0,
                  pinned INTEGER NOT NULL DEFAULT 0,
                  pin_order_key TEXT,
                  created_at TEXT NOT NULL,
                  updated_at TEXT NOT NULL
                );
                INSERT INTO threads_migrated (id, title, cwd, current_provider, current_model, permission_mode, goal, goal_paused, pinned, pin_order_key, created_at, updated_at)
                  SELECT id, title, cwd, current_provider,
                    CASE WHEN current_model IN ('gpt-6.1-sol', 'claude-sonnet-4-5') THEN NULL ELSE current_model END,
                    'default', NULL, 0, 0, NULL, created_at, updated_at
                  FROM threads;
                DROP TABLE threads;
                ALTER TABLE threads_migrated RENAME TO threads;
                "#,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn map_review_note(row: &rusqlite::Row) -> rusqlite::Result<ReviewNoteRow> {
    Ok(ReviewNoteRow {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        turn: row.get(2)?,
        path: row.get(3)?,
        line: row.get::<_, i64>(4)?.max(0) as usize,
        added: row.get::<_, i64>(5)? != 0,
        body: row.get(6)?,
        diff_fingerprint: row.get(7)?,
        created_at: row.get(8)?,
        sent_at: row.get(9)?,
    })
}

fn map_thread(row: &rusqlite::Row) -> rusqlite::Result<ThreadRow> {
    let provider: String = row.get(3)?;
    let permission_mode: String = row.get(7)?;
    let session_mode: String = row.get(10)?;
    Ok(ThreadRow {
        id: row.get(0)?,
        title: row.get(1)?,
        cwd: row.get(2)?,
        current_provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Claude),
        current_model: row.get(4)?,
        reasoning_effort: row.get(11)?,
        permission_mode: PermissionMode::parse(&permission_mode).unwrap_or(PermissionMode::Default),
        goal: row.get(8)?,
        goal_paused: row.get::<_, i64>(9)? != 0,
        session_mode: SessionMode::parse(&session_mode).unwrap_or(SessionMode::Single),
        pinned: row.get::<_, i64>(12)? != 0,
        pin_order_key: row.get(13)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn map_orchestration_run(row: &rusqlite::Row) -> rusqlite::Result<OrchestrationRunRow> {
    let provider: String = row.get(2)?;
    Ok(OrchestrationRunRow {
        id: row.get(0)?,
        goal: row.get(1)?,
        coordinator_provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Codex),
        coordinator_model: row.get(3)?,
        summary: row.get(4)?,
        plan_json: row.get(5)?,
        status: row.get(6)?,
        dirty_source: row.get::<_, i64>(7)? != 0,
        created_at: row.get(8)?,
        finished_at: row.get(9)?,
        archived_at: row.get(10)?,
        owner_pid: row.get(11)?,
        owner_start: row.get(12)?,
    })
}

fn map_orchestration_task(row: &rusqlite::Row) -> rusqlite::Result<OrchestrationTaskRow> {
    let provider: String = row.get(4)?;
    Ok(OrchestrationTaskRow {
        id: row.get(0)?,
        run_id: row.get(1)?,
        position: row.get(2)?,
        title: row.get(3)?,
        provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Codex),
        model: row.get(5)?,
        prompt: row.get(6)?,
        worktree_path: row.get(7)?,
        branch: row.get(8)?,
        commit_sha: row.get(9)?,
        thread_id: row.get(10)?,
        status: row.get(11)?,
        text: row.get(12)?,
        error: row.get(13)?,
        created_at: row.get(14)?,
        finished_at: row.get(15)?,
        merged_at: row.get(16)?,
        pr_url: row.get(17)?,
        snapshot_base: row.get(18)?,
        snapshot_tree: row.get(19)?,
        reasoning_effort: row.get(20)?,
    })
}

const THREAD_COLUMNS: &str = "id, title, cwd, current_provider, current_model, created_at, updated_at, permission_mode, goal, goal_paused, session_mode, reasoning_effort, pinned, pin_order_key";

/// Per-project prompt history cap, Claude Code style.
pub const PROMPT_HISTORY_LIMIT: i64 = 100;

impl BatonStore {
    pub fn open(path: &Path) -> Result<BatonStore> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(path)?;
        // Several baton processes can share the same store (for example two
        // project windows). Wait for another writer instead of failing the
        // command immediately with SQLITE_BUSY.
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        apply_schema(&db)?;
        Ok(BatonStore { db })
    }

    pub fn memory() -> Result<BatonStore> {
        let db = Connection::open_in_memory()?;
        apply_schema(&db)?;
        Ok(BatonStore { db })
    }

    pub fn create_thread(&self, input: NewThread) -> Result<ThreadRow> {
        self.create_thread_with_id(input, Uuid::new_v4().to_string())
    }

    pub fn create_thread_with_mode(
        &self,
        input: NewThread,
        session_mode: SessionMode,
    ) -> Result<ThreadRow> {
        self.create_thread_with_id_and_mode(input, Uuid::new_v4().to_string(), session_mode)
    }

    pub fn create_thread_with_id(&self, input: NewThread, id: String) -> Result<ThreadRow> {
        self.create_thread_with_id_and_mode(input, id, SessionMode::Single)
    }

    pub fn create_thread_with_id_and_mode(
        &self,
        input: NewThread,
        id: String,
        session_mode: SessionMode,
    ) -> Result<ThreadRow> {
        let now = now_iso();
        self.db.execute(
            "INSERT INTO threads (id, title, cwd, current_provider, current_model, permission_mode, goal, goal_paused, session_mode, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'default', NULL, 0, ?, ?, ?)",
            params![id, input.title, input.cwd, input.current_provider.as_str(), input.current_model, session_mode.as_str(), now, now],
        )?;
        Ok(ThreadRow {
            id,
            title: input.title,
            cwd: input.cwd,
            current_provider: input.current_provider,
            current_model: input.current_model,
            reasoning_effort: None,
            permission_mode: PermissionMode::Default,
            goal: None,
            goal_paused: false,
            session_mode,
            pinned: false,
            pin_order_key: None,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Branches a thread's conversation into a new thread without copying
    /// native provider sessions. The first turn in the fork therefore gets a
    /// transcript handoff rather than sharing a live provider session.
    pub fn fork_thread(&self, source_id: &str, title: &str) -> Result<ThreadRow> {
        self.fork_thread_through_turn(source_id, title, None)
    }

    /// Validates a fork cutoff and returns the provider turn id to use for a
    /// native fork. A None result means the whole thread is eligible. The
    /// caller can run the provider fork before creating the baton thread.
    pub fn fork_provider_turn_id(
        &self,
        source_id: &str,
        through_turn: Option<i64>,
    ) -> Result<Option<String>> {
        self.get_thread(source_id)?
            .ok_or_else(|| anyhow::anyhow!("thread not found: {source_id}"))?;
        let turns = self.list_turn_records(source_id)?;
        match through_turn {
            Some(cutoff) => {
                let turn = turns
                    .iter()
                    .find(|turn| turn.turn == cutoff)
                    .ok_or_else(|| anyhow::anyhow!("turn {cutoff} not found in thread"))?;
                if !turn.phase.is_terminal() {
                    anyhow::bail!("turn {cutoff} is not terminal");
                }
                Ok(turn.provider_turn_id.clone())
            }
            None => {
                if turns.iter().any(|turn| !turn.phase.is_terminal()) {
                    anyhow::bail!("cannot fork while a turn is open");
                }
                Ok(None)
            }
        }
    }

    /// Branches a thread through a specific completed turn. Later turns,
    /// including a currently open turn, are left in the source thread.
    pub fn fork_thread_through_turn(
        &self,
        source_id: &str,
        title: &str,
        through_turn: Option<i64>,
    ) -> Result<ThreadRow> {
        let source = self
            .get_thread(source_id)?
            .ok_or_else(|| anyhow::anyhow!("thread not found: {source_id}"))?;
        let mut messages = self.list_messages(source_id)?;
        let mut turns = self.list_turn_records(source_id)?;
        match through_turn {
            Some(cutoff) => {
                let turn = turns
                    .iter()
                    .find(|turn| turn.turn == cutoff)
                    .ok_or_else(|| anyhow::anyhow!("turn {cutoff} not found in thread"))?;
                if !turn.phase.is_terminal() {
                    anyhow::bail!("turn {cutoff} is not terminal");
                }
                messages.retain(|message| message.turn <= cutoff);
                turns.retain(|turn| turn.turn <= cutoff);
            }
            None => {
                if turns.iter().any(|turn| !turn.phase.is_terminal()) {
                    anyhow::bail!("cannot fork while a turn is open");
                }
            }
        }
        let id = Uuid::new_v4().to_string();
        let now = now_iso();
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO threads (id, title, cwd, current_provider, current_model, reasoning_effort, permission_mode, goal, goal_paused, session_mode, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                id,
                title,
                source.cwd,
                source.current_provider.as_str(),
                source.current_model,
                source.reasoning_effort,
                source.permission_mode.as_str(),
                source.goal,
                source.goal_paused as i64,
                source.session_mode.as_str(),
                now,
                now,
            ],
        )?;
        for message in messages {
            tx.execute(
                "INSERT INTO messages (thread_id, turn, role, provider, model, text, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    id,
                    message.turn,
                    message.role,
                    message.provider,
                    message.model,
                    message.text,
                    message.created_at,
                ],
            )?;
        }
        for turn in turns {
            tx.execute(
                "INSERT INTO turns (thread_id, turn, provider, model, phase, error, provider_turn_id, started_at, ended_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    id,
                    turn.turn,
                    turn.provider.as_str(),
                    turn.model,
                    turn.phase.as_str(),
                    turn.error,
                    turn.provider_turn_id,
                    turn.started_at,
                    turn.ended_at,
                ],
            )?;
        }
        tx.commit()?;
        self.get_thread(&id)?
            .ok_or_else(|| anyhow::anyhow!("forked thread disappeared: {id}"))
    }

    pub fn draft_thread(&self, input: NewThread) -> ThreadRow {
        let now = now_iso();
        ThreadRow {
            id: Uuid::new_v4().to_string(),
            title: input.title,
            cwd: input.cwd,
            current_provider: input.current_provider,
            current_model: input.current_model,
            reasoning_effort: None,
            permission_mode: PermissionMode::Default,
            goal: None,
            goal_paused: false,
            session_mode: SessionMode::Single,
            pinned: false,
            pin_order_key: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Lists threads for a directory, accepting both the caller's path and its
    /// canonical form. A symlinked invocation path must not hide threads that
    /// were created through the real project path, or vice versa.
    pub fn list_threads_for_path(&self, cwd: &Path) -> Result<Vec<ThreadRow>> {
        self.list_threads_for_path_ordered(cwd, false)
    }

    /// Display order puts pinned threads first. Semantic resolution uses
    /// `list_threads_for_path`, which stays recency-first.
    pub fn list_threads_for_path_display(&self, cwd: &Path) -> Result<Vec<ThreadRow>> {
        self.list_threads_for_path_ordered(cwd, true)
    }

    fn list_threads_for_path_ordered(
        &self,
        cwd: &Path,
        pinned_first: bool,
    ) -> Result<Vec<ThreadRow>> {
        let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let rows = self.list_threads_ordered(None, pinned_first)?;
        Ok(rows
            .into_iter()
            .filter(|thread| {
                let stored = Path::new(&thread.cwd);
                stored == cwd
                    || stored
                        .canonicalize()
                        .map(|stored| stored == canonical)
                        .unwrap_or(false)
            })
            .collect())
    }

    pub fn list_threads(&self, cwd: Option<&str>) -> Result<Vec<ThreadRow>> {
        self.list_threads_ordered(cwd, false)
    }

    pub fn list_threads_display(&self, cwd: Option<&str>) -> Result<Vec<ThreadRow>> {
        self.list_threads_ordered(cwd, true)
    }

    pub fn latest_turn_phases(&self, thread_ids: &[String]) -> Result<HashMap<String, TurnPhase>> {
        if thread_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut phases = HashMap::new();
        for chunk in thread_ids.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let mut statement = self.db.prepare(&format!(
                "SELECT t.thread_id, t.phase FROM turns t JOIN (SELECT thread_id, MAX(turn) AS turn FROM turns WHERE thread_id IN ({placeholders}) GROUP BY thread_id) latest ON latest.thread_id = t.thread_id AND latest.turn = t.turn"
            ))?;
            let rows = statement.query_map(params_from_iter(chunk.iter()), |row| {
                let id: String = row.get(0)?;
                let phase: String = row.get(1)?;
                Ok((id, TurnPhase::parse(&phase).unwrap_or(TurnPhase::Unverified)))
            })?;
            for row in rows {
                let (id, phase) = row?;
                phases.insert(id, phase);
            }
        }
        Ok(phases)
    }

    fn list_threads_ordered(
        &self,
        cwd: Option<&str>,
        pinned_first: bool,
    ) -> Result<Vec<ThreadRow>> {
        let order = if pinned_first {
            "pinned DESC, pin_order_key DESC, updated_at DESC, rowid DESC"
        } else {
            "updated_at DESC, rowid DESC"
        };
        let mut statement = match cwd {
            Some(_cwd) => self.db.prepare(&format!(
                "SELECT {THREAD_COLUMNS} FROM threads WHERE cwd = ? ORDER BY {order}"
            ))?,
            None => self.db.prepare(&format!(
                "SELECT {THREAD_COLUMNS} FROM threads ORDER BY {order}"
            ))?,
        };
        let rows = match cwd {
            Some(cwd) => statement.query_map(params![cwd], map_thread)?.collect::<rusqlite::Result<Vec<_>>>()?,
            None => statement.query_map([], map_thread)?.collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    pub fn get_thread(&self, id: &str) -> Result<Option<ThreadRow>> {
        self.db
            .query_row(
                &format!("SELECT {THREAD_COLUMNS} FROM threads WHERE id = ?"),
                params![id],
                map_thread,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn rename_thread(&self, id: &str, title: &str) -> Result<()> {
        self.db.execute(
            "UPDATE threads SET title = ?, updated_at = ? WHERE id = ?",
            params![title, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_pinned(&self, id: &str, pinned: bool) -> Result<()> {
        let transaction = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        if pinned {
            let key = self.next_pin_order_key()?;
            let changed = transaction.execute(
                "UPDATE threads SET pinned = 1, pin_order_key = COALESCE(pin_order_key, ?) WHERE id = ?",
                params![key, id],
            )?;
            if changed != 1 {
                anyhow::bail!("thread {id} not found");
            }
        } else {
            let changed = transaction.execute(
                "UPDATE threads SET pinned = 0, pin_order_key = NULL WHERE id = ?",
                params![id],
            )?;
            if changed != 1 {
                anyhow::bail!("thread {id} not found");
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn move_thread_pin(&self, id: &str, cwd: &Path, up: bool) -> Result<PinMoveOutcome> {
        let candidates = path_candidates(cwd)
            .into_iter()
            .map(|candidate| candidate.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let transaction = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let thread = match self.get_thread(id)? {
            Some(thread) => thread,
            None => return Ok(PinMoveOutcome::NotFound),
        };
        if !thread.pinned {
            return Ok(PinMoveOutcome::NotPinned);
        }
        let pinned = {
            let placeholders = vec!["?"; candidates.len()].join(", ");
            let mut statement = transaction.prepare(&format!(
                "SELECT {THREAD_COLUMNS} FROM threads WHERE pinned = 1 AND cwd IN ({placeholders}) ORDER BY pin_order_key DESC, updated_at DESC, rowid DESC"
            ))?;
            let rows = statement
                .query_map(params_from_iter(candidates.iter()), map_thread)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let Some(index) = pinned.iter().position(|thread| thread.id == id) else {
            return Ok(PinMoveOutcome::NotPinned);
        };
        let neighbor = if up {
            index.checked_sub(1)
        } else {
            (index + 1 < pinned.len()).then_some(index + 1)
        };
        let Some(neighbor) = neighbor else {
            return Ok(if up {
                PinMoveOutcome::AlreadyFirst
            } else {
                PinMoveOutcome::AlreadyLast
            });
        };
        let Some(current_key) = pinned[index].pin_order_key.as_deref() else {
            return Ok(PinMoveOutcome::NotFound);
        };
        let Some(neighbor_key) = pinned[neighbor].pin_order_key.as_deref() else {
            return Ok(PinMoveOutcome::NotFound);
        };
        let changed = transaction.execute(
            "UPDATE threads SET pin_order_key = ? WHERE id = ?",
            params![neighbor_key, pinned[index].id],
        )?;
        if changed != 1 {
            anyhow::bail!("thread {} not found while moving pin", pinned[index].id);
        }
        let changed = transaction.execute(
            "UPDATE threads SET pin_order_key = ? WHERE id = ?",
            params![current_key, pinned[neighbor].id],
        )?;
        if changed != 1 {
            anyhow::bail!("thread {} not found while moving pin", pinned[neighbor].id);
        }
        transaction.commit()?;
        Ok(PinMoveOutcome::Moved)
    }

    fn next_pin_order_key(&self) -> Result<String> {
        let max: Option<String> = self.db.query_row(
            "SELECT MAX(pin_order_key) FROM threads WHERE pin_order_key IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(next_pin_order_key_from(
            Utc::now(),
            max.as_deref().and_then(parse_pin_order_key),
        ))
    }

    pub fn set_thread_cwd(&self, id: &str, cwd: &str) -> Result<()> {
        self.db.execute(
            "UPDATE threads SET cwd = ?, updated_at = ? WHERE id = ?",
            params![cwd, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_session_mode(&self, id: &str, mode: SessionMode) -> Result<()> {
        self.db.execute(
            "UPDATE threads SET session_mode = ?, updated_at = ? WHERE id = ?",
            params![mode.as_str(), now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_title(&self, id: &str, title: &str) -> Result<()> {
        self.db.execute(
            "UPDATE threads SET title = ?, updated_at = ? WHERE id = ?",
            params![title, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_provider(&self, id: &str, provider: ProviderId, model: Option<String>) -> Result<()> {
        self.db.execute(
            "UPDATE threads SET current_provider = ?, current_model = ?, updated_at = ? WHERE id = ?",
            params![provider.as_str(), model, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_model(&self, id: &str, model: &str) -> Result<()> {
        self.db.execute(
            "UPDATE threads SET current_model = ?, updated_at = ? WHERE id = ?",
            params![model, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_effort(&self, id: &str, effort: Option<&str>) -> Result<()> {
        self.db.execute(
            "UPDATE threads SET reasoning_effort = ?, updated_at = ? WHERE id = ?",
            params![effort, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_permission_mode(&self, id: &str, mode: PermissionMode) -> Result<()> {
        self.db.execute(
            "UPDATE threads SET permission_mode = ?, updated_at = ? WHERE id = ?",
            params![mode.as_str(), now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_goal(&self, id: &str, goal: Option<&str>, paused: bool) -> Result<()> {
        self.db.execute(
            "UPDATE threads SET goal = ?, goal_paused = ?, updated_at = ? WHERE id = ?",
            params![goal, if paused { 1 } else { 0 }, now_iso(), id],
        )?;
        Ok(())
    }

    /// Records a submitted prompt for a project, keeping the newest
    /// [PROMPT_HISTORY_LIMIT] entries and skipping consecutive repeats.
    pub fn record_prompt(&self, cwd: &str, text: &str) -> Result<()> {
        let latest: Option<String> = self
            .db
            .query_row(
                "SELECT text FROM prompt_history WHERE cwd = ? ORDER BY id DESC LIMIT 1",
                params![cwd],
                |row| row.get(0),
            )
            .optional()?;
        if latest.as_deref() == Some(text) {
            return Ok(());
        }
        self.db.execute(
            "INSERT INTO prompt_history (cwd, text, created_at) VALUES (?, ?, ?)",
            params![cwd, text, now_iso()],
        )?;
        self.db.execute(
            "DELETE FROM prompt_history WHERE cwd = ? AND id NOT IN (
                 SELECT id FROM prompt_history WHERE cwd = ? ORDER BY id DESC LIMIT ?
             )",
            params![cwd, cwd, PROMPT_HISTORY_LIMIT],
        )?;
        Ok(())
    }

    /// Adds a governed memory. Project memories are scoped to one cwd; global
    /// memories apply everywhere. An identical memory is returned as-is.
    pub fn add_memory(&self, scope: &str, cwd: &str, text: &str) -> Result<i64> {
        let scope = scope.trim();
        if scope != "global" && scope != "project" {
            anyhow::bail!("memory scope must be global or project");
        }
        let text = text.trim();
        if text.is_empty() {
            anyhow::bail!("memory text is empty");
        }
        if text.chars().count() > MEMORY_TEXT_LIMIT {
            anyhow::bail!("memory text is longer than {MEMORY_TEXT_LIMIT} characters");
        }
        let cwd = if scope == "global" { "" } else { cwd };
        let count: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM memories WHERE scope = ? AND cwd = ?",
            params![scope, cwd],
            |row| row.get(0),
        )?;
        if count >= MEMORY_SCOPE_LIMIT {
            anyhow::bail!("memory limit reached ({MEMORY_SCOPE_LIMIT}); remove one first");
        }
        self.db.execute(
            "INSERT OR IGNORE INTO memories (scope, cwd, text, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?)",
            params![scope, cwd, text, now_iso(), now_iso()],
        )?;
        let id = self.db.query_row(
            "SELECT id FROM memories WHERE scope = ? AND cwd = ? AND text = ?",
            params![scope, cwd, text],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// Project memories for this cwd plus every global memory. Project entries
    /// come first because they are more specific.
    pub fn list_memories(&self, cwd: &str) -> Result<Vec<MemoryRow>> {
        let mut statement = self.db.prepare(
            "SELECT id, scope, cwd, text, created_at, updated_at
             FROM memories
             WHERE scope = 'global' OR (scope = 'project' AND cwd = ?)
             ORDER BY CASE scope WHEN 'project' THEN 0 ELSE 1 END, id ASC",
        )?;
        let rows = statement
            .query_map(params![cwd], |row| {
                Ok(MemoryRow {
                    id: row.get(0)?,
                    scope: row.get(1)?,
                    cwd: row.get(2)?,
                    text: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Records a picked project file. The newest pick moves to the front and
    /// the table keeps only the most recent `RECENT_FILE_LIMIT` paths per project.
    pub fn record_recent_file(&self, project_path: &str, path: &str) -> Result<()> {
        let picked_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        let transaction = self.db.unchecked_transaction()?;
        let seq: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM recent_files WHERE project_path = ?1",
            params![project_path],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO recent_files (project_path, path, picked_at, seq)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_path, path) DO UPDATE SET
               picked_at = excluded.picked_at,
               seq = excluded.seq",
            params![project_path, path, picked_at, seq],
        )?;
        transaction.execute(
            "DELETE FROM recent_files
             WHERE project_path = ?1 AND path NOT IN (
               SELECT path FROM recent_files
               WHERE project_path = ?1
               ORDER BY seq DESC, picked_at DESC, path ASC
               LIMIT ?2
             )",
            params![project_path, RECENT_FILE_LIMIT as i64],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Most recently picked paths for one project, newest first.
    pub fn list_recent_files(&self, project_path: &str, limit: usize) -> Result<Vec<String>> {
        let mut statement = self.db.prepare(
            "SELECT path FROM recent_files
             WHERE project_path = ?1
             ORDER BY seq DESC, picked_at DESC, path ASC
             LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![project_path, limit as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    /// Removes recent-file rows whose project directory no longer exists.
    pub fn prune_missing_recent_projects(&self) -> Result<usize> {
        let project_paths = {
            let mut statement = self
                .db
                .prepare("SELECT DISTINCT project_path FROM recent_files")?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            rows
        };
        let mut removed = 0;
        for project_path in project_paths {
            if !Path::new(&project_path).is_dir() {
                removed += self.db.execute(
                    "DELETE FROM recent_files WHERE project_path = ?",
                    params![project_path],
                )?;
            }
        }
        Ok(removed)
    }

    /// Every memory row, used by the read-only MCP `all` view.
    pub fn list_all_memories(&self) -> Result<Vec<MemoryRow>> {
        let mut statement = self.db.prepare(
            "SELECT id, scope, cwd, text, created_at, updated_at
             FROM memories ORDER BY scope, cwd, id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(MemoryRow {
                    id: row.get(0)?,
                    scope: row.get(1)?,
                    cwd: row.get(2)?,
                    text: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Removes a memory only when it is global or belongs to this project.
    pub fn remove_memory(&self, cwd: &str, id: i64) -> Result<bool> {
        let changed = self.db.execute(
            "DELETE FROM memories WHERE id = ? AND (scope = 'global' OR cwd = ?)",
            params![id, cwd],
        )?;
        Ok(changed > 0)
    }

    /// Clears either the project memories for this cwd or every global memory.
    pub fn clear_memories(&self, cwd: &str, global: bool) -> Result<usize> {
        let changed = if global {
            self.db.execute("DELETE FROM memories WHERE scope = 'global'", [])?
        } else {
            self.db.execute(
                "DELETE FROM memories WHERE scope = 'project' AND cwd = ?",
                params![cwd],
            )?
        };
        Ok(changed)
    }

    /// Project prompt history, oldest first.
    pub fn list_prompt_history(&self, cwd: &str) -> Result<Vec<String>> {
        let mut statement = self
            .db
            .prepare("SELECT text FROM prompt_history WHERE cwd = ? ORDER BY id ASC")?;
        let rows = statement
            .query_map(params![cwd], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Saves a line-anchored review note before the user sends it. The diff
    /// fingerprint prevents a reused turn number from attaching an old note to
    /// a different diff after rewind.
    pub fn add_review_note(
        &self,
        thread_id: &str,
        turn: i64,
        path: &str,
        line: usize,
        added: bool,
        body: &str,
        diff_fingerprint: &str,
    ) -> Result<i64> {
        self.db.execute(
            "INSERT INTO review_notes (thread_id, turn, path, line, added, body, diff_fingerprint, created_at, sent_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)",
            params![thread_id, turn, path, line as i64, if added { 1 } else { 0 }, body, diff_fingerprint, now_iso()],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    /// Notes that are still waiting to be sent for this exact diff.
    pub fn list_unsent_review_notes(
        &self,
        thread_id: &str,
        turn: i64,
        diff_fingerprint: &str,
    ) -> Result<Vec<ReviewNoteRow>> {
        let mut statement = self.db.prepare(
            "SELECT id, thread_id, turn, path, line, added, body, diff_fingerprint, created_at, sent_at
             FROM review_notes
             WHERE thread_id = ? AND turn = ? AND diff_fingerprint = ? AND sent_at IS NULL
             ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![thread_id, turn, diff_fingerprint], map_review_note)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Records delivery without deleting the note. The text itself becomes a
    /// normal user message; this timestamp keeps a reopen from resending it.
    pub fn mark_review_notes_sent(&self, thread_id: &str, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let sent_at = now_iso();
        let tx = self.db.unchecked_transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE review_notes SET sent_at = ? WHERE thread_id = ? AND id = ? AND sent_at IS NULL",
                params![sent_at, thread_id, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_review_notes(&self, thread_id: &str, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tx = self.db.unchecked_transaction()?;
        for id in ids {
            tx.execute(
                "DELETE FROM review_notes WHERE thread_id = ? AND id = ?",
                params![thread_id, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn touch_thread(&self, id: &str) -> Result<()> {
        self.db.execute(
            "UPDATE threads SET updated_at = ? WHERE id = ?",
            params![now_iso(), id],
        )?;
        Ok(())
    }

    pub fn upsert_provider_session(
        &self,
        thread_id: &str,
        provider: impl Into<crate::types::ProviderKey>,
        native_session_id: &str,
        model: Option<String>,
        rollout_path: Option<String>,
    ) -> Result<()> {
        let provider = provider.into();
        self.db.execute(
            r#"INSERT INTO provider_sessions (thread_id, provider, native_session_id, model, rollout_path, updated_at)
               VALUES (?, ?, ?, ?, ?, ?)
               ON CONFLICT (thread_id, provider) DO UPDATE SET
                 native_session_id = excluded.native_session_id,
                 model = COALESCE(excluded.model, model),
                 rollout_path = COALESCE(excluded.rollout_path, rollout_path),
                 updated_at = excluded.updated_at"#,
            params![thread_id, provider.as_str(), native_session_id, model, rollout_path, now_iso()],
        )?;
        Ok(())
    }

    /// Records how far this provider's native session has read the thread.
    pub fn mark_provider_session_covered(
        &self,
        thread_id: &str,
        provider: impl Into<crate::types::ProviderKey>,
        turn: i64,
    ) -> Result<()> {
        let provider = provider.into();
        self.db.execute(
            "UPDATE provider_sessions SET covered_turn = MAX(covered_turn, ?) WHERE thread_id = ? AND provider = ?",
            params![turn, thread_id, provider.as_str()],
        )?;
        Ok(())
    }

    pub fn get_provider_session(
        &self,
        thread_id: &str,
        provider: impl Into<crate::types::ProviderKey>,
    ) -> Result<Option<ProviderSessionRow>> {
        let provider = provider.into();
        self.db
            .query_row(
                "SELECT thread_id, provider, native_session_id, model, rollout_path, updated_at, covered_turn FROM provider_sessions WHERE thread_id = ? AND provider = ?",
                params![thread_id, provider.as_str()],
                |row| {
                    let provider: String = row.get(1)?;
                    Ok(ProviderSessionRow {
                        thread_id: row.get(0)?,
                        provider: crate::types::ProviderKey::from_stored(&provider),
                        native_session_id: row.get(2)?,
                        model: row.get(3)?,
                        rollout_path: row.get(4)?,
                        updated_at: row.get(5)?,
                        covered_turn: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_provider_sessions(&self, thread_id: &str) -> Result<Vec<ProviderSessionRow>> {
        let mut statement = self.db.prepare(
            "SELECT thread_id, provider, native_session_id, model, rollout_path, updated_at, covered_turn FROM provider_sessions WHERE thread_id = ? ORDER BY provider",
        )?;
        let rows = statement
            .query_map(params![thread_id], |row| {
                let provider: String = row.get(1)?;
                Ok(ProviderSessionRow {
                    thread_id: row.get(0)?,
                    provider: crate::types::ProviderKey::from_stored(&provider),
                    native_session_id: row.get(2)?,
                    model: row.get(3)?,
                    rollout_path: row.get(4)?,
                    updated_at: row.get(5)?,
                    covered_turn: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_thread_by_native_session(&self, provider: ProviderId, native_session_id: &str) -> Result<Option<ThreadRow>> {
        self.db
            .query_row(
                "SELECT t.id, t.title, t.cwd, t.current_provider, t.current_model, t.created_at, t.updated_at, t.permission_mode, t.goal, t.goal_paused, t.session_mode, t.reasoning_effort, t.pinned, t.pin_order_key FROM threads t JOIN provider_sessions s ON s.thread_id = t.id WHERE s.provider = ? AND s.native_session_id = ?",
                params![provider.as_str(), native_session_id],
                |row| {
                    let provider: String = row.get(3)?;
                    let permission_mode: String = row.get(7)?;
                    let session_mode: String = row.get(10)?;
                    Ok(ThreadRow {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        cwd: row.get(2)?,
                        current_provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Claude),
                        current_model: row.get(4)?,
                        reasoning_effort: row.get(11)?,
                        permission_mode: PermissionMode::parse(&permission_mode)
                            .unwrap_or(PermissionMode::Default),
                        goal: row.get(8)?,
                        goal_paused: row.get::<_, i64>(9)? != 0,
                        session_mode: SessionMode::parse(&session_mode)
                            .unwrap_or(SessionMode::Single),
                        pinned: row.get::<_, i64>(12)? != 0,
                        pin_order_key: row.get(13)?,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn next_turn_number(&self, thread_id: &str) -> Result<i64> {
        let max_turn: i64 = self
            .db
            .query_row(
                "SELECT COALESCE(MAX(turn), 0) FROM messages WHERE thread_id = ?",
                params![thread_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(max_turn + 1)
    }

    fn map_turn(row: &rusqlite::Row) -> rusqlite::Result<TurnRow> {
        let provider: String = row.get(2)?;
        let phase: String = row.get(4)?;
        Ok(TurnRow {
            thread_id: row.get(0)?,
            turn: row.get(1)?,
            provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Claude),
            model: row.get(3)?,
            phase: TurnPhase::parse(&phase).unwrap_or(TurnPhase::Unverified),
            error: row.get(5)?,
            provider_turn_id: row.get(6)?,
            started_at: row.get(7)?,
            ended_at: row.get(8)?,
        })
    }

    /// Starts or restarts the durable record for one turn.
    pub fn start_turn_record(
        &self,
        thread_id: &str,
        turn: i64,
        provider: ProviderId,
        model: Option<&str>,
    ) -> Result<()> {
        self.db.execute(
            r#"INSERT INTO turns (thread_id, turn, provider, model, phase, error, provider_turn_id, started_at, ended_at)
               VALUES (?, ?, ?, ?, 'running', NULL, NULL, ?, NULL)
               ON CONFLICT (thread_id, turn) DO UPDATE SET
                 provider = excluded.provider,
                 model = excluded.model,
                 phase = 'running',
                 error = NULL,
                 provider_turn_id = NULL,
                 started_at = excluded.started_at,
                 ended_at = NULL"#,
            params![thread_id, turn, provider.as_str(), model, now_iso()],
        )?;
        Ok(())
    }

    pub fn finish_turn_record(
        &self,
        thread_id: &str,
        turn: i64,
        phase: TurnPhase,
        error: Option<&str>,
    ) -> Result<()> {
        self.db.execute(
            "UPDATE turns SET phase = ?, error = ?, ended_at = ? WHERE thread_id = ? AND turn = ?",
            params![phase.as_str(), error, now_iso(), thread_id, turn],
        )?;
        Ok(())
    }

    /// Moves an open turn between queued and running without ending it.
    pub fn set_turn_phase(&self, thread_id: &str, turn: i64, phase: TurnPhase) -> Result<()> {
        self.db.execute(
            "UPDATE turns SET phase = ?, error = NULL, ended_at = NULL WHERE thread_id = ? AND turn = ?",
            params![phase.as_str(), thread_id, turn],
        )?;
        Ok(())
    }

    pub fn set_turn_provider_id(&self, thread_id: &str, turn: i64, provider_turn_id: &str) -> Result<()> {
        self.db.execute(
            "UPDATE turns SET provider_turn_id = ? WHERE thread_id = ? AND turn = ?",
            params![provider_turn_id, thread_id, turn],
        )?;
        Ok(())
    }

    pub fn get_turn_record(&self, thread_id: &str, turn: i64) -> Result<Option<TurnRow>> {
        self.db
            .query_row(
                "SELECT thread_id, turn, provider, model, phase, error, provider_turn_id, started_at, ended_at FROM turns WHERE thread_id = ? AND turn = ?",
                params![thread_id, turn],
                Self::map_turn,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_turn_records(&self, thread_id: &str) -> Result<Vec<TurnRow>> {
        let mut statement = self.db.prepare(
            "SELECT thread_id, turn, provider, model, phase, error, provider_turn_id, started_at, ended_at FROM turns WHERE thread_id = ? ORDER BY turn",
        )?;
        let rows = statement
            .query_map(params![thread_id], Self::map_turn)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Failed and unverified turns with the first user message of each turn.
    /// The first message is the original prompt; later user messages in the
    /// same turn are steers and are not part of a retry payload.
    pub fn list_retryable_turns(&self, thread_id: &str) -> Result<Vec<RetryableTurnRow>> {
        let mut statement = self.db.prepare(
            "SELECT t.turn, t.provider, t.model, t.phase,
                    CASE WHEN t.error IS NULL THEN NULL ELSE SUBSTR(t.error, 1, 160) END,
                    m.id, SUBSTR(m.text, 1, 240), LENGTH(m.text)
             FROM turns t
             JOIN messages m ON m.id = (
                 SELECT first.id FROM messages first
                 WHERE first.thread_id = t.thread_id AND first.turn = t.turn AND first.role = 'user'
                 ORDER BY first.id LIMIT 1
             )
             WHERE t.thread_id = ? AND t.phase IN ('failed', 'unverified')
             ORDER BY t.turn DESC",
        )?;
        let rows = statement
            .query_map(params![thread_id], |row| {
                let provider: String = row.get(1)?;
                let phase: String = row.get(3)?;
                Ok(RetryableTurnRow {
                    turn: row.get(0)?,
                    provider: crate::types::ProviderKey::from_stored(&provider),
                    model: row.get(2)?,
                    phase: TurnPhase::parse(&phase).unwrap_or(TurnPhase::Unverified),
                    error_preview: row.get(4)?,
                    message_id: row.get(5)?,
                    prompt_preview: row.get(6)?,
                    prompt_len: row.get::<_, i64>(7)?.max(0) as usize,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The full first user message for one confirmed retry row.
    pub fn retry_prompt(
        &self,
        thread_id: &str,
        turn: i64,
        message_id: i64,
    ) -> Result<Option<String>> {
        self.db
            .query_row(
                "SELECT text FROM messages WHERE id = ? AND thread_id = ? AND turn = ? AND role = 'user'",
                params![message_id, thread_id, turn],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn turn_has_user_message(&self, thread_id: &str, turn: i64) -> Result<bool> {
        self.db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE thread_id = ? AND turn = ? AND role = 'user')",
                params![thread_id, turn],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// The strongest durable reason this thread cannot start another turn.
    /// This is not a distributed lease; startup and retry reconciliation own
    /// the decision that an orchestration owner is gone.
    pub fn thread_open_work(&self, thread_id: &str) -> Result<Option<ThreadOpenWork>> {
        let (turn, queue, native_rewind, orchestration): (bool, bool, bool, bool) = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE thread_id = ?1 AND phase IN ('queued', 'running', 'settling')),
                    EXISTS(SELECT 1 FROM queued_messages WHERE thread_id = ?1),
                    EXISTS(SELECT 1 FROM native_rewinds WHERE thread_id = ?1),
                    EXISTS(SELECT 1 FROM orchestration_tasks task
                           JOIN orchestration_runs run ON run.id = task.run_id
                           WHERE task.thread_id = ?1 AND run.status = 'running')",
            params![thread_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        Ok(if turn {
            Some(ThreadOpenWork::Turn)
        } else if queue {
            Some(ThreadOpenWork::Queue)
        } else if native_rewind {
            Some(ThreadOpenWork::NativeRewind)
        } else if orchestration {
            Some(ThreadOpenWork::Orchestration)
        } else {
            None
        })
    }

    /// A process that starts after a crash must not show a running turn as
    /// live. Mark unfinished turns in this project as unverified.
    pub fn reconcile_running_turns(&self, cwd: Option<&str>) -> Result<Vec<TurnRow>> {
        let rows = match cwd {
            Some(cwd) => {
                let mut statement = self.db.prepare(
                    "SELECT tu.thread_id, tu.turn, tu.provider, tu.model, tu.phase, tu.error, tu.provider_turn_id, tu.started_at, tu.ended_at
                     FROM turns tu JOIN threads t ON t.id = tu.thread_id
                     WHERE t.cwd = ? AND tu.phase IN ('queued', 'running', 'settling') ORDER BY tu.started_at",
                )?;
                let rows = statement
                    .query_map(params![cwd], Self::map_turn)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            }
            None => {
                let mut statement = self.db.prepare(
                    "SELECT thread_id, turn, provider, model, phase, error, provider_turn_id, started_at, ended_at
                     FROM turns WHERE phase IN ('queued', 'running', 'settling') ORDER BY started_at",
                )?;
                let rows = statement
                    .query_map([], Self::map_turn)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            }
        };
        for row in &rows {
            self.db.execute(
                "UPDATE turns SET phase = 'unverified', error = COALESCE(error, 'baton exited before the turn settled'), ended_at = ? WHERE thread_id = ? AND turn = ?",
                params![now_iso(), row.thread_id, row.turn],
            )?;
        }
        Ok(rows)
    }

    /// Records a provider child so a later baton process can identify and reap
    /// it if this process exits without disposing it. The command line is the
    /// identity check: a reused PID with a different command must never be
    /// killed.
    pub fn record_provider_process(
        &self,
        pid: u32,
        provider: ProviderId,
        port: Option<u16>,
        command: &str,
        process_start: Option<&str>,
        cwd: &str,
        owner_pid: u32,
    ) -> Result<()> {
        self.db.execute(
            r#"INSERT INTO provider_processes (pid, provider, port, command, process_start, cwd, owner_pid, started_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(pid) DO UPDATE SET
                 provider = excluded.provider,
                 port = excluded.port,
                 command = excluded.command,
                 process_start = excluded.process_start,
                 cwd = excluded.cwd,
                 owner_pid = excluded.owner_pid,
                 started_at = excluded.started_at"#,
            params![pid, provider.as_str(), port, command, process_start, cwd, owner_pid, now_iso()],
        )?;
        Ok(())
    }

    pub fn remove_provider_process(&self, pid: u32) -> Result<()> {
        self.db.execute(
            "DELETE FROM provider_processes WHERE pid = ?",
            params![pid],
        )?;
        Ok(())
    }

    pub fn list_provider_processes(&self) -> Result<Vec<ProviderProcessRow>> {
        let mut statement = self.db.prepare(
            "SELECT pid, provider, port, command, process_start, cwd, owner_pid, started_at
             FROM provider_processes ORDER BY started_at, pid",
        )?;
        let rows = statement
            .query_map([], |row| {
                let provider: String = row.get(1)?;
                Ok(ProviderProcessRow {
                    pid: row.get::<_, i64>(0)?.max(0) as u32,
                    provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Claude),
                    port: row.get::<_, Option<i64>>(2)?.map(|port| port.clamp(0, u16::MAX as i64) as u16),
                    command: row.get(3)?,
                    process_start: row.get(4)?,
                    cwd: row.get(5)?,
                    owner_pid: row.get::<_, i64>(6)?.max(0) as u32,
                    started_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn enqueue_message(&self, thread_id: &str, text: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO queued_messages (thread_id, text, created_at) VALUES (?, ?, ?)",
            params![thread_id, text, now_iso()],
        )?;
        Ok(())
    }

    pub fn list_queued_messages(&self, thread_id: &str) -> Result<Vec<String>> {
        let mut statement = self
            .db
            .prepare("SELECT text FROM queued_messages WHERE thread_id = ? ORDER BY id")?;
        let rows = statement
            .query_map(params![thread_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    /// Removes and returns the oldest queued message for a thread.
    pub fn pop_queued_message(&self, thread_id: &str) -> Result<Option<String>> {
        let tx = self.db.unchecked_transaction()?;
        let text: Option<String> = tx
            .query_row(
                "SELECT text FROM queued_messages WHERE thread_id = ? ORDER BY id LIMIT 1",
                params![thread_id],
                |row| row.get(0),
            )
            .optional()?;
        if text.is_some() {
            tx.execute(
                "DELETE FROM queued_messages WHERE id = (
                    SELECT id FROM queued_messages WHERE thread_id = ? ORDER BY id LIMIT 1
                 )",
                params![thread_id],
            )?;
        }
        tx.commit()?;
        Ok(text)
    }

    /// Removes and returns the queued message at `index`, counted from zero.
    pub fn take_queued_message(&self, thread_id: &str, index: usize) -> Result<Option<String>> {
        let tx = self.db.unchecked_transaction()?;
        let id: Option<i64> = tx
            .query_row(
                "SELECT id FROM queued_messages WHERE thread_id = ? ORDER BY id LIMIT 1 OFFSET ?",
                params![thread_id, index as i64],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        let text: String = tx.query_row(
            "SELECT text FROM queued_messages WHERE id = ?",
            params![id],
            |row| row.get(0),
        )?;
        tx.execute("DELETE FROM queued_messages WHERE id = ?", params![id])?;
        tx.commit()?;
        Ok(Some(text))
    }

    /// Rewrites the whole queue in one transaction, preserving the given order.
    pub fn replace_queued_messages(&self, thread_id: &str, messages: &[String]) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM queued_messages WHERE thread_id = ?",
            params![thread_id],
        )?;
        for text in messages {
            tx.execute(
                "INSERT INTO queued_messages (thread_id, text, created_at) VALUES (?, ?, ?)",
                params![thread_id, text, now_iso()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_queued_messages(&self, thread_id: &str) -> Result<()> {
        self.db.execute(
            "DELETE FROM queued_messages WHERE thread_id = ?",
            params![thread_id],
        )?;
        Ok(())
    }

    pub fn add_message(
        &self,
        thread_id: &str,
        turn: i64,
        role: &str,
        provider: Option<&str>,
        model: Option<&str>,
        text: &str,
    ) -> Result<i64> {
        self.db.execute(
            "INSERT INTO messages (thread_id, turn, role, provider, model, text, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![thread_id, turn, role, provider, model, text, now_iso()],
        )?;
        let id = self.db.last_insert_rowid();
        self.touch_thread(thread_id)?;
        Ok(id)
    }

    pub fn list_recent_messages(&self, thread_id: &str, limit: usize) -> Result<Vec<MessageRow>> {
        let mut statement = self.db.prepare(
            "SELECT id, thread_id, turn, role, provider, model, text, created_at
             FROM messages WHERE thread_id = ? ORDER BY id DESC LIMIT ?",
        )?;
        let mut rows = statement
            .query_map(params![thread_id, limit as i64], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    thread_id: row.get(1)?,
                    turn: row.get(2)?,
                    role: row.get(3)?,
                    provider: row.get(4)?,
                    model: row.get(5)?,
                    text: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }

    pub fn list_messages(&self, thread_id: &str) -> Result<Vec<MessageRow>> {
        let mut statement = self
            .db
            .prepare("SELECT id, thread_id, turn, role, provider, model, text, created_at FROM messages WHERE thread_id = ? ORDER BY id")?;
        let rows = statement
            .query_map(params![thread_id], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    thread_id: row.get(1)?,
                    turn: row.get(2)?,
                    role: row.get(3)?,
                    provider: row.get(4)?,
                    model: row.get(5)?,
                    text: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Substring search across conversation text and thread titles. A cwd
    /// limits results to one project; None searches every durable thread.
    pub fn search_messages(
        &self,
        cwd: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MessageSearchRow>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let escaped = query
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let limit = limit.clamp(1, 500) as i64;
        let mut statement = match cwd {
            Some(_) => self.db.prepare(
                "SELECT m.id, m.thread_id, t.title, m.turn, m.role, m.provider, m.model, m.text, m.created_at
                 FROM messages m JOIN threads t ON t.id = m.thread_id
                 WHERE t.cwd = ? AND (lower(m.text) LIKE ? ESCAPE '\\' OR lower(t.title) LIKE ? ESCAPE '\\')
                 ORDER BY m.id DESC LIMIT ?",
            )?,
            None => self.db.prepare(
                "SELECT m.id, m.thread_id, t.title, m.turn, m.role, m.provider, m.model, m.text, m.created_at
                 FROM messages m JOIN threads t ON t.id = m.thread_id
                 WHERE lower(m.text) LIKE ? ESCAPE '\\' OR lower(t.title) LIKE ? ESCAPE '\\'
                 ORDER BY m.id DESC LIMIT ?",
            )?,
        };
        let map = |row: &rusqlite::Row| {
            Ok(MessageSearchRow {
                message_id: row.get(0)?,
                thread_id: row.get(1)?,
                title: row.get(2)?,
                turn: row.get(3)?,
                role: row.get(4)?,
                provider: row.get(5)?,
                model: row.get(6)?,
                text: row.get(7)?,
                created_at: row.get(8)?,
            })
        };
        let rows = match cwd {
            Some(cwd) => statement
                .query_map(params![cwd, pattern, pattern, limit], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => statement
                .query_map(params![pattern, pattern, limit], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    pub fn add_checkpoint(&self, thread_id: &str, turn: i64, git_ref: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO checkpoints (thread_id, turn, git_ref, created_at) VALUES (?, ?, ?, ?) ON CONFLICT (thread_id, turn) DO UPDATE SET git_ref = excluded.git_ref",
            params![thread_id, turn, git_ref, now_iso()],
        )?;
        Ok(())
    }

    pub fn list_checkpoints(&self, thread_id: &str) -> Result<Vec<CheckpointRow>> {
        let mut statement = self.db.prepare(
            "SELECT thread_id, turn, git_ref, created_at FROM checkpoints WHERE thread_id = ? ORDER BY turn",
        )?;
        let rows = statement
            .query_map(params![thread_id], |row| {
                Ok(CheckpointRow {
                    thread_id: row.get(0)?,
                    turn: row.get(1)?,
                    git_ref: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_checkpoint(&self, thread_id: &str, turn: i64) -> Result<Option<CheckpointRow>> {
        self.db
            .query_row(
                "SELECT thread_id, turn, git_ref, created_at FROM checkpoints WHERE thread_id = ? AND turn = ?",
                params![thread_id, turn],
                |row| {
                    Ok(CheckpointRow {
                        thread_id: row.get(0)?,
                        turn: row.get(1)?,
                        git_ref: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn add_usage(&self, thread_id: &str, turn: i64, provider: ProviderId, model: Option<&str>, usage: &TurnUsage) -> Result<()> {
        self.db.execute(
            "INSERT INTO usage (thread_id, turn, provider, model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                thread_id,
                turn,
                provider.as_str(),
                model,
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_tokens,
                usage.cache_creation_tokens,
                usage.cost_usd,
                now_iso()
            ],
        )?;
        Ok(())
    }

    pub fn list_usage(&self, thread_id: Option<&str>) -> Result<Vec<UsageRow>> {
        let (sql, binds): (String, Vec<String>) = match thread_id {
            Some(thread_id) => (
                "SELECT id, thread_id, turn, provider, model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd, created_at FROM usage WHERE thread_id = ? ORDER BY id".to_string(),
                vec![thread_id.to_string()],
            ),
            None => (
                "SELECT id, thread_id, turn, provider, model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd, created_at FROM usage ORDER BY id".to_string(),
                Vec::new(),
            ),
        };
        let mut statement = self.db.prepare(&sql)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(binds), |row| {
                let provider: String = row.get(3)?;
                Ok(UsageRow {
                    thread_id: row.get(1)?,
                    turn: row.get(2)?,
                    provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Claude),
                    model: row.get(4)?,
                    usage: TurnUsage {
                        input_tokens: row.get(5)?,
                        output_tokens: row.get(6)?,
                        cache_read_tokens: row.get(7)?,
                        cache_creation_tokens: row.get(8)?,
                        cost_usd: row.get(9)?,
                    },
                    created_at: row.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_native_rewind_pending(
        &self,
        thread_id: &str,
        provider: ProviderId,
        turn: i64,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO native_rewinds (thread_id, provider, turn, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(thread_id) DO UPDATE SET provider = excluded.provider, turn = excluded.turn, created_at = excluded.created_at",
            params![thread_id, provider.as_str(), turn, now_iso()],
        )?;
        Ok(())
    }

    pub fn native_rewind_pending(
        &self,
        thread_id: &str,
    ) -> Result<Option<NativeRewindPending>> {
        let row = self
            .db
            .query_row(
                "SELECT thread_id, provider, turn, created_at FROM native_rewinds WHERE thread_id = ?",
                params![thread_id],
                |row| {
                    let provider: String = row.get(1)?;
                    Ok(NativeRewindPending {
                        thread_id: row.get(0)?,
                        provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Codex),
                        turn: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn clear_native_rewind_pending(&self, thread_id: &str) -> Result<()> {
        self.db.execute(
            "DELETE FROM native_rewinds WHERE thread_id = ?",
            params![thread_id],
        )?;
        Ok(())
    }

    pub fn rewind_thread(&self, thread_id: &str, from_turn: i64) -> Result<()> {
        self.rewind_thread_inner(thread_id, from_turn, false)
    }

    /// Rewinds Baton's copy but keeps the native provider session. Use this
    /// after a provider-native rollback has already truncated provider history.
    pub fn rewind_thread_keeping_provider_sessions(
        &self,
        thread_id: &str,
        from_turn: i64,
    ) -> Result<()> {
        self.rewind_thread_inner(thread_id, from_turn, true)
    }

    fn rewind_thread_inner(
        &self,
        thread_id: &str,
        from_turn: i64,
        keep_provider_sessions: bool,
    ) -> Result<()> {
        if let Some(pending) = self.native_rewind_pending(thread_id)? {
            if pending.turn != from_turn {
                anyhow::bail!(
                    "thread has a pending {} native rewind to turn {}; run `baton rewind -t {} --turn {}` without --native to finish it first",
                    pending.provider,
                    pending.turn,
                    &thread_id[..8.min(thread_id.len())],
                    pending.turn
                );
            }
        }
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM messages WHERE thread_id = ? AND turn >= ?",
            params![thread_id, from_turn],
        )?;
        tx.execute(
            "DELETE FROM usage WHERE thread_id = ? AND turn >= ?",
            params![thread_id, from_turn],
        )?;
        tx.execute(
            "DELETE FROM turns WHERE thread_id = ? AND turn >= ?",
            params![thread_id, from_turn],
        )?;
        tx.execute(
            "DELETE FROM queued_messages WHERE thread_id = ?",
            params![thread_id],
        )?;
        tx.execute(
            "DELETE FROM review_notes WHERE thread_id = ? AND turn >= ?",
            params![thread_id, from_turn],
        )?;
        if !keep_provider_sessions {
            tx.execute("DELETE FROM provider_sessions WHERE thread_id = ?", params![thread_id])?;
        }
        tx.execute(
            "UPDATE threads SET updated_at = ? WHERE id = ?",
            params![now_iso(), thread_id],
        )?;
        tx.execute(
            "DELETE FROM native_rewinds WHERE thread_id = ?",
            params![thread_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn create_orchestration_run(
        &self,
        id: &str,
        goal: &str,
        coordinator_provider: ProviderId,
        coordinator_model: &str,
        summary: Option<&str>,
        plan_json: &str,
        dirty_source: bool,
    ) -> Result<()> {
        self.create_orchestration_run_owned(
            id,
            goal,
            coordinator_provider,
            coordinator_model,
            summary,
            plan_json,
            dirty_source,
            None,
            None,
        )
    }

    pub fn create_orchestration_run_owned(
        &self,
        id: &str,
        goal: &str,
        coordinator_provider: ProviderId,
        coordinator_model: &str,
        summary: Option<&str>,
        plan_json: &str,
        dirty_source: bool,
        owner_pid: Option<u32>,
        owner_start: Option<&str>,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO orchestration_runs (id, goal, coordinator_provider, coordinator_model, summary, plan_json, status, dirty_source, created_at, owner_pid, owner_start) VALUES (?, ?, ?, ?, ?, ?, 'running', ?, ?, ?, ?)",
            params![
                id,
                goal,
                coordinator_provider.as_str(),
                coordinator_model,
                summary,
                plan_json,
                dirty_source as i64,
                now_iso(),
                owner_pid,
                owner_start,
            ],
        )?;
        Ok(())
    }

    pub fn finish_orchestration_run(&self, id: &str, status: &str) -> Result<()> {
        self.db.execute(
            "UPDATE orchestration_runs SET status = ?, finished_at = ? WHERE id = ?",
            params![status, now_iso(), id],
        )?;
        Ok(())
    }

    /// Converts a run whose orchestrator process is gone into a terminal
    /// verdict while preserving every worker that already completed.
    pub fn abandon_orchestration_run(&self, run_id: &str, error: &str) -> Result<()> {
        let now = now_iso();
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "UPDATE orchestration_tasks SET status = 'failed', error = COALESCE(NULLIF(error, ''), ?), finished_at = COALESCE(finished_at, ?) WHERE run_id = ? AND status IN ('queued', 'running')",
            params![error, now, run_id],
        )?;
        tx.execute(
            "UPDATE turns SET phase = 'unverified', error = COALESCE(error, ?), ended_at = COALESCE(ended_at, ?) WHERE thread_id IN (SELECT thread_id FROM orchestration_tasks WHERE run_id = ? AND thread_id IS NOT NULL) AND phase IN ('queued', 'running', 'settling')",
            params![error, now, run_id],
        )?;
        let total: i64 = tx.query_row(
            "SELECT COUNT(*) FROM orchestration_tasks WHERE run_id = ?",
            params![run_id],
            |row| row.get(0),
        )?;
        let completed: i64 = tx.query_row(
            "SELECT COUNT(*) FROM orchestration_tasks WHERE run_id = ? AND status = 'completed'",
            params![run_id],
            |row| row.get(0),
        )?;
        let cancelled: i64 = tx.query_row(
            "SELECT COUNT(*) FROM orchestration_tasks WHERE run_id = ? AND status = 'cancelled'",
            params![run_id],
            |row| row.get(0),
        )?;
        let needs_input: i64 = tx.query_row(
            "SELECT COUNT(*) FROM orchestration_tasks WHERE run_id = ? AND status = 'needs_input'",
            params![run_id],
            |row| row.get(0),
        )?;
        let status = if cancelled > 0 {
            "cancelled"
        } else if needs_input > 0 {
            "needs_input"
        } else if total > 0 && completed == total {
            "completed"
        } else if completed > 0 {
            "partial"
        } else {
            "failed"
        };
        tx.execute(
            "UPDATE orchestration_runs SET status = ?, finished_at = ? WHERE id = ? AND status = 'running'",
            params![status, now, run_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn archive_orchestration_run(&self, id: &str) -> Result<()> {
        self.db.execute(
            "UPDATE orchestration_runs SET archived_at = COALESCE(archived_at, ?) WHERE id = ?",
            params![now_iso(), id],
        )?;
        Ok(())
    }

    pub fn add_orchestration_task(
        &self,
        id: &str,
        run_id: &str,
        position: i64,
        title: &str,
        provider: ProviderId,
        model: &str,
        prompt: &str,
        worktree_path: Option<&str>,
        branch: Option<&str>,
        commit_sha: Option<&str>,
        thread_id: Option<&str>,
        status: &str,
        text: Option<&str>,
        error: Option<&str>,
        created_at: &str,
        finished_at: Option<&str>,
        snapshot_base: Option<&str>,
        snapshot_tree: Option<&str>,
        reasoning_effort: Option<&str>,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO orchestration_tasks (id, run_id, position, title, provider, model, prompt, worktree_path, branch, commit_sha, thread_id, status, text, error, created_at, finished_at, snapshot_base, snapshot_tree, reasoning_effort) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                id,
                run_id,
                position,
                title,
                provider.as_str(),
                model,
                prompt,
                worktree_path,
                branch,
                commit_sha,
                thread_id,
                status,
                text,
                error,
                created_at,
                finished_at,
                snapshot_base,
                snapshot_tree,
                reasoning_effort,
            ],
        )?;
        Ok(())
    }

    pub fn mark_orchestration_task_merged(&self, id: &str, merged_at: &str) -> Result<()> {
        self.db.execute(
            "UPDATE orchestration_tasks SET merged_at = ? WHERE id = ?",
            params![merged_at, id],
        )?;
        Ok(())
    }

    pub fn mark_orchestration_task_pr(&self, id: &str, pr_url: &str) -> Result<()> {
        self.db.execute(
            "UPDATE orchestration_tasks SET pr_url = ? WHERE id = ?",
            params![pr_url, id],
        )?;
        Ok(())
    }

    pub fn list_orchestration_runs(&self, limit: usize) -> Result<Vec<OrchestrationRunRow>> {
        let mut statement = self.db.prepare(
            "SELECT id, goal, coordinator_provider, coordinator_model, summary, plan_json, status, dirty_source, created_at, finished_at, archived_at, owner_pid, owner_start FROM orchestration_runs ORDER BY created_at DESC LIMIT ?",
        )?;
        let rows = statement
            .query_map(params![limit as i64], map_orchestration_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_orchestration_runs(&self, run_id: &str) -> Result<Vec<OrchestrationRunRow>> {
        let mut statement = self.db.prepare(
            "SELECT id, goal, coordinator_provider, coordinator_model, summary, plan_json, status, dirty_source, created_at, finished_at, archived_at, owner_pid, owner_start FROM orchestration_runs WHERE id = ? OR id LIKE ? ORDER BY created_at DESC LIMIT 3",
        )?;
        let pattern = format!("{run_id}%");
        let rows = statement
            .query_map(params![run_id, pattern], map_orchestration_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_running_orchestration_runs(&self) -> Result<Vec<OrchestrationRunRow>> {
        let mut statement = self.db.prepare(
            "SELECT id, goal, coordinator_provider, coordinator_model, summary, plan_json, status, dirty_source, created_at, finished_at, archived_at, owner_pid, owner_start FROM orchestration_runs WHERE status = 'running' ORDER BY created_at",
        )?;
        let rows = statement
            .query_map([], map_orchestration_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn shipped_worktree_branches(&self) -> Result<Vec<String>> {
        let mut statement = self.db.prepare(
            "SELECT DISTINCT branch FROM orchestration_tasks WHERE pr_url IS NOT NULL AND branch IS NOT NULL",
        )?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_orchestration_tasks(&self, run_id: &str) -> Result<Vec<OrchestrationTaskRow>> {
        let mut statement = self.db.prepare(
            "SELECT id, run_id, position, title, provider, model, prompt, worktree_path, branch, commit_sha, thread_id, status, text, error, created_at, finished_at, merged_at, pr_url, snapshot_base, snapshot_tree, reasoning_effort FROM orchestration_tasks WHERE run_id = ? ORDER BY position, created_at, id",
        )?;
        let rows = statement
            .query_map(params![run_id], map_orchestration_task)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn orchestration_task_for_thread(
        &self,
        thread_id: &str,
    ) -> Result<Option<OrchestrationTaskRow>> {
        let mut statement = self.db.prepare(
            "SELECT id, run_id, position, title, provider, model, prompt, worktree_path, branch, commit_sha, thread_id, status, text, error, created_at, finished_at, merged_at, pr_url, snapshot_base, snapshot_tree, reasoning_effort FROM orchestration_tasks WHERE thread_id = ? ORDER BY created_at DESC LIMIT 1",
        )?;
        let row = statement
            .query_row(params![thread_id], map_orchestration_task)
            .optional()?;
        Ok(row)
    }

    pub fn update_orchestration_task_failure(
        &self,
        id: &str,
        status: &str,
        text: Option<&str>,
        error: &str,
    ) -> Result<()> {
        self.db.execute(
            "UPDATE orchestration_tasks SET status = ?, text = ?, error = ?, finished_at = ? WHERE id = ?",
            params![status, text, error, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn update_orchestration_task_success(
        &self,
        id: &str,
        text: Option<&str>,
        commit_sha: Option<&str>,
        clear_markers: bool,
    ) -> Result<()> {
        let finished_at = now_iso();
        if clear_markers {
            self.db.execute(
                "UPDATE orchestration_tasks SET status = 'completed', text = ?, error = NULL, commit_sha = COALESCE(?, commit_sha), finished_at = ?, merged_at = NULL, pr_url = NULL WHERE id = ?",
                params![text, commit_sha, finished_at, id],
            )?;
        } else {
            self.db.execute(
                "UPDATE orchestration_tasks SET status = 'completed', text = ?, error = NULL, commit_sha = COALESCE(?, commit_sha), finished_at = ? WHERE id = ?",
                params![text, commit_sha, finished_at, id],
            )?;
        }
        Ok(())
    }

    pub fn update_orchestration_task_needs_input(
        &self,
        id: &str,
        text: Option<&str>,
        commit_sha: Option<&str>,
        clear_markers: bool,
    ) -> Result<()> {
        let finished_at = now_iso();
        if clear_markers {
            self.db.execute(
                "UPDATE orchestration_tasks SET status = 'needs_input', text = ?, error = NULL, commit_sha = COALESCE(?, commit_sha), finished_at = ?, merged_at = NULL, pr_url = NULL WHERE id = ?",
                params![text, commit_sha, finished_at, id],
            )?;
        } else {
            self.db.execute(
                "UPDATE orchestration_tasks SET status = 'needs_input', text = ?, error = NULL, commit_sha = COALESCE(?, commit_sha), finished_at = ? WHERE id = ?",
                params![text, commit_sha, finished_at, id],
            )?;
        }
        Ok(())
    }

    pub fn refresh_orchestration_run_status(&self, run_id: &str) -> Result<()> {
        let tasks = self.list_orchestration_tasks(run_id)?;
        if tasks.is_empty() {
            return Ok(());
        }
        let completed = tasks.iter().filter(|task| task.status == "completed").count();
        let cancelled = tasks.iter().any(|task| task.status == "cancelled");
        let needs_input = tasks.iter().any(|task| task.status == "needs_input");
        let running = tasks.iter().any(|task| task.status == "running");
        let status = if running {
            "running"
        } else if cancelled {
            "cancelled"
        } else if needs_input {
            "needs_input"
        } else if completed == tasks.len() {
            "completed"
        } else if completed == 0 {
            "failed"
        } else {
            "partial"
        };
        self.db.execute(
            "UPDATE orchestration_runs SET status = ?, finished_at = ? WHERE id = ?",
            params![status, if status == "running" { None } else { Some(now_iso()) }, run_id],
        )?;
        Ok(())
    }

    pub fn import_thread(
        &self,
        input: NewThread,
        native_session_id: &str,
        rollout_path: Option<String>,
        messages: &[ImportedMessage],
    ) -> Result<ThreadRow> {
        let now = now_iso();
        let thread = self.create_thread_with_id(input, Uuid::new_v4().to_string())?;
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO provider_sessions (thread_id, provider, native_session_id, model, rollout_path, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
            params![
                thread.id,
                thread.current_provider.as_str(),
                native_session_id,
                thread.current_model,
                rollout_path,
                now
            ],
        )?;
        let mut turn = 0i64;
        for message in messages {
            if message.role == "user" {
                turn += 1;
            }
            tx.execute(
                "INSERT INTO messages (thread_id, turn, role, provider, model, text, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    thread.id,
                    turn,
                    message.role,
                    thread.current_provider.as_str(),
                    thread.current_model,
                    message.text,
                    message.created_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(thread)
    }
}

pub struct ImportedMessage {
    pub role: &'static str,
    pub text: String,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_files_upsert_and_order() {
        let store = BatonStore::memory().unwrap();
        store.record_recent_file("/p", "a.rs").unwrap();
        store.record_recent_file("/p", "b.rs").unwrap();
        store.record_recent_file("/p", "a.rs").unwrap();
        assert_eq!(
            store.list_recent_files("/p", 10).unwrap(),
            vec!["a.rs".to_string(), "b.rs".to_string()]
        );
    }

    #[test]
    fn recent_files_are_project_scoped() {
        let store = BatonStore::memory().unwrap();
        store.record_recent_file("/p", "a.rs").unwrap();
        store.record_recent_file("/q", "b.rs").unwrap();
        assert_eq!(
            store.list_recent_files("/p", 10).unwrap(),
            vec!["a.rs".to_string()]
        );
        assert_eq!(
            store.list_recent_files("/q", 10).unwrap(),
            vec!["b.rs".to_string()]
        );
    }

    #[test]
    fn recent_files_are_capped() {
        let store = BatonStore::memory().unwrap();
        for index in 0..(RECENT_FILE_LIMIT + 5) {
            store
                .record_recent_file("/p", &format!("f{index}.rs"))
                .unwrap();
        }
        let recent = store.list_recent_files("/p", 1000).unwrap();
        assert_eq!(recent.len(), RECENT_FILE_LIMIT);
        assert!(!recent.contains(&"f0.rs".to_string()));
        assert!(recent.contains(&format!("f{}.rs", RECENT_FILE_LIMIT + 4)));
    }

    #[test]
    fn missing_recent_projects_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let store = BatonStore::memory().unwrap();
        store
            .record_recent_file(project.to_str().unwrap(), "a.rs")
            .unwrap();
        assert_eq!(store.prune_missing_recent_projects().unwrap(), 0);
        std::fs::remove_dir(&project).unwrap();
        assert_eq!(store.prune_missing_recent_projects().unwrap(), 1);
        assert!(store
            .list_recent_files(project.to_str().unwrap(), 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn recent_files_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite");
        {
            let store = BatonStore::open(&path).unwrap();
            store.record_recent_file("/p", "src/main.rs").unwrap();
        }
        let store = BatonStore::open(&path).unwrap();
        assert_eq!(
            store.list_recent_files("/p", 10).unwrap(),
            vec!["src/main.rs".to_string()]
        );
    }

    #[test]
    fn pin_order_keys_advance_within_one_microsecond() {
        let base = DateTime::parse_from_rfc3339("2026-01-02T03:04:05.006000Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = base + Duration::nanoseconds(789);
        assert_eq!(
            next_pin_order_key_from(now, Some(base)),
            "2026-01-02T03:04:05.006001Z"
        );
        let later = base + Duration::microseconds(2);
        assert_eq!(
            next_pin_order_key_from(later, Some(base)),
            "2026-01-02T03:04:05.006002Z"
        );
    }

    #[test]
    fn retryable_turns_keep_the_first_prompt_and_exclude_open_or_completed_rows() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "retry".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Claude,
                current_model: None,
            })
            .unwrap();
        let first_prompt = "a".repeat(300);
        store
            .add_message(&thread.id, 1, "user", Some("claude"), None, &first_prompt)
            .unwrap();
        store
            .add_message(&thread.id, 1, "user", Some("claude"), None, "steer")
            .unwrap();
        let first_error = format!("first\n{}", "e".repeat(220));
        store
            .start_turn_record(&thread.id, 1, ProviderId::Claude, None)
            .unwrap();
        store
            .finish_turn_record(&thread.id, 1, TurnPhase::Failed, Some(&first_error))
            .unwrap();
        store
            .add_message(&thread.id, 2, "user", Some("claude"), None, "done")
            .unwrap();
        store
            .start_turn_record(&thread.id, 2, ProviderId::Claude, None)
            .unwrap();
        store
            .finish_turn_record(&thread.id, 2, TurnPhase::Completed, None)
            .unwrap();
        store
            .add_message(&thread.id, 3, "user", Some("claude"), None, "open")
            .unwrap();
        store
            .start_turn_record(&thread.id, 3, ProviderId::Claude, None)
            .unwrap();

        let rows = store.list_retryable_turns(&thread.id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].turn, 1);
        assert_eq!(rows[0].provider.as_str(), "claude");
        assert_eq!(rows[0].phase, TurnPhase::Failed);
        assert_eq!(rows[0].error_preview.as_ref().unwrap().chars().count(), 160);
        assert!(rows[0]
            .error_preview
            .as_deref()
            .unwrap()
            .starts_with("first\n"));
        assert_eq!(rows[0].prompt_preview.chars().count(), 240);
        assert_eq!(rows[0].prompt_len, 300);
        assert_eq!(
            store
                .retry_prompt(&thread.id, 1, rows[0].message_id)
                .unwrap()
                .as_deref(),
            Some(first_prompt.as_str())
        );
        assert!(store
            .retry_prompt(&thread.id, 2, rows[0].message_id)
            .unwrap()
            .is_none());
        assert_eq!(
            store.thread_open_work(&thread.id).unwrap(),
            Some(ThreadOpenWork::Turn)
        );
        store
            .set_turn_phase(&thread.id, 3, TurnPhase::Queued)
            .unwrap();
        assert_eq!(
            store.thread_open_work(&thread.id).unwrap(),
            Some(ThreadOpenWork::Turn)
        );
        store
            .set_turn_phase(&thread.id, 3, TurnPhase::Settling)
            .unwrap();
        assert_eq!(
            store.thread_open_work(&thread.id).unwrap(),
            Some(ThreadOpenWork::Turn)
        );

        store
            .finish_turn_record(&thread.id, 3, TurnPhase::Completed, None)
            .unwrap();
        assert_eq!(store.thread_open_work(&thread.id).unwrap(), None);

        store
            .start_turn_record(&thread.id, 4, ProviderId::Claude, None)
            .unwrap();
        store
            .finish_turn_record(&thread.id, 4, TurnPhase::Failed, Some("no prompt"))
            .unwrap();
        store
            .add_message(&thread.id, 5, "user", Some("claude"), None, "stopped")
            .unwrap();
        store
            .start_turn_record(&thread.id, 5, ProviderId::Claude, None)
            .unwrap();
        store
            .finish_turn_record(&thread.id, 5, TurnPhase::Interrupted, None)
            .unwrap();
        assert!(!store.turn_has_user_message(&thread.id, 4).unwrap());
        assert!(store.turn_has_user_message(&thread.id, 5).unwrap());
        let rows = store.list_retryable_turns(&thread.id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].turn, 1);

        store.enqueue_message(&thread.id, "queued").unwrap();
        assert_eq!(
            store.thread_open_work(&thread.id).unwrap(),
            Some(ThreadOpenWork::Queue)
        );
        store.clear_queued_messages(&thread.id).unwrap();
        assert_eq!(store.thread_open_work(&thread.id).unwrap(), None);

        store
            .set_native_rewind_pending(&thread.id, ProviderId::Codex, 2)
            .unwrap();
        assert_eq!(
            store.thread_open_work(&thread.id).unwrap(),
            Some(ThreadOpenWork::NativeRewind)
        );
        store.clear_native_rewind_pending(&thread.id).unwrap();
        assert_eq!(store.thread_open_work(&thread.id).unwrap(), None);

        store
            .db
            .execute(
                "INSERT INTO orchestration_runs (id, goal, coordinator_provider, coordinator_model, plan_json, status, created_at) VALUES ('run', 'goal', 'claude', 'm', '{}', 'running', 'now')",
                [],
            )
            .unwrap();
        store
            .db
            .execute(
                "INSERT INTO orchestration_tasks (id, run_id, title, provider, model, prompt, thread_id, status, created_at) VALUES ('task', 'run', 'title', 'claude', 'm', 'prompt', ?, 'queued', 'now')",
                params![thread.id],
            )
            .unwrap();
        assert_eq!(
            store.thread_open_work(&thread.id).unwrap(),
            Some(ThreadOpenWork::Orchestration)
        );
        store
            .db
            .execute(
                "UPDATE orchestration_runs SET status = 'completed' WHERE id = 'run'",
                [],
            )
            .unwrap();
        assert_eq!(store.thread_open_work(&thread.id).unwrap(), None);
    }
}
