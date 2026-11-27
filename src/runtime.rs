use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::adapters::{BatonAdapter, ModelListInput, SteerHandle, SteerOutcome, SteerRequest, TurnEvent, TurnStartInput};
use crate::checkpoints::capture_checkpoint;
use crate::config::{provider_auth_env, resolve_config};
use crate::handoff::{derive_title, git_status_summary, HandoffMessage};
use crate::store::{BatonStore, NewThread, SubmitRefused, ThreadRow};
use crate::types::{PermissionMode, ProviderId, RateLimitInfo, TurnPhase, TurnUsage};

pub type SharedStore = Arc<Mutex<BatonStore>>;
pub type SharedAdapter = Arc<tokio::sync::Mutex<Box<dyn BatonAdapter>>>;
pub type ThreadTurnEventSender = std::sync::mpsc::Sender<(String, TurnEvent)>;

pub enum EngineCommand {
    Submit {
        thread_id: String,
        text: String,
        permission_mode: PermissionMode,
        skip_handoff: bool,
        expected_provider: Option<ProviderId>,
        retry: bool,
        drain_queued: bool,
    },
    Steer {
        thread_id: String,
        text: String,
    },
    ListModels {
        thread_id: String,
        provider: ProviderId,
        request_id: u64,
    },
    ReadRateLimits {
        cwd: String,
        provider: ProviderId,
        request_id: u64,
    },
    Interrupt {
        thread_id: String,
    },
    Quit,
}

pub enum EngineEvent {
    UserEcho { thread_id: String, turn: i64, text: String, provider: ProviderId, model: Option<String> },
    TurnQueued { thread_id: String, provider: ProviderId },
    TurnStarted { thread_id: String },
    Turn(TurnEvent),
    SteerRejected { thread_id: String, text: String, reason: String },
    ModelsLoaded {
        thread_id: String,
        provider: ProviderId,
        request_id: u64,
        models: Vec<String>,
        live: usize,
        cached: usize,
        error: Option<String>,
    },
    RateLimitsLoaded {
        provider: ProviderId,
        request_id: u64,
        info: Option<RateLimitInfo>,
        error: Option<String>,
    },
    TurnRejected {
        thread_id: String,
        text: String,
        reason: String,
        retry: bool,
    },
    TurnFinished { thread_id: String, ok: bool, error: Option<String>, phase: TurnPhase },
}

struct TurnOutcome {
    final_text: String,
    usage: Option<TurnUsage>,
    native_session_id: Option<String>,
    rollout_path: Option<String>,
}

/// Durable state for one turn. Early returns, panics in the provider task, or
/// a closed stream still leave a terminal phase instead of a silent gap.
struct TurnLedger {
    store: SharedStore,
    thread_id: String,
    turn: i64,
    settled: bool,
}

impl TurnLedger {
    fn submitted(store: &SharedStore, thread_id: &str, turn: i64) -> TurnLedger {
        TurnLedger {
            store: store.clone(),
            thread_id: thread_id.to_string(),
            turn,
            settled: false,
        }
    }

    fn settle(&mut self, phase: TurnPhase, error: Option<&str>) {
        if self.settled {
            return;
        }
        if let Ok(store) = self.store.lock() {
            let _ = store.finish_turn_record(&self.thread_id, self.turn, phase, error);
        }
        self.settled = true;
    }

    fn set_phase(&mut self, phase: TurnPhase) {
        if self.settled {
            return;
        }
        if let Ok(store) = self.store.lock() {
            let _ = store.set_turn_phase(&self.thread_id, self.turn, phase);
        }
    }
}

impl Drop for TurnLedger {
    fn drop(&mut self) {
        if !self.settled {
            // A dropped ledger means the task ended without a provider
            // verdict. Do not claim the provider failed.
            self.settle(
                TurnPhase::Unverified,
                Some("turn ended without a terminal state"),
            );
        }
    }
}

/// Owns the engine's in-memory claim on a thread for the whole spawned task.
/// `run_turn` normally calls `finish`, but a panic or a dropped future still
/// releases the thread, its cancellation slot, and tells the UI the turn is
/// unverified. Without this guard a panicking adapter leaves `active_threads`
/// permanently set and every later submit is rejected.
struct TurnRunGuard {
    store: SharedStore,
    active_threads: Arc<Mutex<HashSet<String>>>,
    turn_cancellations: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
    engine_event_sender: std::sync::mpsc::Sender<EngineEvent>,
    thread_id: String,
    expected_turn: i64,
    finished: bool,
}

impl TurnRunGuard {
    fn new(
        store: SharedStore,
        active_threads: Arc<Mutex<HashSet<String>>>,
        turn_cancellations: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
        engine_event_sender: std::sync::mpsc::Sender<EngineEvent>,
        thread_id: String,
        expected_turn: i64,
    ) -> Self {
        Self {
            store,
            active_threads,
            turn_cancellations,
            engine_event_sender,
            thread_id,
            expected_turn,
            finished: false,
        }
    }

    fn finish(&mut self, phase: TurnPhase, error: Option<String>) {
        self.release_claim();
        self.finished = true;
        let ok = matches!(phase, TurnPhase::Completed);
        let _ = self.engine_event_sender.send(EngineEvent::TurnFinished {
            thread_id: self.thread_id.clone(),
            ok,
            error,
            phase,
        });
    }

    fn abandon(&mut self) {
        self.release_claim();
        self.finished = true;
    }

    fn release_claim(&self) {
        self.turn_cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.thread_id);
        self.active_threads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.thread_id);
    }
}

impl Drop for TurnRunGuard {
    fn drop(&mut self) {
        if self.finished {
            // `finish` already released the claim. A new turn for this thread
            // may have inserted its own cancellation sender since then; do not
            // remove that newer claim.
            return;
        }
        self.release_claim();
        let record = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_turn_record(&self.thread_id, self.expected_turn)
            .ok()
            .flatten();
        let Some(record) = record.filter(record_is_owned_here) else {
            return;
        };
        let (phase, error) = (record.phase, record.error);
        let ok = matches!(phase, TurnPhase::Completed);
        let _ = self.engine_event_sender.send(EngineEvent::TurnFinished {
            thread_id: self.thread_id.clone(),
            ok,
            error,
            phase,
        });
    }
}

fn record_is_owned_here(record: &crate::store::TurnRow) -> bool {
    let pid = std::process::id();
    record.owner_pid == Some(pid) && record.owner_start == crate::process::process_start(pid)
}

const QUIT_DISPOSE_TIMEOUT: Duration = Duration::from_secs(1);

fn phase_for_error(error: &str) -> TurnPhase {
    if error.to_ascii_lowercase().contains("interrupt") {
        TurnPhase::Interrupted
    } else {
        TurnPhase::Failed
    }
}

/// The exact turn a provider adapter is running. Steering is validated
/// against this claim, not just against the thread, so a steer cannot land in
/// a later turn for the same thread.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProviderClaim {
    thread_id: String,
    turn: i64,
}

type ProviderClaims = Arc<Mutex<HashMap<ProviderId, ProviderClaim>>>;

/// Marks the turn that currently owns a provider adapter. This keeps steering
/// and interrupts from reaching another thread's live turn.
struct ProviderOwnerGuard {
    claims: ProviderClaims,
    provider: ProviderId,
    claim: ProviderClaim,
}

impl ProviderOwnerGuard {
    fn claim(claims: ProviderClaims, provider: ProviderId, thread_id: &str, turn: i64) -> Self {
        let claim = ProviderClaim {
            thread_id: thread_id.to_string(),
            turn,
        };
        claims.lock().unwrap().insert(provider, claim.clone());
        Self {
            claims,
            provider,
            claim,
        }
    }
}

impl Drop for ProviderOwnerGuard {
    fn drop(&mut self) {
        if let Ok(mut claims) = self.claims.lock() {
            if claims.get(&self.provider) == Some(&self.claim) {
                claims.remove(&self.provider);
            }
        }
    }
}

/// One tool invocation of a turn, kept for the conversation history.
struct ToolRecord {
    id: Option<String>,
    label: String,
    /// None while running, Some(false) when it failed.
    ok: Option<bool>,
}

/// Pairs ToolStarted/ToolCompleted events the same way the live TUI list does:
/// a completion settles the most recent still-running record with that label.
fn record_tool_event(records: &mut Vec<ToolRecord>, event: &TurnEvent) {
    match event {
        TurnEvent::ToolStarted { id, tool, detail } => {
            let label = match detail {
                Some(detail) => format!("{tool} {detail}"),
                None => tool.clone(),
            };
            if let Some(id) = id.as_deref() {
                if let Some(record) = records
                    .iter_mut()
                    .rev()
                    .find(|record| record.ok.is_none() && record.id.as_deref() == Some(id))
                {
                    record.label = label;
                    return;
                }
            }
            records.push(ToolRecord { id: id.clone(), label, ok: None });
        }
        TurnEvent::ToolCompleted { id, tool, detail, exit_code } => {
            let label = match detail {
                Some(detail) => format!("{tool} {detail}"),
                None => tool.clone(),
            };
            let failed = exit_code.map(|code| code != 0).unwrap_or(false);
            // Prefer the provider's call id. Two identical commands running at
            // once must not settle each other's row.
            let matched = match id.as_deref() {
                Some(id) => records
                    .iter_mut()
                    .rev()
                    .find(|record| record.ok.is_none() && record.id.as_deref() == Some(id)),
                None => records
                    .iter_mut()
                    .rev()
                    .find(|record| record.ok.is_none() && record.label == label),
            };
            match matched {
                Some(record) => record.ok = Some(!failed),
                None => records.push(ToolRecord { id: id.clone(), label, ok: Some(!failed) }),
            }
        }
        _ => {}
    }
}

struct SubagentRecord {
    call_id: Option<String>,
    name: String,
    detail: Option<String>,
    status: String,
    started_at: String,
    finished_at: Option<String>,
}

fn subagent_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn record_subagent_event(records: &mut Vec<SubagentRecord>, event: &TurnEvent) {
    match event {
        TurnEvent::SubagentStarted { id, name, detail } => {
            if let Some(id) = id.as_deref() {
                if let Some(record) = records.iter_mut().rev().find(|record| {
                    record.status == "running" && record.call_id.as_deref() == Some(id)
                }) {
                    record.name = name.clone();
                    record.detail = detail.clone();
                    return;
                }
            }
            records.push(SubagentRecord {
                call_id: id.clone(),
                name: name.clone(),
                detail: detail.clone(),
                status: "running".to_string(),
                started_at: subagent_stamp(),
                finished_at: None,
            });
        }
        TurnEvent::SubagentFinished {
            id,
            name,
            status,
            detail,
        } => {
            let matched = match id.as_deref() {
                Some(id) => records.iter_mut().rev().find(|record| {
                    record.status == "running" && record.call_id.as_deref() == Some(id)
                }),
                None => records
                    .iter_mut()
                    .rev()
                    .find(|record| record.status == "running" && record.name == *name),
            };
            match matched {
                Some(record) => {
                    record.status = status.clone();
                    record.finished_at = Some(subagent_stamp());
                    if detail.is_some() {
                        record.detail = detail.clone();
                    }
                }
                None => {
                    let now = subagent_stamp();
                    records.push(SubagentRecord {
                        call_id: id.clone(),
                        name: name.clone(),
                        detail: detail.clone(),
                        status: status.clone(),
                        started_at: now.clone(),
                        finished_at: Some(now),
                    });
                }
            }
        }
        _ => {}
    }
}

fn persist_subagent_records(
    store: &BatonStore,
    thread_id: &str,
    turn: i64,
    provider: ProviderId,
    records: &[SubagentRecord],
) -> Result<()> {
    for record in records {
        // A turn that ends with a child still running leaves an honest
        // `unknown` row instead of a stale `running` that never updates.
        let status = if record.status == "running" {
            "unknown"
        } else {
            record.status.as_str()
        };
        store.add_subagent(
            thread_id,
            turn,
            provider.as_str(),
            record.call_id.as_deref(),
            &record.name,
            record.detail.as_deref(),
            status,
            &record.started_at,
            record.finished_at.as_deref(),
        )?;
    }
    Ok(())
}

fn persist_tool_records(
    store: &BatonStore,
    thread_id: &str,
    turn: i64,
    provider: ProviderId,
    records: &[ToolRecord],
) -> Result<()> {
    for record in records {
        let marker = match record.ok {
            Some(true) => "✓",
            Some(false) => "✗",
            None => "·",
        };
        store.add_message(thread_id, turn, "tool", Some(provider.as_str()), None, &format!("{marker} {}", record.label))?;
    }
    Ok(())
}

