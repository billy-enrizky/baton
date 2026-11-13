//! Provider process wiring shared by the CLI, MCP, and the TUI engine.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::adapters::opencode::{OpenCodeAdapter, OpenCodeChild, OpenCodeSpawner};
use crate::adapters::{
    BatonAdapter, CodexAdapter, ForkSessionInput, RevertSessionInput, TransportHalves,
};
use crate::config::{provider_auth_env, resolve_config};
use crate::process::{self, ProcessRecorder};
use crate::runtime::{SharedAdapter, SharedStore};
use crate::store::{BatonStore, ThreadRow};
use crate::types::{NativeSessionRef, ProviderId};

pub fn spawn_tracked_provider(
    mut command: tokio::process::Command,
    provider: ProviderId,
    recorder: &ProcessRecorder,
) -> Result<TransportHalves> {
    command.kill_on_drop(true);
    let child = command.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("{provider} did not expose a process id"))?;
    let command_line = process::command_line(pid).unwrap_or_default();
    let process_start = process::process_start(pid);
    // Start the guard before the durable record, so every recorded child has
    // a watchdog even if this process dies immediately after the write.
    let _ = process::spawn_parent_watchdog(pid);
    recorder.record(
        pid,
        provider,
        None,
        &command_line,
        process_start.as_deref(),
    )?;
    let recorder = recorder.clone();
    crate::adapters::child_halves_with_cleanup(
        child,
        Some(Box::new(move || recorder.forget(pid))),
    )
}

pub fn build_codex_adapter(store: &SharedStore, cwd: &PathBuf) -> SharedAdapter {
    let recorder = ProcessRecorder::new(store.clone(), cwd.to_string_lossy().to_string());
    build_codex_adapter_with_recorder(cwd, recorder)
}

pub fn build_codex_adapter_with_recorder(
    cwd: &PathBuf,
    recorder: ProcessRecorder,
) -> SharedAdapter {
    let cwd_for_codex = cwd.clone();
    let spawner = Arc::new(
        move |auth_env: &[(String, String)]| -> Result<TransportHalves> {
            let mut command = tokio::process::Command::new("codex");
            command.args(["app-server"]);
            command.current_dir(&cwd_for_codex);
            command.stdin(std::process::Stdio::piped());
            command.stdout(std::process::Stdio::piped());
            command.stderr(std::process::Stdio::null());
            for (key, value) in auth_env {
                command.env(key, value);
            }
            spawn_tracked_provider(command, ProviderId::Codex, &recorder)
        },
    );
    Arc::new(tokio::sync::Mutex::new(
        Box::new(CodexAdapter::new(spawner)) as Box<dyn BatonAdapter>,
    ))
}

pub fn build_opencode_adapter(store: &SharedStore, cwd: &PathBuf) -> SharedAdapter {
    let recorder = ProcessRecorder::new(store.clone(), cwd.to_string_lossy().to_string());
    build_opencode_adapter_with_recorder(cwd, recorder)
}

pub fn build_opencode_adapter_with_recorder(
    _cwd: &PathBuf,
    recorder: ProcessRecorder,
) -> SharedAdapter {
    let recorder_for_spawner = recorder.clone();
    let spawner: OpenCodeSpawner = Arc::new(
        move |_auth_env: &[(String, String)]| -> Result<OpenCodeChild> {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            drop(listener);
            let password = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            let mut command = tokio::process::Command::new("opencode");
            command.args(["serve", "--hostname=127.0.0.1", "--port", &port.to_string()]);
            command.env("OPENCODE_PASSWORD", &password);
            command.env("OPENCODE_SERVER_PASSWORD", &password);
            command.env("OPENCODE_CONFIG_CONTENT", "{}");
            command.stdin(std::process::Stdio::null());
            command.stdout(std::process::Stdio::piped());
            command.stderr(std::process::Stdio::null());
            // The server never sees stdin EOF (it is null), so it must die with baton.
            command.kill_on_drop(true);
            let mut child = command.spawn()?;
            let pid = child
                .id()
                .ok_or_else(|| anyhow::anyhow!("opencode did not expose a process id"))?;
            let command_line = process::command_line(pid).unwrap_or_else(|| {
                format!("opencode serve --hostname=127.0.0.1 --port {port}")
            });
            let process_start = process::process_start(pid);
            // Start the guard before the durable record, so every recorded
            // child has a watchdog even if this process dies after the write.
            let _ = process::spawn_parent_watchdog(pid);
            if let Err(error) = recorder_for_spawner.record(
                pid,
                ProviderId::OpenCode,
                Some(port),
                &command_line,
                process_start.as_deref(),
            ) {
                let _ = child.start_kill();
                return Err(error);
            }
            Ok(OpenCodeChild {
                port,
                password,
                child,
            })
        },
    );
    Arc::new(tokio::sync::Mutex::new(
        Box::new(OpenCodeAdapter::with_recorder(spawner, recorder)) as Box<dyn BatonAdapter>,
    ))
}

