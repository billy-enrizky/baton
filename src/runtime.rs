use anyhow::Result;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::adapters::{BatonAdapter, SteerHandle, TurnEvent, TurnEventSender, TurnStartInput};
use crate::checkpoints::capture_checkpoint;
use crate::config::{provider_auth_env, resolve_config};
use crate::handoff::{build_handoff, derive_title, git_status_summary, wrap_prompt_with_handoff, HandoffMessage};
use crate::store::{BatonStore, NewThread, ThreadRow};
use crate::types::{PermissionMode, ProviderId, TurnPhase, TurnUsage};

pub type SharedStore = Arc<Mutex<BatonStore>>;
pub type SharedAdapter = Arc<tokio::sync::Mutex<Box<dyn BatonAdapter>>>;

pub enum EngineCommand {
    Submit { thread_id: String, text: String, permission_mode: PermissionMode },
    Steer { thread_id: String, text: String },
    Interrupt,
    Quit,
}

pub enum EngineEvent {
    UserEcho { thread_id: String, turn: i64, text: String, provider: ProviderId, model: Option<String> },
    TurnStarted { thread_id: String },
    Turn(TurnEvent),
    SteerRejected { thread_id: String, text: String },
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
}

impl Drop for TurnLedger {
    fn drop(&mut self) {
        if !self.settled {
            self.settle(
                TurnPhase::Failed,
                Some("turn ended without a terminal state"),
            );
        }
    }
}

