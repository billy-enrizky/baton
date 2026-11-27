use anyhow::Result;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{
    params, params_from_iter, Connection, OpenFlags, OptionalExtension, Transaction,
    TransactionBehavior,
};
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::path::{Path, PathBuf};
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
    /// Set together: when the snooze was requested and when it ends.
    pub snoozed_at: Option<String>,
    pub snoozed_until: Option<String>,
    /// Hidden from the default thread list and the inbox while set.
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Why an archive or unarchive did or did not change a thread. A running turn
/// is the claim that keeps a thread from being hidden mid-flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveOutcome {
    Archived,
    Unarchived,
    AlreadyArchived,
    NotArchived,
    RunningTurn,
    NotFound,
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
    pub owner_pid: Option<u32>,
    pub owner_start: Option<String>,
    pub owner_grant_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRow {
    pub grant_id: String,
    pub installation_id: String,
    pub secret_hash: String,
    pub scopes: Vec<String>,
    pub permission_ceiling: String,
    pub project_allowlist: Vec<String>,
    pub issued_at: String,
    pub expires_at: String,
    pub revoked_at: Option<String>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantSummary {
    pub grant_id: String,
    pub scopes: Vec<String>,
    pub permission_ceiling: String,
    pub project_allowlist: Vec<String>,
    pub issued_at: String,
    pub expires_at: String,
    pub revoked_at: Option<String>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAuditEvent {
    pub actor: String,
    pub environment: String,
    pub target: Option<String>,
    pub thread_id: Option<String>,
    pub action: String,
    pub scope: String,
    pub confirmation: String,
    pub payload_digest: String,
    pub result: String,
    pub retention: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEventRow {
    pub id: i64,
    pub actor: String,
    pub environment: String,
    pub target: Option<String>,
    pub thread_id: Option<String>,
    pub action: String,
    pub scope: String,
    pub confirmation: String,
    pub payload_digest: String,
    pub result: String,
    pub created_at: String,
    pub retention: String,
}

/// One completed renewal: the window before the change, the resulting row, and
/// the audit event written with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenewedGrant {
    pub previous_expires_at: String,
    pub grant: GrantSummary,
    pub event: AuditEventRow,
}

/// The three outcomes of a renewal attempt. A revoked grant is named rather
/// than folded into "missing", because the operator has to pair again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenewGrantOutcome {
    Renewed(Box<RenewedGrant>),
    Missing,
    Revoked,
}

impl GrantRow {
    pub fn summary(&self) -> GrantSummary {
        GrantSummary {
            grant_id: self.grant_id.clone(),
            scopes: self.scopes.clone(),
            permission_ceiling: self.permission_ceiling.clone(),
            project_allowlist: self.project_allowlist.clone(),
            issued_at: self.issued_at.clone(),
            expires_at: self.expires_at.clone(),
            revoked_at: self.revoked_at.clone(),
            label: self.label.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostClaimRow {
    pub host_id: String,
    pub kind: String,
    pub installation_id: String,
    pub pid: u32,
    pub process_start: String,
    pub claimed_at: String,
    pub heartbeat_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostClaimAttempt {
    Claimed(HostClaimRow),
    Owned { pid: u32, process_start: String },
    OpenWork { pid: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeoverAttempt {
    Taken(HostClaimRow),
    NoClaim,
    Changed { pid: u32, process_start: String },
    NotLive { pid: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TakeoverSettlement {
    pub turns: usize,
    pub runs: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSettlement {
    pub tasks: usize,
    pub turns: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostWorkSettlement {
    pub turns: usize,
    pub tasks: usize,
    pub runs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunVerdict {
    Completed,
    Partial,
    Failed,
    Cancelled,
    NeedsReview,
    NeedsInput,
}

impl RunVerdict {
    pub const ALL: [RunVerdict; 6] = [
        RunVerdict::Completed,
        RunVerdict::Partial,
        RunVerdict::Failed,
        RunVerdict::Cancelled,
        RunVerdict::NeedsReview,
        RunVerdict::NeedsInput,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RunVerdict::Completed => "completed",
            RunVerdict::Partial => "partial",
            RunVerdict::Failed => "failed",
            RunVerdict::Cancelled => "cancelled",
            RunVerdict::NeedsReview => "needs_review",
            RunVerdict::NeedsInput => "needs_input",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotTheHost {
    pub owner_pid: Option<u32>,
    pub host_id: Option<String>,
}

impl fmt::Display for NotTheHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.owner_pid, self.host_id.as_deref()) {
            (Some(pid), Some(host_id)) => formatter.write_str(&format!(
                "not_the_host: the store is claimed by pid {pid} as host {host_id}"
            )),
            (Some(pid), None) => {
                formatter.write_str(&format!("not_the_host: the store is claimed by pid {pid}"))
            }
            (None, _) => {
                formatter.write_str("not_the_host: this store no longer has a live host claim")
            }
        }
    }
}

impl std::error::Error for NotTheHost {}

/// A client found a live host claim without carrying that host's generation.
/// The message is the operator-facing exit-4 refusal, without the `baton: `
/// prefix that the binary adds on stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRefusal {
    pub owner_pid: u32,
    pub owner_start: String,
}

impl fmt::Display for ClientRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&format!(
            "pid {} started {} owns this store; stop it or use baton host takeover --pid {}",
            self.owner_pid, self.owner_start, self.owner_pid
        ))
    }
}

impl std::error::Error for ClientRefusal {}

impl ClientRefusal {
    pub fn exit_code(&self) -> i32 {
        4
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitRefused(pub ThreadOpenWork);

impl fmt::Display for SubmitRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.0 {
            ThreadOpenWork::Turn => "a turn is already running for this thread",
            ThreadOpenWork::Queue => {
                "the thread has queued messages; send or clear them before starting a turn"
            }
            ThreadOpenWork::NativeRewind => "a provider-native rewind is still pending",
            ThreadOpenWork::Orchestration => "an orchestration is still running for this thread",
        })
    }
}

impl std::error::Error for SubmitRefused {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlan {
    pub provider: ProviderId,
    pub plan_id: Option<String>,
    pub turn_id: Option<String>,
    pub entries: Vec<crate::types::PlanEntry>,
    pub explanation: Option<String>,
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
    pub review_status: Option<String>,
    pub review_text: Option<String>,
    pub reviewed_at: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

/// One worker that cannot continue without a person. The question is already
/// extracted from the worker's last message so a caller never has to parse it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerQuestionRow {
    pub task_id: String,
    pub run_id: String,
    pub thread_id: String,
    pub title: String,
    pub question: String,
    pub created_at: String,
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
pub struct SubagentRow {
    pub id: i64,
    pub thread_id: String,
    pub turn: i64,
    pub provider: String,
    pub call_id: Option<String>,
    pub name: String,
    pub detail: Option<String>,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
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
    pub owner_start: Option<String>,
    pub host_id: Option<String>,
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

pub(crate) fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn store_home(path: &Path) -> Option<PathBuf> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

pub const SCHEMA_VERSION: i64 = 7;
/// The first schema with the `host_claim` table. A client can check ownership
/// only from this version on, before it runs any migration DDL.
const HOST_CLAIM_SCHEMA_VERSION: i64 = 2;

pub struct BatonStore {
    db: Connection,
    home: Option<PathBuf>,
    host_id: Option<String>,
}

pub(crate) struct WriteGuard<'a> {
    tx: Transaction<'a>,
    _lock: Option<crate::claim::WriteLock>,
}

impl Deref for WriteGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.tx
    }
}

impl WriteGuard<'_> {
    fn commit(self) -> Result<()> {
        self.tx.commit()?;
        Ok(())
    }
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
  owner_pid INTEGER,
  owner_start TEXT,
  owner_grant_id TEXT,
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
CREATE TABLE IF NOT EXISTS subagents (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  thread_id TEXT NOT NULL,
  turn INTEGER NOT NULL,
  provider TEXT NOT NULL,
  call_id TEXT,
  name TEXT NOT NULL,
  detail TEXT,
  status TEXT NOT NULL,
  started_at TEXT NOT NULL,
  finished_at TEXT
);
CREATE INDEX IF NOT EXISTS subagents_thread_turn_idx ON subagents (thread_id, turn, id);
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
CREATE TABLE IF NOT EXISTS side_chats (
  thread_id TEXT PRIMARY KEY,
  source_thread_id TEXT NOT NULL,
  created_at TEXT NOT NULL
);
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
  owner_start TEXT,
  host_id TEXT,
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
CREATE TABLE IF NOT EXISTS plans (
  thread_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  plan_id TEXT,
  turn_id TEXT,
  entries TEXT NOT NULL,
  explanation TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (thread_id, provider)
);
CREATE TABLE IF NOT EXISTS provider_rate_limits (
  provider TEXT PRIMARY KEY,
  info TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS grants (
  grant_id TEXT PRIMARY KEY,
  installation_id TEXT NOT NULL,
  secret_hash TEXT NOT NULL,
  scopes TEXT NOT NULL,
  permission_ceiling TEXT NOT NULL,
  project_allowlist TEXT NOT NULL,
  issued_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  revoked_at TEXT,
  label TEXT
);
CREATE TABLE IF NOT EXISTS audit_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  actor TEXT NOT NULL CHECK (length(actor) > 0),
  environment TEXT NOT NULL CHECK (length(environment) > 0),
  target TEXT CHECK (target IS NULL OR length(target) > 0),
  thread_id TEXT,
  action TEXT NOT NULL CHECK (length(action) > 0),
  scope TEXT NOT NULL CHECK (length(scope) > 0),
  confirmation TEXT NOT NULL CHECK (length(confirmation) > 0),
  payload_digest TEXT NOT NULL CHECK (length(payload_digest) = 64),
  result TEXT NOT NULL CHECK (length(result) > 0),
  created_at TEXT NOT NULL CHECK (length(created_at) > 0),
  retention TEXT NOT NULL CHECK (length(retention) > 0)
);
CREATE TRIGGER IF NOT EXISTS audit_events_no_update
BEFORE UPDATE ON audit_events
BEGIN
  SELECT RAISE(ABORT, 'audit_events_append_only');
END;
CREATE TRIGGER IF NOT EXISTS audit_events_no_delete
BEFORE DELETE ON audit_events
BEGIN
  SELECT RAISE(ABORT, 'audit_events_append_only');
END;
CREATE TABLE IF NOT EXISTS host_claim (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  host_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('serve')),
  installation_id TEXT NOT NULL,
  pid INTEGER NOT NULL,
  process_start TEXT NOT NULL,
  claimed_at TEXT NOT NULL,
  heartbeat_at TEXT NOT NULL
);
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
    ensure_column(db, "turns", "owner_grant_id", "TEXT")?;
    ensure_column(db, "recent_files", "seq", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(db, "orchestration_tasks", "commit_sha", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "merged_at", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "position", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(db, "orchestration_tasks", "pr_url", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "review_status", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "review_text", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "reviewed_at", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "snapshot_base", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "snapshot_tree", "TEXT")?;
    ensure_column(db, "orchestration_tasks", "reasoning_effort", "TEXT")?;
    ensure_column(db, "orchestration_runs", "dirty_source", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(db, "orchestration_runs", "archived_at", "TEXT")?;
    ensure_column(db, "orchestration_runs", "owner_pid", "INTEGER")?;
    ensure_column(db, "orchestration_runs", "owner_start", "TEXT")?;
    ensure_column(db, "provider_processes", "owner_start", "TEXT")?;
    ensure_column(db, "provider_processes", "host_id", "TEXT")?;
    ensure_column(db, "audit_events", "target", "TEXT")?;
    db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
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
    ensure_column(db, "threads", "snoozed_at", "TEXT")?;
    ensure_column(db, "threads", "snoozed_until", "TEXT")?;
    ensure_column(db, "threads", "archived_at", "TEXT")?;
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
        snoozed_at: row.get(14)?,
        snoozed_until: row.get(15)?,
        archived_at: row.get(16)?,
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
        review_status: row.get(14)?,
        review_text: row.get(15)?,
        reviewed_at: row.get(16)?,
        created_at: row.get(17)?,
        finished_at: row.get(18)?,
        merged_at: row.get(19)?,
        pr_url: row.get(20)?,
        snapshot_base: row.get(21)?,
        snapshot_tree: row.get(22)?,
        reasoning_effort: row.get(23)?,
    })
}

const THREAD_COLUMNS: &str = "id, title, cwd, current_provider, current_model, created_at, updated_at, permission_mode, goal, goal_paused, session_mode, reasoning_effort, pinned, pin_order_key, snoozed_at, snoozed_until, archived_at";

/// The same list with table aliases, for queries that join another table.
/// Keep the order identical to `THREAD_COLUMNS`: `map_thread` reads positions.
const THREAD_COLUMNS_ALIASED: &str = "t.id, t.title, t.cwd, t.current_provider, t.current_model, t.created_at, t.updated_at, t.permission_mode, t.goal, t.goal_paused, t.session_mode, t.reasoning_effort, t.pinned, t.pin_order_key, t.snoozed_at, t.snoozed_until, t.archived_at";

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
        db.pragma_update(None, "journal_mode", "WAL")?;
        apply_schema(&db)?;
        Ok(BatonStore {
            db,
            home: store_home(path),
            host_id: None,
        })
    }

    /// Opens the writable store as a host generation. Every write must match a
    /// live claim carrying this id. Claiming itself uses the untracked row path.
    pub fn open_hosted(path: &Path, host_id: &str) -> Result<BatonStore> {
        let mut store = Self::open(path)?;
        store.host_id = Some(host_id.to_string());
        Ok(store)
    }

    pub fn host_id(&self) -> Option<&str> {
        self.host_id.as_deref()
    }

    /// Open a store for a CLI client. A trusted host child keeps its generation.
    /// Every other client checks the live claim without running schema DDL, and
    /// refuses with exit 4 while a host owns the store. The check and the
    /// subsequent writable open share the write lock, so claim acquisition
    /// cannot slip between them.
    pub fn open_client(path: &Path, host_id: Option<&str>) -> Result<BatonStore> {
        if let Some(host_id) = host_id {
            return Self::open_hosted(path, host_id);
        }
        let lock = if let Some(home) = store_home(path) {
            match crate::claim::acquire_write_lock(&home, crate::claim::LOCK_WAIT)? {
                crate::claim::LockAttempt::Acquired(lock) => Some(lock),
                crate::claim::LockAttempt::Busy => {
                    anyhow::bail!("write_lock_busy: another Baton process is writing")
                }
                crate::claim::LockAttempt::Unsupported => None,
            }
        } else {
            None
        };
        if !path.exists() {
            return Self::open(path);
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            anyhow::bail!(
                "schema_too_new: store schema {version}, binary expects {SCHEMA_VERSION}"
            );
        }
        if version < SCHEMA_VERSION {
            // A schema that predates host_claim cannot be host-owned. For
            // later schemas, read the claim before running DDL, so a new
            // binary never migrates a store an older host still owns.
            let claim = if version >= HOST_CLAIM_SCHEMA_VERSION {
                Self::get_host_claim_from(&db)?
            } else {
                None
            };
            drop(db);
            if let Some(claim) = claim.filter(|claim| {
                crate::process::process_identity_alive(claim.pid, Some(&claim.process_start))
            }) {
                return Err(ClientRefusal {
                    owner_pid: claim.pid,
                    owner_start: claim.process_start,
                }
                .into());
            }
            return Self::open(path);
        }
        let claim = Self::get_host_claim_from(&db)?;
        drop(db);
        if let Some(claim) = claim.filter(|claim| {
            crate::process::process_identity_alive(claim.pid, Some(&claim.process_start))
        }) {
            return Err(ClientRefusal {
                owner_pid: claim.pid,
                owner_start: claim.process_start,
            }
            .into());
        }
        let store = Self::open(path);
        drop(lock);
        store
    }

    /// Open an existing store without running schema migration or taking a
    /// writer path. This is the read-only client entry point for a host-owned
    /// store. A pre-versioned or future schema is refused rather than guessed.
    pub fn open_read_only(path: &Path) -> Result<BatonStore> {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < SCHEMA_VERSION {
            anyhow::bail!(
                "schema_too_old: store schema {version}, binary expects {SCHEMA_VERSION}"
            );
        }
        if version > SCHEMA_VERSION {
            anyhow::bail!(
                "schema_too_new: store schema {version}, binary expects {SCHEMA_VERSION}"
            );
        }
        db.pragma_update(None, "query_only", true)?;
        Ok(BatonStore {
            db,
            home: store_home(path),
            host_id: None,
        })
    }

    pub fn memory() -> Result<BatonStore> {
        let db = Connection::open_in_memory()?;
        apply_schema(&db)?;
        Ok(BatonStore {
            db,
            home: None,
            host_id: None,
        })
    }

    fn write(&self) -> Result<WriteGuard<'_>> {
        let lock = if let Some(home) = self.home.as_deref() {
            match crate::claim::acquire_write_lock(home, crate::claim::LOCK_WAIT)? {
                crate::claim::LockAttempt::Acquired(lock) => Some(lock),
                crate::claim::LockAttempt::Busy => {
                    anyhow::bail!("write_lock_busy: another Baton process is writing")
                }
                crate::claim::LockAttempt::Unsupported => None,
            }
        } else {
            None
        };
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        self.ensure_write_fence(&tx)?;
        Ok(WriteGuard { tx, _lock: lock })
    }

    fn write_execute(&self, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
        let write = self.write()?;
        let changed = write.execute(sql, params)?;
        write.commit()?;
        Ok(changed)
    }

    fn ensure_write_fence(&self, db: &Connection) -> Result<()> {
        let claim = Self::get_host_claim_from(db)?;
        let live = claim.and_then(|claim| {
            crate::process::process_identity_alive(claim.pid, Some(&claim.process_start))
                .then_some(claim)
        });
        match (self.host_id.as_deref(), live.as_ref()) {
            (Some(expected), Some(claim)) if claim.host_id == expected => Ok(()),
            (Some(_), claim) => Err(NotTheHost {
                owner_pid: claim.map(|claim| claim.pid),
                host_id: claim.map(|claim| claim.host_id.clone()),
            }
            .into()),
            (None, Some(claim)) => Err(NotTheHost {
                owner_pid: Some(claim.pid),
                host_id: Some(claim.host_id.clone()),
            }
            .into()),
            (None, None) => Ok(()),
        }
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
        self.write_execute(
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
            snoozed_at: None,
            snoozed_until: None,
            archived_at: None,
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
        let (thread, _) =
            self.fork_thread_through_turn_inner(source_id, title, through_turn, None)?;
        Ok(thread)
    }

    /// A fork and its audit event commit together. The event's thread id is
    /// replaced with the new thread id; the caller supplies a digest over the
    /// source, title, and cutoff.
    pub fn fork_thread_through_turn_with_audit(
        &self,
        source_id: &str,
        title: &str,
        through_turn: Option<i64>,
        event: &NewAuditEvent,
    ) -> Result<(ThreadRow, AuditEventRow)> {
        let (thread, audit) =
            self.fork_thread_through_turn_inner(source_id, title, through_turn, Some(event))?;
        let audit = audit.ok_or_else(|| anyhow::anyhow!("fork audit event was not written"))?;
        Ok((thread, audit))
    }

    fn fork_thread_through_turn_inner(
        &self,
        source_id: &str,
        title: &str,
        through_turn: Option<i64>,
        event: Option<&NewAuditEvent>,
    ) -> Result<(ThreadRow, Option<AuditEventRow>)> {
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
        let tx = self.write()?;
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
        let audit = if let Some(event) = event {
            let mut event = event.clone();
            event.thread_id = Some(id.clone());
            let created_at = now_iso();
            let audit_id = Self::insert_audit_event_in(&tx, &event, &created_at)?;
            Some(Self::audit_event_row(audit_id, created_at, &event))
        } else {
            None
        };
        tx.commit()?;
        let thread = self
            .get_thread(&id)?
            .ok_or_else(|| anyhow::anyhow!("forked thread disappeared: {id}"))?;
        Ok((thread, audit))
    }

    /// Marks a forked thread as a side chat without changing the source.
    pub fn mark_side_chat(&self, thread_id: &str, source_thread_id: &str) -> Result<()> {
        self.write_execute(
            "INSERT OR REPLACE INTO side_chats (thread_id, source_thread_id, created_at) VALUES (?, ?, ?)",
            params![thread_id, source_thread_id, now_iso()],
        )?;
        Ok(())
    }

    /// The thread a side chat was forked from, if this thread is one.
    pub fn side_chat_source(&self, thread_id: &str) -> Result<Option<ThreadRow>> {
        let source: Option<String> = self
            .db
            .query_row(
                "SELECT source_thread_id FROM side_chats WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .optional()?;
        match source {
            Some(source) => self.get_thread(&source),
            None => Ok(None),
        }
    }

    /// When the side chat was forked. Messages created after this time are the
    /// fork delta that `/merge-back` transfers to the source.
    pub fn side_chat_forked_at(&self, thread_id: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT created_at FROM side_chats WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .optional()?)
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
            snoozed_at: None,
            snoozed_until: None,
            archived_at: None,
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

    pub fn list_threads_for_path_display_all(&self, cwd: &Path) -> Result<Vec<ThreadRow>> {
        self.list_threads_for_path_matching(cwd, true, true)
    }

    fn list_threads_for_path_ordered(
        &self,
        cwd: &Path,
        pinned_first: bool,
    ) -> Result<Vec<ThreadRow>> {
        self.list_threads_for_path_matching(cwd, pinned_first, false)
    }

    fn list_threads_for_path_matching(
        &self,
        cwd: &Path,
        pinned_first: bool,
        include_archived: bool,
    ) -> Result<Vec<ThreadRow>> {
        let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let rows = self.list_threads_matching(None, pinned_first, include_archived)?;
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

    /// Includes archived threads. The default lists hide them.
    pub fn list_threads_all(&self, cwd: Option<&str>) -> Result<Vec<ThreadRow>> {
        self.list_threads_matching(cwd, false, true)
    }

    pub fn list_threads_display_all(&self, cwd: Option<&str>) -> Result<Vec<ThreadRow>> {
        self.list_threads_matching(cwd, true, true)
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
        self.list_threads_matching(cwd, pinned_first, false)
    }

    fn list_threads_matching(
        &self,
        cwd: Option<&str>,
        pinned_first: bool,
        include_archived: bool,
    ) -> Result<Vec<ThreadRow>> {
        let order = if pinned_first {
            "pinned DESC, pin_order_key DESC, updated_at DESC, rowid DESC"
        } else {
            "updated_at DESC, rowid DESC"
        };
        let archived = if include_archived {
            ""
        } else {
            " AND archived_at IS NULL"
        };
        let mut statement = match cwd {
            Some(_cwd) => self.db.prepare(&format!(
                "SELECT {THREAD_COLUMNS} FROM threads WHERE cwd = ?{archived} ORDER BY {order}"
            ))?,
            None => self.db.prepare(&format!(
                "SELECT {THREAD_COLUMNS} FROM threads WHERE 1 = 1{archived} ORDER BY {order}"
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
        self.write_execute(
            "UPDATE threads SET title = ?, updated_at = ? WHERE id = ?",
            params![title, now_iso(), id],
        )?;
        Ok(())
    }

    /// Records a snooze in one write, so a thread never has one timestamp
    /// without the other. The end time comes from `snooze::SnoozePreset`.
    pub fn set_thread_snoozed(&self, id: &str, until: &str) -> Result<()> {
        self.set_thread_snoozed_at(id, &now_iso(), until)
    }

    pub fn set_thread_snoozed_at(&self, id: &str, snoozed_at: &str, until: &str) -> Result<()> {
        let changed = self.write_execute(
            "UPDATE threads SET snoozed_at = ?, snoozed_until = ? WHERE id = ?",
            params![snoozed_at, until, id],
        )?;
        if changed != 1 {
            anyhow::bail!("thread {id} not found");
        }
        Ok(())
    }

    /// One snapshot per provider. A restart keeps the last windows the harness
    /// reported until a newer snapshot replaces them.
    pub fn save_rate_limits(
        &self,
        provider: ProviderId,
        info: &crate::types::RateLimitInfo,
    ) -> Result<()> {
        self.write_execute(
            "INSERT INTO provider_rate_limits (provider, info, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(provider) DO UPDATE SET info = excluded.info, updated_at = excluded.updated_at",
            params![provider.as_str(), serde_json::to_string(info)?, now_iso()],
        )?;
        Ok(())
    }

    pub fn load_rate_limits(&self) -> Result<HashMap<ProviderId, crate::types::RateLimitInfo>> {
        let mut statement = self
            .db
            .prepare("SELECT provider, info FROM provider_rate_limits")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut limits = HashMap::new();
        for row in rows {
            let (provider, info) = row?;
            let Some(provider) = ProviderId::parse(&provider) else {
                continue;
            };
            if let Ok(info) = serde_json::from_str(&info) {
                limits.insert(provider, info);
            }
        }
        Ok(limits)
    }

    /// Hides a thread from the default lists and the inbox. A thread with a
    /// running turn is refused: that turn is the operation claim. The check and
    /// the write share one immediate transaction, so two processes cannot both
    /// see an idle thread and archive it mid-turn.
    pub fn archive_thread(&self, id: &str) -> Result<ArchiveOutcome> {
        let transaction = self.write()?;
        let archived_at: Option<Option<String>> = transaction
            .query_row(
                "SELECT archived_at FROM threads WHERE id = ?",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let outcome = match archived_at {
            None => ArchiveOutcome::NotFound,
            Some(Some(_)) => ArchiveOutcome::AlreadyArchived,
            Some(None) => {
                let running: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM turns WHERE thread_id = ? AND phase NOT IN ('completed', 'failed', 'interrupted', 'unverified')",
                    params![id],
                    |row| row.get(0),
                )?;
                if running > 0 {
                    ArchiveOutcome::RunningTurn
                } else {
                    transaction.execute(
                        "UPDATE threads SET archived_at = ?, snoozed_at = NULL, snoozed_until = NULL WHERE id = ?",
                        params![now_iso(), id],
                    )?;
                    ArchiveOutcome::Archived
                }
            }
        };
        transaction.commit()?;
        Ok(outcome)
    }

    pub fn unarchive_thread(&self, id: &str) -> Result<ArchiveOutcome> {
        let transaction = self.write()?;
        let archived_at: Option<Option<String>> = transaction
            .query_row(
                "SELECT archived_at FROM threads WHERE id = ?",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let outcome = match archived_at {
            None => ArchiveOutcome::NotFound,
            Some(None) => ArchiveOutcome::NotArchived,
            Some(Some(_)) => {
                transaction.execute(
                    "UPDATE threads SET archived_at = NULL WHERE id = ?",
                    params![id],
                )?;
                ArchiveOutcome::Unarchived
            }
        };
        transaction.commit()?;
        Ok(outcome)
    }

    pub fn clear_thread_snooze(&self, id: &str) -> Result<bool> {
        let changed = self.write_execute(
            "UPDATE threads SET snoozed_at = NULL, snoozed_until = NULL WHERE id = ? AND (snoozed_at IS NOT NULL OR snoozed_until IS NOT NULL)",
            params![id],
        )?;
        Ok(changed == 1)
    }

    pub fn set_thread_pinned(&self, id: &str, pinned: bool) -> Result<()> {
        let transaction = self.write()?;
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
        let transaction = self.write()?;
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
        self.write_execute(
            "UPDATE threads SET cwd = ?, updated_at = ? WHERE id = ?",
            params![cwd, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_session_mode(&self, id: &str, mode: SessionMode) -> Result<()> {
        self.write_execute(
            "UPDATE threads SET session_mode = ?, updated_at = ? WHERE id = ?",
            params![mode.as_str(), now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_title(&self, id: &str, title: &str) -> Result<()> {
        self.write_execute(
            "UPDATE threads SET title = ?, updated_at = ? WHERE id = ?",
            params![title, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_provider(&self, id: &str, provider: ProviderId, model: Option<String>) -> Result<()> {
        self.write_execute(
            "UPDATE threads SET current_provider = ?, current_model = ?, updated_at = ? WHERE id = ?",
            params![provider.as_str(), model, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_model(&self, id: &str, model: &str) -> Result<()> {
        self.write_execute(
            "UPDATE threads SET current_model = ?, updated_at = ? WHERE id = ?",
            params![model, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_effort(&self, id: &str, effort: Option<&str>) -> Result<()> {
        self.write_execute(
            "UPDATE threads SET reasoning_effort = ?, updated_at = ? WHERE id = ?",
            params![effort, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_permission_mode(&self, id: &str, mode: PermissionMode) -> Result<()> {
        self.write_execute(
            "UPDATE threads SET permission_mode = ?, updated_at = ? WHERE id = ?",
            params![mode.as_str(), now_iso(), id],
        )?;
        Ok(())
    }

    pub fn set_thread_goal(&self, id: &str, goal: Option<&str>, paused: bool) -> Result<()> {
        self.write_execute(
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
        self.write_execute(
            "INSERT INTO prompt_history (cwd, text, created_at) VALUES (?, ?, ?)",
            params![cwd, text, now_iso()],
        )?;
        self.write_execute(
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
        self.write_execute(
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
        let transaction = self.write()?;
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
                removed += self.write_execute(
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
        let changed = self.write_execute(
            "DELETE FROM memories WHERE id = ? AND (scope = 'global' OR cwd = ?)",
            params![id, cwd],
        )?;
        Ok(changed > 0)
    }

    /// Clears either the project memories for this cwd or every global memory.
    pub fn clear_memories(&self, cwd: &str, global: bool) -> Result<usize> {
        let changed = if global {
            self.write_execute("DELETE FROM memories WHERE scope = 'global'", [])?
        } else {
            self.write_execute(
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
        self.write_execute(
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
        let tx = self.write()?;
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
        let tx = self.write()?;
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
        self.write_execute(
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
        self.write_execute(
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
        self.write_execute(
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

    pub fn save_plan(&self, thread_id: &str, plan: &StoredPlan) -> Result<()> {
        let entries = serde_json::to_string(&plan.entries)?;
        self.write_execute(
            r#"INSERT INTO plans (thread_id, provider, plan_id, turn_id, entries, explanation, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT (thread_id, provider) DO UPDATE SET
                 plan_id = excluded.plan_id,
                 turn_id = excluded.turn_id,
                 entries = excluded.entries,
                 explanation = excluded.explanation,
                 updated_at = excluded.updated_at"#,
            params![
                thread_id,
                plan.provider.as_str(),
                plan.plan_id,
                plan.turn_id,
                entries,
                plan.explanation,
                now_iso()
            ],
        )?;
        Ok(())
    }

    pub fn remove_plan(&self, thread_id: &str, provider: ProviderId) -> Result<()> {
        self.write_execute(
            "DELETE FROM plans WHERE thread_id = ? AND provider = ?",
            params![thread_id, provider.as_str()],
        )?;
        Ok(())
    }

    pub fn remove_plans_for_thread(&self, thread_id: &str) -> Result<()> {
        self.write_execute(
            "DELETE FROM plans WHERE thread_id = ?",
            params![thread_id],
        )?;
        Ok(())
    }

    pub fn list_plans(&self, thread_id: &str) -> Result<Vec<StoredPlan>> {
        let mut statement = self
            .db
            .prepare("SELECT provider, plan_id, turn_id, entries, explanation FROM plans WHERE thread_id = ? ORDER BY provider")?;
        let rows = statement.query_map(params![thread_id], |row| {
            let provider: String = row.get(0)?;
            let entries: String = row.get(3)?;
            Ok((provider, row.get(1)?, row.get(2)?, entries, row.get(4)?))
        })?;
        let mut plans = Vec::new();
        for row in rows {
            let (provider, plan_id, turn_id, entries, explanation) = row?;
            let Some(provider) = ProviderId::parse(&provider) else {
                continue;
            };
            let entries = serde_json::from_str(&entries).unwrap_or_default();
            plans.push(StoredPlan {
                provider,
                plan_id,
                turn_id,
                entries,
                explanation,
            });
        }
        Ok(plans)
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
                &format!(
                    "SELECT {THREAD_COLUMNS_ALIASED} FROM threads t JOIN provider_sessions s ON s.thread_id = t.id WHERE s.provider = ? AND s.native_session_id = ?"
                ),
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
                        snoozed_at: row.get(14)?,
                        snoozed_until: row.get(15)?,
                        archived_at: row.get(16)?,
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
            owner_pid: row.get::<_, Option<i64>>(9)?.map(|pid| pid as u32),
            owner_start: row.get(10)?,
            owner_grant_id: row.get(11)?,
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
        let pid = std::process::id();
        let owner_pid = i64::from(pid);
        let owner_start = crate::process::process_start(pid);
        self.write_execute(
            r#"INSERT INTO turns (thread_id, turn, provider, model, phase, error, provider_turn_id, started_at, ended_at, owner_pid, owner_start)
               VALUES (?, ?, ?, ?, 'running', NULL, NULL, ?, NULL, ?, ?)
               ON CONFLICT (thread_id, turn) DO UPDATE SET
                 provider = excluded.provider,
                 model = excluded.model,
                 phase = 'running',
                 error = NULL,
                 provider_turn_id = NULL,
                 started_at = excluded.started_at,
                 ended_at = NULL,
                 owner_pid = excluded.owner_pid,
                 owner_start = excluded.owner_start"#,
            params![thread_id, turn, provider.as_str(), model, now_iso(), owner_pid, owner_start],
        )?;
        Ok(())
    }

    /// Inserts a turn and its audit event in one immediate transaction. A
    /// refused or failed submission leaves neither the message, the turn row,
    /// nor the event behind.
    pub fn submit_turn_with_audit(
        &self,
        thread_id: &str,
        provider: ProviderId,
        model: Option<&str>,
        text: &str,
        drain_queued: bool,
        event: &NewAuditEvent,
    ) -> Result<(i64, AuditEventRow)> {
        let created_at = now_iso();
        let tx = self.write()?;
        let turn = Self::insert_turn_on(&tx, thread_id, provider, model, text, drain_queued)?;
        let id = Self::insert_audit_event_in(&tx, event, &created_at)?;
        tx.commit()?;
        Ok((turn, Self::audit_event_row(id, created_at, event)))
    }

    /// Test seeding path without an audit event. Production submissions use
    /// `submit_turn_with_audit` so the append-only log names every turn.
    #[cfg(test)]
    pub fn submit_turn(
        &self,
        thread_id: &str,
        provider: ProviderId,
        model: Option<&str>,
        text: &str,
        drain_queued: bool,
    ) -> Result<i64> {
        let tx = self.write()?;
        let turn = Self::insert_turn_on(&tx, thread_id, provider, model, text, drain_queued)?;
        tx.commit()?;
        Ok(turn)
    }

    /// Inserts the user message and the running turn row, after proving no
    /// other work owns the thread. A queue drain skips only the queue guard
    /// because the caller already popped one row.
    fn insert_turn_on(
        tx: &Connection,
        thread_id: &str,
        provider: ProviderId,
        model: Option<&str>,
        text: &str,
        drain_queued: bool,
    ) -> Result<i64> {
        if let Some(work) = Self::thread_open_work_on(tx, thread_id, drain_queued)? {
            return Err(SubmitRefused(work).into());
        }
        let turn: i64 = tx.query_row(
            "SELECT COALESCE(MAX(turn), 0) + 1 FROM messages WHERE thread_id = ?",
            params![thread_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO messages (thread_id, turn, role, provider, model, text, created_at) VALUES (?, ?, 'user', ?, ?, ?, ?)",
            params![thread_id, turn, provider.as_str(), model, text, now_iso()],
        )?;
        let pid = std::process::id();
        let owner_pid = i64::from(pid);
        let owner_start = crate::process::process_start(pid);
        tx.execute(
            r#"INSERT INTO turns (thread_id, turn, provider, model, phase, error, provider_turn_id, started_at, ended_at, owner_pid, owner_start)
               VALUES (?, ?, ?, ?, 'running', NULL, NULL, ?, NULL, ?, ?)
               ON CONFLICT (thread_id, turn) DO UPDATE SET
                 provider = excluded.provider,
                 model = excluded.model,
                 phase = 'running',
                 error = NULL,
                 provider_turn_id = NULL,
                 started_at = excluded.started_at,
                 ended_at = NULL,
                 owner_pid = excluded.owner_pid,
                 owner_start = excluded.owner_start"#,
            params![thread_id, turn, provider.as_str(), model, now_iso(), owner_pid, owner_start],
        )?;
        tx.execute(
            "UPDATE threads SET updated_at = ? WHERE id = ?",
            params![now_iso(), thread_id],
        )?;
        Ok(turn)
    }

    pub fn finish_turn_record(
        &self,
        thread_id: &str,
        turn: i64,
        phase: TurnPhase,
        error: Option<&str>,
    ) -> Result<()> {
        self.write_execute(
            "UPDATE turns SET phase = ?, error = ?, ended_at = ? WHERE thread_id = ? AND turn = ?",
            params![phase.as_str(), error, now_iso(), thread_id, turn],
        )?;
        Ok(())
    }

    /// Moves an open turn between queued and running without ending it.
    pub fn set_turn_phase(&self, thread_id: &str, turn: i64, phase: TurnPhase) -> Result<()> {
        self.write_execute(
            "UPDATE turns SET phase = ?, error = NULL, ended_at = NULL WHERE thread_id = ? AND turn = ?",
            params![phase.as_str(), thread_id, turn],
        )?;
        Ok(())
    }

    pub fn set_turn_provider_id(&self, thread_id: &str, turn: i64, provider_turn_id: &str) -> Result<()> {
        self.write_execute(
            "UPDATE turns SET provider_turn_id = ? WHERE thread_id = ? AND turn = ?",
            params![provider_turn_id, thread_id, turn],
        )?;
        Ok(())
    }

    pub fn count_open_turns(&self, thread_id: Option<&str>) -> Result<usize> {
        let count: i64 = match thread_id {
            Some(thread_id) => self.db.query_row(
                "SELECT COUNT(*) FROM turns WHERE thread_id = ? AND phase IN ('queued', 'running', 'settling')",
                params![thread_id],
                |row| row.get(0),
            )?,
            None => self.db.query_row(
                "SELECT COUNT(*) FROM turns WHERE phase IN ('queued', 'running', 'settling')",
                [],
                |row| row.get(0),
            )?,
        };
        Ok(count.max(0) as usize)
    }

    pub fn get_turn_record(&self, thread_id: &str, turn: i64) -> Result<Option<TurnRow>> {
        self.db
            .query_row(
                "SELECT thread_id, turn, provider, model, phase, error, provider_turn_id, started_at, ended_at, owner_pid, owner_start, owner_grant_id FROM turns WHERE thread_id = ? AND turn = ?",
                params![thread_id, turn],
                Self::map_turn,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_turn_records(&self, thread_id: &str) -> Result<Vec<TurnRow>> {
        let mut statement = self.db.prepare(
            "SELECT thread_id, turn, provider, model, phase, error, provider_turn_id, started_at, ended_at, owner_pid, owner_start, owner_grant_id FROM turns WHERE thread_id = ? ORDER BY turn",
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
        Self::thread_open_work_on(&self.db, thread_id, false)
    }

    fn thread_open_work_on(
        db: &Connection,
        thread_id: &str,
        allow_queued: bool,
    ) -> Result<Option<ThreadOpenWork>> {
        let (turn, queue, native_rewind, orchestration): (bool, bool, bool, bool) = db.query_row(
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
        } else if queue && !allow_queued {
            Some(ThreadOpenWork::Queue)
        } else if native_rewind {
            Some(ThreadOpenWork::NativeRewind)
        } else if orchestration {
            Some(ThreadOpenWork::Orchestration)
        } else {
            None
        })
    }

    /// The first thread under `root` with durable open work. A file restore
    /// rewrites a whole checkout, so it must see work that belongs to another
    /// thread in the same repository rather than only the thread being restored.
    pub fn open_work_under(&self, root: &Path) -> Result<Option<(String, ThreadOpenWork)>> {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        for thread in self.list_threads(None)? {
            let cwd = Path::new(&thread.cwd);
            let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
            if !cwd.starts_with(&root) {
                continue;
            }
            if let Some(work) = self.thread_open_work(&thread.id)? {
                return Ok(Some((thread.id, work)));
            }
        }
        Ok(None)
    }

    pub fn get_host_claim(&self) -> Result<Option<HostClaimRow>> {
        Self::get_host_claim_from(&self.db)
    }

    fn get_host_claim_from(db: &Connection) -> Result<Option<HostClaimRow>> {
        db.query_row(
            "SELECT host_id, kind, installation_id, pid, process_start, claimed_at, heartbeat_at FROM host_claim WHERE singleton = 1",
            [],
            |row| {
                Ok(HostClaimRow {
                    host_id: row.get(0)?,
                    kind: row.get(1)?,
                    installation_id: row.get(2)?,
                    pid: row.get::<_, i64>(3)?.max(0) as u32,
                    process_start: row.get(4)?,
                    claimed_at: row.get(5)?,
                    heartbeat_at: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub(crate) fn claim_host_row(
        &self,
        host_id: &str,
        installation_id: &str,
        pid: u32,
        process_start: &str,
        alive: &dyn Fn(u32, Option<&str>) -> bool,
    ) -> Result<HostClaimAttempt> {
        if self.host_id.as_deref() != Some(host_id) {
            anyhow::bail!("host store must be opened with open_hosted and the same host id");
        }
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        if let Some(claim) = Self::get_host_claim_from(&tx)? {
            if alive(claim.pid, Some(&claim.process_start)) {
                return Ok(HostClaimAttempt::Owned {
                    pid: claim.pid,
                    process_start: claim.process_start,
                });
            }
        }
        let turn_owners = {
            let mut statement = tx.prepare(
                "SELECT owner_pid, owner_start FROM turns WHERE phase IN ('queued', 'running', 'settling') AND owner_pid IS NOT NULL",
            )?;
            let owners = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?.max(0) as u32,
                        row.get::<_, Option<String>>(1)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            owners
        };
        for (owner_pid, owner_start) in turn_owners {
            if alive(owner_pid, owner_start.as_deref()) {
                return Ok(HostClaimAttempt::OpenWork { pid: owner_pid });
            }
        }
        let run_owners = {
            let mut statement = tx.prepare(
                "SELECT owner_pid, owner_start FROM orchestration_runs WHERE status = 'running' AND owner_pid IS NOT NULL",
            )?;
            let owners = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?.max(0) as u32,
                        row.get::<_, Option<String>>(1)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            owners
        };
        for (owner_pid, owner_start) in run_owners {
            if alive(owner_pid, owner_start.as_deref()) {
                return Ok(HostClaimAttempt::OpenWork { pid: owner_pid });
            }
        }
        let now = now_iso();
        tx.execute(
            r#"INSERT INTO host_claim (singleton, host_id, kind, installation_id, pid, process_start, claimed_at, heartbeat_at)
               VALUES (1, ?, 'serve', ?, ?, ?, ?, ?)
               ON CONFLICT(singleton) DO UPDATE SET
                 host_id = excluded.host_id,
                 kind = excluded.kind,
                 installation_id = excluded.installation_id,
                 pid = excluded.pid,
                 process_start = excluded.process_start,
                 claimed_at = excluded.claimed_at,
                 heartbeat_at = excluded.heartbeat_at"#,
            params![host_id, installation_id, i64::from(pid), process_start, now, now],
        )?;
        tx.commit()?;
        Ok(HostClaimAttempt::Claimed(HostClaimRow {
            host_id: host_id.to_string(),
            kind: "serve".to_string(),
            installation_id: installation_id.to_string(),
            pid,
            process_start: process_start.to_string(),
            claimed_at: now.clone(),
            heartbeat_at: now,
        }))
    }

    /// Replaces a live claim only when the caller names its exact pid and
    /// process start, and that process is still live. The takeover store must
    /// already carry the new host id. The row keeps its installation id.
    pub(crate) fn takeover_host_row(
        &self,
        new_host_id: &str,
        expected_pid: u32,
        expected_start: &str,
        pid: u32,
        process_start: &str,
    ) -> Result<TakeoverAttempt> {
        if self.host_id.as_deref() != Some(new_host_id) {
            anyhow::bail!("host store must be opened with open_hosted and the new host id");
        }
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let Some(claim) = Self::get_host_claim_from(&tx)? else {
            return Ok(TakeoverAttempt::NoClaim);
        };
        if claim.pid != expected_pid || claim.process_start != expected_start {
            return Ok(TakeoverAttempt::Changed {
                pid: claim.pid,
                process_start: claim.process_start,
            });
        }
        if !crate::process::process_identity_alive(claim.pid, Some(&claim.process_start)) {
            return Ok(TakeoverAttempt::NotLive { pid: claim.pid });
        }
        let now = now_iso();
        tx.execute(
            "UPDATE host_claim SET host_id = ?, pid = ?, process_start = ?, claimed_at = ?, heartbeat_at = ? WHERE singleton = 1",
            params![new_host_id, i64::from(pid), process_start, now, now],
        )?;
        tx.commit()?;
        Ok(TakeoverAttempt::Taken(HostClaimRow {
            host_id: new_host_id.to_string(),
            kind: claim.kind,
            installation_id: claim.installation_id,
            pid,
            process_start: process_start.to_string(),
            claimed_at: now.clone(),
            heartbeat_at: now,
        }))
    }

    /// Ends every open turn and running orchestration after a takeover claim
    /// is installed. Task rows keep their completed or failed verdicts; only
    /// queued and running work is converted.
    pub fn settle_open_work_after_takeover(&self, error: &str) -> Result<TakeoverSettlement> {
        let runs = self.list_running_orchestration_runs()?;
        let turns = self.write_execute(
            "UPDATE turns SET phase = 'unverified', error = COALESCE(error, ?), ended_at = COALESCE(ended_at, ?) WHERE phase IN ('queued', 'running', 'settling')",
            params![error, now_iso()],
        )?;
        for run in &runs {
            self.abandon_orchestration_run(&run.id, error)?;
        }
        Ok(TakeoverSettlement {
            turns,
            runs: runs.len(),
        })
    }

    pub fn release_host_claim(&self, host_id: &str) -> Result<bool> {
        let write = self.write()?;
        let changed = write.execute(
            "DELETE FROM host_claim WHERE singleton = 1 AND host_id = ?",
            params![host_id],
        )?;
        write.commit()?;
        Ok(changed == 1)
    }

    fn insert_audit_event_in(
        db: &Connection,
        event: &NewAuditEvent,
        created_at: &str,
    ) -> Result<i64> {
        db.execute(
            "INSERT INTO audit_events (actor, environment, target, thread_id, action, scope, confirmation, payload_digest, result, created_at, retention) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                event.actor,
                event.environment,
                event.target,
                event.thread_id,
                event.action,
                event.scope,
                event.confirmation,
                event.payload_digest,
                event.result,
                created_at,
                event.retention,
            ],
        )?;
        Ok(db.last_insert_rowid())
    }

    fn audit_event_row(id: i64, created_at: String, event: &NewAuditEvent) -> AuditEventRow {
        AuditEventRow {
            id,
            actor: event.actor.clone(),
            environment: event.environment.clone(),
            target: event.target.clone(),
            thread_id: event.thread_id.clone(),
            action: event.action.clone(),
            scope: event.scope.clone(),
            confirmation: event.confirmation.clone(),
            payload_digest: event.payload_digest.clone(),
            result: event.result.clone(),
            created_at,
            retention: event.retention.clone(),
        }
    }

    pub fn append_audit_event(&self, event: &NewAuditEvent) -> Result<AuditEventRow> {
        let created_at = now_iso();
        let write = self.write()?;
        let id = Self::insert_audit_event_in(&write, event, &created_at)?;
        write.commit()?;
        Ok(Self::audit_event_row(id, created_at, event))
    }

    pub fn list_audit_events(&self, limit: usize) -> Result<Vec<AuditEventRow>> {
        let limit = limit.min(i64::MAX as usize) as i64;
        let mut statement = self.db.prepare(
            "SELECT id, actor, environment, target, thread_id, action, scope, confirmation, payload_digest, result, created_at, retention FROM audit_events ORDER BY id DESC LIMIT ?",
        )?;
        let rows = statement
            .query_map(params![limit], Self::map_audit_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn insert_grant_in(db: &Connection, grant: &GrantRow) -> Result<()> {
        db.execute(
            "INSERT INTO grants (grant_id, installation_id, secret_hash, scopes, permission_ceiling, project_allowlist, issued_at, expires_at, revoked_at, label) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                grant.grant_id,
                grant.installation_id,
                grant.secret_hash,
                serde_json::to_string(&grant.scopes)?,
                grant.permission_ceiling,
                serde_json::to_string(&grant.project_allowlist)?,
                grant.issued_at,
                grant.expires_at,
                grant.revoked_at,
                grant.label,
            ],
        )?;
        Ok(())
    }

    pub fn insert_grant(&self, grant: &GrantRow) -> Result<()> {
        let write = self.write()?;
        Self::insert_grant_in(&write, grant)?;
        write.commit()?;
        Ok(())
    }

    /// A grant row and its audit event commit together. If the audit event is
    /// rejected, the grant does not exist.
    pub fn insert_grant_with_audit(
        &self,
        grant: &GrantRow,
        event: &NewAuditEvent,
    ) -> Result<AuditEventRow> {
        let created_at = now_iso();
        let write = self.write()?;
        Self::insert_grant_in(&write, grant)?;
        let id = Self::insert_audit_event_in(&write, event, &created_at)?;
        write.commit()?;
        Ok(Self::audit_event_row(id, created_at, event))
    }

    pub fn list_grant_rows(&self) -> Result<Vec<GrantRow>> {
        let mut statement = self.db.prepare(
            "SELECT grant_id, installation_id, secret_hash, scopes, permission_ceiling, project_allowlist, issued_at, expires_at, revoked_at, label FROM grants ORDER BY issued_at, rowid",
        )?;
        let rows = statement
            .query_map([], Self::map_grant)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_grant_row(&self, grant_id: &str) -> Result<Option<GrantRow>> {
        self.db
            .query_row(
                "SELECT grant_id, installation_id, secret_hash, scopes, permission_ceiling, project_allowlist, issued_at, expires_at, revoked_at, label FROM grants WHERE grant_id = ?",
                params![grant_id],
                Self::map_grant,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn revoke_grant_row(&self, grant_id: &str) -> Result<bool> {
        let changed = self.write_execute(
            "UPDATE grants SET revoked_at = COALESCE(revoked_at, ?) WHERE grant_id = ?",
            params![now_iso(), grant_id],
        )?;
        Ok(changed == 1)
    }

    /// Revoking a present grant and its audit event commit together. A missing
    /// grant writes no event and returns `None`.
    pub fn revoke_grant_row_with_audit(
        &self,
        grant_id: &str,
        event: &NewAuditEvent,
    ) -> Result<Option<AuditEventRow>> {
        let created_at = now_iso();
        let write = self.write()?;
        let changed = write.execute(
            "UPDATE grants SET revoked_at = COALESCE(revoked_at, ?) WHERE grant_id = ?",
            params![created_at, grant_id],
        )?;
        if changed == 0 {
            write.commit()?;
            return Ok(None);
        }
        let id = Self::insert_audit_event_in(&write, event, &created_at)?;
        write.commit()?;
        Ok(Some(Self::audit_event_row(id, created_at, event)))
    }

    /// Extends one live grant's expiry and commits its audit event together.
    /// The stored window is never shortened, the secret hash is untouched, and
    /// a missing or revoked grant writes neither a change nor an event.
    pub fn renew_grant_row_with_audit(
        &self,
        grant_id: &str,
        requested_expires_at: &str,
        event: &NewAuditEvent,
    ) -> Result<RenewGrantOutcome> {
        let created_at = now_iso();
        let write = self.write()?;
        let row = write
            .query_row(
                "SELECT grant_id, installation_id, secret_hash, scopes, permission_ceiling, project_allowlist, issued_at, expires_at, revoked_at, label FROM grants WHERE grant_id = ?",
                params![grant_id],
                Self::map_grant,
            )
            .optional()?;
        let Some(mut row) = row else {
            write.commit()?;
            return Ok(RenewGrantOutcome::Missing);
        };
        if row.revoked_at.is_some() {
            write.commit()?;
            return Ok(RenewGrantOutcome::Revoked);
        }
        let previous_expires_at = row.expires_at.clone();
        row.expires_at = later_expiry(&previous_expires_at, requested_expires_at);
        write.execute(
            "UPDATE grants SET expires_at = ? WHERE grant_id = ?",
            params![row.expires_at, grant_id],
        )?;
        let id = Self::insert_audit_event_in(&write, event, &created_at)?;
        write.commit()?;
        Ok(RenewGrantOutcome::Renewed(Box::new(RenewedGrant {
            previous_expires_at,
            grant: row.summary(),
            event: Self::audit_event_row(id, created_at, event),
        })))
    }

    fn map_audit_event(row: &rusqlite::Row) -> rusqlite::Result<AuditEventRow> {
        Ok(AuditEventRow {
            id: row.get(0)?,
            actor: row.get(1)?,
            environment: row.get(2)?,
            target: row.get(3)?,
            thread_id: row.get(4)?,
            action: row.get(5)?,
            scope: row.get(6)?,
            confirmation: row.get(7)?,
            payload_digest: row.get(8)?,
            result: row.get(9)?,
            created_at: row.get(10)?,
            retention: row.get(11)?,
        })
    }

    fn map_grant(row: &rusqlite::Row) -> rusqlite::Result<GrantRow> {
        let scopes: String = row.get(3)?;
        let allowlist: String = row.get(5)?;
        // A corrupt list fails closed to no scopes rather than granting
        // authority the row does not describe.
        Ok(GrantRow {
            grant_id: row.get(0)?,
            installation_id: row.get(1)?,
            secret_hash: row.get(2)?,
            scopes: serde_json::from_str(&scopes).unwrap_or_default(),
            permission_ceiling: row.get(4)?,
            project_allowlist: serde_json::from_str(&allowlist).unwrap_or_default(),
            issued_at: row.get(6)?,
            expires_at: row.get(7)?,
            revoked_at: row.get(8)?,
            label: row.get(9)?,
        })
    }

    /// A process that starts after a crash must not show a running turn as
    /// live. Mark unfinished turns in this project as unverified, but leave a
    /// turn alone while its recorded owner is still alive with the same start
    /// identity. Ownerless and reused-PID rows are settled.
    pub fn reconcile_running_turns(&self, cwd: Option<&str>) -> Result<Vec<TurnRow>> {
        let rows = match cwd {
            Some(cwd) => {
                let mut statement = self.db.prepare(
                    "SELECT tu.thread_id, tu.turn, tu.provider, tu.model, tu.phase, tu.error, tu.provider_turn_id, tu.started_at, tu.ended_at, tu.owner_pid, tu.owner_start, tu.owner_grant_id
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
                    "SELECT thread_id, turn, provider, model, phase, error, provider_turn_id, started_at, ended_at, owner_pid, owner_start, owner_grant_id
                     FROM turns WHERE phase IN ('queued', 'running', 'settling') ORDER BY started_at",
                )?;
                let rows = statement
                    .query_map([], Self::map_turn)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            }
        };
        let mut settled = Vec::new();
        for row in rows {
            if row.owner_pid.is_some_and(|pid| {
                crate::process::process_identity_alive(pid, row.owner_start.as_deref())
            }) {
                continue;
            }
            let changed = self.write_execute(
                "UPDATE turns SET phase = 'unverified', error = COALESCE(error, 'baton exited before the turn settled'), ended_at = ? WHERE thread_id = ? AND turn = ? AND phase IN ('queued', 'running', 'settling') AND owner_pid IS ? AND owner_start IS ?",
                params![
                    now_iso(),
                    row.thread_id,
                    row.turn,
                    row.owner_pid.map(i64::from),
                    row.owner_start.as_deref()
                ],
            )?;
            if changed == 1 {
                settled.push(row);
            }
        }
        Ok(settled)
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
        owner_start: Option<&str>,
        host_id: Option<&str>,
    ) -> Result<()> {
        self.write_execute(
            r#"INSERT INTO provider_processes (pid, provider, port, command, process_start, cwd, owner_pid, owner_start, host_id, started_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(pid) DO UPDATE SET
                 provider = excluded.provider,
                 port = excluded.port,
                 command = excluded.command,
                 process_start = excluded.process_start,
                 cwd = excluded.cwd,
                 owner_pid = excluded.owner_pid,
                 owner_start = excluded.owner_start,
                 host_id = excluded.host_id,
                 started_at = excluded.started_at"#,
            params![
                pid,
                provider.as_str(),
                port,
                command,
                process_start,
                cwd,
                owner_pid,
                owner_start,
                host_id,
                now_iso()
            ],
        )?;
        Ok(())
    }

    pub fn remove_provider_process(&self, pid: u32) -> Result<()> {
        self.write_execute("DELETE FROM provider_processes WHERE pid = ?", params![pid])?;
        Ok(())
    }

    pub fn list_provider_processes(&self) -> Result<Vec<ProviderProcessRow>> {
        let mut statement = self.db.prepare(
            "SELECT pid, provider, port, command, process_start, cwd, owner_pid, owner_start, host_id, started_at
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
                    owner_start: row.get(7)?,
                    host_id: row.get(8)?,
                    started_at: row.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn enqueue_message(&self, thread_id: &str, text: &str) -> Result<()> {
        self.write_execute(
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
        let tx = self.write()?;
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
        let tx = self.write()?;
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
        let tx = self.write()?;
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
        self.write_execute(
            "DELETE FROM queued_messages WHERE thread_id = ?",
            params![thread_id],
        )?;
        Ok(())
    }

    /// Clearing a queue and its audit event commit together. A clear with no
    /// queued rows is still recorded as an attempted mutation.
    pub fn clear_queued_messages_with_audit(
        &self,
        thread_id: &str,
        event: &NewAuditEvent,
    ) -> Result<AuditEventRow> {
        let created_at = now_iso();
        let write = self.write()?;
        write.execute(
            "DELETE FROM queued_messages WHERE thread_id = ?",
            params![thread_id],
        )?;
        let id = Self::insert_audit_event_in(&write, event, &created_at)?;
        write.commit()?;
        Ok(Self::audit_event_row(id, created_at, event))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_subagent(
        &self,
        thread_id: &str,
        turn: i64,
        provider: &str,
        call_id: Option<&str>,
        name: &str,
        detail: Option<&str>,
        status: &str,
        started_at: &str,
        finished_at: Option<&str>,
    ) -> Result<()> {
        self.write_execute(
            "INSERT INTO subagents (thread_id, turn, provider, call_id, name, detail, status, started_at, finished_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![thread_id, turn, provider, call_id, name, detail, status, started_at, finished_at],
        )?;
        Ok(())
    }

    pub fn list_subagents(&self, thread_id: &str) -> Result<Vec<SubagentRow>> {
        let mut statement = self.db.prepare(
            "SELECT id, thread_id, turn, provider, call_id, name, detail, status, started_at, finished_at FROM subagents WHERE thread_id = ? ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![thread_id], |row| {
                Ok(SubagentRow {
                    id: row.get(0)?,
                    thread_id: row.get(1)?,
                    turn: row.get(2)?,
                    provider: row.get(3)?,
                    call_id: row.get(4)?,
                    name: row.get(5)?,
                    detail: row.get(6)?,
                    status: row.get(7)?,
                    started_at: row.get(8)?,
                    finished_at: row.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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
        self.write_execute(
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
        self.write_execute(
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
        self.write_execute(
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
        self.write_execute(
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
        self.write_execute(
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
        let tx = self.write()?;
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
        self.write_execute(
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
        self.write_execute(
            "UPDATE orchestration_runs SET status = ?, finished_at = ? WHERE id = ?",
            params![status, now_iso(), id],
        )?;
        Ok(())
    }

    /// Converts a run whose orchestrator process is gone into a terminal
    /// verdict while preserving every worker that already completed.
    pub fn abandon_orchestration_run(&self, run_id: &str, error: &str) -> Result<()> {
        let now = now_iso();
        let tx = self.write()?;
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
        let blocked_reviews: i64 = tx.query_row(
            "SELECT COUNT(*) FROM orchestration_tasks WHERE run_id = ? AND review_status IN ('pending', 'request_changes', 'failed')",
            params![run_id],
            |row| row.get(0),
        )?;
        let status = if cancelled > 0 {
            "cancelled"
        } else if needs_input > 0 {
            "needs_input"
        } else if blocked_reviews > 0 {
            "needs_review"
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

    /// Terminalizes one running orchestration from the operator surface. A
    /// completed verdict is accepted only when every task completed and no
    /// turn is still open.
    pub fn settle_orchestration_run(
        &self,
        run_id: &str,
        verdict: RunVerdict,
        force: bool,
    ) -> Result<RunSettlement> {
        let short = &run_id[..8.min(run_id.len())];
        let now = now_iso();
        let tx = self.write()?;
        let (current, owner_pid, owner_start): (String, Option<u32>, Option<String>) = tx
            .query_row(
                "SELECT status, owner_pid, owner_start FROM orchestration_runs WHERE id = ?",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("no orchestration run matches {run_id}"))?;
        if current != "running" {
            anyhow::bail!("run {short} is already {current}");
        }
        if !force {
            if let Some(pid) = owner_pid {
                if crate::process::process_identity_alive(pid, owner_start.as_deref()) {
                    anyhow::bail!(
                        "run {short} is still owned by live process {pid}; stop it or pass --force"
                    );
                }
            }
        }
        let (total_tasks, completed_tasks, open_tasks, open_turns): (i64, i64, i64, i64) = tx
            .query_row(
                "SELECT
                   (SELECT COUNT(*) FROM orchestration_tasks WHERE run_id = ?1),
                   (SELECT COUNT(*) FROM orchestration_tasks WHERE run_id = ?1 AND status = 'completed'),
                   (SELECT COUNT(*) FROM orchestration_tasks WHERE run_id = ?1 AND status IN ('queued', 'running')),
                   (SELECT COUNT(*) FROM turns WHERE thread_id IN (SELECT thread_id FROM orchestration_tasks WHERE run_id = ?1 AND thread_id IS NOT NULL) AND phase IN ('queued', 'running', 'settling'))",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let task_status = match verdict {
            RunVerdict::Completed => {
                if total_tasks == 0 {
                    anyhow::bail!("cannot settle run {short} as completed because it has no tasks");
                }
                if open_tasks > 0 {
                    anyhow::bail!(
                        "cannot settle run {short} as completed with {open_tasks} open task(s)"
                    );
                }
                if completed_tasks != total_tasks {
                    anyhow::bail!(
                        "cannot settle run {short} as completed: {completed_tasks} of {total_tasks} task(s) completed"
                    );
                }
                if open_turns > 0 {
                    anyhow::bail!(
                        "cannot settle run {short} as completed with {open_turns} open turn(s)"
                    );
                }
                None
            }
            RunVerdict::Cancelled => Some("cancelled"),
            RunVerdict::Partial
            | RunVerdict::Failed
            | RunVerdict::NeedsReview
            | RunVerdict::NeedsInput => Some("failed"),
        };
        let error = format!("settled by operator as {}", verdict.as_str());
        let tasks = match task_status {
            Some(task_status) => tx.execute(
                "UPDATE orchestration_tasks SET status = ?, error = COALESCE(NULLIF(error, ''), ?), finished_at = COALESCE(finished_at, ?) WHERE run_id = ? AND status IN ('queued', 'running')",
                params![task_status, error, now, run_id],
            )?,
            None => 0,
        };
        let turns = if task_status.is_some() {
            tx.execute(
                "UPDATE turns SET phase = 'unverified', error = COALESCE(error, ?), ended_at = COALESCE(ended_at, ?) WHERE thread_id IN (SELECT thread_id FROM orchestration_tasks WHERE run_id = ? AND thread_id IS NOT NULL) AND phase IN ('queued', 'running', 'settling')",
                params![error, now, run_id],
            )?
        } else {
            0
        };
        let closed = tx.execute(
            "UPDATE orchestration_runs SET status = ?, finished_at = ? WHERE id = ? AND status = 'running'",
            params![verdict.as_str(), now, run_id],
        )?;
        if closed != 1 {
            anyhow::bail!("run {short} changed while it was being settled; retry");
        }
        tx.commit()?;
        Ok(RunSettlement { tasks, turns })
    }

    /// D-5 shutdown for work this host owns. A deliberate host stop records
    /// its turns as unverified with the host reason, cancels open tasks, and
    /// lets the run verdict derive to cancelled. Work owned by another
    /// process is untouched.
    pub fn cancel_host_work(
        &self,
        owner_pid: u32,
        owner_start: Option<&str>,
        reason: &str,
    ) -> Result<HostWorkSettlement> {
        let now = now_iso();
        let turns = self.write_execute(
            "UPDATE turns SET phase = 'unverified', error = COALESCE(error, ?), ended_at = COALESCE(ended_at, ?) WHERE phase IN ('queued', 'running', 'settling') AND owner_pid = ? AND owner_start IS ?",
            params![reason, now, owner_pid, owner_start],
        )?;
        let runs = {
            let mut statement = self.db.prepare(
                "SELECT id FROM orchestration_runs WHERE status = 'running' AND owner_pid = ? AND owner_start IS ? ORDER BY created_at",
            )?;
            let rows = statement
                .query_map(params![owner_pid, owner_start], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut tasks = 0;
        if !runs.is_empty() {
            let tx = self.write()?;
            for run_id in &runs {
                tasks += tx.execute(
                    "UPDATE orchestration_tasks SET status = 'cancelled', error = COALESCE(NULLIF(error, ''), ?), finished_at = COALESCE(finished_at, ?) WHERE run_id = ? AND status IN ('queued', 'running')",
                    params![reason, now, run_id],
                )?;
            }
            tx.commit()?;
            for run_id in &runs {
                self.abandon_orchestration_run(run_id, reason)?;
            }
        }
        Ok(HostWorkSettlement { turns, tasks, runs })
    }

    pub fn archive_orchestration_run(&self, id: &str) -> Result<()> {
        self.write_execute(
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
        self.write_execute(
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
        self.write_execute(
            "UPDATE orchestration_tasks SET merged_at = ? WHERE id = ?",
            params![merged_at, id],
        )?;
        Ok(())
    }

    pub fn mark_orchestration_task_pr(&self, id: &str, pr_url: &str) -> Result<()> {
        self.write_execute(
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
            "SELECT id, run_id, position, title, provider, model, prompt, worktree_path, branch, commit_sha, thread_id, status, text, error, review_status, review_text, reviewed_at, created_at, finished_at, merged_at, pr_url, snapshot_base, snapshot_tree, reasoning_effort FROM orchestration_tasks WHERE run_id = ? ORDER BY position, created_at, id",
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
            "SELECT id, run_id, position, title, provider, model, prompt, worktree_path, branch, commit_sha, thread_id, status, text, error, review_status, review_text, reviewed_at, created_at, finished_at, merged_at, pr_url, snapshot_base, snapshot_tree, reasoning_effort FROM orchestration_tasks WHERE thread_id = ? ORDER BY created_at DESC LIMIT 1",
        )?;
        let row = statement
            .query_row(params![thread_id], map_orchestration_task)
            .optional()?;
        Ok(row)
    }

    /// Every running worker that is waiting for an answer, newest run first.
    /// Tasks without a thread cannot be answered from the TUI, so they are not
    /// offered; the run report still names them.
    pub fn list_worker_questions(&self) -> Result<Vec<WorkerQuestionRow>> {
        let mut statement = self.db.prepare(
            "SELECT task.id, task.run_id, task.thread_id, task.title, task.text, task.created_at
             FROM orchestration_tasks task
             JOIN orchestration_runs run ON run.id = task.run_id
             WHERE task.status = 'needs_input'
               AND task.thread_id IS NOT NULL
               AND run.archived_at IS NULL
             ORDER BY task.created_at DESC, task.position",
        )?;
        let rows = statement.query_map([], |row| {
            let text: Option<String> = row.get(4)?;
            Ok(WorkerQuestionRow {
                task_id: row.get(0)?,
                run_id: row.get(1)?,
                thread_id: row.get(2)?,
                title: row.get(3)?,
                question: text
                    .as_deref()
                    .and_then(crate::orchestrate::worker_question)
                    .unwrap_or_else(|| "worker needs input".to_string()),
                created_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record the reviewer verdict for one task. Reviews are written after the
    /// task row exists so a reviewer failure cannot lose the worker result.
    pub fn set_orchestration_task_review(
        &self,
        run_id: &str,
        id: &str,
        status: &str,
        text: Option<&str>,
        reviewed_at: &str,
    ) -> Result<()> {
        self.write_execute(
            "UPDATE orchestration_tasks SET review_status = ?, review_text = ?, reviewed_at = ? WHERE run_id = ? AND id = ?",
            params![status, text, reviewed_at, run_id, id],
        )?;
        Ok(())
    }

    pub fn update_orchestration_task_failure(
        &self,
        id: &str,
        status: &str,
        text: Option<&str>,
        error: &str,
    ) -> Result<()> {
        self.write_execute(
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
            self.write_execute(
                "UPDATE orchestration_tasks SET status = 'completed', text = ?, error = NULL, commit_sha = COALESCE(?, commit_sha), finished_at = ?, merged_at = NULL, pr_url = NULL, review_status = 'pending', review_text = NULL, reviewed_at = NULL WHERE id = ?",
                params![text, commit_sha, finished_at, id],
            )?;
        } else {
            self.write_execute(
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
            self.write_execute(
                "UPDATE orchestration_tasks SET status = 'needs_input', text = ?, error = NULL, commit_sha = COALESCE(?, commit_sha), finished_at = ?, merged_at = NULL, pr_url = NULL, review_status = 'pending', review_text = NULL, reviewed_at = NULL WHERE id = ?",
                params![text, commit_sha, finished_at, id],
            )?;
        } else {
            self.write_execute(
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
        let blocked_reviews = tasks.iter().any(|task| {
            matches!(
                task.review_status.as_deref(),
                Some("pending") | Some("request_changes") | Some("failed")
            )
        });
        let status = if running {
            "running"
        } else if cancelled {
            "cancelled"
        } else if needs_input {
            "needs_input"
        } else if blocked_reviews {
            "needs_review"
        } else if completed == tasks.len() {
            "completed"
        } else if completed == 0 {
            "failed"
        } else {
            "partial"
        };
        self.write_execute(
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
        let tx = self.write()?;
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

/// The later of the stored and requested instants, in the same UTC millisecond
/// form the grant mint writes. An unparseable stored value preserves nothing,
/// so the requested instant wins; an unparseable request leaves the row alone.
fn later_expiry(stored: &str, requested: &str) -> String {
    let format = |instant: &chrono::DateTime<chrono::Utc>| {
        instant.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    match (
        stored.parse::<chrono::DateTime<chrono::Utc>>(),
        requested.parse::<chrono::DateTime<chrono::Utc>>(),
    ) {
        (Ok(stored), Ok(requested)) => format(&stored.max(requested)),
        (_, Ok(requested)) => format(&requested),
        (_, Err(_)) => stored.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_audit_event(action: &str) -> NewAuditEvent {
        NewAuditEvent {
            actor: "host_terminal".into(),
            environment: "installation".into(),
            target: None,
            thread_id: None,
            action: action.into(),
            scope: "admin".into(),
            confirmation: "host_terminal".into(),
            payload_digest: "a".repeat(64),
            result: "ok".into(),
            retention: "durable".into(),
        }
    }

    fn sample_grant(grant_id: &str) -> GrantRow {
        GrantRow {
            grant_id: grant_id.into(),
            installation_id: "installation".into(),
            secret_hash: "hash".into(),
            scopes: vec!["read".into()],
            permission_ceiling: "default".into(),
            project_allowlist: Vec::new(),
            issued_at: now_iso(),
            expires_at: now_iso(),
            revoked_at: None,
            label: None,
        }
    }

    #[test]
    fn open_sets_wal_and_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        {
            let store = BatonStore::open(&path).unwrap();
            thread_for_archive(&store, "wal");
        }
        let db = Connection::open(&path).unwrap();
        let journal: String = db
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal.to_ascii_lowercase(), "wal");
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn a_version_one_store_gains_the_host_claim_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let db = Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", 1).unwrap();
        drop(db);

        let store = BatonStore::open(&path).unwrap();
        assert!(store.get_host_claim().unwrap().is_none());
        let db = Connection::open(&path).unwrap();
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn a_version_two_store_gains_provider_host_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE provider_processes (
               pid INTEGER PRIMARY KEY,
               provider TEXT NOT NULL,
               port INTEGER,
               command TEXT NOT NULL,
               process_start TEXT,
               cwd TEXT NOT NULL,
               owner_pid INTEGER NOT NULL,
               started_at TEXT NOT NULL
             );
             PRAGMA user_version = 2;",
        )
        .unwrap();
        drop(db);

        let store = BatonStore::open(&path).unwrap();
        store
            .record_provider_process(
                42,
                ProviderId::Codex,
                None,
                "codex app-server",
                Some("start"),
                "/work",
                7,
                Some("owner-start"),
                Some("host-one"),
            )
            .unwrap();
        let row = store.list_provider_processes().unwrap().remove(0);
        assert_eq!(row.owner_start.as_deref(), Some("owner-start"));
        assert_eq!(row.host_id.as_deref(), Some("host-one"));
        let db = Connection::open(&path).unwrap();
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn every_store_mutation_uses_the_write_seam() {
        let source = include_str!("store.rs");
        let direct_execute = ["self.db", ".execute"].concat();
        let direct_tx = ["self.db", ".unchecked_transaction"].concat();
        let raw_tx = ["Transaction::new_unchecked(&self", ".db"].concat();
        assert!(
            !source.contains(&direct_execute),
            "a store write bypassed write_execute"
        );
        assert!(
            !source.contains(&direct_tx),
            "a store write bypassed write()"
        );
        assert_eq!(
            source.matches(&raw_tx).count(),
            3,
            "only write(), claim_host_row, and takeover_host_row may open the raw transaction"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_live_host_claim_fences_out_a_local_writer() {
        use crate::claim::{acquire, ClaimOutcome, SystemProbe};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let host_id = uuid::Uuid::new_v4().to_string();
        let host = BatonStore::open_hosted(&path, &host_id).unwrap();
        let local = BatonStore::open(&path).unwrap();
        assert!(matches!(
            acquire(&host, dir.path(), &host_id, &SystemProbe).unwrap(),
            ClaimOutcome::Claimed(_)
        ));
        let error = local
            .create_thread(NewThread {
                title: "local".into(),
                cwd: "/local".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap_err();
        assert!(error.downcast_ref::<NotTheHost>().is_some(), "{error}");
        host.create_thread(NewThread {
            title: "host".into(),
            cwd: "/host".into(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
        assert!(host.release_host_claim(&host_id).unwrap());
        local
            .create_thread(NewThread {
                title: "local after release".into(),
                cwd: "/local".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_host_store_needs_its_live_claim() {
        use crate::claim::{acquire, ClaimOutcome, SystemProbe};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let host_id = uuid::Uuid::new_v4().to_string();
        let host = BatonStore::open_hosted(&path, &host_id).unwrap();
        let error = host
            .create_thread(NewThread {
                title: "before claim".into(),
                cwd: "/host".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<NotTheHost>(),
            Some(NotTheHost { owner_pid: None, .. })
        ));
        assert!(matches!(
            acquire(&host, dir.path(), &host_id, &SystemProbe).unwrap(),
            ClaimOutcome::Claimed(_)
        ));
        assert!(host.release_host_claim(&host_id).unwrap());
        let error = host
            .create_thread(NewThread {
                title: "after release".into(),
                cwd: "/host".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap_err();
        assert!(error.downcast_ref::<NotTheHost>().is_some(), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_plain_store_cannot_release_a_live_host_claim() {
        use crate::claim::{acquire, ClaimOutcome, SystemProbe};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let host_id = uuid::Uuid::new_v4().to_string();
        let host = BatonStore::open_hosted(&path, &host_id).unwrap();
        let local = BatonStore::open(&path).unwrap();
        assert!(matches!(
            acquire(&host, dir.path(), &host_id, &SystemProbe).unwrap(),
            ClaimOutcome::Claimed(_)
        ));
        let error = local.release_host_claim(&host_id).unwrap_err();
        assert!(error.downcast_ref::<NotTheHost>().is_some(), "{error}");
        assert!(host.get_host_claim().unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_client_open_refuses_a_live_host_without_running_ddl() {
        use crate::claim::{acquire, ClaimOutcome, SystemProbe};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let host_id = uuid::Uuid::new_v4().to_string();
        let host = BatonStore::open_hosted(&path, &host_id).unwrap();
        assert!(matches!(
            acquire(&host, dir.path(), &host_id, &SystemProbe).unwrap(),
            ClaimOutcome::Claimed(_)
        ));
        let db = Connection::open(&path).unwrap();
        db.execute_batch("DROP TABLE provider_processes").unwrap();
        drop(db);

        let error = match BatonStore::open_client(&path, None) {
            Ok(_) => panic!("a live host must refuse a plain client"),
            Err(error) => error,
        };
        let refusal = error
            .downcast_ref::<ClientRefusal>()
            .unwrap_or_else(|| panic!("expected ClientRefusal, got {error}"));
        assert_eq!(refusal.owner_pid, std::process::id());
        assert_eq!(refusal.exit_code(), 4);
        let message = refusal.to_string();
        assert!(message.contains("owns this store"), "{message}");
        assert!(message.contains("baton host takeover --pid"), "{message}");

        let db = Connection::open(&path).unwrap();
        let table: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'provider_processes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table, 0, "a refused client must not run schema DDL");
        drop(db);

        assert!(host.release_host_claim(&host_id).unwrap());
        let client = BatonStore::open_client(&path, None).unwrap();
        client
            .create_thread(NewThread {
                title: "after release".into(),
                cwd: "/client".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let db = Connection::open(&path).unwrap();
        let table: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'provider_processes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table, 1, "the writable client restores the schema");
    }

    #[cfg(unix)]
    #[test]
    fn takeover_settlement_ends_open_turns_and_running_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let store = BatonStore::open(&path).unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "taken over".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn(
                &thread.id,
                ProviderId::Codex,
                Some("model"),
                "open work",
                false,
            )
            .unwrap();
        let owner_pid = std::process::id();
        let owner_start = crate::process::process_start(owner_pid);
        store
            .create_orchestration_run_owned(
                "run",
                "goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
                Some(owner_pid),
                owner_start.as_deref(),
            )
            .unwrap();

        let settlement = store
            .settle_open_work_after_takeover("host pid 42 was taken over")
            .unwrap();
        assert_eq!(settlement.turns, 1);
        assert_eq!(settlement.runs, 1);
        let turn = store.get_turn_record(&thread.id, 1).unwrap().unwrap();
        assert_eq!(turn.phase, TurnPhase::Unverified);
        assert!(turn.error.as_deref().unwrap_or("").contains("taken over"));
        let run = store
            .list_orchestration_runs(1)
            .unwrap()
            .into_iter()
            .next()
            .expect("run");
        assert_ne!(run.status, "running");
        assert!(run.finished_at.is_some());
    }

    fn add_run(store: &BatonStore, id: &str, owner: Option<u32>) {
        let owner_start = owner.and_then(crate::process::process_start);
        store
            .create_orchestration_run_owned(
                id,
                "goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
                owner,
                owner_start.as_deref(),
            )
            .unwrap();
    }

    fn add_run_task(
        store: &BatonStore,
        run_id: &str,
        id: &str,
        status: &str,
        thread_id: Option<&str>,
    ) {
        let now = now_iso();
        store
            .add_orchestration_task(
                id,
                run_id,
                1,
                id,
                ProviderId::Codex,
                "model",
                "prompt",
                None,
                None,
                None,
                thread_id,
                status,
                None,
                None,
                &now,
                None,
                None,
                None,
                None,
            )
            .unwrap();
    }

    fn run_row(store: &BatonStore, id: &str) -> OrchestrationRunRow {
        store
            .find_orchestration_runs(id)
            .unwrap()
            .into_iter()
            .find(|run| run.id == id)
            .unwrap()
    }

    #[test]
    fn settle_completed_refuses_open_or_unfinished_work() {
        let store = BatonStore::memory().unwrap();
        add_run(&store, "run-a", None);
        add_run_task(&store, "run-a", "t1", "completed", None);
        add_run_task(&store, "run-a", "t2", "failed", None);
        let error = store
            .settle_orchestration_run("run-a", RunVerdict::Completed, false)
            .unwrap_err();
        assert!(
            error.to_string().contains("1 of 2 task(s) completed"),
            "{error}"
        );
        assert_eq!(run_row(&store, "run-a").status, "running");

        store
            .update_orchestration_task_success("t2", None, None, false)
            .unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "worker".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn(&thread.id, ProviderId::Codex, Some("model"), "open", false)
            .unwrap();
        add_run_task(&store, "run-a", "t3", "running", Some(&thread.id));
        let error = store
            .settle_orchestration_run("run-a", RunVerdict::Completed, false)
            .unwrap_err();
        assert!(error.to_string().contains("open task(s)"), "{error}");
        store
            .update_orchestration_task_success("t3", None, None, false)
            .unwrap();
        let error = store
            .settle_orchestration_run("run-a", RunVerdict::Completed, false)
            .unwrap_err();
        assert!(error.to_string().contains("open turn(s)"), "{error}");
        assert_eq!(run_row(&store, "run-a").status, "running");

        store
            .finish_turn_record(&thread.id, 1, TurnPhase::Completed, None)
            .unwrap();
        let settlement = store
            .settle_orchestration_run("run-a", RunVerdict::Completed, false)
            .unwrap();
        assert_eq!(settlement, RunSettlement { tasks: 0, turns: 0 });
        let run = run_row(&store, "run-a");
        assert_eq!(run.status, "completed");
        assert!(run.finished_at.is_some());
        let turn = store.get_turn_record(&thread.id, 1).unwrap().unwrap();
        assert_eq!(turn.phase, TurnPhase::Completed);
    }

    #[test]
    fn settle_completed_refuses_a_run_without_tasks() {
        let store = BatonStore::memory().unwrap();
        add_run(&store, "run-empty", None);
        let error = store
            .settle_orchestration_run("run-empty", RunVerdict::Completed, false)
            .unwrap_err();
        assert!(
            error.to_string().contains("because it has no tasks"),
            "{error}"
        );
        assert_eq!(run_row(&store, "run-empty").status, "running");
    }

    #[test]
    fn settle_cancelled_closes_tasks_and_turns_together() {
        let store = BatonStore::memory().unwrap();
        add_run(&store, "run-b", None);
        let thread = store
            .create_thread(NewThread {
                title: "worker".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn(&thread.id, ProviderId::Codex, Some("model"), "open", false)
            .unwrap();
        add_run_task(&store, "run-b", "queued", "queued", None);
        add_run_task(&store, "run-b", "running", "running", Some(&thread.id));
        let settlement = store
            .settle_orchestration_run("run-b", RunVerdict::Cancelled, false)
            .unwrap();
        assert_eq!(settlement, RunSettlement { tasks: 2, turns: 1 });
        let tasks = store.list_orchestration_tasks("run-b").unwrap();
        assert!(tasks.iter().all(|task| task.status == "cancelled"));
        assert!(tasks.iter().all(|task| task.finished_at.is_some()));
        assert!(tasks
            .iter()
            .all(|task| task.error.as_deref().unwrap_or("").contains("cancelled")));
        let turn = store.get_turn_record(&thread.id, 1).unwrap().unwrap();
        assert_eq!(turn.phase, TurnPhase::Unverified);
        assert!(turn.error.as_deref().unwrap_or("").contains("cancelled"));
        let run = run_row(&store, "run-b");
        assert_eq!(run.status, "cancelled");
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn settle_terminal_verdicts_mark_open_tasks_failed() {
        for (run_id, verdict, expected) in [
            ("run-p", RunVerdict::Partial, "partial"),
            ("run-f", RunVerdict::Failed, "failed"),
            ("run-r", RunVerdict::NeedsReview, "needs_review"),
            ("run-i", RunVerdict::NeedsInput, "needs_input"),
        ] {
            let store = BatonStore::memory().unwrap();
            add_run(&store, run_id, None);
            add_run_task(&store, run_id, "open", "queued", None);
            let settlement = store
                .settle_orchestration_run(run_id, verdict, false)
                .unwrap();
            assert_eq!(settlement.tasks, 1);
            let tasks = store.list_orchestration_tasks(run_id).unwrap();
            assert_eq!(tasks[0].status, "failed");
            assert!(tasks[0].error.as_deref().unwrap_or("").contains(expected));
            assert_eq!(run_row(&store, run_id).status, expected);
        }
    }

    #[test]
    fn cancel_host_work_settles_only_the_named_owner() {
        let store = BatonStore::memory().unwrap();
        let host_pid = 4_000_001;
        let host_start = "host-start";
        let other_pid = 4_000_002;
        let host_thread = store
            .create_thread(NewThread {
                title: "host".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn(&host_thread.id, ProviderId::Codex, None, "host", false)
            .unwrap();
        let other_thread = store
            .create_thread(NewThread {
                title: "other".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn(&other_thread.id, ProviderId::Codex, None, "other", false)
            .unwrap();
        store
            .write_execute(
                "UPDATE turns SET owner_pid = ?, owner_start = ? WHERE thread_id = ?",
                params![host_pid, host_start, host_thread.id],
            )
            .unwrap();
        store
            .write_execute(
                "UPDATE turns SET owner_pid = ?, owner_start = NULL WHERE thread_id = ?",
                params![other_pid, other_thread.id],
            )
            .unwrap();
        store
            .create_orchestration_run_owned(
                "run-host",
                "host goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
                Some(host_pid),
                Some(host_start),
            )
            .unwrap();
        store
            .add_orchestration_task(
                "task-host",
                "run-host",
                1,
                "host task",
                ProviderId::Codex,
                "model",
                "do host",
                Some("/tmp/host-worktree"),
                None,
                None,
                Some(&host_thread.id),
                "queued",
                None,
                None,
                "2026-11-26T00:00:00.000Z",
                None,
                None,
                None,
                None,
            )
            .unwrap();
        store
            .create_orchestration_run_owned(
                "run-other",
                "other goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
                Some(other_pid),
                None,
            )
            .unwrap();
        store
            .add_orchestration_task(
                "task-other",
                "run-other",
                1,
                "other task",
                ProviderId::Codex,
                "model",
                "do other",
                None,
                None,
                None,
                None,
                "queued",
                None,
                None,
                "2026-11-26T00:00:00.000Z",
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let settlement = store
            .cancel_host_work(host_pid, Some(host_start), "host shut down")
            .unwrap();
        assert_eq!(
            settlement,
            HostWorkSettlement {
                turns: 1,
                tasks: 1,
                runs: vec!["run-host".to_string()],
            }
        );
        let host_turn = store.get_turn_record(&host_thread.id, 1).unwrap().unwrap();
        assert_eq!(host_turn.phase, TurnPhase::Unverified);
        assert_eq!(host_turn.error.as_deref(), Some("host shut down"));
        let other_turn = store.get_turn_record(&other_thread.id, 1).unwrap().unwrap();
        assert_eq!(
            other_turn.phase,
            TurnPhase::Running,
            "another owner's turn is untouched"
        );
        let host_task = store.list_orchestration_tasks("run-host").unwrap();
        assert_eq!(host_task[0].status, "cancelled");
        assert_eq!(host_task[0].error.as_deref(), Some("host shut down"));
        assert_eq!(
            host_task[0].worktree_path.as_deref(),
            Some("/tmp/host-worktree")
        );
        assert_eq!(run_row(&store, "run-host").status, "cancelled");
        let other_task = store.list_orchestration_tasks("run-other").unwrap();
        assert_eq!(other_task[0].status, "queued");
        assert_eq!(
            run_row(&store, "run-other").status,
            "running",
            "another owner's run is untouched"
        );
    }

    #[test]
    fn settle_refuses_a_missing_or_terminal_run() {
        let store = BatonStore::memory().unwrap();
        let error = store
            .settle_orchestration_run("missing", RunVerdict::Failed, false)
            .unwrap_err();
        assert!(
            error.to_string().contains("no orchestration run matches"),
            "{error}"
        );
        add_run(&store, "run-t", None);
        store
            .finish_orchestration_run("run-t", "completed")
            .unwrap();
        let error = store
            .settle_orchestration_run("run-t", RunVerdict::Cancelled, false)
            .unwrap_err();
        assert!(error.to_string().contains("already completed"), "{error}");
        assert_eq!(run_row(&store, "run-t").status, "completed");
    }

    #[cfg(unix)]
    #[test]
    fn settle_refuses_a_live_owner_without_force() {
        let store = BatonStore::memory().unwrap();
        add_run(&store, "run-live", Some(std::process::id()));
        add_run_task(&store, "run-live", "open", "running", None);
        let error = store
            .settle_orchestration_run("run-live", RunVerdict::Cancelled, false)
            .unwrap_err();
        assert!(
            error.to_string().contains("still owned by live process"),
            "{error}"
        );
        assert_eq!(run_row(&store, "run-live").status, "running");
        let settlement = store
            .settle_orchestration_run("run-live", RunVerdict::Cancelled, true)
            .unwrap();
        assert_eq!(settlement.tasks, 1);
        assert_eq!(run_row(&store, "run-live").status, "cancelled");
    }

    #[test]
    fn a_version_three_store_gains_the_turn_grant_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let db = Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", 3).unwrap();
        db.execute_batch(
            "CREATE TABLE turns (
               thread_id TEXT NOT NULL,
               turn INTEGER NOT NULL,
               provider TEXT NOT NULL,
               model TEXT,
               phase TEXT NOT NULL,
               error TEXT,
               provider_turn_id TEXT,
               started_at TEXT NOT NULL,
               ended_at TEXT,
               owner_pid INTEGER,
               owner_start TEXT,
               PRIMARY KEY (thread_id, turn)
             );",
        )
        .unwrap();
        drop(db);

        let store = BatonStore::open(&path).unwrap();
        drop(store);
        let db = Connection::open(&path).unwrap();
        let columns: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('turns') WHERE name = 'owner_grant_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 1, "the migration adds the grant column");
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn turn_rows_read_the_grant_owner() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "grant".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn(&thread.id, ProviderId::Codex, None, "work", false)
            .unwrap();
        let turn = store.get_turn_record(&thread.id, 1).unwrap().unwrap();
        assert_eq!(turn.owner_grant_id, None, "local work has no grant");

        store
            .write_execute(
                "UPDATE turns SET owner_grant_id = ? WHERE thread_id = ? AND turn = 1",
                params!["grant-1", thread.id],
            )
            .unwrap();
        let turn = store.get_turn_record(&thread.id, 1).unwrap().unwrap();
        assert_eq!(turn.owner_grant_id.as_deref(), Some("grant-1"));
        let listed = store.list_turn_records(&thread.id).unwrap();
        assert_eq!(listed[0].owner_grant_id.as_deref(), Some("grant-1"));
    }

    #[test]
    fn a_version_four_store_gains_the_grants_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let db = Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", 4).unwrap();
        drop(db);

        let store = BatonStore::open(&path).unwrap();
        drop(store);
        let db = Connection::open(&path).unwrap();
        let table: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'grants'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table, 1, "the migration creates the grants table");
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn a_version_five_store_gains_append_only_audit_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let db = Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", 5).unwrap();
        drop(db);

        let store = BatonStore::open(&path).unwrap();
        drop(store);
        let db = Connection::open(&path).unwrap();
        let table: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'audit_events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table, 1, "the migration creates the audit table");
        let triggers: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'audit_events_no_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            triggers, 2,
            "the migration creates both append-only triggers"
        );
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn a_version_six_store_gains_the_audit_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            r#"
CREATE TABLE audit_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  actor TEXT NOT NULL,
  environment TEXT NOT NULL,
  thread_id TEXT,
  action TEXT NOT NULL,
  scope TEXT NOT NULL,
  confirmation TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  result TEXT NOT NULL,
  created_at TEXT NOT NULL,
  retention TEXT NOT NULL
);
"#,
        )
        .unwrap();
        db.execute(
            "INSERT INTO audit_events (actor, environment, thread_id, action, scope, confirmation, payload_digest, result, created_at, retention) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                "host_terminal",
                "installation",
                None::<String>,
                "grant.mint",
                "admin",
                "host_terminal",
                "a".repeat(64),
                "ok",
                "2026-11-01T00:00:00.000Z",
                "durable"
            ],
        )
        .unwrap();
        db.pragma_update(None, "user_version", 6).unwrap();
        drop(db);

        let store = BatonStore::open(&path).unwrap();
        let events = store.list_audit_events(10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].target, None,
            "a row written before the column has no target"
        );
        let mut event = sample_audit_event("grant.mint");
        event.target = Some("grant-1".into());
        store.append_audit_event(&event).unwrap();
        let events = store.list_audit_events(10).unwrap();
        assert_eq!(events[0].target.as_deref(), Some("grant-1"));
        assert_eq!(events[1].target, None);
    }

    #[test]
    fn audit_events_are_newest_first_and_append_only() {
        let store = BatonStore::memory().unwrap();
        let first = store
            .append_audit_event(&sample_audit_event("grant.mint"))
            .unwrap();
        let second = store
            .append_audit_event(&sample_audit_event("grant.revoke"))
            .unwrap();
        assert!(second.id > first.id);
        let listed = store.list_audit_events(10).unwrap();
        assert_eq!(listed[0].id, second.id);
        assert_eq!(listed[1].id, first.id);
        let limited = store.list_audit_events(1).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].id, second.id);

        let update = store
            .write_execute("UPDATE audit_events SET action = 'changed'", [])
            .unwrap_err();
        assert!(update.to_string().contains("audit_events_append_only"));
        let delete = store
            .write_execute("DELETE FROM audit_events", [])
            .unwrap_err();
        assert!(delete.to_string().contains("audit_events_append_only"));
        assert_eq!(store.list_audit_events(10).unwrap().len(), 2);
    }

    #[test]
    fn a_grant_and_its_audit_event_commit_together() {
        let store = BatonStore::memory().unwrap();
        let grant = sample_grant("00000000-0000-4000-8000-000000000001");
        let mut invalid = sample_audit_event("grant.mint");
        invalid.payload_digest = "short".into();
        assert!(store.insert_grant_with_audit(&grant, &invalid).is_err());
        assert!(store.find_grant_row(&grant.grant_id).unwrap().is_none());
        assert!(store.list_audit_events(10).unwrap().is_empty());

        let event = store
            .insert_grant_with_audit(&grant, &sample_audit_event("grant.mint"))
            .unwrap();
        assert_eq!(event.action, "grant.mint");
        assert_eq!(store.list_audit_events(10).unwrap().len(), 1);
        assert!(store.find_grant_row(&grant.grant_id).unwrap().is_some());
    }

    #[test]
    fn a_revoke_and_its_audit_event_commit_together() {
        let store = BatonStore::memory().unwrap();
        let grant = sample_grant("00000000-0000-4000-8000-000000000002");
        store.insert_grant(&grant).unwrap();
        let mut invalid = sample_audit_event("grant.revoke");
        invalid.payload_digest = "short".into();
        assert!(store
            .revoke_grant_row_with_audit(&grant.grant_id, &invalid)
            .is_err());
        let untouched = store.find_grant_row(&grant.grant_id).unwrap().unwrap();
        assert!(untouched.revoked_at.is_none());
        assert!(store.list_audit_events(10).unwrap().is_empty());

        let revocation = store
            .revoke_grant_row_with_audit(&grant.grant_id, &sample_audit_event("grant.revoke"))
            .unwrap()
            .expect("a present grant writes an audit event");
        assert_eq!(revocation.action, "grant.revoke");
        assert!(store
            .find_grant_row(&grant.grant_id)
            .unwrap()
            .unwrap()
            .revoked_at
            .is_some());
        assert_eq!(store.list_audit_events(10).unwrap().len(), 1);
    }

    #[test]
    fn a_renewal_and_its_audit_event_commit_together() {
        let store = BatonStore::memory().unwrap();
        let grant = sample_grant("00000000-0000-4000-8000-000000000003");
        let original = grant.expires_at.clone();
        store.insert_grant(&grant).unwrap();
        let later = (chrono::Utc::now() + chrono::Duration::days(60))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let mut invalid = sample_audit_event("grant.renew");
        invalid.payload_digest = "short".into();
        assert!(store
            .renew_grant_row_with_audit(&grant.grant_id, &later, &invalid)
            .is_err());
        let untouched = store.find_grant_row(&grant.grant_id).unwrap().unwrap();
        assert_eq!(untouched.expires_at, original);
        assert!(store.list_audit_events(10).unwrap().is_empty());

        let outcome = store
            .renew_grant_row_with_audit(&grant.grant_id, &later, &sample_audit_event("grant.renew"))
            .unwrap();
        let RenewGrantOutcome::Renewed(payload) = outcome else {
            panic!("a live grant renews: {outcome:?}");
        };
        assert_eq!(payload.previous_expires_at, original);
        assert_eq!(payload.grant.expires_at, later);
        assert_eq!(payload.event.action, "grant.renew");
        let stored = store.find_grant_row(&grant.grant_id).unwrap().unwrap();
        assert_eq!(stored.secret_hash, grant.secret_hash);
        assert_eq!(store.list_audit_events(10).unwrap().len(), 1);

        let sooner = (chrono::Utc::now() + chrono::Duration::days(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let outcome = store
            .renew_grant_row_with_audit(&grant.grant_id, &sooner, &sample_audit_event("grant.renew"))
            .unwrap();
        let RenewGrantOutcome::Renewed(payload) = outcome else {
            panic!("a live grant renews: {outcome:?}");
        };
        assert_eq!(payload.grant.expires_at, later, "a renewal never shortens");

        store.revoke_grant_row(&grant.grant_id).unwrap();
        let outcome = store
            .renew_grant_row_with_audit(&grant.grant_id, &later, &sample_audit_event("grant.renew"))
            .unwrap();
        assert_eq!(outcome, RenewGrantOutcome::Revoked);
        assert_eq!(store.list_audit_events(10).unwrap().len(), 2);

        let outcome = store
            .renew_grant_row_with_audit(
                "00000000-0000-4000-8000-000000000004",
                &later,
                &sample_audit_event("grant.renew"),
            )
            .unwrap();
        assert_eq!(outcome, RenewGrantOutcome::Missing);
        assert_eq!(store.list_audit_events(10).unwrap().len(), 2);
    }

    #[test]
    fn a_turn_and_its_audit_event_commit_together() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "audited".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let event = |thread_id: &str| {
            let mut event = sample_audit_event("turn.submit");
            event.actor = "host_engine".into();
            event.scope = "operate".into();
            event.confirmation = "host_session".into();
            event.thread_id = Some(thread_id.to_string());
            event
        };

        let mut invalid = event(&thread.id);
        invalid.payload_digest = "short".into();
        assert!(store
            .submit_turn_with_audit(
                &thread.id,
                ProviderId::Codex,
                None,
                "hello",
                false,
                &invalid
            )
            .is_err());
        assert!(store.list_messages(&thread.id).unwrap().is_empty());
        assert!(store.get_turn_record(&thread.id, 1).unwrap().is_none());
        assert!(store.list_audit_events(10).unwrap().is_empty());

        let (turn, written) = store
            .submit_turn_with_audit(
                &thread.id,
                ProviderId::Codex,
                None,
                "hello",
                false,
                &event(&thread.id),
            )
            .unwrap();
        assert_eq!(turn, 1);
        assert_eq!(written.action, "turn.submit");
        assert_eq!(written.actor, "host_engine");
        assert_eq!(written.scope, "operate");
        assert_eq!(written.confirmation, "host_session");
        assert_eq!(written.thread_id.as_deref(), Some(thread.id.as_str()));
        assert_eq!(store.list_messages(&thread.id).unwrap().len(), 1);
        assert_eq!(store.list_audit_events(10).unwrap().len(), 1);

        let refused = store
            .submit_turn_with_audit(
                &thread.id,
                ProviderId::Codex,
                None,
                "again",
                false,
                &event(&thread.id),
            )
            .unwrap_err();
        assert!(
            refused.downcast_ref::<SubmitRefused>().is_some(),
            "{refused}"
        );
        assert_eq!(store.list_audit_events(10).unwrap().len(), 1);
    }

    #[test]
    fn a_queue_clear_and_its_audit_event_commit_together() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "queue".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store.enqueue_message(&thread.id, "one").unwrap();
        let mut invalid = sample_audit_event("queue.clear");
        invalid.thread_id = Some(thread.id.clone());
        invalid.payload_digest = "short".into();
        assert!(store
            .clear_queued_messages_with_audit(&thread.id, &invalid)
            .is_err());
        assert_eq!(
            store.list_queued_messages(&thread.id).unwrap(),
            vec!["one".to_string()]
        );
        assert!(store.list_audit_events(10).unwrap().is_empty());

        let mut valid = sample_audit_event("queue.clear");
        valid.thread_id = Some(thread.id.clone());
        let event = store
            .clear_queued_messages_with_audit(&thread.id, &valid)
            .unwrap();
        assert_eq!(event.action, "queue.clear");
        assert_eq!(event.thread_id.as_deref(), Some(thread.id.as_str()));
        assert!(store.list_queued_messages(&thread.id).unwrap().is_empty());
        assert_eq!(store.list_audit_events(10).unwrap().len(), 1);
    }

    #[test]
    fn a_fork_and_its_audit_event_commit_together() {
        let store = BatonStore::memory().unwrap();
        let source = store
            .create_thread(NewThread {
                title: "source".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn(&source.id, ProviderId::Codex, None, "hello", false)
            .unwrap();
        store
            .finish_turn_record(&source.id, 1, TurnPhase::Completed, None)
            .unwrap();

        let mut invalid = sample_audit_event("thread.fork");
        invalid.payload_digest = "short".into();
        assert!(store
            .fork_thread_through_turn_with_audit(&source.id, "fork", Some(1), &invalid)
            .is_err());
        assert_eq!(store.list_threads(None).unwrap().len(), 1);
        assert!(store.list_audit_events(10).unwrap().is_empty());

        let (forked, audit) = store
            .fork_thread_through_turn_with_audit(
                &source.id,
                "fork",
                Some(1),
                &sample_audit_event("thread.fork"),
            )
            .unwrap();
        assert_ne!(forked.id, source.id);
        assert_eq!(audit.action, "thread.fork");
        assert_eq!(audit.thread_id.as_deref(), Some(forked.id.as_str()));
        assert_eq!(store.list_threads(None).unwrap().len(), 2);
        assert_eq!(store.list_audit_events(10).unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_client_open_refuses_an_old_store_with_a_live_host_without_running_ddl() {
        use crate::claim::{acquire, ClaimOutcome, SystemProbe};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let host_id = uuid::Uuid::new_v4().to_string();
        let host = BatonStore::open_hosted(&path, &host_id).unwrap();
        assert!(matches!(
            acquire(&host, dir.path(), &host_id, &SystemProbe).unwrap(),
            ClaimOutcome::Claimed(_)
        ));
        {
            let db = Connection::open(&path).unwrap();
            db.pragma_update(None, "user_version", 4).unwrap();
        }
        let error = match BatonStore::open_client(&path, None) {
            Ok(_) => panic!("a live host must refuse the client"),
            Err(error) => error,
        };
        assert!(error.downcast_ref::<ClientRefusal>().is_some(), "{error}");
        {
            let db = Connection::open(&path).unwrap();
            let version: i64 = db
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, 4, "a refused client does not migrate the store");
        }
        assert!(host.release_host_claim(&host_id).unwrap());
        drop(BatonStore::open_client(&path, None).unwrap());
        let db = Connection::open(&path).unwrap();
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn a_client_open_migrates_an_old_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let db = Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", 1).unwrap();
        drop(db);

        let store = BatonStore::open_client(&path, None).unwrap();
        store
            .create_thread(NewThread {
                title: "migrated".into(),
                cwd: "/client".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let db = Connection::open(&path).unwrap();
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn read_only_open_reads_without_running_ddl_or_allowing_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        {
            let store = BatonStore::open(&path).unwrap();
            thread_for_archive(&store, "read-only");
        }
        let db = Connection::open(&path).unwrap();
        db.execute_batch("DROP TABLE plans").unwrap();
        drop(db);

        let store = BatonStore::open_read_only(&path).unwrap();
        assert_eq!(store.list_threads(None).unwrap().len(), 1);
        let error = store
            .create_thread(NewThread {
                title: "write".into(),
                cwd: "/tmp/read-only".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap_err();
        assert!(
            error.to_string().to_ascii_lowercase().contains("readonly")
                || error.to_string().to_ascii_lowercase().contains("read-only"),
            "{error}"
        );
        drop(store);
        let db = Connection::open(&path).unwrap();
        let plans: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'plans'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(plans, 0, "read-only open must not run schema DDL");
    }

    #[test]
    fn read_only_open_refuses_an_unversioned_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        drop(Connection::open(&path).unwrap());
        let error = BatonStore::open_read_only(&path)
            .err()
            .expect("schema refusal");
        assert!(error.to_string().contains("schema_too_old"), "{error}");
    }

    #[test]
    fn read_only_open_refuses_a_future_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let db = Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(db);
        let error = BatonStore::open_read_only(&path)
            .err()
            .expect("schema refusal");
        assert!(error.to_string().contains("schema_too_new"), "{error}");
    }

    fn stored_plan(provider: ProviderId, content: &str) -> StoredPlan {
        StoredPlan {
            provider,
            plan_id: Some("plan-1".into()),
            turn_id: Some("turn-1".into()),
            entries: vec![crate::types::PlanEntry {
                content: content.into(),
                status: "in_progress".into(),
                priority: Some("high".into()),
                kind: crate::types::PlanKind::Items,
                uri: None,
            }],
            explanation: Some("two steps".into()),
        }
    }

    fn thread_for_archive(store: &BatonStore, title: &str) -> ThreadRow {
        store
            .create_thread(NewThread {
                title: title.to_string(),
                cwd: "/tmp/archive-test".to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap()
    }

    fn sample_rate_limits() -> crate::types::RateLimitInfo {
        crate::types::RateLimitInfo {
            plan_type: Some("plus".into()),
            primary: Some(crate::types::RateLimitWindow {
                used_percent: 12,
                window_duration_mins: Some(300),
                resets_at: Some(1_767_000_000),
            }),
            secondary: None,
            credits_balance: Some("12.50".into()),
            credits_unlimited: false,
            reported_at: "2026-11-26T03:00:00Z".into(),
        }
    }

    #[test]
    fn rate_limits_round_trip_and_skip_invalid_rows() {
        let store = BatonStore::memory().unwrap();
        let info = sample_rate_limits();
        store.save_rate_limits(ProviderId::Codex, &info).unwrap();
        let loaded = store.load_rate_limits().unwrap();
        assert_eq!(loaded.get(&ProviderId::Codex), Some(&info));

        store
            .db
            .execute(
                "INSERT INTO provider_rate_limits (provider, info, updated_at) VALUES ('claude', 'not json', 'now')",
                [],
            )
            .unwrap();
        store
            .db
            .execute(
                "INSERT INTO provider_rate_limits (provider, info, updated_at) VALUES ('retired', '{}', 'now')",
                [],
            )
            .unwrap();
        let loaded = store.load_rate_limits().unwrap();
        assert_eq!(loaded.len(), 1, "invalid and unknown providers are skipped");
        assert_eq!(loaded.get(&ProviderId::Codex), Some(&info));

        let updated = crate::types::RateLimitInfo {
            primary: Some(crate::types::RateLimitWindow {
                used_percent: 40,
                window_duration_mins: Some(300),
                resets_at: None,
            }),
            ..info.clone()
        };
        store.save_rate_limits(ProviderId::Codex, &updated).unwrap();
        assert_eq!(
            store
                .load_rate_limits()
                .unwrap()
                .get(&ProviderId::Codex)
                .unwrap()
                .primary
                .as_ref()
                .unwrap()
                .used_percent,
            40,
            "a newer snapshot replaces the row"
        );
    }

    #[test]
    fn archive_hides_a_thread_and_unarchive_restores_it() {
        let store = BatonStore::memory().unwrap();
        let thread = thread_for_archive(&store, "later");
        assert_eq!(
            store.archive_thread(&thread.id).unwrap(),
            ArchiveOutcome::Archived
        );
        assert!(
            store.list_threads(None).unwrap().is_empty(),
            "the default list hides archived threads"
        );
        assert_eq!(store.list_threads_all(None).unwrap().len(), 1);
        assert!(store.list_threads_display_all(None).unwrap()[0]
            .archived_at
            .is_some());
        assert_eq!(
            store.unarchive_thread(&thread.id).unwrap(),
            ArchiveOutcome::Unarchived
        );
        assert_eq!(store.list_threads(None).unwrap().len(), 1);
        assert_eq!(
            store.unarchive_thread(&thread.id).unwrap(),
            ArchiveOutcome::NotArchived
        );
    }

    #[test]
    fn archive_refuses_a_running_turn_and_clears_a_snooze() {
        let store = BatonStore::memory().unwrap();
        let thread = thread_for_archive(&store, "busy");
        let until = "2099-01-01T00:00:00Z";
        store.set_thread_snoozed(&thread.id, until).unwrap();
        store
            .start_turn_record(&thread.id, 1, ProviderId::Codex, None)
            .unwrap();
        assert_eq!(
            store.archive_thread(&thread.id).unwrap(),
            ArchiveOutcome::RunningTurn
        );
        assert_eq!(store.list_threads(None).unwrap().len(), 1);
        store
            .finish_turn_record(&thread.id, 1, crate::types::TurnPhase::Completed, None)
            .unwrap();
        assert_eq!(
            store.archive_thread(&thread.id).unwrap(),
            ArchiveOutcome::Archived
        );
        let archived = store.list_threads_all(None).unwrap().remove(0);
        assert!(archived.archived_at.is_some());
        assert!(
            archived.snoozed_until.is_none(),
            "archive supersedes snooze"
        );
        assert_eq!(
            store.archive_thread(&thread.id).unwrap(),
            ArchiveOutcome::AlreadyArchived
        );
        assert_eq!(
            store.archive_thread("missing").unwrap(),
            ArchiveOutcome::NotFound
        );
    }

    #[test]
    fn archive_survives_a_store_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let thread_id = {
            let store = BatonStore::open(&path).unwrap();
            let thread = thread_for_archive(&store, "persisted");
            store.archive_thread(&thread.id).unwrap();
            thread.id
        };
        let store = BatonStore::open(&path).unwrap();
        assert!(store.list_threads(None).unwrap().is_empty());
        let archived = store.list_threads_all(None).unwrap().remove(0);
        assert_eq!(archived.id, thread_id);
        assert!(archived.archived_at.is_some());
    }

    #[test]
    fn plans_round_trip_per_provider() {
        let store = BatonStore::memory().unwrap();
        store
            .save_plan("thread-1", &stored_plan(ProviderId::Codex, "inspect"))
            .unwrap();
        store
            .save_plan("thread-1", &stored_plan(ProviderId::Claude, "review"))
            .unwrap();
        let plans = store.list_plans("thread-1").unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].provider, ProviderId::Claude);
        assert_eq!(plans[1].provider, ProviderId::Codex);
        assert_eq!(plans[1].entries[0].content, "inspect");
        assert_eq!(plans[1].entries[0].priority.as_deref(), Some("high"));
        assert_eq!(plans[1].explanation.as_deref(), Some("two steps"));
        assert_eq!(plans[1].plan_id.as_deref(), Some("plan-1"));
        assert_eq!(plans[1].turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn plan_documents_round_trip_through_the_store() {
        let store = BatonStore::memory().unwrap();
        let markdown = StoredPlan {
            provider: ProviderId::Antigravity,
            plan_id: Some("plan-markdown".into()),
            turn_id: None,
            entries: vec![crate::types::PlanEntry::markdown("# Ship it")],
            explanation: None,
        };
        store.save_plan("thread-1", &markdown).unwrap();
        let stored = store.list_plans("thread-1").unwrap().remove(0);
        assert_eq!(stored.entries[0].kind, crate::types::PlanKind::Markdown);
        assert_eq!(stored.entries[0].content, "# Ship it");

        let file = StoredPlan {
            provider: ProviderId::Antigravity,
            plan_id: Some("plan-file".into()),
            turn_id: None,
            entries: vec![crate::types::PlanEntry::file("file:///workspace/PLAN.md")],
            explanation: None,
        };
        store.save_plan("thread-1", &file).unwrap();
        let stored = store.list_plans("thread-1").unwrap().remove(0);
        assert_eq!(stored.entries[0].kind, crate::types::PlanKind::File);
        assert_eq!(
            stored.entries[0].uri.as_deref(),
            Some("file:///workspace/PLAN.md")
        );
    }

    #[test]
    fn saving_a_plan_replaces_the_same_provider() {
        let store = BatonStore::memory().unwrap();
        store
            .save_plan("thread-1", &stored_plan(ProviderId::Codex, "first"))
            .unwrap();
        store
            .save_plan("thread-1", &stored_plan(ProviderId::Codex, "second"))
            .unwrap();
        let plans = store.list_plans("thread-1").unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].entries[0].content, "second");
    }

    #[test]
    fn plans_are_scoped_and_removable() {
        let store = BatonStore::memory().unwrap();
        store
            .save_plan("thread-1", &stored_plan(ProviderId::Codex, "one"))
            .unwrap();
        store
            .save_plan("thread-2", &stored_plan(ProviderId::Codex, "two"))
            .unwrap();
        store.remove_plan("thread-1", ProviderId::Codex).unwrap();
        assert!(store.list_plans("thread-1").unwrap().is_empty());
        assert_eq!(store.list_plans("thread-2").unwrap().len(), 1);
        store.remove_plans_for_thread("thread-2").unwrap();
        assert!(store.list_plans("thread-2").unwrap().is_empty());
    }

    #[test]
    fn unknown_stored_providers_are_skipped() {
        let store = BatonStore::memory().unwrap();
        store
            .save_plan("thread-1", &stored_plan(ProviderId::Codex, "known"))
            .unwrap();
        store
            .db
            .execute(
                "INSERT INTO plans (thread_id, provider, entries, updated_at) VALUES (?, ?, ?, ?)",
                params!["thread-1", "mystery", "[]", now_iso()],
            )
            .unwrap();
        let plans = store.list_plans("thread-1").unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].provider, ProviderId::Codex);
    }

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

    #[test]
    fn submit_turn_writes_the_message_and_owner_in_one_transaction() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "submit".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();

        let turn = store
            .submit_turn(&thread.id, ProviderId::Codex, Some("gpt"), "hello", false)
            .unwrap();
        assert_eq!(turn, 1);

        let messages = store.list_messages(&thread.id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "hello");
        let turn = store
            .get_turn_record(&thread.id, 1)
            .unwrap()
            .expect("turn exists");
        assert_eq!(turn.phase, TurnPhase::Running);
        assert_eq!(turn.owner_pid, Some(std::process::id()));
        assert_eq!(
            turn.owner_start,
            crate::process::process_start(std::process::id())
        );
    }

    #[test]
    fn submit_turn_rolls_back_when_the_turn_write_fails() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "atomic".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .db
            .execute_batch(
                "CREATE TRIGGER refuse_turn BEFORE INSERT ON turns BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();

        let error = store
            .submit_turn(&thread.id, ProviderId::Codex, None, "hello", false)
            .unwrap_err();
        assert!(error.to_string().contains("refused"), "{error}");
        assert!(store.list_messages(&thread.id).unwrap().is_empty());
        assert!(store.get_turn_record(&thread.id, 1).unwrap().is_none());
        assert_eq!(store.thread_open_work(&thread.id).unwrap(), None);
    }

    #[test]
    fn submit_turn_refuses_an_open_turn_without_adding_a_message() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "owner".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn(&thread.id, ProviderId::Codex, None, "first", false)
            .unwrap();

        let error = store
            .submit_turn(&thread.id, ProviderId::Codex, None, "second", false)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<SubmitRefused>(),
            Some(&SubmitRefused(ThreadOpenWork::Turn))
        );
        let messages = store.list_messages(&thread.id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "first");
    }

    #[test]
    fn submit_turn_can_drain_one_queued_message_but_not_skip_the_queue_guard() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "queue".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn(&thread.id, ProviderId::Codex, None, "first", false)
            .unwrap();
        store
            .finish_turn_record(&thread.id, 1, TurnPhase::Completed, None)
            .unwrap();
        store.enqueue_message(&thread.id, "queued one").unwrap();
        store.enqueue_message(&thread.id, "queued two").unwrap();

        let error = store
            .submit_turn(&thread.id, ProviderId::Codex, None, "second", false)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<SubmitRefused>(),
            Some(&SubmitRefused(ThreadOpenWork::Queue))
        );
        assert_eq!(store.list_messages(&thread.id).unwrap().len(), 1);

        assert_eq!(
            store
                .submit_turn(&thread.id, ProviderId::Codex, None, "second", true)
                .unwrap(),
            2
        );
        assert_eq!(store.list_messages(&thread.id).unwrap().len(), 2);
        assert_eq!(store.list_queued_messages(&thread.id).unwrap().len(), 2);
    }

    #[test]
    fn submit_turn_refuses_a_pending_native_rewind() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "rewind".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .set_native_rewind_pending(&thread.id, ProviderId::Codex, 1)
            .unwrap();

        let error = store
            .submit_turn(&thread.id, ProviderId::Codex, None, "hello", false)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<SubmitRefused>(),
            Some(&SubmitRefused(ThreadOpenWork::NativeRewind))
        );
        assert!(store.list_messages(&thread.id).unwrap().is_empty());
    }

    #[test]
    fn reconcile_running_turns_keeps_a_live_owner() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "live owner".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .start_turn_record(&thread.id, 1, ProviderId::Codex, None)
            .unwrap();

        assert!(store
            .reconcile_running_turns(Some("/work"))
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .get_turn_record(&thread.id, 1)
                .unwrap()
                .expect("turn")
                .phase,
            TurnPhase::Running
        );
    }

    #[cfg(unix)]
    #[test]
    fn reconcile_running_turns_settles_dead_ownerless_and_reused_rows() {
        let store = BatonStore::memory().unwrap();
        let mut ids = Vec::new();
        for title in ["dead", "ownerless", "reused"] {
            let thread = store
                .create_thread(NewThread {
                    title: title.into(),
                    cwd: "/work".into(),
                    current_provider: ProviderId::Codex,
                    current_model: None,
                })
                .unwrap();
            store
                .start_turn_record(&thread.id, 1, ProviderId::Codex, None)
                .unwrap();
            ids.push(thread.id);
        }
        store
            .db
            .execute(
                "UPDATE turns SET owner_pid = 0, owner_start = NULL WHERE thread_id = ?",
                params![ids[0]],
            )
            .unwrap();
        store
            .db
            .execute(
                "UPDATE turns SET owner_pid = NULL, owner_start = NULL WHERE thread_id = ?",
                params![ids[1]],
            )
            .unwrap();
        store
            .db
            .execute(
                "UPDATE turns SET owner_pid = ?, owner_start = 'not this process' WHERE thread_id = ?",
                params![std::process::id(), ids[2]],
            )
            .unwrap();

        assert_eq!(
            store.reconcile_running_turns(Some("/work")).unwrap().len(),
            3
        );
        for id in ids {
            assert_eq!(
                store.get_turn_record(&id, 1).unwrap().expect("turn").phase,
                TurnPhase::Unverified
            );
        }
    }

    #[test]
    fn reconcile_running_turns_without_a_project_covers_every_project() {
        let store = BatonStore::memory().unwrap();
        let mut ids = Vec::new();
        for (title, cwd) in [("one", "/one"), ("two", "/two")] {
            let thread = store
                .create_thread(NewThread {
                    title: title.into(),
                    cwd: cwd.into(),
                    current_provider: ProviderId::Codex,
                    current_model: None,
                })
                .unwrap();
            store
                .start_turn_record(&thread.id, 1, ProviderId::Codex, None)
                .unwrap();
            store
                .db
                .execute(
                    "UPDATE turns SET owner_pid = 0, owner_start = NULL WHERE thread_id = ?",
                    params![thread.id],
                )
                .unwrap();
            ids.push(thread.id);
        }
        let live = store
            .create_thread(NewThread {
                title: "live".into(),
                cwd: "/live".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .start_turn_record(&live.id, 1, ProviderId::Codex, None)
            .unwrap();

        assert_eq!(store.reconcile_running_turns(None).unwrap().len(), 2);
        for id in ids {
            assert_eq!(
                store.get_turn_record(&id, 1).unwrap().expect("turn").phase,
                TurnPhase::Unverified
            );
        }
        assert_eq!(
            store
                .get_turn_record(&live.id, 1)
                .unwrap()
                .expect("turn")
                .phase,
            TurnPhase::Running
        );
    }

    #[test]
    fn subagents_round_trip_in_order() {
        let store = BatonStore::memory().unwrap();
        let thread = store.draft_thread(NewThread {
            title: "subagents".into(),
            cwd: "/repo".into(),
            current_provider: ProviderId::Claude,
            current_model: None,
        });
        store
            .add_subagent(
                &thread.id,
                1,
                "claude",
                Some("toolu_1"),
                "Explore",
                Some("Find the parser"),
                "done",
                "2026-01-01T00:00:00Z",
                Some("2026-01-01T00:01:00Z"),
            )
            .unwrap();
        store
            .add_subagent(
                &thread.id,
                2,
                "codex",
                None,
                "review",
                None,
                "unknown",
                "2026-01-01T00:02:00Z",
                None,
            )
            .unwrap();
        let rows = store.list_subagents(&thread.id).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Explore");
        assert_eq!(rows[0].call_id.as_deref(), Some("toolu_1"));
        assert_eq!(rows[0].status, "done");
        assert_eq!(rows[0].detail.as_deref(), Some("Find the parser"));
        assert_eq!(rows[1].provider, "codex");
        assert_eq!(rows[1].status, "unknown");
        assert!(rows[1].finished_at.is_none());
    }
}