/// Fork a provider-native session when the provider supports it. Returns the
/// provider, new native session id, and model used for the fork.
pub async fn fork_native_session(
    store: &BatonStore,
    home: &Path,
    source: &ThreadRow,
    through_turn: Option<i64>,
    recorder: ProcessRecorder,
) -> Result<Option<(ProviderId, String, String)>> {
    match source.current_provider {
        ProviderId::Codex => Ok(fork_codex_session(store, home, source, through_turn, recorder)
            .await?
            .map(|(id, model)| (ProviderId::Codex, id, model))),
        ProviderId::OpenCode => Ok(fork_opencode_session(store, home, source, through_turn, recorder)
            .await?
            .map(|(id, model)| (ProviderId::OpenCode, id, model))),
        provider => anyhow::bail!(
            "native fork is not implemented for {provider}; omit --native to fork this transcript"
        ),
    }
}

async fn fork_opencode_session(
    store: &BatonStore,
    home: &Path,
    source: &ThreadRow,
    through_turn: Option<i64>,
    recorder: ProcessRecorder,
) -> Result<Option<(String, String)>> {
    if through_turn.is_some() {
        anyhow::bail!("opencode native fork supports whole sessions only; omit --turn");
    }
    let session = store
        .get_provider_session(&source.id, ProviderId::OpenCode)?
        .ok_or_else(|| anyhow::anyhow!("source thread has no opencode session"))?;
    let resolved = resolve_config(home, Path::new(&source.cwd));
    let provider_config = resolved.for_provider(ProviderId::OpenCode);
    let model = source
        .current_model
        .clone()
        .unwrap_or_else(|| provider_config.model.clone());
    let adapter = build_opencode_adapter_with_recorder(&PathBuf::from(&source.cwd), recorder);
    let mut adapter = adapter.lock().await;
    let native = adapter
        .fork_session(ForkSessionInput {
            cwd: source.cwd.clone(),
            model: model.clone(),
            session: NativeSessionRef {
                native_session_id: session.native_session_id,
                rollout_path: session.rollout_path,
            },
            auth_env: provider_auth_env(provider_config),
            last_turn_id: None,
        })
        .await;
    let _ = adapter.dispose().await;
    let native = native?;
    Ok(native.map(|id| (id, model)))
}

#[derive(Debug, Clone, Copy)]
pub struct NativeRewindOutcome {
    pub provider_retry_skipped: bool,
}

