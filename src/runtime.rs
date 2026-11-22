use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::adapters::{BatonAdapter, ModelListInput, SteerHandle, SteerOutcome, SteerRequest, TurnEvent, TurnStartInput};
use crate::checkpoints::capture_checkpoint;
use crate::config::{provider_auth_env, resolve_config};
use crate::handoff::{derive_title, git_status_summary, HandoffMessage};
use crate::store::{BatonStore, NewThread, ThreadRow};
use crate::types::{PermissionMode, ProviderId, TurnPhase, TurnUsage};

pub type SharedStore = Arc<Mutex<BatonStore>>;
pub type SharedAdapter = Arc<tokio::sync::Mutex<Box<dyn BatonAdapter>>>;
pub type ThreadTurnEventSender = std::sync::mpsc::Sender<(String, TurnEvent)>;

pub enum EngineCommand {
    Submit { thread_id: String, text: String, permission_mode: PermissionMode },
    Steer { thread_id: String, text: String },
    ListModels { thread_id: String, provider: ProviderId, request_id: u64 },
    Interrupt { thread_id: String },
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
        error: Option<String>,
    },
    TurnRejected { thread_id: String, text: String, reason: String },
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
    fn start(
        store: &SharedStore,
        thread_id: &str,
        turn: i64,
        provider: ProviderId,
        model: &str,
    ) -> Result<TurnLedger> {
        store
            .lock()
            .unwrap()
            .start_turn_record(thread_id, turn, provider, Some(model))?;
        Ok(TurnLedger {
            store: store.clone(),
            thread_id: thread_id.to_string(),
            turn,
            settled: false,
        })
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
        let (phase, error) = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_turn_record(&self.thread_id, self.expected_turn)
            .ok()
            .flatten()
            .map(|record| (record.phase, record.error))
            .unwrap_or_else(|| {
                (
                    TurnPhase::Unverified,
                    Some("turn task ended without a terminal state".to_string()),
                )
            });
        let ok = matches!(phase, TurnPhase::Completed);
        let _ = self.engine_event_sender.send(EngineEvent::TurnFinished {
            thread_id: self.thread_id.clone(),
            ok,
            error,
            phase,
        });
    }
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
                        (thread.current_provider, model)
                    })
                };
                let claim = resolved.as_ref().and_then(|(provider, _)| {
                    provider_claims.lock().unwrap().get(provider).cloned()
                });
                let handle = resolved.as_ref().and_then(|(provider, _)| {
                    steer_handles
                        .iter()
                        .find(|(candidate, _)| candidate == provider)
                        .map(|(_, handle)| handle.clone())
                });
                let Some((provider, model)) = resolved else {
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
                tokio::spawn(async move {
                    let outcome = handle(SteerRequest {
                        thread_id: thread_id.clone(),
                        turn,
                        text: text.clone(),
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
                    let (models, error) = match result {
                        Ok(models) => (models, None),
                        Err(error) => (Vec::new(), Some(error.to_string())),
                    };
                    let _ = sender.send(EngineEvent::ModelsLoaded {
                        thread_id,
                        provider,
                        request_id,
                        models,
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
            EngineCommand::Submit { thread_id, text, permission_mode } => {
                let accepted = active_threads
                    .lock()
                    .unwrap()
                    .insert(thread_id.clone());
                if !accepted {
                    let _ = engine_event_sender.send(EngineEvent::TurnRejected {
                        thread_id,
                        text,
                        reason: "a turn is already running for this thread".into(),
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
        &engine_event_sender,
        &turn_event_sender,
    )
    .await;
    let record = if expected_turn > 0 {
        store
            .lock()
            .unwrap()
            .get_turn_record(&thread_id, expected_turn)
            .unwrap_or(None)
    } else {
        None
    };
    let phase = record
        .as_ref()
        .map(|record| record.phase)
        .unwrap_or(TurnPhase::Unverified);
    let error = record
        .as_ref()
        .and_then(|record| record.error.clone())
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
) -> Result<Vec<String>> {
    let cwd = {
        let store = store.lock().unwrap();
        store
            .get_thread(thread_id)?
            .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))?
            .cwd
    };
    let config = resolve_config(home, std::path::Path::new(&cwd));
    if provider == ProviderId::Claude {
        let models = crate::harness_config::read_claude_model_catalog(
            &crate::paths::claude_global_settings_dir(),
        );
        if !models.is_empty() {
            return Ok(models);
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
                .await
                .map_err(|_| anyhow::anyhow!("{provider} model catalog timed out"))??;
                return Ok(models);
            }
        }
    }
    anyhow::bail!("{provider} is busy; keeping the cached model list")
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
    engine_event_sender: &std::sync::mpsc::Sender<EngineEvent>,
    turn_event_sender: &ThreadTurnEventSender,
) -> Result<()> {
    let (provider, cwd, thread_title, thread_model, reasoning_effort) = {
        let store = store.lock().unwrap();
        let thread = store
            .get_thread(thread_id)?
            .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))?;
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
        let prior = store.list_messages(thread_id)?;
        let turn = store.next_turn_number(thread_id)?;
        store.add_message(thread_id, turn, "user", Some(provider.as_str()), Some(&model), text)?;
        let session = store.get_provider_session(thread_id, provider)?;
        (prior, turn, session)
    };

    let mut turn_ledger = TurnLedger::start(store, thread_id, turn, provider, &model)?;

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

    let mut prompt = prompt_text.clone();
    // A native session has already read the turns up to its marker. Sending it
    // again would spend cache and repeat context, so only unseen turns travel.
    let covered_turn = session.as_ref().map(|row| row.covered_turn).unwrap_or(0);
    let unseen: Vec<&crate::store::MessageRow> = prior_messages
        .iter()
        .filter(|message| message.turn > covered_turn)
        .collect();
    if !unseen.is_empty() {
        let handoff_messages: Vec<HandoffMessage> = unseen
            .iter()
            .map(|message| HandoffMessage {
                role: message.role.clone(),
                provider: message.provider.clone(),
                model: message.model.clone(),
                text: message.text.clone(),
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
    let start_captured = {
        let start_path = std::path::PathBuf::from(&cwd_for_checkpoint);
        start_path.exists() && {
            let start_ref = crate::checkpoints::checkpoint_start_ref(thread_id, turn);
            crate::checkpoints::capture_checkpoint_at(
                &start_path,
                &start_ref,
                &format!("baton checkpoint {thread_id} turn {turn} start"),
            )
            .is_ok()
        }
    };

    let (event_sender, event_receiver) = std::sync::mpsc::channel::<TurnEvent>();
    let tui_sender = turn_event_sender.clone();
    let event_thread_id = thread_id.to_string();
    let outcome_slot: Arc<Mutex<Option<TurnOutcome>>> = Arc::new(Mutex::new(None));
    let failure_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let interrupted_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let tools_slot: Arc<Mutex<Vec<ToolRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let outcome_for_thread = outcome_slot.clone();
    let failure_for_thread = failure_slot.clone();
    let interrupted_for_thread = interrupted_slot.clone();
    let tools_for_thread = tools_slot.clone();
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
            if let Ok(reference) = capture_checkpoint(&cwd_path, thread_id, turn) {
                store
                    .lock()
                    .unwrap()
                    .add_checkpoint(thread_id, turn, &reference)?;
                // The turn's own changes, kept in the conversation as a diff.
                if start_captured {
                    let start = crate::checkpoints::checkpoint_start_ref(thread_id, turn);
                    if let Ok(diff) =
                        crate::checkpoints::diff_checkpoints(&cwd_path, &start, &reference)
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

    #[derive(Clone, Copy)]
    enum AdapterMode {
        Silent,
        Complete,
        Fail,
        Interrupt,
        Slow,
        Panic,
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
            &engine_sender,
            &turn_sender,
        )
        .await;
        (store, thread.id, result)
    }

    struct CapturingAdapter {
        prompt: Arc<Mutex<Option<String>>>,
        session_id: Option<String>,
    }

    #[async_trait::async_trait]
    impl BatonAdapter for CapturingAdapter {
        fn id(&self) -> ProviderId {
            ProviderId::Codex
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
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(
            Box::new(CapturingAdapter {
                prompt: captured.clone(),
                session_id: None,
            }) as Box<dyn BatonAdapter>,
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
            "hello $memories",
            PermissionMode::Default,
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
            })
            .unwrap();
        assert_eq!(next_started(&mut started_receiver).await, first.id);

        command_sender
            .send(EngineCommand::Submit {
                thread_id: second.id.clone(),
                text: "second prompt".into(),
                permission_mode: PermissionMode::Default,
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
            })
            .unwrap();
        assert_eq!(next_started(&mut started_receiver).await, first.id);
        command_sender
            .send(EngineCommand::Submit {
                thread_id: second.id.clone(),
                text: "second prompt".into(),
                permission_mode: PermissionMode::Default,
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
        let ledger = TurnLedger::start(&store, &thread.id, 1, ProviderId::Codex, "m").unwrap();
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
        let adapters: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(
            Box::new(IdleAdapter) as Box<dyn BatonAdapter>
        ))];
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
                error,
            } => {
                assert_eq!(thread_id, thread.id);
                assert_eq!(provider, ProviderId::Codex);
                assert_eq!(request_id, 7);
                assert_eq!(models, vec!["live:/tmp/models".to_string()]);
                assert!(error.is_none());
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