/// Builds one adapter per provider for a working directory. The spawners
/// capture the directory because each provider CLI starts there; a host that
/// serves one project reuses this set for every turn.
pub fn build_adapters(
    store: &SharedStore,
    home: &std::path::Path,
    cwd: &std::path::Path,
) -> Vec<SharedAdapter> {
    let process_recorder =
        crate::process::ProcessRecorder::new(store.clone(), cwd.to_string_lossy().to_string());
    let claude_recorder = process_recorder.clone();
    let grok_recorder = process_recorder.clone();
    let cwd_for_grok = cwd.to_path_buf();
    let grok_spawner = Arc::new(
        move |auth_env: &[(String, String)]| -> anyhow::Result<crate::adapters::TransportHalves> {
            let mut command = tokio::process::Command::new("grok");
            command.args(["agent", "stdio"]);
            command.current_dir(&cwd_for_grok);
            command.env("GROK_OAUTH2_REFERRER", "baton");
            command.stdin(std::process::Stdio::piped());
            command.stdout(std::process::Stdio::piped());
            command.stderr(std::process::Stdio::null());
            for (key, value) in auth_env {
                command.env(key, value);
            }
            crate::provider_runtime::spawn_tracked_provider(
                command,
                ProviderId::Grok,
                &grok_recorder,
            )
        },
    );
    let antigravity_recorder = process_recorder.clone();
    let cwd_for_antigravity = cwd.to_path_buf();
    let antigravity_home = home.join("antigravity");
    let antigravity_gemini = antigravity_home.join("gemini");
    let antigravity_spawner = Arc::new(
        move |auth_env: &[(String, String)]| -> anyhow::Result<crate::adapters::TransportHalves> {
            std::fs::create_dir_all(&antigravity_gemini)?;
            // The agy CLI serves ACP itself. The explicit gemini dir and app
            // data dir keep baton's profile isolated from the user's IDE.
            let mut command = tokio::process::Command::new("agy");
            command.arg("acp");
            command.arg("--gemini-dir").arg(&antigravity_gemini);
            command.arg("--app-data-dir").arg("antigravity-acp");
            command.current_dir(&cwd_for_antigravity);
            command.env("AGY_ACP_FORCE_FILE_STORAGE", "1");
            command.env("GEMINI_HOME", &antigravity_gemini);
            if !auth_env
                .iter()
                .any(|(key, _)| key == "GEMINI_API_KEY" || key == "GOOGLE_API_KEY")
            {
                command.env_remove("GEMINI_API_KEY");
                command.env_remove("GOOGLE_API_KEY");
            }
            command.stdin(std::process::Stdio::piped());
            command.stdout(std::process::Stdio::piped());
            command.stderr(std::process::Stdio::null());
            for (key, value) in auth_env {
                command.env(key, value);
            }
            crate::provider_runtime::spawn_tracked_provider(
                command,
                ProviderId::Antigravity,
                &antigravity_recorder,
            )
        },
    );
    let cursor_recorder = process_recorder.clone();
    let cwd_for_cursor = cwd.to_path_buf();
    let cursor_spawner = Arc::new(
        move |auth_env: &[(String, String)]| -> anyhow::Result<crate::adapters::TransportHalves> {
            // Cursor's ACP authenticate opens a browser login. Fail fast with
            // a clear hint instead of hanging a headless turn.
            if auth_env.is_empty() {
                let status = std::process::Command::new("cursor-agent")
                    .arg("status")
                    .output()?;
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&status.stdout),
                    String::from_utf8_lossy(&status.stderr)
                );
                if !status.status.success() || text.to_ascii_lowercase().contains("not logged in") {
                    anyhow::bail!(
                        "cursor-agent is not logged in; run `cursor-agent login`, then retry"
                    );
                }
            }
            let mut command = tokio::process::Command::new("cursor-agent");
            command.arg("acp");
            command.current_dir(&cwd_for_cursor);
            command.stdin(std::process::Stdio::piped());
            command.stdout(std::process::Stdio::piped());
            command.stderr(std::process::Stdio::null());
            for (key, value) in auth_env {
                command.env(key, value);
            }
            crate::provider_runtime::spawn_tracked_provider(
                command,
                ProviderId::Cursor,
                &cursor_recorder,
            )
        },
    );
    vec![
        Arc::new(tokio::sync::Mutex::new(
            Box::new(crate::adapters::claude::ClaudeAdapter::new(
                crate::adapters::claude::ClaudeAdapter::real_spawner_with_recorder(claude_recorder),
            )) as Box<dyn crate::adapters::BatonAdapter>,
        )),
        crate::provider_runtime::build_codex_adapter(store, cwd),
        Arc::new(tokio::sync::Mutex::new(
            Box::new(crate::adapters::acp::AcpAdapter::new(
                ProviderId::Cursor,
                cursor_spawner,
            )) as Box<dyn crate::adapters::BatonAdapter>,
        )),
        Arc::new(tokio::sync::Mutex::new(
            Box::new(crate::adapters::acp::AcpAdapter::new(
                ProviderId::Grok,
                grok_spawner,
            )) as Box<dyn crate::adapters::BatonAdapter>,
        )),
        crate::provider_runtime::build_opencode_adapter(store, cwd),
        Arc::new(tokio::sync::Mutex::new(
            Box::new(crate::adapters::acp::AcpAdapter::new(
                ProviderId::Antigravity,
                antigravity_spawner,
            )) as Box<dyn crate::adapters::BatonAdapter>,
        )),
    ]
}

pub async fn run_engine(
    store: SharedStore,
    home: std::path::PathBuf,
    adapters: Vec<SharedAdapter>,
    interrupt_handles: Vec<(ProviderId, Arc<dyn Fn() + Send + Sync>)>,
    steer_handles: Vec<(ProviderId, SteerHandle)>,
    mut command_receiver: tokio::sync::mpsc::UnboundedReceiver<EngineCommand>,
    engine_event_sender: std::sync::mpsc::Sender<EngineEvent>,
    turn_event_sender: ThreadTurnEventSender,
) -> Result<()> {
    let active_threads: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let adapter_indices: Arc<HashMap<ProviderId, usize>> = Arc::new({
        let mut indices = HashMap::new();
        for (index, adapter) in adapters.iter().enumerate() {
            if let Ok(adapter) = adapter.try_lock() {
                indices.insert(adapter.id(), index);
            }
        }
        indices
    });
    let provider_claims: ProviderClaims = Arc::new(Mutex::new(HashMap::new()));
    let turn_cancellations: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    while let Some(command) = command_receiver.recv().await {
        match command {
            EngineCommand::Quit => {
                // Wake queued turns, ask the provider owners to stop, then
                // dispose without letting one stuck adapter hold the process
                // open forever.
                let cancellations = turn_cancellations
                    .lock()
                    .unwrap()
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                for cancel in cancellations {
                    let _ = cancel.send(true);
                }
                let claims = provider_claims.lock().unwrap().clone();
                for provider in claims.keys().copied() {
                    if let Some((_, handle)) = interrupt_handles
                        .iter()
                        .find(|(candidate, _)| *candidate == provider)
                    {
                        handle();
                    }
                }
                for adapter in &adapters {
                    if let Ok(mut adapter) =
                        tokio::time::timeout(QUIT_DISPOSE_TIMEOUT, adapter.lock()).await
                    {
                        let _ = adapter.dispose().await;
                    }
                }
                break;
            }
            EngineCommand::Steer { thread_id, text } => {
                let resolved = {
                    let store = store.lock().unwrap();
                    store.get_thread(&thread_id).ok().flatten().map(|thread| {
                        let model = thread.current_model.clone().unwrap_or_else(|| {
                            crate::models::default_model(thread.current_provider).to_string()
                        });
                        (thread.current_provider, model, thread.cwd)
                    })
                };
                let claim = resolved.as_ref().and_then(|(provider, _, _)| {
                    provider_claims.lock().unwrap().get(provider).cloned()
                });
                let handle = resolved.as_ref().and_then(|(provider, _, _)| {
                    steer_handles
                        .iter()
                        .find(|(candidate, _)| candidate == provider)
                        .map(|(_, handle)| handle.clone())
                });
                let Some((provider, model, thread_cwd)) = resolved else {
                    let _ = engine_event_sender.send(EngineEvent::SteerRejected {
                        thread_id,
                        text,
                        reason: "thread not found".into(),
                    });
                    continue;
                };
                let Some(claim) = claim else {
                    let _ = engine_event_sender.send(EngineEvent::SteerRejected {
                        thread_id,
                        text,
                        reason: "no live turn for this harness".into(),
                    });
                    continue;
                };
                let Some(handle) = handle else {
                    let _ = engine_event_sender.send(EngineEvent::SteerRejected {
                        thread_id,
                        text,
                        reason: "this harness does not support steering".into(),
                    });
                    continue;
                };
                if claim.thread_id != thread_id {
                    let _ = engine_event_sender.send(EngineEvent::SteerRejected {
                        thread_id,
                        text,
                        reason: "another thread owns this harness".into(),
                    });
                    continue;
                }
                // Delivery is asynchronous and turn-scoped. The echo and the
                // stored message wait for the harness verdict so the
                // transcript never claims a steer that was not delivered.
                let turn = claim.turn;
                let sender = engine_event_sender.clone();
                let store = store.clone();
                let steer_text = if skill_dispatch_enabled() {
                    crate::skill_dispatch::prompt_for_provider_from_in(
                        provider,
                        &text,
                        None,
                        std::path::Path::new(&thread_cwd),
                    )
                } else {
                    text.clone()
                };
                tokio::spawn(async move {
                    let outcome = handle(SteerRequest {
                        thread_id: thread_id.clone(),
                        turn,
                        text: steer_text,
                    })
                    .await;
                    if outcome == SteerOutcome::Delivered {
                        {
                            let store = store.lock().unwrap();
                            let _ = store.add_message(
                                &thread_id,
                                turn,
                                "user",
                                Some(provider.as_str()),
                                Some(&model),
                                &text,
                            );
                        }
                        let _ = sender.send(EngineEvent::UserEcho {
                            thread_id,
                            turn,
                            text,
                            provider,
                            model: Some(model),
                        });
                    } else {
                        let reason = match outcome {
                            SteerOutcome::StaleTurn => {
                                "the turn ended before the harness took the message".to_string()
                            }
                            SteerOutcome::Unavailable(reason) => reason,
                            SteerOutcome::Delivered => unreachable!(),
                        };
                        let _ = sender.send(EngineEvent::SteerRejected {
                            thread_id,
                            text,
                            reason,
                        });
                    }
                });
            }
            EngineCommand::ListModels {
                thread_id,
                provider,
                request_id,
            } => {
                let store = store.clone();
                let home = home.clone();
                let adapters = adapters.clone();
                let sender = engine_event_sender.clone();
                tokio::spawn(async move {
                    let result = load_provider_models(&store, &home, &adapters, &thread_id, provider).await;
                    let (models, live, cached, error) = match result {
                        Ok(catalog) => (catalog.models, catalog.live, catalog.cached, catalog.error),
                        Err(error) => (Vec::new(), 0, 0, Some(error.to_string())),
                    };
                    let _ = sender.send(EngineEvent::ModelsLoaded {
                        thread_id,
                        provider,
                        request_id,
                        models,
                        live,
                        cached,
                        error,
                    });
                });
            }
            EngineCommand::ReadRateLimits {
                cwd,
                provider,
                request_id,
            } => {
                let home = home.clone();
                let adapters = adapters.clone();
                let adapter_indices = adapter_indices.clone();
                let sender = engine_event_sender.clone();
                tokio::spawn(async move {
                    let result = load_provider_rate_limits(
                        &home,
                        &adapters,
                        &adapter_indices,
                        &cwd,
                        provider,
                    )
                    .await;
                    let (info, error) = match result {
                        Ok(info) => (info, None),
                        Err(error) => (None, Some(error.to_string())),
                    };
                    let _ = sender.send(EngineEvent::RateLimitsLoaded {
                        provider,
                        request_id,
                        info,
                        error,
                    });
                });
            }
            EngineCommand::Interrupt { thread_id } => {
                let provider = store
                    .lock()
                    .unwrap()
                    .get_thread(&thread_id)
                    .ok()
                    .flatten()
                    .map(|thread| thread.current_provider);
                if let Some(provider) = provider {
                    let owner = provider_claims
                        .lock()
                        .unwrap()
                        .get(&provider)
                        .map(|claim| claim.thread_id.clone());
                    let cancel = turn_cancellations
                        .lock()
                        .unwrap()
                        .get(&thread_id)
                        .cloned();
                    if let Some(cancel) = cancel {
                        let _ = cancel.send(true);
                    }
                    if owner.as_deref() == Some(thread_id.as_str()) {
                        if let Some((_, handle)) = interrupt_handles
                            .iter()
                            .find(|(candidate, _)| *candidate == provider)
                        {
                            handle();
                        }
                    }
                }
            }
            EngineCommand::Submit {
                thread_id,
                text,
                permission_mode,
                skip_handoff,
                expected_provider,
                retry,
                drain_queued,
            } => {
                if let Some(expected_provider) = expected_provider {
                    let current = store
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get_thread(&thread_id);
                    match current {
                        Ok(Some(thread)) if thread.current_provider == expected_provider => {}
                        Ok(Some(thread)) => {
                            let _ = engine_event_sender.send(EngineEvent::TurnRejected {
                                thread_id,
                                text,
                                reason: format!(
                                    "the thread harness changed to {} while retry was confirming",
                                    thread.current_provider
                                ),
                                retry,
                            });
                            continue;
                        }
                        Ok(None) => {
                            let _ = engine_event_sender.send(EngineEvent::TurnRejected {
                                thread_id,
                                text,
                                reason: "the thread no longer exists while retry was confirming"
                                    .into(),
                                retry,
                            });
                            continue;
                        }
                        Err(error) => {
                            let _ = engine_event_sender.send(EngineEvent::TurnRejected {
                                thread_id,
                                text,
                                reason: format!(
                                    "cannot read the thread for retry admission: {error}"
                                ),
                                retry,
                            });
                            continue;
                        }
                    }
                }
                let accepted = active_threads.lock().unwrap().insert(thread_id.clone());
                if !accepted {
                    let _ = engine_event_sender.send(EngineEvent::TurnRejected {
                        thread_id,
                        text,
                        reason: "a turn is already running for this thread".into(),
                        retry,
                    });
                    continue;
                }
                let (cancel_sender, cancel_receiver) = tokio::sync::watch::channel(false);
                turn_cancellations
                    .lock()
                    .unwrap()
                    .insert(thread_id.clone(), cancel_sender);
                tokio::spawn(run_turn(
                    store.clone(),
                    home.clone(),
                    adapters.clone(),
                    adapter_indices.clone(),
                    provider_claims.clone(),
                    turn_cancellations.clone(),
                    cancel_receiver,
                    thread_id,
                    text,
                    permission_mode,
                    skip_handoff,
                    expected_provider,
                    retry,
                    drain_queued,
                    engine_event_sender.clone(),
                    turn_event_sender.clone(),
                    active_threads.clone(),
                ));
            }
        }
    }
    Ok(())
}