/// Completes a Codex native rewind with a durable recovery marker. If a
/// pending marker already exists for this turn, the provider call is skipped
/// and only Baton's store rewind is retried.
pub async fn rewind_to_turn(
    store: &BatonStore,
    home: &Path,
    source: &ThreadRow,
    turn: i64,
    recorder: ProcessRecorder,
) -> Result<NativeRewindOutcome> {
    if !matches!(
        source.current_provider,
        ProviderId::Codex | ProviderId::OpenCode
    ) {
        anyhow::bail!("native rewind is implemented for codex and opencode");
    }
    if let Some(pending) = store.native_rewind_pending(&source.id)? {
        if pending.turn != turn {
            anyhow::bail!(
                "thread has a pending {} native rewind to turn {}; run `baton rewind -t {} --turn {}` without --native to finish it first",
                pending.provider,
                pending.turn,
                &source.id[..8.min(source.id.len())],
                pending.turn
            );
        }
        store.rewind_thread_keeping_provider_sessions(&source.id, turn)?;
        return Ok(NativeRewindOutcome {
            provider_retry_skipped: true,
        });
    }
    let provider = source.current_provider;
    let reverted = revert_native_session(store, home, source, turn, recorder).await?;
    if !reverted {
        anyhow::bail!("{provider} did not confirm native rewind");
    }
    if let Err(error) = store.set_native_rewind_pending(&source.id, provider, turn) {
        anyhow::bail!(
            "{provider} history was reverted, but baton could not record the recovery marker: {error}. Rerun `baton rewind -t {} --turn {turn}` without --native to finish it.",
            &source.id[..8.min(source.id.len())]
        );
    }
    if let Err(error) = store.rewind_thread_keeping_provider_sessions(&source.id, turn) {
        anyhow::bail!(
            "{provider} history was reverted, but baton state was not: {error}. Rerun the same command to finish the store rewind."
        );
    }
    Ok(NativeRewindOutcome {
        provider_retry_skipped: false,
    })
}

async fn revert_native_session(
    store: &BatonStore,
    home: &Path,
    source: &ThreadRow,
    from_turn: i64,
    recorder: ProcessRecorder,
) -> Result<bool> {
    match source.current_provider {
        ProviderId::Codex => revert_codex_session(store, home, source, from_turn, recorder).await,
        ProviderId::OpenCode => {
            revert_opencode_session(store, home, source, from_turn, recorder).await
        }
        other => anyhow::bail!("native rewind is implemented for codex and opencode, not {other}"),
    }
}

/// Revert the source OpenCode session before a completed turn. OpenCode's
/// revert stage cuts history at the user message id recorded as the turn's
/// provider id, and `files: false` leaves the working tree to Baton
/// checkpoints.
pub async fn revert_opencode_session(
    store: &BatonStore,
    home: &Path,
    source: &ThreadRow,
    from_turn: i64,
    recorder: ProcessRecorder,
) -> Result<bool> {
    if source.current_provider != ProviderId::OpenCode {
        anyhow::bail!("opencode native rewind requires an opencode thread");
    }
    let session = store
        .get_provider_session(&source.id, ProviderId::OpenCode)?
        .ok_or_else(|| anyhow::anyhow!("source thread has no opencode session"))?;
    let message_id = opencode_revert_message_id(store, &source.id, from_turn)?;
    let resolved = resolve_config(home, Path::new(&source.cwd));
    let adapter =
        build_opencode_adapter_with_recorder(&PathBuf::from(&source.cwd), recorder);
    let mut adapter = adapter.lock().await;
    let result = adapter
        .revert_session(RevertSessionInput {
            session: NativeSessionRef {
                native_session_id: session.native_session_id,
                rollout_path: session.rollout_path,
            },
            before_turn_id: message_id,
            auth_env: provider_auth_env(resolved.for_provider(ProviderId::OpenCode)),
        })
        .await;
    let _ = adapter.dispose().await;
    result
}

fn opencode_revert_message_id(
    store: &BatonStore,
    thread_id: &str,
    from_turn: i64,
) -> Result<String> {
    let turn = store
        .get_turn_record(thread_id, from_turn)?
        .ok_or_else(|| anyhow::anyhow!("turn {from_turn} not found in thread"))?;
    if !turn.phase.is_terminal() {
        anyhow::bail!("turn {from_turn} is not terminal; interrupt it or use the transcript rewind");
    }
    turn.provider_turn_id.ok_or_else(|| {
        anyhow::anyhow!(
            "turn {from_turn} has no opencode message id; native rewind can only cut turns recorded after Baton captured OpenCode message ids"
        )
    })
}

