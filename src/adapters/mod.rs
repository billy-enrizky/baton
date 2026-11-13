use anyhow::Result;
use async_trait::async_trait;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::oneshot;

use crate::types::{ApprovalDecision, NativeSessionRef, PermissionMode, TurnUsage};

/// Explicit lifecycle for one provider adapter. The engine can read this
/// without taking the adapter mutex, so a busy or stuck turn remains
/// observable instead of looking like a locked black box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterState {
    Unloaded,
    Starting,
    Ready,
    TurnRunning,
    Cancelling,
    Disposing,
    Disposed,
}

impl AdapterState {
    pub fn as_str(self) -> &'static str {
        match self {
            AdapterState::Unloaded => "unloaded",
            AdapterState::Starting => "starting",
            AdapterState::Ready => "ready",
            AdapterState::TurnRunning => "turn-running",
            AdapterState::Cancelling => "cancelling",
            AdapterState::Disposing => "disposing",
            AdapterState::Disposed => "disposed",
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            AdapterState::Unloaded => 0,
            AdapterState::Starting => 1,
            AdapterState::Ready => 2,
            AdapterState::TurnRunning => 3,
            AdapterState::Cancelling => 4,
            AdapterState::Disposing => 5,
            AdapterState::Disposed => 6,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => AdapterState::Starting,
            2 => AdapterState::Ready,
            3 => AdapterState::TurnRunning,
            4 => AdapterState::Cancelling,
            5 => AdapterState::Disposing,
            6 => AdapterState::Disposed,
            _ => AdapterState::Unloaded,
        }
    }
}

/// A cheap, cloneable handle to one adapter's lifecycle. Transitions only move
/// forward from an allowed source state, so a late `ready()` cannot resurrect
/// a disposing adapter and a cancellation cannot overwrite an idle adapter.
#[derive(Debug, Clone)]
pub struct AdapterLifecycle {
    state: Arc<AtomicU8>,
    thread_id: Arc<Mutex<Option<String>>>,
}