async fn run_turn(
    store: SharedStore,
    home: std::path::PathBuf,
    adapters: Vec<SharedAdapter>,
    adapter_indices: Arc<HashMap<ProviderId, usize>>,
    provider_claims: ProviderClaims,
    turn_cancellations: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
    cancel_receiver: tokio::sync::watch::Receiver<bool>,
    thread_id: String,
    text: String,
    permission_mode: PermissionMode,
    skip_handoff: bool,
    expected_provider: Option<ProviderId>,
    retry: bool,
    drain_queued: bool,
    engine_event_sender: std::sync::mpsc::Sender<EngineEvent>,
    turn_event_sender: ThreadTurnEventSender,
    active_threads: Arc<Mutex<HashSet<String>>>,
) {
    let expected_turn = store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .next_turn_number(&thread_id)
        .unwrap_or(0);
    let mut guard = TurnRunGuard::new(
        store.clone(),
        active_threads.clone(),
        turn_cancellations.clone(),
        engine_event_sender.clone(),
        thread_id.clone(),
        expected_turn,
    );
    let result = execute_turn(
        &store,
        &home,
        &adapters,
        &adapter_indices,
        &provider_claims,
        cancel_receiver,
        &thread_id,
        &text,
        permission_mode,
        skip_handoff,
        expected_provider,
        drain_queued,
        &engine_event_sender,
        &turn_event_sender,
    )
    .await;
    if let Err(error) = &result {
        if let Some(refused) = error.downcast_ref::<SubmitRefused>() {
            guard.abandon();
            let _ = engine_event_sender.send(EngineEvent::TurnRejected {
                thread_id,
                text,
                reason: refused.to_string(),
                retry,
            });
            return;
        }
    }
    let record = if expected_turn > 0 {
        store
            .lock()
            .unwrap()
            .get_turn_record(&thread_id, expected_turn)
            .unwrap_or(None)
    } else {
        None
    };
    let Some(record) = record.filter(record_is_owned_here) else {
        guard.abandon();
        return;
    };
    let phase = record.phase;
    let error = record
        .error
        .clone()
        .or_else(|| result.as_ref().err().map(ToString::to_string));
    guard.finish(phase, error);
}

/// Asks one provider for its live model catalog without blocking the engine
/// command loop. A busy adapter is reported as busy, not waited on: a picker
/// should keep its static fallback rather than stall behind a turn.
async fn load_provider_models(
    store: &SharedStore,
    home: &std::path::PathBuf,
    adapters: &[SharedAdapter],
    thread_id: &str,
    provider: ProviderId,
) -> Result<crate::models::ModelCatalog> {
    let cwd = {
        let store = store.lock().unwrap();
        store
            .get_thread(thread_id)?
            .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))?
            .cwd
    };
    let config = resolve_config(home, std::path::Path::new(&cwd));
    let cached = crate::models::read_cached_catalog(home, provider);
    if provider == ProviderId::Claude {
        let models = crate::harness_config::read_claude_model_catalog(
            &crate::paths::claude_global_settings_dir(),
        );
        if !models.is_empty() {
            return Ok(finish_catalog(home, provider, models, cached));
        }
    }
    let input = ModelListInput {
        cwd,
        auth_env: provider_auth_env(config.for_provider(provider)),
    };
    for adapter in adapters {
        if let Ok(mut adapter) = adapter.try_lock() {
            if adapter.id() == provider {
                let models = tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    adapter.list_models(input),
                )
                .await;
                return match models {
                    Ok(Ok(models)) => Ok(finish_catalog(home, provider, models, cached)),
                    Ok(Err(error)) => Ok(cached_catalog(provider, cached, error.to_string())),
                    Err(_) => Ok(cached_catalog(
                        provider,
                        cached,
                        format!("{provider} model catalog timed out"),
                    )),
                };
            }
        }
    }
    Ok(cached_catalog(
        provider,
        cached,
        format!("{provider} is busy"),
    ))
}

/// Reads a rate-limit snapshot while the harness is idle. A busy adapter is
/// reported instead of waited on, so a running turn never stalls the report.
async fn load_provider_rate_limits(
    home: &std::path::Path,
    adapters: &[SharedAdapter],
    adapter_indices: &HashMap<ProviderId, usize>,
    cwd: &str,
    provider: ProviderId,
) -> Result<Option<RateLimitInfo>> {
    let config = resolve_config(home, std::path::Path::new(cwd));
    let auth_env = provider_auth_env(config.for_provider(provider));
    let index = adapter_indices
        .get(&provider)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("{provider} is not available"))?;
    let mut adapter = adapters[index]
        .try_lock()
        .map_err(|_| anyhow::anyhow!("{provider} is busy; try again when the harness is idle"))?;
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        adapter.read_rate_limits(&auth_env),
    )
    .await
    .map_err(|_| anyhow::anyhow!("{provider} rate-limit read timed out"))?
}

/// A shorter live list does not replace a fuller cached one, so a degraded
/// probe cannot shrink what the picker knows.
fn finish_catalog(
    home: &std::path::Path,
    provider: ProviderId,
    live: Vec<String>,
    cached: Vec<String>,
) -> crate::models::ModelCatalog {
    if !live.is_empty() && live.len() >= cached.len() {
        let _ = crate::models::write_cached_catalog(home, provider, &live);
    }
    let cached_only = cached
        .iter()
        .filter(|model| {
            !live
                .iter()
                .any(|live_model| live_model.trim() == model.trim())
        })
        .count();
    let models = crate::models::merge_catalogs(provider, live.clone(), &cached);
    crate::models::ModelCatalog {
        models,
        live: live.len(),
        cached: cached_only,
        error: None,
    }
}

fn cached_catalog(
    provider: ProviderId,
    cached: Vec<String>,
    error: String,
) -> crate::models::ModelCatalog {
    let models = crate::models::merge_catalogs(provider, Vec::new(), &cached);
    crate::models::ModelCatalog {
        models,
        live: 0,
        cached: cached.len(),
        error: Some(error),
    }
}

