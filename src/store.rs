use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use uuid::Uuid;

use crate::types::{PermissionMode, ProviderId, TurnPhase, TurnUsage};

#[derive(Debug, Clone)]
pub struct ThreadRow {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub current_provider: ProviderId,
    pub current_model: Option<String>,
    pub permission_mode: PermissionMode,
    pub goal: Option<String>,
    pub goal_paused: bool,
    pub created_at: String,
    pub updated_at: String,
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
pub struct ProviderSessionRow {
    pub thread_id: String,
    pub provider: ProviderId,
    pub native_session_id: String,
    pub model: Option<String>,
    pub rollout_path: Option<String>,
    pub updated_at: String,
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
pub struct CheckpointRow {
    pub thread_id: String,
    pub turn: i64,
    pub git_ref: String,
    pub created_at: String,
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
  permission_mode TEXT NOT NULL DEFAULT 'default',
  goal TEXT,
  goal_paused INTEGER NOT NULL DEFAULT 0,
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
"#;

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
    ensure_column(db, "threads", "goal", "TEXT")?;
    ensure_column(db, "threads", "goal_paused", "INTEGER NOT NULL DEFAULT 0")?;
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
                  created_at TEXT NOT NULL,
                  updated_at TEXT NOT NULL
                );
                INSERT INTO threads_migrated (id, title, cwd, current_provider, current_model, permission_mode, goal, goal_paused, created_at, updated_at)
                  SELECT id, title, cwd, current_provider,
                    CASE WHEN current_model IN ('gpt-6.1-sol', 'claude-sonnet-4-5') THEN NULL ELSE current_model END,
                    'default', NULL, 0, created_at, updated_at
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
    Ok(ThreadRow {
        id: row.get(0)?,
        title: row.get(1)?,
        cwd: row.get(2)?,
        current_provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Claude),
        current_model: row.get(4)?,
        permission_mode: PermissionMode::parse(&permission_mode).unwrap_or(PermissionMode::Default),
        goal: row.get(8)?,
        goal_paused: row.get::<_, i64>(9)? != 0,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

const THREAD_COLUMNS: &str = "id, title, cwd, current_provider, current_model, created_at, updated_at, permission_mode, goal, goal_paused";

/// Per-project prompt history cap, Claude Code style.
pub const PROMPT_HISTORY_LIMIT: i64 = 100;

impl BatonStore {
    pub fn open(path: &Path) -> Result<BatonStore> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(path)?;
        db.execute_batch(MIGRATIONS)?;
        migrate_thread_model_nullable(&db)?;
        ensure_thread_columns(&db)?;
        Ok(BatonStore { db })
    }

    pub fn memory() -> Result<BatonStore> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(MIGRATIONS)?;
        Ok(BatonStore { db })
    }

    pub fn create_thread(&self, input: NewThread) -> Result<ThreadRow> {
        self.create_thread_with_id(input, Uuid::new_v4().to_string())
    }

