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

#[derive(Debug)]
pub enum TurnEvent {
    TextDelta { text: String },
    AssistantText { text: String },
    ToolStarted { tool: String, detail: Option<String> },
    ToolCompleted { tool: String, detail: Option<String>, exit_code: Option<i64> },
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
    TurnFailed { error: String },
}

pub type TurnEventSender = std::sync::mpsc::Sender<TurnEvent>;

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
        Ok(())
    }
}

pub fn child_halves(child: Child) -> Result<TransportHalves> {
    let mut child = child;
    let stdin = child.stdin.take().expect("child stdin piped");
    let stdout = child.stdout.take().expect("child stdout piped");
    Ok((
        Box::new(ChildReader { reader: BufReader::new(stdout) }),
        Box::new(ChildWriter { stdin: Some(stdin), child: Some(child) }),
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
    async fn start_turn(
        &mut self,
        input: TurnStartInput,
        session: Option<NativeSessionRef>,
        emit: TurnEventSender,
    ) -> Result<()>;
    async fn dispose(&mut self) -> Result<()>;
}

pub mod claude;
pub mod codex;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