fn phase_for_error(error: &str) -> TurnPhase {
    if error.to_ascii_lowercase().contains("interrupt") {
        TurnPhase::Interrupted
    } else {
        TurnPhase::Failed
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
    interrupt_handles: Vec<Arc<dyn Fn() + Send + Sync>>,
    steer_handles: Vec<(ProviderId, SteerHandle)>,
    mut command_receiver: tokio::sync::mpsc::UnboundedReceiver<EngineCommand>,
    engine_event_sender: std::sync::mpsc::Sender<EngineEvent>,
    turn_event_sender: TurnEventSender,
) -> Result<()> {
    let active_threads: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    while let Some(command) = command_receiver.recv().await {
        match command {
            EngineCommand::Quit => {
                for adapter in &adapters {
                    let _ = adapter.lock().await.dispose().await;
                }
                break;
            }
            EngineCommand::Steer { thread_id, text } => {
                let provider = store
                    .lock()
                    .unwrap()
                    .get_thread(&thread_id)
                    .ok()
                    .flatten()
                    .map(|thread| thread.current_provider);
                let accepted = provider
                    .and_then(|provider| {
                        steer_handles
                            .iter()
                            .find(|(candidate, _)| *candidate == provider)
                            .map(|(_, handle)| handle(text.clone()))
                    })
                    .unwrap_or(false);
                if accepted {
                    let (provider, model, turn) = {
                        let store = store.lock().unwrap();
                        let thread = store.get_thread(&thread_id).ok().flatten();
                        let turn = store.next_turn_number(&thread_id).unwrap_or(1).saturating_sub(1);
                        thread
                            .map(|thread| {
                                let model = thread
                                    .current_model
                                    .clone()
                                    .unwrap_or_else(|| crate::models::default_model(thread.current_provider).to_string());
                                (thread.current_provider, model, turn)
                            })
                            .unwrap_or((ProviderId::Codex, String::new(), turn))
                    };
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
                    let _ = engine_event_sender.send(EngineEvent::UserEcho {
                        thread_id,
                        turn,
                        text,
                        provider,
                        model: Some(model),
                    });
                } else {
                    let _ = engine_event_sender.send(EngineEvent::SteerRejected {
                        thread_id,
                        text,
                    });
                }
            }
            EngineCommand::Interrupt => {
                for handle in &interrupt_handles {
                    handle();
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
                tokio::spawn(run_turn(
                    store.clone(),
                    home.clone(),
                    adapters.clone(),
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
    thread_id: String,
    text: String,
    permission_mode: PermissionMode,
    engine_event_sender: std::sync::mpsc::Sender<EngineEvent>,
    turn_event_sender: TurnEventSender,
    active_threads: Arc<Mutex<HashSet<String>>>,
) {
    let expected_turn = store
        .lock()
        .unwrap()
        .next_turn_number(&thread_id)
        .unwrap_or(0);
    let result = execute_turn(&store, &home, &adapters, &thread_id, &text, permission_mode, &engine_event_sender, &turn_event_sender).await;
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
    let ok = matches!(phase, TurnPhase::Completed);
    active_threads.lock().unwrap().remove(&thread_id);
    let _ = engine_event_sender.send(EngineEvent::TurnFinished {
        thread_id,
        ok,
        error,
        phase,
    });
}

async fn execute_turn(
    store: &SharedStore,
    home: &std::path::PathBuf,
    adapters: &[SharedAdapter],
    thread_id: &str,
    text: &str,
    permission_mode: PermissionMode,
    engine_event_sender: &std::sync::mpsc::Sender<EngineEvent>,
    turn_event_sender: &TurnEventSender,
) -> Result<()> {
    let (provider, cwd, thread_title, thread_model) = {
        let store = store.lock().unwrap();
        let thread = store
            .get_thread(thread_id)?
            .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))?;
        (thread.current_provider, thread.cwd, thread.title, thread.current_model)
    };

    let config = resolve_config(home, std::path::Path::new(&cwd));
    let provider_config = config.for_provider(provider);
    let model = thread_model.clone().unwrap_or_else(|| provider_config.model.clone());
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
    let _ = engine_event_sender.send(EngineEvent::TurnStarted { thread_id: thread_id.to_string() });
    let _ = Instant::now();

    if thread_title == "New thread" {
        let title = derive_title(text);
        store.lock().unwrap().rename_thread(thread_id, &title)?;
    }

    let mut prompt = text.to_string();
    if session.is_none() && !prior_messages.is_empty() {
        let mut handoff_messages: Vec<HandoffMessage> = prior_messages
            .iter()
            .map(|message| HandoffMessage {
                role: message.role.clone(),
                provider: message.provider.clone(),
                model: message.model.clone(),
                text: message.text.clone(),
            })
            .collect();
        handoff_messages.push(HandoffMessage {
            role: "user".to_string(),
            provider: Some(provider.as_str().to_string()),
            model: Some(model.clone()),
            text: text.to_string(),
        });
        let git_status = git_status_summary(std::path::Path::new(&cwd), 50);
        let handoff = build_handoff(&cwd, &thread_title, &handoff_messages, git_status.as_deref(), None);
        prompt = wrap_prompt_with_handoff(&handoff, text);
    }

    let cwd_for_checkpoint = cwd.clone();
    let input = TurnStartInput {
        thread_id: thread_id.to_string(),
        cwd,
        model: model.clone(),
        prompt,
        permission_mode,
        auth_env,
    };

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

    let adapter = adapters
        .iter()
        .find(|adapter| adapter.try_lock().map(|adapter| adapter.id() == provider).unwrap_or(false))
        .ok_or_else(|| anyhow::anyhow!("no adapter for {provider}"))?;

    let (event_sender, event_receiver) = std::sync::mpsc::channel::<TurnEvent>();
    let tui_sender = turn_event_sender.clone();
    let outcome_slot: Arc<Mutex<Option<TurnOutcome>>> = Arc::new(Mutex::new(None));
    let failure_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let interrupted_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let tools_slot: Arc<Mutex<Vec<ToolRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let outcome_for_thread = outcome_slot.clone();
    let failure_for_thread = failure_slot.clone();
    let interrupted_for_thread = interrupted_slot.clone();
    let tools_for_thread = tools_slot.clone();
    let forwarder = std::thread::spawn(move || {
        for event in event_receiver {
            match &event {
                TurnEvent::TurnCompleted { final_text, usage, native_session_id, rollout_path } => {
                    *outcome_for_thread.lock().unwrap() = Some(TurnOutcome {
                        final_text: final_text.clone(),
                        usage: *usage,
                        native_session_id: native_session_id.clone(),
                        rollout_path: rollout_path.clone(),
                    });
                    let _ = tui_sender.send(event);
                    break;
                }
                TurnEvent::TurnInterrupted { reason } => {
                    *interrupted_for_thread.lock().unwrap() =
                        Some(reason.clone().unwrap_or_else(|| "turn interrupted".into()));
                    let _ = tui_sender.send(event);
                    break;
                }
                TurnEvent::TurnFailed { error } => {
                    *failure_for_thread.lock().unwrap() = Some(error.clone());
                    let _ = tui_sender.send(event);
                    break;
                }
                _ => {
                    record_tool_event(&mut tools_for_thread.lock().unwrap(), &event);
                    let _ = tui_sender.send(event);
                }
            }
        }
    });

    let session_ref = session.map(|session| crate::types::NativeSessionRef {
        native_session_id: session.native_session_id,
        rollout_path: session.rollout_path,
    });
    let adapter_result = {
        let mut adapter = adapter.lock().await;
        adapter.start_turn(input, session_ref, event_sender).await
    };
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
                store.upsert_provider_session(thread_id, provider, native_session_id, Some(model.clone()), outcome.rollout_path.clone())?;
            }
        }
        let cwd_path = std::path::PathBuf::from(&cwd_for_checkpoint);
        if cwd_path.exists() {
            if let Ok(reference) = capture_checkpoint(&cwd_path, thread_id, turn) {
                store.lock().unwrap().add_checkpoint(thread_id, turn, &reference)?;
                // The turn's own changes, kept in the conversation as a diff.
                if start_captured {
                    let start = crate::checkpoints::checkpoint_start_ref(thread_id, turn);
                    if let Ok(diff) = crate::checkpoints::diff_checkpoints(&cwd_path, &start, &reference) {
                        if !diff.trim().is_empty() {
                            let capped = cap_diff(&diff);
                            let _ = store.lock().unwrap().add_message(thread_id, turn, "diff", None, None, &capped);
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
        let created = store.create_thread_with_id(
            NewThread {
                title: thread.title.clone(),
                cwd: thread.cwd.clone(),
                current_provider: thread.current_provider,
                current_model: thread.current_model.clone(),
            },
            thread.id.clone(),
        )?;
        *thread = created;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum AdapterMode {
        Silent,
        Complete,
        Fail,
        Interrupt,
        Slow,
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
                    let _ = emit.send(TurnEvent::TurnCompleted {
                        final_text: "done".into(),
                        usage: None,
                        native_session_id: Some("native-1".into()),
                        rollout_path: None,
                    });
                }
                AdapterMode::Fail => {
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
        let (engine_sender, _engine_receiver) = std::sync::mpsc::channel();
        let (turn_sender, _turn_receiver) = std::sync::mpsc::channel();
        let result = execute_turn(
            &store,
            &home.path().to_path_buf(),
            &adapters,
            &thread.id,
            "hello",
            PermissionMode::Default,
            &engine_sender,
            &turn_sender,
        )
        .await;
        (store, thread.id, result)
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

    #[tokio::test]
    async fn engine_accepts_steering_and_rejects_it_without_a_handle() {
        use std::sync::atomic::{AtomicBool, Ordering};

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
            Box::new(IdleAdapter) as Box<dyn BatonAdapter>
        ))];
        let accepted = Arc::new(AtomicBool::new(false));
        let accepted_handle = accepted.clone();
        let steer: SteerHandle = Arc::new(move |text: String| {
            accepted_handle.store(text == "try this", Ordering::SeqCst);
            true
        });
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
        command_sender
            .send(EngineCommand::Steer {
                thread_id: thread.id.clone(),
                text: "try this".into(),
            })
            .unwrap();

        let echo = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(EngineEvent::UserEcho { text, .. }) = event_receiver.try_recv() {
                    break text;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("steer is accepted");
        assert_eq!(echo, "try this");
        assert!(accepted.load(Ordering::SeqCst));

        let messages = store.lock().unwrap().list_messages(&thread.id).unwrap();
        assert_eq!(messages.len(), 1, "steering text is persisted in the conversation");
        assert_eq!(messages[0].text, "try this");

        // A provider without a steer handle falls back to the queue event.
        let (command_sender2, command_receiver2) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender2, event_receiver2) = std::sync::mpsc::channel();
        let (turn_sender2, _turn_receiver2) = std::sync::mpsc::channel();
        let adapters2: Vec<SharedAdapter> = vec![Arc::new(tokio::sync::Mutex::new(
            Box::new(IdleAdapter) as Box<dyn BatonAdapter>
        ))];
        let engine2 = tokio::spawn(run_engine(
            store.clone(),
            std::path::PathBuf::from("/tmp"),
            adapters2,
            Vec::new(),
            Vec::new(),
            command_receiver2,
            event_sender2,
            turn_sender2,
        ));
        command_sender2
            .send(EngineCommand::Steer {
                thread_id: thread.id.clone(),
                text: "queue me".into(),
            })
            .unwrap();
        let rejected = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(EngineEvent::SteerRejected { text, .. }) = event_receiver2.try_recv() {
                    break text;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("missing handle rejects steering");
        assert_eq!(rejected, "queue me");

        command_sender.send(EngineCommand::Quit).unwrap();
        command_sender2.send(EngineCommand::Quit).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), engine2).await;
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
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
        assert_eq!(finished, 1, "only the accepted turn finishes");
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
        let messages = store.lock().unwrap().list_messages(&thread_id).unwrap();
        assert!(messages.iter().any(|message| message.role == "assistant" && message.text == "done"));
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