/// Revert the source Codex session before a completed provider turn. The
/// caller must keep the provider session row when it rewinds Baton state.
pub async fn revert_codex_session(
    store: &BatonStore,
    home: &Path,
    source: &ThreadRow,
    from_turn: i64,
    recorder: ProcessRecorder,
) -> Result<bool> {
    if source.current_provider != ProviderId::Codex {
        anyhow::bail!("native rewind is implemented for codex only");
    }
    let session = store
        .get_provider_session(&source.id, ProviderId::Codex)?
        .ok_or_else(|| anyhow::anyhow!("source thread has no codex session"))?;
    let turn = store
        .get_turn_record(&source.id, from_turn)?
        .ok_or_else(|| anyhow::anyhow!("turn {from_turn} not found in thread"))?;
    if !turn.phase.is_terminal() {
        anyhow::bail!("turn {from_turn} is not terminal; interrupt it or use the transcript rewind");
    }
    let before_turn_id = turn
        .provider_turn_id
        .ok_or_else(|| anyhow::anyhow!("turn {from_turn} has no codex provider turn id"))?;
    let resolved = resolve_config(home, Path::new(&source.cwd));
    let adapter = build_codex_adapter_with_recorder(&PathBuf::from(&source.cwd), recorder);
    let mut adapter = adapter.lock().await;
    let result = adapter
        .revert_session(RevertSessionInput {
            session: NativeSessionRef {
                native_session_id: session.native_session_id,
                rollout_path: session.rollout_path,
            },
            before_turn_id,
            auth_env: provider_auth_env(resolved.for_provider(ProviderId::Codex)),
        })
        .await;
    let _ = adapter.dispose().await;
    result
}

