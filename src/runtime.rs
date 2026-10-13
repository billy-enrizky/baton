use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::adapters::{BatonAdapter, TurnEvent, TurnEventSender, TurnStartInput};
use crate::checkpoints::capture_checkpoint;
use crate::config::{provider_auth_env, resolve_config};
use crate::handoff::{build_handoff, derive_title, git_status_summary, wrap_prompt_with_handoff, HandoffMessage};
use crate::store::{BatonStore, NewThread, ThreadRow};
use crate::types::{PermissionMode, ProviderId, TurnUsage};

pub type SharedStore = Arc<Mutex<BatonStore>>;
pub type SharedAdapter = Arc<tokio::sync::Mutex<Box<dyn BatonAdapter>>>;

pub enum EngineCommand {
    Submit { thread_id: String, text: String, permission_mode: PermissionMode },
    Interrupt,
    Quit,
}

pub enum EngineEvent {
    UserEcho { thread_id: String, turn: i64, text: String, provider: ProviderId, model: Option<String> },
    TurnStarted { thread_id: String },
    Turn(TurnEvent),
    TurnFinished { thread_id: String, ok: bool, error: Option<String> },
}

struct TurnOutcome {
    final_text: String,
    usage: Option<TurnUsage>,
    native_session_id: Option<String>,
    rollout_path: Option<String>,
}

pub async fn run_engine(
    store: SharedStore,
    home: std::path::PathBuf,
    adapters: Vec<SharedAdapter>,
    interrupt_handles: Vec<Arc<dyn Fn() + Send + Sync>>,
    mut command_receiver: tokio::sync::mpsc::UnboundedReceiver<EngineCommand>,
    engine_event_sender: std::sync::mpsc::Sender<EngineEvent>,
    turn_event_sender: TurnEventSender,
) -> Result<()> {
    while let Some(command) = command_receiver.recv().await {
        match command {
            EngineCommand::Quit => break,
            EngineCommand::Interrupt => {
                for handle in &interrupt_handles {
                    handle();
                }
            }
            EngineCommand::Submit { thread_id, text, permission_mode } => {
                tokio::spawn(run_turn(
                    store.clone(),
                    home.clone(),
                    adapters.clone(),
                    thread_id,
                    text,
                    permission_mode,
                    engine_event_sender.clone(),
                    turn_event_sender.clone(),
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
) {
    let result = execute_turn(&store, &home, &adapters, &thread_id, &text, permission_mode, &engine_event_sender, &turn_event_sender).await;
    let (ok, error) = match result {
        Ok(()) => (true, None),
        Err(error) => (false, Some(error.to_string())),
    };
    let _ = engine_event_sender.send(EngineEvent::TurnFinished {
        thread_id,
        ok,
        error,
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

    let adapter = adapters
        .iter()
        .find(|adapter| adapter.try_lock().map(|adapter| adapter.id() == provider).unwrap_or(false))
        .ok_or_else(|| anyhow::anyhow!("no adapter for {provider}"))?;

    let (event_sender, event_receiver) = std::sync::mpsc::channel::<TurnEvent>();
    let tui_sender = turn_event_sender.clone();
    let outcome_slot: Arc<Mutex<Option<TurnOutcome>>> = Arc::new(Mutex::new(None));
    let failure_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let outcome_for_thread = outcome_slot.clone();
    let failure_for_thread = failure_slot.clone();
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
                TurnEvent::TurnFailed { error } => {
                    *failure_for_thread.lock().unwrap() = Some(error.clone());
                    let _ = tui_sender.send(event);
                    break;
                }
                _ => {
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
    let _ = adapter_result;
    let _ = forwarder.join();

    if let Some(error) = failure_slot.lock().unwrap().take() {
        return Err(anyhow::anyhow!("{error}"));
    }

    if let Some(outcome) = outcome_slot.lock().unwrap().take() {
        {
            let store = store.lock().unwrap();
            store.add_message(thread_id, turn, "assistant", Some(provider.as_str()), Some(&model), &outcome.final_text)?;
            if let Some(usage) = outcome.usage {
                store.add_usage(thread_id, turn, provider, Some(&model), &usage)?;
            }
            if let Some(native_session_id) = outcome.native_session_id {
                store.upsert_provider_session(thread_id, provider, &native_session_id, Some(model.clone()), outcome.rollout_path.clone())?;
            }
        }
        let cwd_path = std::path::PathBuf::from(&cwd_for_checkpoint);
        if cwd_path.exists() {
            if let Ok(reference) = capture_checkpoint(&cwd_path, thread_id, turn) {
                store.lock().unwrap().add_checkpoint(thread_id, turn, &reference)?;
            }
        }
    }
    Ok(())
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