impl Default for AdapterLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl AdapterLifecycle {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(AdapterState::Unloaded.as_u8())),
            thread_id: Arc::new(Mutex::new(None)),
        }
    }

    pub fn state(&self) -> AdapterState {
        AdapterState::from_u8(self.state.load(Ordering::SeqCst))
    }

    pub fn thread_id(&self) -> Option<String> {
        self.thread_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn transition(&self, from: &[AdapterState], to: AdapterState, thread: Option<&str>) -> bool {
        let current = self.state.load(Ordering::SeqCst);
        if !from.iter().any(|state| state.as_u8() == current) {
            return false;
        }
        if self
            .state
            .compare_exchange(current, to.as_u8(), Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        *self
            .thread_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = thread.map(str::to_string);
        true
    }

    pub fn starting(&self, thread_id: &str) {
        self.transition(
            &[AdapterState::Unloaded, AdapterState::Ready],
            AdapterState::Starting,
            Some(thread_id),
        );
    }

    pub fn turn_running(&self, thread_id: &str) {
        self.transition(
            &[
                AdapterState::Unloaded,
                AdapterState::Starting,
                AdapterState::Ready,
            ],
            AdapterState::TurnRunning,
            Some(thread_id),
        );
    }

    pub fn ready(&self) {
        self.transition(
            &[
                AdapterState::Starting,
                AdapterState::TurnRunning,
                AdapterState::Cancelling,
            ],
            AdapterState::Ready,
            None,
        );
    }

    pub fn cancelling(&self) {
        let thread = self.thread_id();
        self.transition(
            &[AdapterState::TurnRunning],
            AdapterState::Cancelling,
            thread.as_deref(),
        );
    }

    pub fn disposing(&self) {
        self.transition(
            &[
                AdapterState::Unloaded,
                AdapterState::Starting,
                AdapterState::Ready,
                AdapterState::TurnRunning,
                AdapterState::Cancelling,
            ],
            AdapterState::Disposing,
            None,
        );
    }

    pub fn disposed(&self) {
        self.transition(&[AdapterState::Disposing], AdapterState::Disposed, None);
    }

    pub fn describe(&self) -> String {
        let state = self.state().as_str();
        match self.thread_id() {
            Some(thread) if !thread.is_empty() => {
                format!("{state}({})", thread.chars().take(8).collect::<String>())
            }
            _ => state.to_string(),
        }
    }
}

pub(crate) struct AdapterTurnGuard {
    lifecycle: AdapterLifecycle,
}

impl AdapterTurnGuard {
    pub(crate) fn new(lifecycle: AdapterLifecycle) -> Self {
        Self { lifecycle }
    }
}

impl Drop for AdapterTurnGuard {
    fn drop(&mut self) {
        self.lifecycle.ready();
    }
}

#[derive(Debug, Clone)]
pub struct TurnStartInput {
    pub thread_id: String,
    pub cwd: String,
    pub model: String,
    pub effort: Option<String>,
    pub prompt: String,
    pub permission_mode: PermissionMode,
    pub auth_env: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct RevertSessionInput {
    pub session: NativeSessionRef,
    pub before_turn_id: String,
    pub auth_env: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ForkSessionInput {
    pub cwd: String,
    pub model: String,
    pub session: NativeSessionRef,
    pub auth_env: Vec<(String, String)>,
    /// Provider turn id to fork through, inclusive. None forks the whole thread.
    pub last_turn_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModelListInput {
    pub cwd: String,
    pub auth_env: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum TurnEvent {
    ProviderTurnId { id: String },
    /// The provider session id as soon as the adapter knows it, even if the
    /// turn later fails. The engine persists it for resume, fork, and rewind.
    ProviderSession {
        native_session_id: String,
        rollout_path: Option<String>,
    },
    TextDelta { text: String },
    AssistantText { text: String },
    ToolStarted { id: Option<String>, tool: String, detail: Option<String> },
    ToolCompleted { id: Option<String>, tool: String, detail: Option<String>, exit_code: Option<i64> },
    ApprovalRequest {
        id: String,
        kind: String,
        summary: String,
        respond: oneshot::Sender<ApprovalDecision>,
    },
    Usage { usage: TurnUsage },
    TurnCompleted {
        final_text: String,
        usage: Option<TurnUsage>,
        native_session_id: Option<String>,
        rollout_path: Option<String>,
    },
    TurnInterrupted { reason: Option<String> },
    TurnFailed { error: String },
}

pub type TurnEventSender = std::sync::mpsc::Sender<TurnEvent>;
pub type SteerHandle = std::sync::Arc<dyn Fn(String) -> bool + Send + Sync>;

#[async_trait]
pub trait LineReader: Send {
    async fn read_line(&mut self) -> Result<Option<String>>;
}

#[async_trait]
pub trait LineWriter: Send {
    async fn write_line(&mut self, line: &str) -> Result<()>;
    async fn shutdown(&mut self) -> Result<()>;
}

pub type TransportHalves = (Box<dyn LineReader>, Box<dyn LineWriter>);

pub struct ChildReader {
    reader: BufReader<tokio::process::ChildStdout>,
}

#[async_trait]
impl LineReader for ChildReader {
    async fn read_line(&mut self) -> Result<Option<String>> {
        let mut line = String::new();
        let bytes = self.reader.read_line(&mut line).await?;
        if bytes == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
    }
}

pub struct ChildWriter {
    stdin: Option<tokio::process::ChildStdin>,
    child: Option<Child>,
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

#[async_trait]
impl LineWriter for ChildWriter {
    async fn write_line(&mut self, line: &str) -> Result<()> {
        match self.stdin.as_mut() {
            Some(stdin) => {
                stdin.write_all(line.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await?;
                Ok(())
            }
            None => anyhow::bail!("child stdin closed"),
        }
    }

    async fn shutdown(&mut self) -> Result<()> {
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
        Ok(())
    }
}

impl Drop for ChildWriter {
    fn drop(&mut self) {
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

pub fn child_halves(child: Child) -> Result<TransportHalves> {
    child_halves_with_cleanup(child, None)
}

pub fn child_halves_with_cleanup(
    child: Child,
    cleanup: Option<Box<dyn FnOnce() + Send>>,
) -> Result<TransportHalves> {
    let mut child = child;
    let stdin = child.stdin.take().expect("child stdin piped");
    let stdout = child.stdout.take().expect("child stdout piped");
    Ok((
        Box::new(ChildReader { reader: BufReader::new(stdout) }),
        Box::new(ChildWriter {
            stdin: Some(stdin),
            child: Some(child),
            cleanup,
        }),
    ))
}

pub struct DuplexReader {
    reader: BufReader<tokio::io::DuplexStream>,
}

pub struct DuplexWriter {
    write: tokio::io::DuplexStream,
}

#[async_trait]
impl LineReader for DuplexReader {
    async fn read_line(&mut self) -> Result<Option<String>> {
        let mut line = String::new();
        let bytes = self.reader.read_line(&mut line).await?;
        if bytes == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
    }
}

#[async_trait]
impl LineWriter for DuplexWriter {
    async fn write_line(&mut self, line: &str) -> Result<()> {
        self.write.write_all(line.as_bytes()).await?;
        self.write.write_all(b"\n").await?;
        self.write.flush().await?;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }
}

pub fn duplex_halves(buffer_size: usize) -> (TransportHalves, tokio::io::DuplexStream, tokio::io::DuplexStream) {
    let (client_write, server_read) = tokio::io::duplex(buffer_size);
    let (server_write, client_read) = tokio::io::duplex(buffer_size);
    (
        (
            Box::new(DuplexReader { reader: BufReader::new(client_read) }),
            Box::new(DuplexWriter { write: client_write }),
        ),
        server_read,
        server_write,
    )
}

#[async_trait]
pub trait BatonAdapter: Send {
    fn id(&self) -> crate::types::ProviderId;
    /// Optional live lifecycle handle. Real adapters return a handle; test
    /// adapters that do not track a session leave the default.
    fn lifecycle(&self) -> Option<AdapterLifecycle> {
        None
    }
    fn interrupt_handle(&self) -> std::sync::Arc<dyn Fn() + Send + Sync>;
    /// Providers that can inject a correction into a live turn return a
    /// handle here. The engine falls back to queueing when this is None or
    /// when the handle cannot reach an active turn.
    fn steer_handle(&self) -> Option<SteerHandle> {
        None
    }
    /// Live model discovery. The default empty list tells the caller to keep
    /// the static catalog; only adapters with a reliable catalog endpoint
    /// override this.
    async fn list_models(&mut self, _input: ModelListInput) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    /// Provider-native conversation fork. Returning None means this provider
    /// has no native fork and the caller must keep the transcript fallback.
    async fn fork_session(&mut self, _input: ForkSessionInput) -> Result<Option<String>> {
        Ok(None)
    }
    /// Provider-native conversation rollback. Returning false means the
    /// provider cannot revert its own history.
    async fn revert_session(&mut self, _input: RevertSessionInput) -> Result<bool> {
        Ok(false)
    }
    async fn start_turn(
        &mut self,
        input: TurnStartInput,
        session: Option<NativeSessionRef>,
        emit: TurnEventSender,
    ) -> Result<()>;
    async fn dispose(&mut self) -> Result<()>;
}

pub mod acp;
pub mod claude;
pub mod codex;
pub mod opencode;

pub use acp::AcpAdapter;
pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use opencode::OpenCodeAdapter;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn lifecycle_moves_forward_and_never_resurrects() {
        let lifecycle = AdapterLifecycle::new();
        assert_eq!(lifecycle.state(), AdapterState::Unloaded);
        lifecycle.starting("thread-1");
        assert_eq!(lifecycle.state(), AdapterState::Starting);
        assert_eq!(lifecycle.thread_id().as_deref(), Some("thread-1"));
        lifecycle.turn_running("thread-1");
        assert_eq!(lifecycle.state(), AdapterState::TurnRunning);
        lifecycle.cancelling();
        assert_eq!(lifecycle.state(), AdapterState::Cancelling);
        lifecycle.ready();
        assert_eq!(lifecycle.state(), AdapterState::Ready);
        assert_eq!(lifecycle.thread_id(), None);
        lifecycle.disposing();
        assert_eq!(lifecycle.state(), AdapterState::Disposing);
        lifecycle.disposed();
        assert_eq!(lifecycle.state(), AdapterState::Disposed);
        lifecycle.ready();
        lifecycle.starting("thread-2");
        assert_eq!(lifecycle.state(), AdapterState::Disposed);
    }

    #[test]
    fn cancelling_without_a_running_turn_is_ignored() {
        let lifecycle = AdapterLifecycle::new();
        lifecycle.cancelling();
        assert_eq!(lifecycle.state(), AdapterState::Unloaded);
        lifecycle.starting("thread-1");
        lifecycle.cancelling();
        assert_eq!(lifecycle.state(), AdapterState::Starting);
        lifecycle.ready();
        lifecycle.cancelling();
        assert_eq!(lifecycle.state(), AdapterState::Ready);
    }

    #[test]
    fn turn_guard_returns_to_ready_but_not_from_disposing() {
        let lifecycle = AdapterLifecycle::new();
        lifecycle.starting("thread-1");
        {
            let _guard = AdapterTurnGuard::new(lifecycle.clone());
            lifecycle.turn_running("thread-1");
        }
        assert_eq!(lifecycle.state(), AdapterState::Ready);

        lifecycle.starting("thread-2");
        let guard = AdapterTurnGuard::new(lifecycle.clone());
        lifecycle.turn_running("thread-2");
        lifecycle.disposing();
        lifecycle.disposed();
        drop(guard);
        assert_eq!(lifecycle.state(), AdapterState::Disposed);
    }

    #[tokio::test]
    async fn dropping_a_child_writer_runs_cleanup_and_kills_the_child() {
        let mut command = tokio::process::Command::new("sleep");
        command
            .arg("30")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let child = command.spawn().unwrap();
        let cleaned = Arc::new(AtomicBool::new(false));
        let cleaned_flag = cleaned.clone();
        let (reader, writer) = child_halves_with_cleanup(
            child,
            Some(Box::new(move || cleaned_flag.store(true, Ordering::SeqCst))),
        )
        .unwrap();
        drop(reader);
        drop(writer);
        assert!(cleaned.load(Ordering::SeqCst));
    }
}