/// Fork the source Codex session through an optional provider turn. Returns the
/// new native session id and the model used for the fork. The caller creates
/// the baton thread and attaches the session after this succeeds.
pub async fn fork_codex_session(
    store: &BatonStore,
    home: &Path,
    source: &ThreadRow,
    through_turn: Option<i64>,
    recorder: ProcessRecorder,
) -> Result<Option<(String, String)>> {
    if source.current_provider != ProviderId::Codex {
        anyhow::bail!("native fork is implemented for codex only");
    }
    let session = store
        .get_provider_session(&source.id, ProviderId::Codex)?
        .ok_or_else(|| anyhow::anyhow!("source thread has no codex session"))?;
    let resolved = resolve_config(home, Path::new(&source.cwd));
    let provider_config = resolved.for_provider(ProviderId::Codex);
    let model = source
        .current_model
        .clone()
        .unwrap_or_else(|| provider_config.model.clone());
    let last_turn_id = store.fork_provider_turn_id(&source.id, through_turn)?;
    if through_turn.is_some() && last_turn_id.is_none() {
        anyhow::bail!("the selected turn has no codex provider turn id");
    }
    let adapter = build_codex_adapter_with_recorder(&PathBuf::from(&source.cwd), recorder);
    let mut adapter = adapter.lock().await;
    let native = adapter
        .fork_session(ForkSessionInput {
            cwd: source.cwd.clone(),
            model: model.clone(),
            session: NativeSessionRef {
                native_session_id: session.native_session_id,
                rollout_path: session.rollout_path,
            },
            auth_env: provider_auth_env(provider_config),
            last_turn_id,
        })
        .await;
    let _ = adapter.dispose().await;
    let native = native?;
    Ok(native.map(|id| (id, model)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewThread;
    use std::sync::Mutex;

    #[tokio::test]
    async fn native_fork_rejects_unsupported_providers_and_missing_sessions() {
        let store = BatonStore::memory().unwrap();
        let cursor = store
            .create_thread(NewThread {
                title: "cursor".to_string(),
                cwd: "/tmp/project".to_string(),
                current_provider: ProviderId::Cursor,
                current_model: None,
            })
            .unwrap();
        let error = fork_native_session(
            &store,
            Path::new("/tmp"),
            &cursor,
            None,
            ProcessRecorder::disabled(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not implemented for cursor"));

        let opencode = store
            .create_thread(NewThread {
                title: "opencode".to_string(),
                cwd: "/tmp/project".to_string(),
                current_provider: ProviderId::OpenCode,
                current_model: None,
            })
            .unwrap();
        let error = fork_native_session(
            &store,
            Path::new("/tmp"),
            &opencode,
            None,
            ProcessRecorder::disabled(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("source thread has no opencode session"));
    }

    #[test]
    fn opencode_native_rewind_requires_the_turn_message_id() {
        use crate::types::TurnPhase;

        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "opencode".to_string(),
                cwd: "/tmp/project".to_string(),
                current_provider: ProviderId::OpenCode,
                current_model: None,
            })
            .unwrap();
        store
            .start_turn_record(&thread.id, 1, ProviderId::OpenCode, Some("model"))
            .unwrap();
        store
            .finish_turn_record(&thread.id, 1, TurnPhase::Completed, None)
            .unwrap();
        store.set_turn_provider_id(&thread.id, 1, "msg_1").unwrap();
        assert_eq!(
            opencode_revert_message_id(&store, &thread.id, 1).unwrap(),
            "msg_1"
        );

        store
            .start_turn_record(&thread.id, 2, ProviderId::OpenCode, Some("model"))
            .unwrap();
        store
            .finish_turn_record(&thread.id, 2, TurnPhase::Completed, None)
            .unwrap();
        let error = opencode_revert_message_id(&store, &thread.id, 2).unwrap_err();
        assert!(error.to_string().contains("no opencode message id"), "{error}");

        store
            .start_turn_record(&thread.id, 3, ProviderId::OpenCode, Some("model"))
            .unwrap();
        store.set_turn_provider_id(&thread.id, 3, "msg_3").unwrap();
        let error = opencode_revert_message_id(&store, &thread.id, 3).unwrap_err();
        assert!(error.to_string().contains("not terminal"), "{error}");
    }

    #[test]
    fn pending_opencode_native_rewind_finishes_the_store_without_the_provider() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "rewind".to_string(),
                cwd: "/tmp/project".to_string(),
                current_provider: ProviderId::OpenCode,
                current_model: None,
            })
            .unwrap();
        store
            .add_message(&thread.id, 1, "user", None, None, "keep")
            .unwrap();
        store
            .add_message(&thread.id, 2, "user", None, None, "remove")
            .unwrap();
        store
            .set_native_rewind_pending(&thread.id, ProviderId::OpenCode, 2)
            .unwrap();
        let recorder_store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let recorder = ProcessRecorder::new(recorder_store, thread.cwd.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let outcome = runtime
            .block_on(rewind_to_turn(
                &store,
                Path::new("/tmp"),
                &thread,
                2,
                recorder,
            ))
            .unwrap();
        assert!(outcome.provider_retry_skipped);
        assert!(store.native_rewind_pending(&thread.id).unwrap().is_none());
        assert_eq!(store.list_messages(&thread.id).unwrap().len(), 1);
    }

    #[test]
    fn pending_native_rewind_finishes_the_store_without_the_provider() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "rewind".to_string(),
                cwd: "/tmp/project".to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .add_message(&thread.id, 1, "user", None, None, "keep")
            .unwrap();
        store
            .add_message(&thread.id, 2, "user", None, None, "remove")
            .unwrap();
        store
            .set_native_rewind_pending(&thread.id, ProviderId::Codex, 2)
            .unwrap();
        let recorder_store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let recorder = ProcessRecorder::new(recorder_store, thread.cwd.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let outcome = runtime
            .block_on(rewind_to_turn(
                &store,
                Path::new("/tmp"),
                &thread,
                2,
                recorder,
            ))
            .unwrap();
        assert!(outcome.provider_retry_skipped);
        assert!(store.native_rewind_pending(&thread.id).unwrap().is_none());
        let messages = store.list_messages(&thread.id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "keep");
    }
}