    pub fn create_thread_with_id(&self, input: NewThread, id: String) -> Result<ThreadRow> {
        let now = now_iso();
        self.db.execute(
            "INSERT INTO threads (id, title, cwd, current_provider, current_model, permission_mode, goal, goal_paused, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'default', NULL, 0, ?, ?)",
            params![id, input.title, input.cwd, input.current_provider.as_str(), input.current_model, now, now],
        )?;
        Ok(ThreadRow {
            id,
            title: input.title,
            cwd: input.cwd,
            current_provider: input.current_provider,
            current_model: input.current_model,
            permission_mode: PermissionMode::Default,
            goal: None,
            goal_paused: false,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn draft_thread(&self, input: NewThread) -> ThreadRow {
        let now = now_iso();
        ThreadRow {
            id: Uuid::new_v4().to_string(),
            title: input.title,
            cwd: input.cwd,
            current_provider: input.current_provider,
            current_model: input.current_model,
            permission_mode: PermissionMode::Default,
            goal: None,
            goal_paused: false,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Lists threads for a directory, accepting both the caller's path and its
    /// canonical form. A symlinked invocation path must not hide threads that
    /// were created through the real project path, or vice versa.
    pub fn list_threads_for_path(&self, cwd: &Path) -> Result<Vec<ThreadRow>> {
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
        for candidate in candidates {
            let rows = self.list_threads(Some(candidate.to_string_lossy().as_ref()))?;
            if !rows.is_empty() {
                return Ok(rows);
            }
        }
        Ok(Vec::new())
    }

    pub fn list_threads(&self, cwd: Option<&str>) -> Result<Vec<ThreadRow>> {
        let mut statement = match cwd {
            Some(_cwd) => self.db.prepare(&format!(
                "SELECT {THREAD_COLUMNS} FROM threads WHERE cwd = ? ORDER BY updated_at DESC, rowid DESC"
            ))?,
            None => self.db.prepare(&format!(
                "SELECT {THREAD_COLUMNS} FROM threads ORDER BY updated_at DESC, rowid DESC"
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
        provider: ProviderId,
        native_session_id: &str,
        model: Option<String>,
        rollout_path: Option<String>,
    ) -> Result<()> {
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

    pub fn get_provider_session(&self, thread_id: &str, provider: ProviderId) -> Result<Option<ProviderSessionRow>> {
        self.db
            .query_row(
                "SELECT thread_id, provider, native_session_id, model, rollout_path, updated_at FROM provider_sessions WHERE thread_id = ? AND provider = ?",
                params![thread_id, provider.as_str()],
                |row| {
                    let provider: String = row.get(1)?;
                    Ok(ProviderSessionRow {
                        thread_id: row.get(0)?,
                        provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Claude),
                        native_session_id: row.get(2)?,
                        model: row.get(3)?,
                        rollout_path: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_provider_sessions(&self, thread_id: &str) -> Result<Vec<ProviderSessionRow>> {
        let mut statement = self.db.prepare(
            "SELECT thread_id, provider, native_session_id, model, rollout_path, updated_at FROM provider_sessions WHERE thread_id = ? ORDER BY provider",
        )?;
        let rows = statement
            .query_map(params![thread_id], |row| {
                let provider: String = row.get(1)?;
                Ok(ProviderSessionRow {
                    thread_id: row.get(0)?,
                    provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Claude),
                    native_session_id: row.get(2)?,
                    model: row.get(3)?,
                    rollout_path: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_thread_by_native_session(&self, provider: ProviderId, native_session_id: &str) -> Result<Option<ThreadRow>> {
        self.db
            .query_row(
                "SELECT t.id, t.title, t.cwd, t.current_provider, t.current_model, t.created_at, t.updated_at, t.permission_mode, t.goal, t.goal_paused FROM threads t JOIN provider_sessions s ON s.thread_id = t.id WHERE s.provider = ? AND s.native_session_id = ?",
                params![provider.as_str(), native_session_id],
                |row| {
                    let provider: String = row.get(3)?;
                    let permission_mode: String = row.get(7)?;
                    Ok(ThreadRow {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        cwd: row.get(2)?,
                        current_provider: ProviderId::parse(&provider).unwrap_or(ProviderId::Claude),
                        current_model: row.get(4)?,
                        permission_mode: PermissionMode::parse(&permission_mode)
                            .unwrap_or(PermissionMode::Default),
                        goal: row.get(8)?,
                        goal_paused: row.get::<_, i64>(9)? != 0,
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

    pub fn rewind_thread(&self, thread_id: &str, from_turn: i64) -> Result<()> {
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
        tx.execute("DELETE FROM provider_sessions WHERE thread_id = ?", params![thread_id])?;
        tx.execute(
            "UPDATE threads SET updated_at = ? WHERE id = ?",
            params![now_iso(), thread_id],
        )?;
        tx.commit()?;
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

