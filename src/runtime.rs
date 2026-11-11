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

/// One tool invocation of a turn, kept for the conversation history.
struct ToolRecord {
    label: String,
    /// None while running, Some(false) when it failed.
    ok: Option<bool>,
}

/// Pairs ToolStarted/ToolCompleted events the same way the live TUI list does:
/// a completion settles the most recent still-running record with that label.
fn record_tool_event(records: &mut Vec<ToolRecord>, event: &TurnEvent) {
    match event {
        TurnEvent::ToolStarted { tool, detail } => {
            let label = match detail {
                Some(detail) => format!("{tool} {detail}"),
                None => tool.clone(),
            };
            records.push(ToolRecord { label, ok: None });
        }
        TurnEvent::ToolCompleted { tool, detail, exit_code } => {
            let label = match detail {
                Some(detail) => format!("{tool} {detail}"),
                None => tool.clone(),
            };
            let failed = exit_code.map(|code| code != 0).unwrap_or(false);
            match records.iter_mut().rev().find(|record| record.ok.is_none() && record.label == label) {
                Some(record) => record.ok = Some(!failed),
                None => records.push(ToolRecord { label, ok: Some(!failed) }),
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
    mut command_receiver: tokio::sync::mpsc::UnboundedReceiver<EngineCommand>,
    engine_event_sender: std::sync::mpsc::Sender<EngineEvent>,
    turn_event_sender: TurnEventSender,
) -> Result<()> {
    while let Some(command) = command_receiver.recv().await {
        match command {
            EngineCommand::Quit => {
                for adapter in &adapters {
                    let _ = adapter.lock().await.dispose().await;
                }
                break;
            }
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
    let tools_slot: Arc<Mutex<Vec<ToolRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let outcome_for_thread = outcome_slot.clone();
    let failure_for_thread = failure_slot.clone();
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
    let _ = adapter_result;
    let _ = forwarder.join();

    let tool_records = tools_slot.lock().unwrap().drain(..).collect::<Vec<_>>();
    if !tool_records.is_empty() {
        let store = store.lock().unwrap();
        persist_tool_records(&store, thread_id, turn, provider, &tool_records)?;
    }

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
        // Non-TUI consumers (baton exec) read the final text from the engine
        // event channel; the TUI ignores EngineEvent::Turn.
        let _ = engine_event_sender.send(EngineEvent::Turn(TurnEvent::TurnCompleted {
            final_text: outcome.final_text,
            usage: outcome.usage,
            native_session_id: outcome.native_session_id,
            rollout_path: outcome.rollout_path,
        }));
    }
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

    #[test]
    fn tool_events_pair_into_ordered_records() {
        let mut records = Vec::new();
        record_tool_event(&mut records, &TurnEvent::ToolStarted { tool: "shell".into(), detail: Some("cargo test".into()) });
        record_tool_event(&mut records, &TurnEvent::ToolStarted { tool: "edit".into(), detail: Some("src/lib.rs".into()) });
        record_tool_event(&mut records, &TurnEvent::ToolCompleted {
            tool: "shell".into(),
            detail: Some("cargo test".into()),
            exit_code: Some(101),
        });
        record_tool_event(&mut records, &TurnEvent::ToolCompleted {
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
        record_tool_event(&mut records, &TurnEvent::ToolCompleted { tool: "shell".into(), detail: Some("ls".into()), exit_code: Some(0) });
        assert_eq!(records.len(), 3);
        assert_eq!(records[2].label, "shell ls");
        assert_eq!(records[2].ok, Some(true));
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
            ToolRecord { label: "shell cargo test".into(), ok: Some(false) },
            ToolRecord { label: "edit src/lib.rs".into(), ok: Some(true) },
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
