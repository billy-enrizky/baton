use anyhow::Result;
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::oneshot;

use crate::types::{ApprovalDecision, NativeSessionRef, PermissionMode, TurnUsage};

#[derive(Debug, Clone)]
pub struct TurnStartInput {
    pub thread_id: String,
    pub cwd: String,
    pub model: String,
    pub prompt: String,
    pub permission_mode: PermissionMode,
    pub auth_env: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ModelListInput {
    pub cwd: String,
    pub auth_env: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum TurnEvent {
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