async fn execute_turn(
    store: &SharedStore,
    home: &std::path::PathBuf,
    adapters: &[SharedAdapter],
    adapter_indices: &HashMap<ProviderId, usize>,
    provider_claims: &ProviderClaims,
    mut cancel_receiver: tokio::sync::watch::Receiver<bool>,
    thread_id: &str,
    text: &str,
    permission_mode: PermissionMode,
    skip_handoff: bool,
    expected_provider: Option<ProviderId>,
    drain_queued: bool,
    engine_event_sender: &std::sync::mpsc::Sender<EngineEvent>,
    turn_event_sender: &ThreadTurnEventSender,
) -> Result<()> {
    let (provider, cwd, thread_title, thread_model, reasoning_effort) = {
        let store = store.lock().unwrap();
        let thread = store
            .get_thread(thread_id)?
            .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))?;
        if expected_provider.is_some_and(|expected| thread.current_provider != expected) {
            anyhow::bail!("the thread harness changed before the turn started");
        }
        (
            thread.current_provider,
            thread.cwd,
            thread.title,
            thread.current_model,
            thread.reasoning_effort,
        )
    };
    let adapter_index = adapter_indices
        .get(&provider)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("no adapter for {provider}"))?;

    let config = resolve_config(home, std::path::Path::new(&cwd));
    let provider_config = config.for_provider(provider);
    let model = thread_model.clone().unwrap_or_else(|| provider_config.model.clone());
    let memories = store
        .lock()
        .unwrap()
        .list_memories(&crate::memory::project_key(&cwd))?;
    let prompt_text = crate::memory::expand_memories(&memories, text);
    let auth_env = provider_auth_env(provider_config);

    let (prior_messages, turn, session) = {
        let store = store.lock().unwrap();
        let current = store
            .get_thread(thread_id)?
            .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))?;
        if expected_provider.is_some_and(|expected| current.current_provider != expected) {
            anyhow::bail!("the thread harness changed before the turn started");
        }
        let provider = current.current_provider;
        let prior = store.list_messages(thread_id)?;
        let session = store.get_provider_session(thread_id, provider)?;
        let audit = crate::audit::event(
            crate::audit::HOST_ENGINE_ACTOR,
            &crate::claim::installation_id(home).unwrap_or_else(|| "unclaimed".into()),
            Some(thread_id),
            Some(thread_id),
            crate::audit::AuditAction::TurnSubmit,
            &serde_json::json!({
                "thread_id": thread_id,
                "provider": provider.as_str(),
                "model": model,
                "drain_queued": drain_queued,
            }),
        )?;
        let turn = store
            .submit_turn_with_audit(
                thread_id,
                provider,
                Some(&model),
                text,
                drain_queued,
                &audit,
            )?
            .0;
        (prior, turn, session)
    };

    let mut turn_ledger = TurnLedger::submitted(store, thread_id, turn);

    let _ = engine_event_sender.send(EngineEvent::UserEcho {
        thread_id: thread_id.to_string(),
        turn,
        text: text.to_string(),
        provider,
        model: Some(model.clone()),
    });
    let _ = Instant::now();

    if thread_title == "New thread" {
        let title = derive_title(text);
        store.lock().unwrap().rename_thread(thread_id, &title)?;
    }

    let secrets = crate::redact::collect(
        config
            .providers
            .values()
            .filter_map(|provider| provider.api_key.as_deref()),
    );
    let mut prompt = prompt_text.clone();
    // A native session has already read the turns up to its marker. Sending it
    // again would spend cache and repeat context, so only unseen turns travel.
    let covered_turn = session.as_ref().map(|row| row.covered_turn).unwrap_or(0);
    let unseen: Vec<&crate::store::MessageRow> = prior_messages
        .iter()
        .filter(|message| message.turn > covered_turn)
        .collect();
    if !skip_handoff && !unseen.is_empty() {
        let handoff_messages: Vec<HandoffMessage> = unseen
            .iter()
            .map(|message| HandoffMessage {
                role: message.role.clone(),
                provider: message.provider.clone(),
                model: message.model.clone(),
                text: crate::redact::redact(&message.text, &secrets).into_owned(),
            })
            .collect();
        let git_status = git_status_summary(std::path::Path::new(&cwd), 50);
        let intro = if session.is_some() {
            crate::handoff::RESUME_INTRO
        } else {
            crate::handoff::NEW_HARNESS_INTRO
        };
        prompt = crate::handoff::handoff_prompt_with_intro(
            &cwd,
            &thread_title,
            &handoff_messages,
            git_status.as_deref(),
            &prompt_text,
            None,
            intro,
        );
    }
    if skill_dispatch_enabled() {
        prompt = crate::skill_dispatch::prompt_for_provider_from_in(
            provider,
            &prompt,
            Some(crate::skill_dispatch::current_message(&prompt_text)),
            std::path::Path::new(&cwd),
        );
    }

    let cwd_for_checkpoint = cwd.clone();
    let input = TurnStartInput {
        thread_id: thread_id.to_string(),
        turn,
        cwd,
        model: model.clone(),
        effort: reasoning_effort,
        prompt,
        permission_mode,
        auth_env,
    };

    let adapter = &adapters[adapter_index];
    let mut adapter_guard = match adapter.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            turn_ledger.set_phase(TurnPhase::Queued);
            let _ = engine_event_sender.send(EngineEvent::TurnQueued {
                thread_id: thread_id.to_string(),
                provider,
            });
            let guard = tokio::select! {
                guard = adapter.lock() => guard,
                _ = cancel_receiver.changed() => {
                    let message = "interrupted while queued".to_string();
                    turn_ledger.settle(TurnPhase::Interrupted, Some(&message));
                    return Err(anyhow::anyhow!(message));
                }
            };
            guard
        }
    };
    let _provider_owner = ProviderOwnerGuard::claim(provider_claims.clone(), provider, thread_id, turn);
    if *cancel_receiver.borrow() {
        let message = "interrupted while queued".to_string();
        turn_ledger.settle(TurnPhase::Interrupted, Some(&message));
        return Err(anyhow::anyhow!(message));
    }
    turn_ledger.set_phase(TurnPhase::Running);
    let _ = engine_event_sender.send(EngineEvent::TurnStarted { thread_id: thread_id.to_string() });

    // Snapshot the worktree before the turn so its own edits can be diffed.
    // A clean tree is exactly HEAD, so its commit id is the snapshot and the
    // five git processes a snapshot needs are skipped.
    let start_path = std::path::PathBuf::from(&cwd_for_checkpoint);
    let start_state = if start_path.exists() {
        crate::checkpoints::worktree_state(&start_path).ok()
    } else {
        None
    };
    let start_clean = start_state
        .as_ref()
        .map(|state| state.clean)
        .unwrap_or(false);
    let start_head = start_state.as_ref().and_then(|state| state.head.clone());
    let start_reference = match (start_clean, start_head.clone()) {
        (true, Some(head)) => Some(head),
        _ if start_path.exists() => {
            let start_ref = crate::checkpoints::checkpoint_start_ref(thread_id, turn);
            crate::checkpoints::capture_checkpoint_at(
                &start_path,
                &start_ref,
                &format!("baton checkpoint {thread_id} turn {turn} start"),
            )
            .ok()
            .map(|()| start_ref)
        }
        _ => None,
    };

    let (event_sender, event_receiver) = std::sync::mpsc::channel::<TurnEvent>();
    let tui_sender = turn_event_sender.clone();
    let event_thread_id = thread_id.to_string();
    let outcome_slot: Arc<Mutex<Option<TurnOutcome>>> = Arc::new(Mutex::new(None));
    let failure_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let interrupted_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let tools_slot: Arc<Mutex<Vec<ToolRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let subagents_slot: Arc<Mutex<Vec<SubagentRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let outcome_for_thread = outcome_slot.clone();
    let failure_for_thread = failure_slot.clone();
    let interrupted_for_thread = interrupted_slot.clone();
    let tools_for_thread = tools_slot.clone();
    let subagents_for_thread = subagents_slot.clone();
    let store_for_forwarder = store.clone();
    let model_for_forwarder = model.clone();
    let forwarder = std::thread::spawn(move || {
        for event in event_receiver {
            match &event {
                TurnEvent::ProviderTurnId { id } => {
                    if let Ok(store) = store_for_forwarder.lock() {
                        let _ = store.set_turn_provider_id(
                            &event_thread_id,
                            turn,
                            id,
                        );
                    }
                    let _ = tui_sender.send((event_thread_id.clone(), event));
                }
                TurnEvent::ProviderSession {
                    native_session_id,
                    rollout_path,
                } => {
                    if let Ok(store) = store_for_forwarder.lock() {
                        let _ = store.upsert_provider_session(
                            &event_thread_id,
                            provider,
                            native_session_id,
                            Some(model_for_forwarder.clone()),
                            rollout_path.clone(),
                        );
                    }
                    let _ = tui_sender.send((event_thread_id.clone(), event));
                }
                TurnEvent::TurnCompleted { final_text, usage, native_session_id, rollout_path } => {
                    *outcome_for_thread.lock().unwrap() = Some(TurnOutcome {
                        final_text: final_text.clone(),
                        usage: *usage,
                        native_session_id: native_session_id.clone(),
                        rollout_path: rollout_path.clone(),
                    });
                    let _ = tui_sender.send((event_thread_id.clone(), event));
                    break;
                }
                TurnEvent::TurnInterrupted { reason } => {
                    *interrupted_for_thread.lock().unwrap() =
                        Some(reason.clone().unwrap_or_else(|| "turn interrupted".into()));
                    let _ = tui_sender.send((event_thread_id.clone(), event));
                    break;
                }
                TurnEvent::TurnFailed { error } => {
                    *failure_for_thread.lock().unwrap() = Some(error.clone());
                    let _ = tui_sender.send((event_thread_id.clone(), event));
                    break;
                }
                TurnEvent::SubagentStarted { .. } | TurnEvent::SubagentFinished { .. } => {
                    record_subagent_event(&mut subagents_for_thread.lock().unwrap(), &event);
                    let _ = tui_sender.send((event_thread_id.clone(), event));
                }
                _ => {
                    record_tool_event(&mut tools_for_thread.lock().unwrap(), &event);
                    let _ = tui_sender.send((event_thread_id.clone(), event));
                }
            }
        }
    });

    let session_ref = session.map(|session| crate::types::NativeSessionRef {
        native_session_id: session.native_session_id,
        rollout_path: session.rollout_path,
    });
    let adapter_result = adapter_guard.start_turn(input, session_ref, event_sender).await;
    let _ = forwarder.join();

    let tool_records = tools_slot.lock().unwrap().drain(..).collect::<Vec<_>>();
    if !tool_records.is_empty() {
        let result = {
            let store = store.lock().unwrap();
            persist_tool_records(&store, thread_id, turn, provider, &tool_records)
        };
        if let Err(error) = result {
            turn_ledger.settle(TurnPhase::Failed, Some(&error.to_string()));
            return Err(error);
        }
    }

    let subagent_records = subagents_slot.lock().unwrap().drain(..).collect::<Vec<_>>();
    if !subagent_records.is_empty() {
        let result = {
            let store = store.lock().unwrap();
            persist_subagent_records(&store, thread_id, turn, provider, &subagent_records)
        };
        if let Err(error) = result {
            turn_ledger.settle(TurnPhase::Failed, Some(&error.to_string()));
            return Err(error);
        }
    }

    if let Some(reason) = interrupted_slot.lock().unwrap().take() {
        turn_ledger.settle(TurnPhase::Interrupted, Some(&reason));
        return Err(anyhow::anyhow!(reason));
    }

    if let Some(error) = failure_slot.lock().unwrap().take() {
        turn_ledger.settle(phase_for_error(&error), Some(&error));
        return Err(anyhow::anyhow!("{error}"));
    }

    // A provider that closes its stream without a terminal event is not a
    // successful turn. Surface it as an error instead of storing an empty
    // assistant reply and telling every caller the turn worked.
    let outcome = match outcome_slot.lock().unwrap().take() {
        Some(outcome) => outcome,
        None => {
            let (phase, message) = match adapter_result {
                Ok(()) => (
                    TurnPhase::Unverified,
                    "provider ended without a result".to_string(),
                ),
                Err(error) => (
                    TurnPhase::Failed,
                    format!("provider turn failed: {error}"),
                ),
            };
            turn_ledger.settle(phase, Some(&message));
            return Err(anyhow::anyhow!(message));
        }
    };

    {
        {
            let store = store.lock().unwrap();
            store.add_message(thread_id, turn, "assistant", Some(provider.as_str()), Some(&model), &outcome.final_text)?;
            if let Some(usage) = outcome.usage {
                store.add_usage(thread_id, turn, provider, Some(&model), &usage)?;
            }
            if let Some(native_session_id) = outcome.native_session_id.as_ref() {
                store.upsert_provider_session(
                    thread_id,
                    provider,
                    native_session_id,
                    Some(model.clone()),
                    outcome.rollout_path.clone(),
                )?;
                store.mark_provider_session_covered(thread_id, provider, turn)?;
            }
        }
        let cwd_path = std::path::PathBuf::from(&cwd_for_checkpoint);
        if cwd_path.exists() {
            let end_state = crate::checkpoints::worktree_state(&cwd_path).ok();
            if !turn_unchanged(start_clean, start_head.as_deref(), end_state.as_ref()) {
                if let Ok(reference) = capture_checkpoint(&cwd_path, thread_id, turn) {
                    store
                        .lock()
                        .unwrap()
                        .add_checkpoint(thread_id, turn, &reference)?;
                    // The turn's own changes, kept in the conversation as a diff.
                    if let Some(start) = start_reference.as_deref() {
                        if let Ok(diff) =
                            crate::checkpoints::diff_checkpoints(&cwd_path, start, &reference)
                        {
                            if !diff.trim().is_empty() {
                                let capped = cap_diff(&diff);
                                let _ = store
                                    .lock()
                                    .unwrap()
                                    .add_message(thread_id, turn, "diff", None, None, &capped);
                            }
                        }
                    }
                }
            }
        }
    }
    turn_ledger.settle(TurnPhase::Completed, None);
    // Non-TUI consumers (baton exec) read the final text from the engine
    // event channel; the TUI ignores EngineEvent::Turn.
    let _ = engine_event_sender.send(EngineEvent::Turn(TurnEvent::TurnCompleted {
        final_text: outcome.final_text,
        usage: outcome.usage,
        native_session_id: outcome.native_session_id,
        rollout_path: outcome.rollout_path,
    }));
    Ok(())
}

/// A turn that neither edits the tree nor moves HEAD has nothing to restore or
/// show, so it skips the checkpoint write entirely.
fn turn_unchanged(
    start_clean: bool,
    start_head: Option<&str>,
    end: Option<&crate::checkpoints::WorktreeState>,
) -> bool {
    start_clean
        && end
            .map(|state| state.clean && state.head.as_deref() == start_head)
            .unwrap_or(false)
}

fn skill_dispatch_enabled() -> bool {
    std::env::var_os("BATON_DISABLE_SKILL_DISPATCH").is_none()
}

/// Keeps stored diffs bounded: the first 400 lines plus a truncation marker.
fn cap_diff(diff: &str) -> String {
    const MAX_LINES: usize = 400;
    let mut lines = diff.lines();
    let mut capped: Vec<&str> = lines.by_ref().take(MAX_LINES).collect();
    if let Some(_more) = lines.next() {
        capped.push("[...diff truncated]");
    }
    let mut text = capped.join("\n");
    text.push('\n');
    text
}

