use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::adapters::{BatonAdapter, LineReader, LineWriter, TransportHalves, TurnEvent, TurnEventSender, TurnStartInput};
use crate::config::auth_signature;
use crate::types::{ApprovalDecision, NativeSessionRef, PermissionMode, ProviderId, TurnUsage};

struct CodexRuntime {
    reader: AsyncMutex<Box<dyn LineReader>>,
    writer: AsyncMutex<Box<dyn LineWriter>>,
    pending: Mutex<HashMap<u32, oneshot::Sender<Result<Value, String>>>>,
    notifications: tokio::sync::mpsc::UnboundedSender<(String, Value)>,
    current_emit: AsyncMutex<Option<TurnEventSender>>,
    active_thread: AsyncMutex<Option<String>>,
}

pub type HalvesSpawner = Arc<dyn Fn(&[(String, String)]) -> Result<TransportHalves> + Send + Sync>;
type CumulativeMap = Arc<Mutex<HashMap<String, (i64, i64, i64, i64)>>>;

pub struct CodexAdapter {
    spawner: HalvesSpawner,
    runtime: Arc<AsyncMutex<Option<Arc<CodexRuntime>>>>,
    notification_receiver: Option<tokio::sync::mpsc::UnboundedReceiver<(String, Value)>>,
    next_id: Arc<AtomicU32>,
    auth_signature: Arc<Mutex<Option<String>>>,
    last_cumulative: CumulativeMap,
}

fn spawn_reader(runtime: Arc<CodexRuntime>) {
    tokio::spawn(async move {
        loop {
            let line = {
                let mut reader = runtime.reader.lock().await;
                match reader.read_line().await {
                    Ok(Some(line)) => line,
                    _ => break,
                }
            };
            let frame: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let method = frame.get("method").and_then(Value::as_str).map(str::to_string);
            let id = frame.get("id").cloned();
            let response_id = id.clone();
            match (method, id) {
                (Some(method), Some(id)) => {
                    handle_server_request(&runtime, id, &method, frame.get("params").cloned().unwrap_or(Value::Null)).await;
                }
                (Some(method), None) => {
                    let _ = runtime
                        .notifications
                        .send((method, frame.get("params").cloned().unwrap_or(Value::Null)));
                }
                _ => {
                    if let Some(id) = response_id.and_then(|id| id.as_i64()) {
                        let mut pending = runtime.pending.lock().unwrap();
                        if let Some(sender) = pending.remove(&(id as u32)) {
                            let result = match frame.get("error") {
                                Some(error) => Err(error
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown error")
                                    .to_string()),
                                None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
                            };
                            let _ = sender.send(result);
                        }
                    }
                }
            }
        }
    });
}

async fn handle_server_request(
    runtime: &Arc<CodexRuntime>,
    id: Value,
    method: &str,
    params: Value,
) {
    const APPROVAL_METHODS: &[(&str, &str)] = &[
        ("item/commandExecution/requestApproval", "command"),
        ("item/fileChange/requestApproval", "fileChange"),
        ("item/permissions/requestApproval", "permissions"),
    ];
    let kind = APPROVAL_METHODS
        .iter()
        .find_map(|(candidate, kind)| (*candidate == method).then_some(*kind));
    let kind = match kind {
        Some(kind) => kind,
        None => {
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("baton has no handler for {method}") }
            });
            let mut writer = runtime.writer.lock().await;
            if let Ok(serialized) = serde_json::to_string(&response) {
                let _ = writer.write_line(&serialized).await;
            }
            return;
        }
    };
    let summary = params
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            params
                .get("changes")
                .and_then(Value::as_array)
                .map(|changes| {
                    changes
                        .iter()
                        .filter_map(|change| change.get("path").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|value| !value.is_empty())
        })
        .or_else(|| {
            params
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| format!("codex requests {kind} approval"));
    let request_id = format!(
        "{}:{}",
        method,
        params
            .get("approvalId")
            .or_else(|| params.get("itemId"))
            .and_then(Value::as_str)
            .unwrap_or("")
    );
    let (respond, receiver) = oneshot::channel::<ApprovalDecision>();
    {
        let emit = runtime.current_emit.lock().await;
        if let Some(emit) = emit.as_ref() {
            let _ = emit.send(TurnEvent::ApprovalRequest {
                id: request_id,
                kind: kind.to_string(),
                summary,
                respond,
            });
        }
    }
    let runtime = runtime.clone();
    tokio::spawn(async move {
        let decision = match receiver.await {
            Ok(ApprovalDecision::Accept) => "accept",
            Ok(ApprovalDecision::AcceptAlways) => "acceptForSession",
            Ok(ApprovalDecision::Deny) | Err(_) => "deny",
        };
        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "decision": decision }
        });
        if let Ok(serialized) = serde_json::to_string(&response) {
            let mut writer = runtime.writer.lock().await;
            let _ = writer.write_line(&serialized).await;
        }
    });
}

