//! Provider process wiring shared by the CLI, MCP, and the TUI engine.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