pub fn ensure_persisted(store: &BatonStore, thread: &mut ThreadRow) -> Result<()> {
    if store.get_thread(&thread.id)?.is_none() {
        let desired_permission = thread.permission_mode;
        let mut created = store.create_thread_with_id_and_mode(
            NewThread {
                title: thread.title.clone(),
                cwd: thread.cwd.clone(),
                current_provider: thread.current_provider,
                current_model: thread.current_model.clone(),
            },
            thread.id.clone(),
            thread.session_mode,
        )?;
        if desired_permission != created.permission_mode {
            store.set_thread_permission_mode(&created.id, desired_permission)?;
            created.permission_mode = desired_permission;
        }
        if thread.reasoning_effort.is_some() {
            store.set_thread_effort(&created.id, thread.reasoning_effort.as_deref())?;
            created.reasoning_effort = thread.reasoning_effort.clone();
        }
        *thread = created;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::TurnEventSender;

    #[test]
    fn ensure_persisted_keeps_a_drafts_permission_mode() {
        let store = BatonStore::memory().unwrap();
        let mut thread = store.draft_thread(NewThread {
            title: "permissions".to_string(),
            cwd: "/tmp/project".to_string(),
            current_provider: ProviderId::Codex,
            current_model: None,
        });
        thread.permission_mode = PermissionMode::BypassPermissions;
        thread.reasoning_effort = Some("high".to_string());
        ensure_persisted(&store, &mut thread).unwrap();
        assert_eq!(thread.permission_mode, PermissionMode::BypassPermissions);
        let stored = store.get_thread(&thread.id).unwrap().expect("thread persisted");
        assert_eq!(stored.permission_mode, PermissionMode::BypassPermissions);
        assert_eq!(stored.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn a_short_live_catalog_does_not_shrink_the_cache() {
        let home = tempfile::tempdir().unwrap();
        let fuller = [
            "gpt-6.1-sol".to_string(),
            "gpt-6.2-sol".to_string(),
            "gpt-7-preview".to_string(),
        ];
        crate::models::write_cached_catalog(home.path(), ProviderId::Codex, &fuller).unwrap();
        let cached = crate::models::read_cached_catalog(home.path(), ProviderId::Codex);
        let catalog = finish_catalog(
            home.path(),
            ProviderId::Codex,
            vec!["gpt-6.1-sol".to_string()],
            cached,
        );
        assert_eq!(catalog.live, 1);
        assert!(catalog.cached >= 1, "the cached-only models are counted");
        assert!(catalog.models.contains(&"gpt-7-preview".to_string()));
        assert!(catalog.error.is_none());
        let after = crate::models::read_cached_catalog(home.path(), ProviderId::Codex);
        assert!(
            after.contains(&"gpt-7-preview".to_string()),
            "the fuller cache survives a short live probe"
        );
    }

    #[test]
    fn a_failed_live_probe_still_offers_the_cached_and_known_models() {
        let home = tempfile::tempdir().unwrap();
        crate::models::write_cached_catalog(home.path(), ProviderId::Grok, &["grok-5".to_string()])
            .unwrap();
        let cached = crate::models::read_cached_catalog(home.path(), ProviderId::Grok);
        let catalog = cached_catalog(ProviderId::Grok, cached, "timed out".to_string());
        assert_eq!(catalog.error.as_deref(), Some("timed out"));
        assert_eq!(catalog.live, 0);
        assert!(catalog.models.contains(&"grok-5".to_string()));
        assert!(
            catalog.models.contains(&"grok-4.6".to_string()),
            "the static catalog still appears"
        );
    }

    #[test]
    fn only_an_unchanged_clean_turn_skips_the_checkpoint() {
        let clean = crate::checkpoints::WorktreeState {
            clean: true,
            head: Some("a".to_string()),
        };
        assert!(turn_unchanged(true, Some("a"), Some(&clean)));
        assert!(
            !turn_unchanged(true, Some("a"), None),
            "a failed status check falls back to a snapshot"
        );
        let dirty = crate::checkpoints::WorktreeState {
            clean: false,
            head: Some("a".to_string()),
        };
        assert!(!turn_unchanged(true, Some("a"), Some(&dirty)));
        let committed = crate::checkpoints::WorktreeState {
            clean: true,
            head: Some("b".to_string()),
        };
        assert!(!turn_unchanged(true, Some("a"), Some(&committed)));
        assert!(
            !turn_unchanged(false, Some("a"), Some(&clean)),
            "a dirty start still needs the end snapshot"
        );
    }

    #[derive(Clone, Copy)]
    enum AdapterMode {
        Silent,
        Complete,
        Fail,
        Interrupt,
        Slow,
        Panic,
        Subagents,
    }

    struct ScriptAdapter {
        mode: AdapterMode,
    }

    #[async_trait::async_trait]
    impl BatonAdapter for ScriptAdapter {
        fn id(&self) -> ProviderId {
            ProviderId::Codex
        }

        fn interrupt_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
            Arc::new(|| {})
        }

        async fn start_turn(
            &mut self,
            _input: TurnStartInput,
            _session: Option<crate::types::NativeSessionRef>,
            emit: TurnEventSender,
        ) -> Result<()> {
            match self.mode {
                AdapterMode::Silent => drop(emit),
                AdapterMode::Complete => {
                    let _ = emit.send(TurnEvent::ProviderTurnId {
                        id: "provider-turn-1".into(),
                    });
                    let _ = emit.send(TurnEvent::TurnCompleted {
                        final_text: "done".into(),
                        usage: None,
                        native_session_id: Some("native-1".into()),
                        rollout_path: None,
                    });
                }
                AdapterMode::Fail => {
                    let _ = emit.send(TurnEvent::ProviderSession {
                        native_session_id: "native-fail".into(),
                        rollout_path: None,
                    });
                    let _ = emit.send(TurnEvent::TurnFailed {
                        error: "provider died".into(),
                    });
                }
                AdapterMode::Interrupt => {
                    let _ = emit.send(TurnEvent::TurnInterrupted {
                        reason: Some("stopped by user".into()),
                    });
                }
                AdapterMode::Slow => {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    let _ = emit.send(TurnEvent::TurnCompleted {
                        final_text: "slow done".into(),
                        usage: None,
                        native_session_id: None,
                        rollout_path: None,
                    });
                }
                AdapterMode::Subagents => {
                    let _ = emit.send(TurnEvent::SubagentStarted {
                        id: Some("s1".into()),
                        name: "Explore".into(),
                        detail: Some("parser".into()),
                    });
                    let _ = emit.send(TurnEvent::SubagentFinished {
                        id: Some("s1".into()),
                        name: "Explore".into(),
                        status: "done".into(),
                        detail: Some("found the parser".into()),
                    });
                    let _ = emit.send(TurnEvent::SubagentStarted {
                        id: Some("s2".into()),
                        name: "review".into(),
                        detail: None,
                    });
                    let _ = emit.send(TurnEvent::TurnCompleted {
                        final_text: "delegated".into(),
                        usage: None,
                        native_session_id: None,
                        rollout_path: None,
                    });
                }
                AdapterMode::Panic => panic!("scripted adapter panic"),
            }
            Ok(())
        }

        async fn dispose(&mut self) -> Result<()> {
            Ok(())
        }
    }

    async fn run_scripted_turn(mode: AdapterMode) -> (SharedStore, String, Result<()>) {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let cwd = home.path().to_string_lossy().to_string();
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "t".into(),
                cwd: cwd.clone(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(
            Box::new(ScriptAdapter { mode }) as Box<dyn BatonAdapter>
        ))];
        let adapter_indices = HashMap::from([(ProviderId::Codex, 0)]);
        let provider_claims: ProviderClaims = Arc::new(Mutex::new(HashMap::new()));
        let (_cancel_sender, cancel_receiver) = tokio::sync::watch::channel(false);
        let (engine_sender, _engine_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let result = execute_turn(
            &store,
            &home.path().to_path_buf(),
            &adapters,
            &adapter_indices,
            &provider_claims,
            cancel_receiver,
            &thread.id,
            "hello",
            PermissionMode::Default,
            false,
            None,
            false,
            &engine_sender,
            &turn_sender,
        )
        .await;
        (store, thread.id, result)
    }

    struct CapturingAdapter {
        provider: ProviderId,
        prompt: Arc<Mutex<Option<String>>>,
        session_id: Option<String>,
    }

    #[async_trait::async_trait]
    impl BatonAdapter for CapturingAdapter {
        fn id(&self) -> ProviderId {
            self.provider
        }

        fn interrupt_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
            Arc::new(|| {})
        }

        async fn list_models(&mut self, _input: ModelListInput) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn start_turn(
            &mut self,
            input: TurnStartInput,
            _session: Option<crate::types::NativeSessionRef>,
            emit: TurnEventSender,
        ) -> Result<()> {
            *self.prompt.lock().unwrap() = Some(input.prompt);
            let _ = emit.send(TurnEvent::TurnCompleted {
                final_text: "done".into(),
                usage: None,
                native_session_id: self.session_id.clone(),
                rollout_path: None,
            });
            Ok(())
        }

        async fn dispose(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn memories_expand_for_the_harness_but_the_raw_prompt_is_stored() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let cwd = home.path().to_string_lossy().to_string();
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "memories".into(),
                cwd: cwd.clone(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let project = crate::memory::project_key(&cwd);
        store
            .lock()
            .unwrap()
            .add_memory("project", &project, "Use tabs")
            .unwrap();
        store
            .lock()
            .unwrap()
            .add_memory("global", &project, "Answer briefly")
            .unwrap();
        let captured = Arc::new(Mutex::new(None));
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(Box::new(
            CapturingAdapter {
                provider: ProviderId::Codex,
                prompt: captured.clone(),
                session_id: None,
            },
        )
            as Box<dyn BatonAdapter>))];
        let adapter_indices = HashMap::from([(ProviderId::Codex, 0)]);
        let provider_claims: ProviderClaims = Arc::new(Mutex::new(HashMap::new()));
        let (_cancel_sender, cancel_receiver) = tokio::sync::watch::channel(false);
        let (engine_sender, _engine_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let result = execute_turn(
            &store,
            &home.path().to_path_buf(),
            &adapters,
            &adapter_indices,
            &provider_claims,
            cancel_receiver,
            &thread.id,
            "hello $memories",
            PermissionMode::Default,
            false,
            None,
            false,
            &engine_sender,
            &turn_sender,
        )
        .await;
        assert!(result.is_ok());
        let prompt = captured.lock().unwrap().clone().expect("prompt captured");
        assert!(prompt.contains("[memories]"), "{prompt}");
        assert!(prompt.contains("- (project) Use tabs"), "{prompt}");
        assert!(prompt.contains("- (global) Answer briefly"), "{prompt}");
        assert!(!prompt.contains("$memories"), "{prompt}");
        let messages = store.lock().unwrap().list_messages(&thread.id).unwrap();
        assert!(
            messages.iter().any(|message| message.role == "user" && message.text == "hello $memories"),
            "the transcript keeps the raw prompt"
        );
    }

    #[tokio::test]
    async fn claude_payload_uses_native_skill_syntax_and_keeps_the_transcript_token() {
        struct RestoreEnv(Option<std::ffi::OsString>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => std::env::set_var("CLAUDE_CONFIG_DIR", value),
                    None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
                }
            }
        }

        let claude_root = tempfile::tempdir().unwrap();
        let skill_dir = claude_root.path().join("skills").join("review");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: review\n---\n").unwrap();
        let _restore = RestoreEnv(std::env::var_os("CLAUDE_CONFIG_DIR"));
        std::env::set_var("CLAUDE_CONFIG_DIR", claude_root.path());

        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let cwd = home.path().to_string_lossy().to_string();
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "skills".into(),
                cwd: cwd.clone(),
                current_provider: ProviderId::Claude,
                current_model: Some("m".into()),
            })
            .unwrap();
        let captured = Arc::new(Mutex::new(None));
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(Box::new(
            CapturingAdapter {
                provider: ProviderId::Claude,
                prompt: captured.clone(),
                session_id: None,
            },
        )
            as Box<dyn BatonAdapter>))];
        let adapter_indices = HashMap::from([(ProviderId::Claude, 0)]);
        let provider_claims: ProviderClaims = Arc::new(Mutex::new(HashMap::new()));
        let (_cancel_sender, cancel_receiver) = tokio::sync::watch::channel(false);
        let (engine_sender, _engine_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let result = execute_turn(
            &store,
            &home.path().to_path_buf(),
            &adapters,
            &adapter_indices,
            &provider_claims,
            cancel_receiver,
            &thread.id,
            "$review this change",
            PermissionMode::Default,
            false,
            None,
            false,
            &engine_sender,
            &turn_sender,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(
            captured.lock().unwrap().as_deref(),
            Some("/review this change")
        );
        let messages = store.lock().unwrap().list_messages(&thread.id).unwrap();
        assert!(messages
            .iter()
            .any(|message| message.role == "user" && message.text == "$review this change"));
    }

    #[tokio::test]
    async fn a_returning_provider_receives_only_turns_it_has_not_seen() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let cwd = home.path().to_string_lossy().to_string();
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "handoff".into(),
                cwd: cwd.clone(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        {
            let store = store.lock().unwrap();
            store.add_message(&thread.id, 1, "user", Some("codex"), Some("m"), "first question").unwrap();
            store.add_message(&thread.id, 1, "assistant", Some("codex"), Some("m"), "first answer").unwrap();
            store.add_message(&thread.id, 2, "user", Some("claude"), Some("m"), "claude question").unwrap();
            store.add_message(&thread.id, 2, "assistant", Some("claude"), Some("m"), "claude answer").unwrap();
            store
                .upsert_provider_session(
                    &thread.id,
                    ProviderId::Codex,
                    "s1",
                    Some("m".into()),
                    None,
                )
                .unwrap();
            store
                .mark_provider_session_covered(&thread.id, ProviderId::Codex, 1)
                .unwrap();
        }

        let captured = Arc::new(Mutex::new(None));
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(Box::new(
            CapturingAdapter {
                provider: ProviderId::Codex,
                prompt: captured.clone(),
                session_id: Some("s1".into()),
            },
        ) as Box<dyn BatonAdapter>))];
        let adapter_indices = HashMap::from([(ProviderId::Codex, 0)]);
        let provider_claims: ProviderClaims = Arc::new(Mutex::new(HashMap::new()));
        let (_cancel_sender, cancel_receiver) = tokio::sync::watch::channel(false);
        let (engine_sender, _engine_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        execute_turn(
            &store,
            &home.path().to_path_buf(),
            &adapters,
            &adapter_indices,
            &provider_claims,
            cancel_receiver,
            &thread.id,
            "next",
            PermissionMode::Default,
            false,
            None,
            false,
            &engine_sender,
            &turn_sender,
        )
        .await
        .expect("turn completes");

        let prompt = captured.lock().unwrap().clone().expect("prompt captured");
        assert!(prompt.contains(crate::handoff::RESUME_INTRO), "{prompt}");
        assert!(prompt.contains("claude question"), "{prompt}");
        assert!(prompt.contains("claude answer"), "{prompt}");
        assert!(
            !prompt.contains("first question"),
            "turns the provider already saw must not be resent: {prompt}"
        );
        let covered = store
            .lock()
            .unwrap()
            .get_provider_session(&thread.id, ProviderId::Codex)
            .unwrap()
            .unwrap()
            .covered_turn;
        assert_eq!(covered, 3, "a completed turn advances the marker");
    }

    #[tokio::test]
    async fn a_skipped_handoff_sends_the_raw_prompt() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let cwd = home.path().to_string_lossy().to_string();
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "handoff".into(),
                cwd: cwd.clone(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        {
            let store = store.lock().unwrap();
            store.add_message(&thread.id, 1, "user", Some("codex"), Some("m"), "first question").unwrap();
            store.add_message(&thread.id, 1, "assistant", Some("codex"), Some("m"), "first answer").unwrap();
            store.add_message(&thread.id, 2, "user", Some("claude"), Some("m"), "claude question").unwrap();
            store.add_message(&thread.id, 2, "assistant", Some("claude"), Some("m"), "claude answer").unwrap();
            store
                .upsert_provider_session(
                    &thread.id,
                    ProviderId::Codex,
                    "s1",
                    Some("m".into()),
                    None,
                )
                .unwrap();
            store
                .mark_provider_session_covered(&thread.id, ProviderId::Codex, 1)
                .unwrap();
        }

        let captured = Arc::new(Mutex::new(None));
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(Box::new(
            CapturingAdapter {
                provider: ProviderId::Codex,
                prompt: captured.clone(),
                session_id: Some("s1".into()),
            },
        ) as Box<dyn BatonAdapter>))];
        let adapter_indices = HashMap::from([(ProviderId::Codex, 0)]);
        let provider_claims: ProviderClaims = Arc::new(Mutex::new(HashMap::new()));
        let (_cancel_sender, cancel_receiver) = tokio::sync::watch::channel(false);
        let (engine_sender, _engine_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        execute_turn(
            &store,
            &home.path().to_path_buf(),
            &adapters,
            &adapter_indices,
            &provider_claims,
            cancel_receiver,
            &thread.id,
            "next",
            PermissionMode::Default,
            true,
            None,
            false,
            &engine_sender,
            &turn_sender,
        )
        .await
        .expect("turn completes");

        let prompt = captured.lock().unwrap().clone().expect("prompt captured");
        assert_eq!(prompt, "next", "a skipped handoff sends the prompt alone");
    }

    #[tokio::test]
    async fn a_handoff_redacts_configured_secrets() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.json"),
            r#"{"codex":{"api_key":"sk-live-abcdef123456"}}"#,
        )
        .unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let cwd = home.path().to_string_lossy().to_string();
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "redact".into(),
                cwd: cwd.clone(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        {
            let store = store.lock().unwrap();
            store
                .add_message(
                    &thread.id,
                    1,
                    "user",
                    Some("claude"),
                    Some("m"),
                    "the token is sk-live-abcdef123456, do not repeat it",
                )
                .unwrap();
            store
                .add_message(&thread.id, 1, "assistant", Some("claude"), Some("m"), "noted")
                .unwrap();
        }

        let captured = Arc::new(Mutex::new(None));
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(
            Box::new(CapturingAdapter {
                provider: ProviderId::Codex,
                prompt: captured.clone(),
                session_id: None,
            }) as Box<dyn BatonAdapter>,
        ))];
        let adapter_indices = HashMap::from([(ProviderId::Codex, 0)]);
        let provider_claims: ProviderClaims = Arc::new(Mutex::new(HashMap::new()));
        let (_cancel_sender, cancel_receiver) = tokio::sync::watch::channel(false);
        let (engine_sender, _engine_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        execute_turn(
            &store,
            &home.path().to_path_buf(),
            &adapters,
            &adapter_indices,
            &provider_claims,
            cancel_receiver,
            &thread.id,
            "next",
            PermissionMode::Default,
            false,
            None,
            false,
            &engine_sender,
            &turn_sender,
        )
        .await
        .expect("turn completes");

        let prompt = captured.lock().unwrap().clone().expect("prompt captured");
        assert!(
            !prompt.contains("sk-live-abcdef123456"),
            "the configured key must not reach another provider: {prompt}"
        );
        assert!(prompt.contains(crate::redact::PLACEHOLDER), "{prompt}");
    }

    #[tokio::test]
    async fn a_provider_that_has_seen_every_turn_gets_the_raw_prompt() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let cwd = home.path().to_string_lossy().to_string();
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "handoff".into(),
                cwd: cwd.clone(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        {
            let store = store.lock().unwrap();
            store.add_message(&thread.id, 1, "user", Some("codex"), Some("m"), "first question").unwrap();
            store.add_message(&thread.id, 1, "assistant", Some("codex"), Some("m"), "first answer").unwrap();
            store
                .upsert_provider_session(
                    &thread.id,
                    ProviderId::Codex,
                    "s1",
                    Some("m".into()),
                    None,
                )
                .unwrap();
            store
                .mark_provider_session_covered(&thread.id, ProviderId::Codex, 1)
                .unwrap();
        }

        let captured = Arc::new(Mutex::new(None));
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(Box::new(
            CapturingAdapter {
                provider: ProviderId::Codex,
                prompt: captured.clone(),
                session_id: Some("s1".into()),
            },
        ) as Box<dyn BatonAdapter>))];
        let adapter_indices = HashMap::from([(ProviderId::Codex, 0)]);
        let provider_claims: ProviderClaims = Arc::new(Mutex::new(HashMap::new()));
        let (_cancel_sender, cancel_receiver) = tokio::sync::watch::channel(false);
        let (engine_sender, _engine_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        execute_turn(
            &store,
            &home.path().to_path_buf(),
            &adapters,
            &adapter_indices,
            &provider_claims,
            cancel_receiver,
            &thread.id,
            "next",
            PermissionMode::Default,
            false,
            None,
            false,
            &engine_sender,
            &turn_sender,
        )
        .await
        .expect("turn completes");

        let prompt = captured.lock().unwrap().clone().expect("prompt captured");
        assert_eq!(prompt, "next", "a fully caught-up session gets no handoff");
    }

    struct IdleAdapter;

    #[async_trait::async_trait]
    impl BatonAdapter for IdleAdapter {
        fn id(&self) -> ProviderId {
            ProviderId::Codex
        }

        fn interrupt_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
            Arc::new(|| {})
        }

        async fn list_models(&mut self, input: ModelListInput) -> Result<Vec<String>> {
            Ok(vec![format!("live:{}", input.cwd)])
        }

        async fn read_rate_limits(
            &mut self,
            _auth_env: &[(String, String)],
        ) -> Result<Option<RateLimitInfo>> {
            Ok(Some(RateLimitInfo {
                plan_type: Some("plus".into()),
                primary: Some(crate::types::RateLimitWindow {
                    used_percent: 12,
                    window_duration_mins: Some(300),
                    resets_at: None,
                }),
                secondary: None,
                credits_balance: None,
                credits_unlimited: false,
                reported_at: "2026-11-26T03:00:00Z".into(),
            }))
        }

        async fn start_turn(
            &mut self,
            _input: TurnStartInput,
            _session: Option<crate::types::NativeSessionRef>,
            _emit: TurnEventSender,
        ) -> Result<()> {
            Ok(())
        }

        async fn dispose(&mut self) -> Result<()> {
            Ok(())
        }
    }

    struct GateAdapter {
        provider: ProviderId,
        started: tokio::sync::mpsc::UnboundedSender<String>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl BatonAdapter for GateAdapter {
        fn id(&self) -> ProviderId {
            self.provider
        }

        fn interrupt_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
            Arc::new(|| {})
        }

        async fn start_turn(
            &mut self,
            input: TurnStartInput,
            _session: Option<crate::types::NativeSessionRef>,
            emit: TurnEventSender,
        ) -> Result<()> {
            let _ = self.started.send(input.thread_id.clone());
            self.release.notified().await;
            let _ = emit.send(TurnEvent::TurnCompleted {
                final_text: format!("done {}", input.thread_id),
                usage: None,
                native_session_id: None,
                rollout_path: None,
            });
            Ok(())
        }

        async fn dispose(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn gate_shared(provider: ProviderId, started: &tokio::sync::mpsc::UnboundedSender<String>, release: &Arc<tokio::sync::Notify>) -> SharedAdapter {
        Arc::new(tokio::sync::Mutex::new(Box::new(GateAdapter {
            provider,
            started: started.clone(),
            release: release.clone(),
        }) as Box<dyn BatonAdapter>))
    }

    async fn next_started(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(3), receiver.recv())
            .await
            .expect("adapter turn starts")
            .expect("adapter start channel")
    }

    async fn wait_engine_event(
        receiver: &std::sync::mpsc::Receiver<EngineEvent>,
        mut matches: impl FnMut(&EngineEvent) -> bool,
    ) -> EngineEvent {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                while let Ok(event) = receiver.try_recv() {
                    if matches(&event) {
                        return event;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("engine event arrives")
    }

    async fn steer_test_engine(
        steer: SteerHandle,
    ) -> (
        SharedStore,
        ThreadRow,
        Arc<tokio::sync::Notify>,
        tokio::sync::mpsc::UnboundedSender<EngineCommand>,
        std::sync::mpsc::Receiver<EngineEvent>,
        tokio::task::JoinHandle<Result<()>>,
        tokio::sync::mpsc::UnboundedReceiver<String>,
    ) {
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "t".into(),
                cwd: "/tmp".into(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let (started_sender, started_receiver) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let adapters = vec![gate_shared(ProviderId::Codex, &started_sender, &release)];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store.clone(),
            std::path::PathBuf::from("/tmp"),
            adapters,
            Vec::new(),
            vec![(ProviderId::Codex, steer)],
            command_receiver,
            event_sender,
            turn_sender,
        ));
        (
            store,
            thread,
            release,
            command_sender,
            event_receiver,
            engine,
            started_receiver,
        )
    }

    #[tokio::test]
    async fn steering_is_scoped_to_the_owner_and_turn() {
        let observed = Arc::new(Mutex::new(Vec::<SteerRequest>::new()));
        let observed_handle = observed.clone();
        let steer: SteerHandle = Arc::new(move |request: SteerRequest| {
            let observed_handle = observed_handle.clone();
            Box::pin(async move {
                observed_handle.lock().unwrap().push(request);
                SteerOutcome::Delivered
            })
        });
        let (store, thread, release, command_sender, event_receiver, engine, mut started) =
            steer_test_engine(steer).await;
        command_sender
            .send(EngineCommand::Submit {
                thread_id: thread.id.clone(),
                text: "hello".into(),
                permission_mode: PermissionMode::Default,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            })
            .unwrap();
        assert_eq!(next_started(&mut started).await, thread.id);

        command_sender
            .send(EngineCommand::Steer {
                thread_id: thread.id.clone(),
                text: "try this".into(),
            })
            .unwrap();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::UserEcho { text, .. } if text == "try this")
        })
        .await;
        {
            let requests = observed.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].thread_id, thread.id);
            assert_eq!(requests[0].turn, 1, "a steer is scoped to the running turn");
            assert_eq!(requests[0].text, "try this");
        }

        let messages = store.lock().unwrap().list_messages(&thread.id).unwrap();
        assert!(messages.iter().any(|message| message.text == "hello"));
        assert!(messages.iter().any(|message| message.text == "try this"));

        release.notify_one();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnFinished { thread_id, ok: true, .. } if thread_id == &thread.id)
        })
        .await;

        command_sender
            .send(EngineCommand::Steer {
                thread_id: thread.id.clone(),
                text: "queue me".into(),
            })
            .unwrap();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::SteerRejected { text, .. } if text == "queue me")
        })
        .await;

        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[tokio::test]
    async fn a_stale_turn_steer_is_rejected_and_not_recorded() {
        let steer: SteerHandle =
            Arc::new(|_request: SteerRequest| Box::pin(async { SteerOutcome::StaleTurn }));
        let (store, thread, release, command_sender, event_receiver, engine, mut started) =
            steer_test_engine(steer).await;
        command_sender
            .send(EngineCommand::Submit {
                thread_id: thread.id.clone(),
                text: "hello".into(),
                permission_mode: PermissionMode::Default,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            })
            .unwrap();
        assert_eq!(next_started(&mut started).await, thread.id);

        command_sender
            .send(EngineCommand::Steer {
                thread_id: thread.id.clone(),
                text: "too late".into(),
            })
            .unwrap();
        let rejected = wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::SteerRejected { text, .. } if text == "too late")
        })
        .await;
        match rejected {
            EngineEvent::SteerRejected { reason, .. } => {
                assert!(
                    reason.contains("turn ended"),
                    "the rejection explains the stale turn: {reason}"
                );
            }
            _ => unreachable!(),
        }
        let messages = store.lock().unwrap().list_messages(&thread.id).unwrap();
        assert!(
            !messages.iter().any(|message| message.text == "too late"),
            "a steer the harness did not take must not appear in history"
        );

        release.notify_one();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnFinished { thread_id, ok: true, .. } if thread_id == &thread.id)
        })
        .await;
        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[tokio::test]
    async fn interrupt_only_targets_the_owning_provider() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let claude_thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "claude".into(),
                cwd: "/tmp/claude".into(),
                current_provider: ProviderId::Claude,
                current_model: None,
            })
            .unwrap();
        let (started_sender, mut started_receiver) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let adapters = vec![gate_shared(ProviderId::Claude, &started_sender, &release)];
        let codex_interrupted = Arc::new(AtomicBool::new(false));
        let (claude_tx, mut claude_rx) = tokio::sync::mpsc::unbounded_channel();
        let codex_flag = codex_interrupted.clone();
        let handles: Vec<(ProviderId, Arc<dyn Fn() + Send + Sync>)> = vec![
            (
                ProviderId::Codex,
                Arc::new(move || codex_flag.store(true, Ordering::SeqCst)),
            ),
            (
                ProviderId::Claude,
                Arc::new(move || {
                    let _ = claude_tx.send(());
                }),
            ),
        ];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store.clone(),
            std::path::PathBuf::from("/tmp"),
            adapters,
            handles,
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));
        command_sender
            .send(EngineCommand::Submit {
                thread_id: claude_thread.id.clone(),
                text: "hello".into(),
                permission_mode: PermissionMode::Default,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            })
            .unwrap();
        assert_eq!(next_started(&mut started_receiver).await, claude_thread.id);

        command_sender
            .send(EngineCommand::Interrupt {
                thread_id: claude_thread.id.clone(),
            })
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), claude_rx.recv())
            .await
            .expect("claude interrupt arrives")
            .expect("claude interrupt channel");
        assert!(!codex_interrupted.load(Ordering::SeqCst));

        release.notify_one();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnFinished { thread_id, ok: true, .. } if thread_id == &claude_thread.id)
        })
        .await;
        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[tokio::test]
    async fn queues_a_second_thread_on_the_same_provider() {
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let first = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "first".into(),
                cwd: "/tmp".into(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let second = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "second".into(),
                cwd: "/tmp".into(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let (started_sender, mut started_receiver) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let adapters = vec![gate_shared(ProviderId::Codex, &started_sender, &release)];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store.clone(),
            std::path::PathBuf::from("/tmp"),
            adapters,
            Vec::new(),
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));

        command_sender
            .send(EngineCommand::Submit {
                thread_id: first.id.clone(),
                text: "first prompt".into(),
                permission_mode: PermissionMode::Default,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            })
            .unwrap();
        assert_eq!(next_started(&mut started_receiver).await, first.id);

        command_sender
            .send(EngineCommand::Submit {
                thread_id: second.id.clone(),
                text: "second prompt".into(),
                permission_mode: PermissionMode::Default,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            })
            .unwrap();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnQueued { thread_id, provider }
                if thread_id == &second.id && *provider == ProviderId::Codex)
        })
        .await;
        assert_eq!(
            store
                .lock()
                .unwrap()
                .get_turn_record(&second.id, 1)
                .unwrap()
                .expect("second turn record")
                .phase,
            TurnPhase::Queued
        );

        release.notify_one();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnFinished { thread_id, ok: true, .. } if thread_id == &first.id)
        })
        .await;
        assert_eq!(next_started(&mut started_receiver).await, second.id);
        assert_eq!(
            store
                .lock()
                .unwrap()
                .get_turn_record(&second.id, 1)
                .unwrap()
                .expect("second turn record")
                .phase,
            TurnPhase::Running
        );

        release.notify_one();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnFinished { thread_id, ok: true, .. } if thread_id == &second.id)
        })
        .await;

        for thread in [&first, &second] {
            assert_eq!(
                store
                    .lock()
                    .unwrap()
                    .get_turn_record(&thread.id, 1)
                    .unwrap()
                    .expect("turn record")
                    .phase,
                TurnPhase::Completed
            );
            assert!(store
                .lock()
                .unwrap()
                .list_messages(&thread.id)
                .unwrap()
                .iter()
                .any(|message| message.role == "assistant"));
        }

        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[tokio::test]
    async fn interrupting_a_queued_turn_does_not_touch_the_provider_owner() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let first = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "first".into(),
                cwd: "/tmp".into(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let second = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "second".into(),
                cwd: "/tmp".into(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let (started_sender, mut started_receiver) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let adapters = vec![gate_shared(ProviderId::Codex, &started_sender, &release)];
        let interrupts = Arc::new(AtomicUsize::new(0));
        let interrupt_flag = interrupts.clone();
        let handles: Vec<(ProviderId, Arc<dyn Fn() + Send + Sync>)> = vec![(
            ProviderId::Codex,
            Arc::new(move || {
                interrupt_flag.fetch_add(1, Ordering::SeqCst);
            }),
        )];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store.clone(),
            std::path::PathBuf::from("/tmp"),
            adapters,
            handles,
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));

        command_sender
            .send(EngineCommand::Submit {
                thread_id: first.id.clone(),
                text: "first prompt".into(),
                permission_mode: PermissionMode::Default,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            })
            .unwrap();
        assert_eq!(next_started(&mut started_receiver).await, first.id);
        command_sender
            .send(EngineCommand::Submit {
                thread_id: second.id.clone(),
                text: "second prompt".into(),
                permission_mode: PermissionMode::Default,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            })
            .unwrap();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnQueued { thread_id, .. } if thread_id == &second.id)
        })
        .await;

        command_sender
            .send(EngineCommand::Interrupt {
                thread_id: second.id.clone(),
            })
            .unwrap();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnFinished { thread_id, phase: TurnPhase::Interrupted, .. } if thread_id == &second.id)
        })
        .await;
        assert_eq!(interrupts.load(Ordering::SeqCst), 0);
        assert_eq!(
            store
                .lock()
                .unwrap()
                .get_turn_record(&first.id, 1)
                .unwrap()
                .expect("first turn record")
                .phase,
            TurnPhase::Running
        );

        release.notify_one();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnFinished { thread_id, ok: true, .. } if thread_id == &first.id)
        })
        .await;
        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[test]
    fn a_dropped_turn_ledger_is_unverified() {
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "t".into(),
                cwd: "/tmp".into(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        store
            .lock()
            .unwrap()
            .start_turn_record(&thread.id, 1, ProviderId::Codex, Some("m"))
            .unwrap();
        let ledger = TurnLedger::submitted(&store, &thread.id, 1);
        drop(ledger);
        assert_eq!(
            store
                .lock()
                .unwrap()
                .get_turn_record(&thread.id, 1)
                .unwrap()
                .expect("turn record")
                .phase,
            TurnPhase::Unverified
        );
    }

    #[tokio::test]
    async fn quit_does_not_wait_forever_for_a_stuck_adapter() {
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "t".into(),
                cwd: "/tmp".into(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let (started_sender, mut started_receiver) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let adapters = vec![gate_shared(ProviderId::Codex, &started_sender, &release)];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, _event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store,
            std::path::PathBuf::from("/tmp"),
            adapters,
            Vec::new(),
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));
        command_sender
            .send(EngineCommand::Submit {
                thread_id: thread.id.clone(),
                text: "hello".into(),
                permission_mode: PermissionMode::Default,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            })
            .unwrap();
        assert_eq!(next_started(&mut started_receiver).await, thread.id);

        command_sender.send(EngineCommand::Quit).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), engine)
            .await
            .expect("quit returns after the dispose timeout")
            .expect("engine task")
            .expect("engine result");
        release.notify_one();
    }

    struct TaggedAdapter {
        provider: ProviderId,
        reply: &'static str,
    }

    #[async_trait::async_trait]
    impl BatonAdapter for TaggedAdapter {
        fn id(&self) -> ProviderId {
            self.provider
        }

        fn interrupt_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
            Arc::new(|| {})
        }

        async fn start_turn(
            &mut self,
            _input: TurnStartInput,
            _session: Option<crate::types::NativeSessionRef>,
            emit: TurnEventSender,
        ) -> Result<()> {
            let _ = emit.send(TurnEvent::TextDelta {
                text: self.reply.to_string(),
            });
            let _ = emit.send(TurnEvent::TurnCompleted {
                final_text: self.reply.to_string(),
                usage: None,
                native_session_id: None,
                rollout_path: None,
            });
            Ok(())
        }

        async fn dispose(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_turn_events_keep_their_thread_id() {
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let codex_thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "codex".into(),
                cwd: "/tmp/codex".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let claude_thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "claude".into(),
                cwd: "/tmp/claude".into(),
                current_provider: ProviderId::Claude,
                current_model: None,
            })
            .unwrap();
        let adapters: Vec<SharedAdapter> = vec![
            Arc::new(tokio::sync::Mutex::new(
                Box::new(TaggedAdapter {
                    provider: ProviderId::Codex,
                    reply: "codex-reply",
                }) as Box<dyn BatonAdapter>,
            )),
            Arc::new(tokio::sync::Mutex::new(
                Box::new(TaggedAdapter {
                    provider: ProviderId::Claude,
                    reply: "claude-reply",
                }) as Box<dyn BatonAdapter>,
            )),
        ];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, _event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store,
            std::path::PathBuf::from("/tmp"),
            adapters,
            Vec::new(),
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));
        for thread_id in [&codex_thread.id, &claude_thread.id] {
            command_sender
                .send(EngineCommand::Submit {
                    thread_id: thread_id.clone(),
                    text: "hello".into(),
                    permission_mode: PermissionMode::Default,
                    skip_handoff: false,
                    expected_provider: None,
                    retry: false,
                    drain_queued: false,
                })
                .unwrap();
        }
        let mut completed = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline && completed.len() < 2 {
            if let Ok((thread_id, TurnEvent::TurnCompleted { final_text, .. })) =
                turn_receiver.try_recv()
            {
                completed.push((thread_id, final_text));
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        assert_eq!(completed.len(), 2);
        assert!(completed.contains(&(codex_thread.id.clone(), "codex-reply".to_string())));
        assert!(completed.contains(&(claude_thread.id.clone(), "claude-reply".to_string())));
        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[tokio::test]
    async fn engine_lists_models_for_the_requested_thread() {
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "models".into(),
                cwd: "/tmp/models".into(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(Box::new(
            IdleAdapter,
        )
            as Box<dyn BatonAdapter>))];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store,
            std::path::PathBuf::from("/tmp"),
            adapters,
            Vec::new(),
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));
        command_sender
            .send(EngineCommand::ListModels {
                thread_id: thread.id.clone(),
                provider: ProviderId::Codex,
                request_id: 7,
            })
            .unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(event @ EngineEvent::ModelsLoaded { .. }) = event_receiver.try_recv() {
                    break event;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("model event arrives");
        match event {
            EngineEvent::ModelsLoaded {
                thread_id,
                provider,
                request_id,
                models,
                live,
                cached: _,
                error,
            } => {
                assert_eq!(live, 1, "one live model came from the adapter");
                assert_eq!(thread_id, thread.id);
                assert_eq!(provider, ProviderId::Codex);
                assert_eq!(request_id, 7);
                assert_eq!(models.first().map(String::as_str), Some("live:/tmp/models"));
                for known in crate::models::catalog(ProviderId::Codex) {
                    assert!(
                        models.contains(&(*known).to_string()),
                        "known model {known} is missing from {models:?}"
                    );
                }
                assert_eq!(
                    models.len(),
                    1 + crate::models::catalog(ProviderId::Codex).len()
                );
                assert!(error.is_none());
            }
            _ => unreachable!(),
        }
        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[tokio::test]
    async fn engine_reads_rate_limits_without_a_turn() {
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "limits".into(),
                cwd: "/tmp/limits".into(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(Box::new(
            IdleAdapter,
        )
            as Box<dyn BatonAdapter>))];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store,
            std::path::PathBuf::from("/tmp"),
            adapters,
            Vec::new(),
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));
        command_sender
            .send(EngineCommand::ReadRateLimits {
                cwd: thread.cwd.clone(),
                provider: ProviderId::Codex,
                request_id: 9,
            })
            .unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(event @ EngineEvent::RateLimitsLoaded { .. }) = event_receiver.try_recv()
                {
                    break event;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("rate-limit event arrives");
        match event {
            EngineEvent::RateLimitsLoaded {
                provider,
                request_id,
                info,
                error,
            } => {
                assert_eq!(provider, ProviderId::Codex);
                assert_eq!(request_id, 9);
                assert!(error.is_none());
                let info = info.expect("adapter returned a snapshot");
                assert_eq!(info.plan_type.as_deref(), Some("plus"));
                assert_eq!(info.primary.expect("primary").used_percent, 12);
            }
            _ => unreachable!(),
        }
        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[tokio::test]
    async fn engine_rejects_overlapping_turns_for_one_thread() {
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "t".into(),
                cwd: "/tmp".into(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(
            Box::new(ScriptAdapter { mode: AdapterMode::Slow }) as Box<dyn BatonAdapter>
        ))];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store.clone(),
            std::path::PathBuf::from("/tmp"),
            adapters,
            Vec::new(),
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));
        for text in ["first", "second"] {
            command_sender
                .send(EngineCommand::Submit {
                    thread_id: thread.id.clone(),
                    text: text.into(),
                    permission_mode: PermissionMode::Default,
                    skip_handoff: false,
                    expected_provider: None,
                    retry: false,
                    drain_queued: false,
                })
                .unwrap();
        }

        let mut rejected = None;
        let mut finished = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline && (rejected.is_none() || finished == 0) {
            while let Ok(event) = event_receiver.try_recv() {
                match event {
                    EngineEvent::TurnRejected { text, .. } => rejected = Some(text),
                    EngineEvent::TurnFinished { .. } => finished += 1,
                    _ => {}
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(rejected.as_deref(), Some("second"));
        assert_eq!(
            finished, 1,
            "only the accepted turn finishes (rejected: {rejected:?})"
        );
        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[tokio::test]
    async fn retry_admission_rejects_a_changed_provider_before_starting() {
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "retry".into(),
                cwd: "/tmp".into(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (started_sender, mut started_receiver) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let adapters = vec![gate_shared(ProviderId::Claude, &started_sender, &release)];
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store.clone(),
            std::path::PathBuf::from("/tmp"),
            adapters,
            Vec::new(),
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));
        command_sender
            .send(EngineCommand::Submit {
                thread_id: thread.id.clone(),
                text: "retry seed".into(),
                permission_mode: PermissionMode::Default,
                skip_handoff: true,
                expected_provider: Some(ProviderId::Claude),
                retry: true,
                drain_queued: false,
            })
            .unwrap();

        let event = wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnRejected { retry: true, .. })
        })
        .await;
        match event {
            EngineEvent::TurnRejected {
                text,
                reason,
                retry,
                ..
            } => {
                assert_eq!(text, "retry seed");
                assert!(retry);
                assert!(reason.contains("harness changed"), "{reason}");
            }
            _ => panic!("expected a retry rejection"),
        }
        assert!(store
            .lock()
            .unwrap()
            .get_turn_record(&thread.id, 1)
            .unwrap()
            .is_none());
        assert!(store
            .lock()
            .unwrap()
            .list_messages(&thread.id)
            .unwrap()
            .is_empty());
        assert!(started_receiver.try_recv().is_err());
        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[tokio::test]
    async fn a_turn_without_a_terminal_event_is_unverified() {
        let (store, thread_id, result) = run_scripted_turn(AdapterMode::Silent).await;
        let error = result.expect_err("a silent adapter must fail the turn");
        assert!(error.to_string().contains("without a result"), "{error}");
        let messages = store.lock().unwrap().list_messages(&thread_id).unwrap();
        assert!(
            !messages.iter().any(|message| message.role == "assistant"),
            "no empty assistant reply may be stored"
        );
        let turn = store
            .lock()
            .unwrap()
            .get_turn_record(&thread_id, 1)
            .unwrap()
            .expect("turn record");
        assert_eq!(turn.phase, TurnPhase::Unverified);
    }

    #[tokio::test]
    async fn a_panicking_turn_releases_the_thread_and_reports_unverified() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "panic".into(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(
            Box::new(ScriptAdapter { mode: AdapterMode::Panic }) as Box<dyn BatonAdapter>,
        ))];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store.clone(),
            home.path().to_path_buf(),
            adapters,
            Vec::new(),
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));
        for text in ["first", "second"] {
            command_sender
                .send(EngineCommand::Submit {
                    thread_id: thread.id.clone(),
                    text: text.into(),
                    permission_mode: PermissionMode::Default,
                    skip_handoff: false,
                    expected_provider: None,
                    retry: false,
                    drain_queued: false,
                })
                .unwrap();
            let event = wait_engine_event(&event_receiver, |event| {
                matches!(
                    event,
                    EngineEvent::TurnFinished { thread_id, phase, .. }
                        if thread_id == &thread.id && *phase == TurnPhase::Unverified
                )
            })
            .await;
            assert!(matches!(
                event,
                EngineEvent::TurnFinished { ok: false, phase: TurnPhase::Unverified, .. }
            ));
        }
        assert!(
            !event_receiver.try_iter().any(|event| matches!(event, EngineEvent::TurnRejected { .. })),
            "the second submit must be accepted after the first panic"
        );
        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[tokio::test]
    async fn a_refused_submit_reports_the_open_turn_without_finishing_it() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "owned".into(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        store
            .lock()
            .unwrap()
            .start_turn_record(&thread.id, 1, ProviderId::Codex, Some("m"))
            .unwrap();
        let adapters: Vec<SharedAdapter> =
            vec![Arc::new(tokio::sync::Mutex::new(Box::new(ScriptAdapter {
                mode: AdapterMode::Complete,
            })
                as Box<dyn BatonAdapter>))];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store.clone(),
            home.path().to_path_buf(),
            adapters,
            Vec::new(),
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));
        command_sender
            .send(EngineCommand::Submit {
                thread_id: thread.id.clone(),
                text: "second".into(),
                permission_mode: PermissionMode::Default,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            })
            .unwrap();
        let event = wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnRejected { .. })
        })
        .await;
        match event {
            EngineEvent::TurnRejected { reason, .. } => {
                assert!(reason.contains("already running"), "{reason}");
            }
            _ => unreachable!(),
        }
        assert!(
            !event_receiver
                .try_iter()
                .any(|event| matches!(event, EngineEvent::TurnFinished { .. })),
            "a refused submit must not settle another turn"
        );
        let record = store
            .lock()
            .unwrap()
            .get_turn_record(&thread.id, 1)
            .unwrap()
            .expect("the original turn still exists");
        assert_eq!(record.phase, TurnPhase::Running);
        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[tokio::test]
    async fn a_queued_drain_submit_is_admitted_with_remaining_queue_rows() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "drain".into(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: Some("m".into()),
            })
            .unwrap();
        store
            .lock()
            .unwrap()
            .enqueue_message(&thread.id, "queued one")
            .unwrap();
        store
            .lock()
            .unwrap()
            .enqueue_message(&thread.id, "queued two")
            .unwrap();
        let adapters: Vec<SharedAdapter> =
            vec![Arc::new(tokio::sync::Mutex::new(Box::new(ScriptAdapter {
                mode: AdapterMode::Complete,
            })
                as Box<dyn BatonAdapter>))];
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let engine = tokio::spawn(run_engine(
            store.clone(),
            home.path().to_path_buf(),
            adapters,
            Vec::new(),
            Vec::new(),
            command_receiver,
            event_sender,
            turn_sender,
        ));
        command_sender
            .send(EngineCommand::Submit {
                thread_id: thread.id.clone(),
                text: "queued one".into(),
                permission_mode: PermissionMode::Default,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: true,
            })
            .unwrap();
        wait_engine_event(&event_receiver, |event| {
            matches!(event, EngineEvent::TurnFinished { thread_id, phase, .. }
                if thread_id == &thread.id && *phase == TurnPhase::Completed)
        })
        .await;
        let messages = store.lock().unwrap().list_messages(&thread.id).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].text, "queued one");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].text, "done");
        assert_eq!(
            store
                .lock()
                .unwrap()
                .list_queued_messages(&thread.id)
                .unwrap()
                .len(),
            2
        );
        command_sender.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
    }

    #[test]
    fn finished_guard_does_not_remove_a_newer_turn_claim() {
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let active = Arc::new(Mutex::new(HashSet::from(["thread-1".to_string()])));
        let cancellations: Arc<
            Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>,
        > = Arc::new(Mutex::new(HashMap::new()));
        let (old_sender, _old_receiver) = tokio::sync::watch::channel(false);
        cancellations
            .lock()
            .unwrap()
            .insert("thread-1".into(), old_sender);
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let mut guard = TurnRunGuard::new(
            store,
            active.clone(),
            cancellations.clone(),
            event_sender,
            "thread-1".into(),
            1,
        );
        guard.finish(TurnPhase::Completed, None);
        assert!(matches!(
            event_receiver.try_recv(),
            Ok(EngineEvent::TurnFinished { .. })
        ));
        assert!(!active.lock().unwrap().contains("thread-1"));

        // A new turn for the same thread can start as soon as finish releases
        // the claim, before the old guard's Drop runs.
        active.lock().unwrap().insert("thread-1".into());
        let (new_sender, _new_receiver) = tokio::sync::watch::channel(false);
        cancellations
            .lock()
            .unwrap()
            .insert("thread-1".into(), new_sender);
        drop(guard);
        assert!(active.lock().unwrap().contains("thread-1"));
        assert!(cancellations.lock().unwrap().contains_key("thread-1"));
        assert!(
            event_receiver.try_recv().is_err(),
            "finish must emit exactly one terminal event"
        );
    }

    #[tokio::test]
    async fn a_completed_turn_is_recorded() {
        let (store, thread_id, result) = run_scripted_turn(AdapterMode::Complete).await;
        result.expect("turn completes");
        let turn = store
            .lock()
            .unwrap()
            .get_turn_record(&thread_id, 1)
            .unwrap()
            .expect("turn record");
        assert_eq!(turn.phase, TurnPhase::Completed);
        assert!(turn.ended_at.is_some());
        assert_eq!(turn.provider_turn_id.as_deref(), Some("provider-turn-1"));
        let messages = store.lock().unwrap().list_messages(&thread_id).unwrap();
        assert!(messages.iter().any(|message| message.role == "assistant" && message.text == "done"));
        let events = store.lock().unwrap().list_audit_events(10).unwrap();
        let submit = events
            .iter()
            .find(|event| event.action == "turn.submit")
            .expect("the engine writes one turn.submit row");
        assert_eq!(submit.actor, crate::audit::HOST_ENGINE_ACTOR);
        assert_eq!(submit.scope, crate::audit::OPERATE_SCOPE);
        assert_eq!(submit.target.as_deref(), Some(thread_id.as_str()));
        assert_eq!(submit.confirmation, crate::audit::HOST_SESSION_CONFIRMATION);
        assert_eq!(submit.thread_id.as_deref(), Some(thread_id.as_str()));
        assert_eq!(
            events.len(),
            1,
            "one accepted turn writes one row: {events:?}"
        );
    }

    #[tokio::test]
    async fn subagent_events_are_persisted_with_the_turn() {
        let (store, thread_id, result) = run_scripted_turn(AdapterMode::Subagents).await;
        result.expect("scripted subagent turn completes");
        let rows = store.lock().unwrap().list_subagents(&thread_id).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Explore");
        assert_eq!(rows[0].status, "done");
        assert_eq!(rows[0].detail.as_deref(), Some("found the parser"));
        assert!(rows[0].finished_at.is_some());
        assert_eq!(rows[1].name, "review");
        assert_eq!(
            rows[1].status, "unknown",
            "a child still running at turn end is not reported as running forever"
        );
        assert!(rows[1].finished_at.is_none());
    }

    #[tokio::test]
    async fn a_provider_session_is_persisted_even_when_the_turn_fails() {
        let (store, thread_id, result) = run_scripted_turn(AdapterMode::Fail).await;
        assert!(result.is_err());
        let session = store
            .lock()
            .unwrap()
            .get_provider_session(&thread_id, ProviderId::Codex)
            .unwrap()
            .expect("session persisted before the failure");
        assert_eq!(session.native_session_id, "native-fail");
    }

    #[tokio::test]
    async fn an_interrupted_turn_is_recorded_as_interrupted() {
        let (store, thread_id, result) = run_scripted_turn(AdapterMode::Interrupt).await;
        let error = result.expect_err("interrupted turn");
        assert_eq!(error.to_string(), "stopped by user");
        let turn = store
            .lock()
            .unwrap()
            .get_turn_record(&thread_id, 1)
            .unwrap()
            .expect("turn record");
        assert_eq!(turn.phase, TurnPhase::Interrupted);
        assert_eq!(turn.error.as_deref(), Some("stopped by user"));
    }

    #[tokio::test]
    async fn a_failed_turn_is_recorded() {
        let (store, thread_id, result) = run_scripted_turn(AdapterMode::Fail).await;
        let error = result.expect_err("turn fails");
        assert_eq!(error.to_string(), "provider died");
        let turn = store
            .lock()
            .unwrap()
            .get_turn_record(&thread_id, 1)
            .unwrap()
            .expect("turn record");
        assert_eq!(turn.phase, TurnPhase::Failed);
        assert_eq!(turn.error.as_deref(), Some("provider died"));
    }

    #[test]
    fn tool_events_pair_into_ordered_records() {
        let mut records = Vec::new();
        record_tool_event(&mut records, &TurnEvent::ToolStarted { id: Some("c1".into()), tool: "shell".into(), detail: Some("cargo test".into()) });
        record_tool_event(&mut records, &TurnEvent::ToolStarted { id: Some("c2".into()), tool: "edit".into(), detail: Some("src/lib.rs".into()) });
        record_tool_event(&mut records, &TurnEvent::ToolCompleted {
            id: Some("c1".into()),
            tool: "shell".into(),
            detail: Some("cargo test".into()),
            exit_code: Some(101),
        });
        record_tool_event(&mut records, &TurnEvent::ToolCompleted {
            id: Some("c2".into()),
            tool: "edit".into(),
            detail: Some("src/lib.rs".into()),
            exit_code: None,
        });
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].label, "shell cargo test");
        assert_eq!(records[0].ok, Some(false), "non-zero exit marks the record failed");
        assert_eq!(records[1].label, "edit src/lib.rs");
        assert_eq!(records[1].ok, Some(true));

        // a completion with no running match becomes its own record
        record_tool_event(&mut records, &TurnEvent::ToolCompleted { id: Some("c3".into()), tool: "shell".into(), detail: Some("ls".into()), exit_code: Some(0) });
        assert_eq!(records.len(), 3);
        assert_eq!(records[2].label, "shell ls");
        assert_eq!(records[2].ok, Some(true));
    }

    #[test]
    fn subagent_events_pair_into_records() {
        let mut records = Vec::new();
        record_subagent_event(
            &mut records,
            &TurnEvent::SubagentStarted {
                id: Some("s1".into()),
                name: "Explore".into(),
                detail: Some("parser".into()),
            },
        );
        record_subagent_event(
            &mut records,
            &TurnEvent::SubagentStarted {
                id: Some("s1".into()),
                name: "Explore".into(),
                detail: Some("parser".into()),
            },
        );
        assert_eq!(records.len(), 1, "a repeated start updates the same child");
        assert_eq!(records[0].status, "running");
        assert!(records[0].finished_at.is_none());
        record_subagent_event(
            &mut records,
            &TurnEvent::SubagentFinished {
                id: Some("s1".into()),
                name: "Explore".into(),
                status: "done".into(),
                detail: Some("found the parser".into()),
            },
        );
        assert_eq!(records[0].status, "done");
        assert_eq!(records[0].detail.as_deref(), Some("found the parser"));
        assert!(records[0].finished_at.is_some());

        record_subagent_event(
            &mut records,
            &TurnEvent::SubagentFinished {
                id: None,
                name: "review".into(),
                status: "interrupted".into(),
                detail: None,
            },
        );
        assert_eq!(
            records.len(),
            2,
            "a finish without a start becomes its own record"
        );
        assert_eq!(records[1].status, "interrupted");
        assert!(records[1].finished_at.is_some());
    }

    #[test]
    fn repeated_tool_started_updates_the_same_call_id() {
        let mut records = Vec::new();
        record_tool_event(
            &mut records,
            &TurnEvent::ToolStarted {
                id: Some("call-1".into()),
                tool: "read".into(),
                detail: None,
            },
        );
        record_tool_event(
            &mut records,
            &TurnEvent::ToolStarted {
                id: Some("call-1".into()),
                tool: "read".into(),
                detail: Some("/work".into()),
            },
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].label, "read /work");
    }

    #[test]
    fn identical_tool_labels_pair_by_call_id() {
        let mut records = Vec::new();
        record_tool_event(
            &mut records,
            &TurnEvent::ToolStarted {
                id: Some("call-1".into()),
                tool: "shell".into(),
                detail: Some("ls".into()),
            },
        );
        record_tool_event(
            &mut records,
            &TurnEvent::ToolStarted {
                id: Some("call-2".into()),
                tool: "shell".into(),
                detail: Some("ls".into()),
            },
        );
        record_tool_event(
            &mut records,
            &TurnEvent::ToolCompleted {
                id: Some("call-2".into()),
                tool: "shell".into(),
                detail: Some("ls".into()),
                exit_code: Some(1),
            },
        );

        assert_eq!(records[0].ok, None, "the first call is still running");
        assert_eq!(records[1].ok, Some(false), "the second call settled by id");
    }

    #[test]
    fn tool_records_persist_as_transcript_messages() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "t".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let records = vec![
            ToolRecord { id: None, label: "shell cargo test".into(), ok: Some(false) },
            ToolRecord { id: None, label: "edit src/lib.rs".into(), ok: Some(true) },
        ];
        persist_tool_records(&store, &thread.id, 1, ProviderId::Codex, &records).unwrap();
        store.add_message(&thread.id, 1, "assistant", Some("codex"), None, "done").unwrap();

        let messages = store.list_messages(&thread.id).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "tool");
        assert_eq!(messages[0].text, "✗ shell cargo test");
        assert_eq!(messages[1].text, "✓ edit src/lib.rs");
        assert_eq!(messages[2].role, "assistant", "tool history lands before the reply");
    }
}