impl CodexAdapter {
    pub fn new(spawner: HalvesSpawner) -> Self {
        CodexAdapter {
            spawner,
            runtime: Arc::new(AsyncMutex::new(None)),
            notification_receiver: None,
            next_id: Arc::new(AtomicU32::new(1)),
            auth_signature: Arc::new(Mutex::new(None)),
            last_cumulative: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn ensure_runtime(&mut self, auth_env: &[(String, String)]) -> Result<Arc<CodexRuntime>> {
        let signature = auth_signature(auth_env);
        let need_spawn = self.auth_signature.lock().unwrap().as_deref() != Some(signature.as_str());
        if need_spawn {
            let mut slot = self.runtime.lock().await;
            if let Some(runtime) = slot.as_ref() {
                let mut writer = runtime.writer.lock().await;
                let _ = writer.shutdown().await;
            }
            let (reader, writer) = (self.spawner)(auth_env)?;
            let (notification_sender, notification_receiver) = tokio::sync::mpsc::unbounded_channel();
            let runtime = Arc::new(CodexRuntime {
                reader: AsyncMutex::new(reader),
                writer: AsyncMutex::new(writer),
                pending: Mutex::new(HashMap::new()),
                notifications: notification_sender,
                current_emit: AsyncMutex::new(None),
                active_thread: AsyncMutex::new(None),
            });
            spawn_reader(runtime.clone());
            *slot = Some(runtime.clone());
            self.notification_receiver = Some(notification_receiver);
            *self.auth_signature.lock().unwrap() = Some(signature);
        }
        Ok(self
            .runtime
            .lock()
            .await
            .clone()
            .expect("codex runtime spawned"))
    }

    async fn request(&self, runtime: &Arc<CodexRuntime>, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        runtime.pending.lock().unwrap().insert(id, sender);
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        {
            let mut writer = runtime.writer.lock().await;
            writer.write_line(&serde_json::to_string(&frame)?).await?;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(60), receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => anyhow::bail!("codex app-server error: {error}"),
            _ => anyhow::bail!("codex app-server request timed out: {method}"),
        }
    }

    async fn ensure_initialized(&self, runtime: &Arc<CodexRuntime>) -> Result<()> {
        let params = json!({
            "clientInfo": { "name": "baton", "title": "baton", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "experimentalApi": true }
        });
        if let Err(error) = self.request(runtime, "initialize", params).await {
            if !error.to_string().contains("Already initialized") {
                return Err(error);
            }
        }
        let frame = json!({ "jsonrpc": "2.0", "method": "initialized", "params": null });
        let mut writer = runtime.writer.lock().await;
        writer.write_line(&serde_json::to_string(&frame)?).await?;
        Ok(())
    }

    fn handle_notification(
        &self,
        method: &str,
        params: &Value,
        emit: &TurnEventSender,
        active_thread: &str,
        turn_usage: &mut Option<TurnUsage>,
        final_text: &mut String,
    ) {
        let thread_id = params.get("threadId").and_then(Value::as_str).unwrap_or("");
        if method == "thread/tokenUsage/updated" {
            let usage = params.get("tokenUsage").cloned().unwrap_or(Value::Null);
            let cumulative = (
                usage.get("inputTokens").and_then(Value::as_i64).unwrap_or(0),
                usage.get("cachedInputTokens").and_then(Value::as_i64).unwrap_or(0),
                usage.get("cacheWriteInputTokens").and_then(Value::as_i64).unwrap_or(0),
                usage.get("outputTokens").and_then(Value::as_i64).unwrap_or(0),
            );
            if !thread_id.is_empty() {
                self.last_cumulative.lock().unwrap().insert(thread_id.to_string(), cumulative);
            }
            if thread_id == active_thread {
                let usage = TurnUsage {
                    input_tokens: cumulative.0,
                    output_tokens: cumulative.3,
                    cache_read_tokens: cumulative.1,
                    cache_creation_tokens: cumulative.2,
                    cost_usd: None,
                };
                *turn_usage = Some(usage);
                let _ = emit.send(TurnEvent::Usage { usage });
            }
            return;
        }
        if thread_id != active_thread {
            return;
        }
        match method {
            "item/agentMessage/delta" => {
                if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                    if !delta.is_empty() {
                        let _ = emit.send(TurnEvent::TextDelta { text: delta.to_string() });
                    }
                }
            }
            "item/started" | "item/completed" => {
                let item = params.get("item").cloned().unwrap_or(Value::Null);
                let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                let completed = method == "item/completed";
                match item_type {
                    "agentMessage" => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                *final_text = text.to_string();
                                if completed {
                                    let _ = emit.send(TurnEvent::AssistantText { text: text.to_string() });
                                }
                            }
                        }
                    }
                    "commandExecution" => {
                        let command = item.get("command").and_then(Value::as_str).unwrap_or("").to_string();
                        if !completed {
                            let _ = emit.send(TurnEvent::ToolStarted { tool: "shell".into(), detail: Some(command) });
                        } else {
                            let status = item.get("status").and_then(Value::as_str).unwrap_or("");
                            if status == "completed" || status == "failed" {
                                let _ = emit.send(TurnEvent::ToolCompleted {
                                    tool: "shell".into(),
                                    detail: Some(command),
                                    exit_code: item.get("exitCode").and_then(Value::as_i64),
                                });
                            }
                        }
                    }
                    "fileChange" => {
                        let paths = item
                            .get("changes")
                            .and_then(Value::as_array)
                            .map(|changes| {
                                changes
                                    .iter()
                                    .filter_map(|change| change.get("path").and_then(Value::as_str))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .unwrap_or_default();
                        if !completed {
                            let _ = emit.send(TurnEvent::ToolStarted { tool: "edit".into(), detail: Some(paths) });
                        } else {
                            let _ = emit.send(TurnEvent::ToolCompleted { tool: "edit".into(), detail: Some(paths), exit_code: None });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[async_trait]
impl BatonAdapter for CodexAdapter {
    fn id(&self) -> ProviderId {
        ProviderId::Codex
    }

    fn interrupt_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        let runtime = self.runtime.clone();
        let next_id = self.next_id.clone();
        Arc::new(move || {
            let runtime = runtime.clone();
            let next_id = next_id.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let runtime = match runtime.lock().await.clone() {
                        Some(runtime) => runtime,
                        None => return,
                    };
                    let thread_id = runtime.active_thread.lock().await.clone();
                    let thread_id = match thread_id {
                        Some(thread_id) => thread_id,
                        None => return,
                    };
                    let id = next_id.fetch_add(1, Ordering::SeqCst);
                    let frame = json!({ "jsonrpc": "2.0", "id": id, "method": "turn/interrupt", "params": { "threadId": thread_id } });
                    if let Ok(serialized) = serde_json::to_string(&frame) {
                        let mut writer = runtime.writer.lock().await;
                        let _ = writer.write_line(&serialized).await;
                    }
                });
            }
        })
    }

    async fn start_turn(
        &mut self,
        input: TurnStartInput,
        session: Option<NativeSessionRef>,
        emit: TurnEventSender,
    ) -> Result<()> {
        let runtime = self.ensure_runtime(&input.auth_env).await?;
        self.ensure_initialized(&runtime).await?;
        *runtime.current_emit.lock().await = Some(emit.clone());

        let (thread_id, rollout_path) = match session {
            Some(session) => {
                let params = json!({ "threadId": session.native_session_id, "excludeTurns": true });
                self.request(&runtime, "thread/resume", params).await?;
                (session.native_session_id, session.rollout_path)
            }
            None => {
                let params = json!({ "cwd": input.cwd, "model": input.model });
                let started = self.request(&runtime, "thread/start", params).await?;
                let thread = started
                    .get("thread")
                    .and_then(|thread| thread.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let path = started
                    .get("thread")
                    .and_then(|thread| thread.get("path"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                match thread {
                    Some(thread) => (thread, path),
                    None => anyhow::bail!("codex app-server did not return a thread id"),
                }
            }
        };
        *runtime.active_thread.lock().await = Some(thread_id.clone());

        let (approval_policy, sandbox_policy) = match input.permission_mode {
            PermissionMode::BypassPermissions => ("never".to_string(), json!({ "type": "dangerFullAccess" })),
            PermissionMode::Plan => ("on-request".to_string(), json!({ "type": "readOnly", "networkAccess": false })),
            _ => (
                "on-request".to_string(),
                json!({ "type": "workspaceWrite", "writableRoots": [], "networkAccess": false }),
            ),
        };
        let turn_params = json!({
            "threadId": thread_id,
            "cwd": input.cwd,
            "model": input.model,
            "input": [ { "type": "text", "text": input.prompt } ],
            "approvalPolicy": approval_policy,
            "sandboxPolicy": sandbox_policy,
            "approvalsReviewer": "user",
            "summary": "detailed"
        });
        self.request(&runtime, "turn/start", turn_params).await?;

        let mut receiver = match self.notification_receiver.take() {
            Some(receiver) => receiver,
            None => anyhow::bail!("codex notification receiver missing"),
        };
        let mut turn_usage: Option<TurnUsage> = None;
        let mut final_text = String::new();
        let result: Result<()> = loop {
            let (method, params) = match receiver.recv().await {
                Some(event) => event,
                None => break Err(anyhow::anyhow!("codex app-server connection closed")),
            };
            match method.as_str() {
                "turn/completed" => {
                    let completed_thread = params.get("threadId").and_then(Value::as_str).unwrap_or("");
                    if completed_thread == thread_id {
                        let final_message = params
                            .get("turn")
                            .and_then(|turn| turn.get("items"))
                            .and_then(Value::as_array)
                            .and_then(|items| {
                                items
                                    .iter()
                                    .rev()
                                    .find(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"))
                            })
                            .and_then(|item| item.get("text"))
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let text = final_message.unwrap_or_else(|| final_text.clone());
                        let _ = emit.send(TurnEvent::TurnCompleted {
                            final_text: text,
                            usage: turn_usage,
                            native_session_id: Some(thread_id.clone()),
                            rollout_path: rollout_path.clone(),
                        });
                        break Ok(());
                    }
                }
                "turn/failed" => {
                    let failed_thread = params.get("threadId").and_then(Value::as_str).unwrap_or("");
                    if failed_thread == thread_id {
                        let error = params.get("error").and_then(Value::as_str).unwrap_or("codex turn failed");
                        break Err(anyhow::anyhow!("{error}"));
                    }
                }
                _ => {
                    self.handle_notification(&method, &params, &emit, &thread_id, &mut turn_usage, &mut final_text);
                }
            }
        };
        self.notification_receiver = Some(receiver);
        *runtime.current_emit.lock().await = None;
        *runtime.active_thread.lock().await = None;
        result
    }

    async fn dispose(&mut self) -> Result<()> {
        let runtime = self.runtime.lock().await.clone();
        if let Some(runtime) = runtime {
            let mut writer = runtime.writer.lock().await;
            let _ = writer.shutdown().await;
        }
        Ok(())
    }
}
