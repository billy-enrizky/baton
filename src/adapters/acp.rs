use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::adapters::{BatonAdapter, LineReader, LineWriter, TransportHalves, TurnEvent, TurnEventSender, TurnStartInput};
use crate::config::auth_signature;
use crate::types::{ApprovalDecision, NativeSessionRef, ProviderId, TurnUsage};

struct AcpRuntime {
    reader: AsyncMutex<Box<dyn LineReader>>,
    writer: AsyncMutex<Box<dyn LineWriter>>,
    pending: Mutex<HashMap<u32, oneshot::Sender<Result<Value, String>>>>,
    notifications: tokio::sync::mpsc::UnboundedSender<(String, Value)>,
    current_emit: AsyncMutex<Option<TurnEventSender>>,
    active_session: AsyncMutex<Option<String>>,
}

pub type HalvesSpawner = Arc<dyn Fn(&[(String, String)]) -> Result<TransportHalves> + Send + Sync>;

pub struct AcpAdapter {
    provider: ProviderId,
    spawner: HalvesSpawner,
    runtime: Arc<AsyncMutex<Option<Arc<AcpRuntime>>>>,
    notification_receiver: Option<tokio::sync::mpsc::UnboundedReceiver<(String, Value)>>,
    next_id: Arc<AtomicU32>,
    auth_signature: Arc<Mutex<Option<String>>>,
}

fn spawn_reader(runtime: Arc<AcpRuntime>) {
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
    runtime: &Arc<AcpRuntime>,
    id: Value,
    method: &str,
    params: Value,
) {
    if method != "session/request_permission" {
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
    let summary = params
        .get("toolCall")
        .and_then(|tool_call| tool_call.get("title"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "agent requests permission".to_string());
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (respond, receiver) = oneshot::channel::<ApprovalDecision>();
    {
        let emit = runtime.current_emit.lock().await;
        if let Some(emit) = emit.as_ref() {
            let _ = emit.send(TurnEvent::ApprovalRequest {
                id: format!("acp:{summary}"),
                kind: "permissions".into(),
                summary,
                respond,
            });
        }
    }
    let runtime = runtime.clone();
    tokio::spawn(async move {
        let option_id = pick_permission_option(&options, receiver.await.ok());
        let outcome = match option_id {
            Some(option_id) => json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
            None => json!({ "outcome": { "outcome": "cancelled" } }),
        };
        let response = json!({ "jsonrpc": "2.0", "id": id, "result": outcome });
        if let Ok(serialized) = serde_json::to_string(&response) {
            let mut writer = runtime.writer.lock().await;
            let _ = writer.write_line(&serialized).await;
        }
    });
}

fn pick_permission_option(options: &[Value], decision: Option<ApprovalDecision>) -> Option<String> {
    let wanted_kind = match decision {
        Some(ApprovalDecision::Accept) | None => "allow_once",
        Some(ApprovalDecision::AcceptAlways) => "allow_always",
        Some(ApprovalDecision::Deny) => "reject_once",
    };
    let fallback_kind = match decision {
        Some(ApprovalDecision::Deny) => "reject_always",
        _ => "allow_always",
    };
    let find = |kind: &str| {
        options
            .iter()
            .find(|option| option.get("kind").and_then(Value::as_str) == Some(kind))
            .and_then(|option| option.get("optionId"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    find(wanted_kind).or_else(|| find(fallback_kind))
}

fn parse_usage(usage: &Value) -> TurnUsage {
    TurnUsage {
        input_tokens: usage.get("inputTokens").and_then(Value::as_i64).unwrap_or(0),
        output_tokens: usage.get("outputTokens").and_then(Value::as_i64).unwrap_or(0),
        cache_read_tokens: usage
            .get("cacheReadInputTokens")
            .or_else(|| usage.get("cacheReadTokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cache_creation_tokens: usage
            .get("cacheWriteInputTokens")
            .or_else(|| usage.get("cacheCreationInputTokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cost_usd: None,
    }
}

fn handle_session_update(update: &Value, emit: &TurnEventSender, final_text: &mut String) {
    let variant = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
    match variant {
        "agent_message_chunk" => {
            let text = update
                .get("content")
                .and_then(|content| content.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !text.is_empty() {
                let _ = emit.send(TurnEvent::TextDelta { text: text.to_string() });
            }
        }
        "agent_message" => {
            if let Some(blocks) = update.get("content").and_then(Value::as_array) {
                let text: String = blocks
                    .iter()
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("");
                if !text.is_empty() {
                    *final_text = text;
                    let _ = emit.send(TurnEvent::AssistantText { text: final_text.clone() });
                }
            }
        }
        "tool_call" | "tool_call_update" => {
            let title = update
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string();
            let tool_call_id = update.get("toolCallId").and_then(Value::as_str).unwrap_or("").to_string();
            let status = update.get("status").and_then(Value::as_str).unwrap_or("");
            match status {
                "completed" | "failed" => {
                    let _ = emit.send(TurnEvent::ToolCompleted {
                        tool: title,
                        detail: Some(tool_call_id),
                        exit_code: if status == "failed" { Some(1) } else { Some(0) },
                    });
                }
                _ => {
                    let _ = emit.send(TurnEvent::ToolStarted { tool: title, detail: Some(tool_call_id) });
                }
            }
        }
        _ => {}
    }
}

impl AcpAdapter {
    pub fn new(provider: ProviderId, spawner: HalvesSpawner) -> Self {
        AcpAdapter {
            provider,
            spawner,
            runtime: Arc::new(AsyncMutex::new(None)),
            notification_receiver: None,
            next_id: Arc::new(AtomicU32::new(1)),
            auth_signature: Arc::new(Mutex::new(None)),
        }
    }

    async fn ensure_runtime(&mut self, auth_env: &[(String, String)]) -> Result<Arc<AcpRuntime>> {
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
            let runtime = Arc::new(AcpRuntime {
                reader: AsyncMutex::new(reader),
                writer: AsyncMutex::new(writer),
                pending: Mutex::new(HashMap::new()),
                notifications: notification_sender,
                current_emit: AsyncMutex::new(None),
                active_session: AsyncMutex::new(None),
            });
            spawn_reader(runtime.clone());
            *slot = Some(runtime.clone());
            self.notification_receiver = Some(notification_receiver);
            *self.auth_signature.lock().unwrap() = Some(signature);
        }
        Ok(self.runtime.lock().await.clone().expect("acp runtime spawned"))
    }

    async fn request(&self, runtime: &Arc<AcpRuntime>, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        runtime.pending.lock().unwrap().insert(id, sender);
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        {
            let mut writer = runtime.writer.lock().await;
            writer.write_line(&serde_json::to_string(&frame)?).await?;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(120), receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => anyhow::bail!("acp error: {error}"),
            _ => anyhow::bail!("acp request timed out: {method}"),
        }
    }

    async fn notify(&self, runtime: &Arc<AcpRuntime>, method: &str, params: Value) -> Result<()> {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut writer = runtime.writer.lock().await;
        writer.write_line(&serde_json::to_string(&frame)?).await?;
        Ok(())
    }

    async fn ensure_initialized(&self, runtime: &Arc<AcpRuntime>) -> Result<()> {
        let params = json!({
            "protocolVersion": 2,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true },
                "terminal": false
            },
            "clientInfo": { "name": "baton", "title": "baton", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true }
            },
            "info": { "name": "baton", "title": "baton", "version": env!("CARGO_PKG_VERSION") },
            "_meta": { "clientType": "extension" }
        });
        self.request(runtime, "initialize", params).await?;
        Ok(())
    }

    async fn select_model(&self, runtime: &Arc<AcpRuntime>, session_id: &str, model: &str) -> Result<()> {
        let set_model = json!({ "sessionId": session_id, "modelId": model });
        if self.request(runtime, "session/set_model", set_model).await.is_err() {
            let set_config = json!({ "sessionId": session_id, "configId": "model", "value": model });
            let _ = self.request(runtime, "session/set_config_option", set_config).await;
        }
        Ok(())
    }
}

#[async_trait]
impl BatonAdapter for AcpAdapter {
    fn id(&self) -> ProviderId {
        self.provider
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
                    let session_id = runtime.active_session.lock().await.clone();
                    let session_id = match session_id {
                        Some(session_id) => session_id,
                        None => return,
                    };
                    let id = next_id.fetch_add(1, Ordering::SeqCst);
                    let frame = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "session/cancel",
                        "params": { "sessionId": session_id, "_meta": { "cancelTrigger": "ctrl_c" } }
                    });
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

        let session_id = match session {
            Some(session) => {
                let params = json!({ "sessionId": session.native_session_id, "cwd": input.cwd, "mcpServers": [] });
                if self.request(&runtime, "session/resume", params.clone()).await.is_err() {
                    self.request(&runtime, "session/load", params).await?;
                }
                session.native_session_id
            }
            None => {
                let params = json!({ "cwd": input.cwd, "mcpServers": [] });
                let created = self.request(&runtime, "session/new", params).await?;
                match created.get("sessionId").and_then(Value::as_str) {
                    Some(session_id) => session_id.to_string(),
                    None => anyhow::bail!("acp agent did not return a session id"),
                }
            }
        };
        *runtime.active_session.lock().await = Some(session_id.clone());
        self.select_model(&runtime, &session_id, &input.model).await?;

        let prompt_params = json!({
            "sessionId": session_id,
            "prompt": [ { "type": "text", "text": input.prompt } ]
        });
        let prompt_id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, mut prompt_receiver) = oneshot::channel();
        runtime.pending.lock().unwrap().insert(prompt_id, sender);
        let frame = json!({ "jsonrpc": "2.0", "id": prompt_id, "method": "session/prompt", "params": prompt_params });
        {
            let mut writer = runtime.writer.lock().await;
            writer.write_line(&serde_json::to_string(&frame)?).await?;
        }

        let mut receiver = match self.notification_receiver.take() {
            Some(receiver) => receiver,
            None => anyhow::bail!("acp notification receiver missing"),
        };
        let mut final_text = String::new();
        let mut usage: Option<TurnUsage> = None;
        let result: Result<()> = loop {
            tokio::select! {
                notification = receiver.recv() => {
                    let (method, params) = match notification {
                        Some(event) => event,
                        None => break Err(anyhow::anyhow!("acp connection closed")),
                    };
                    if method != "session/update" {
                        continue;
                    }
                    let update_session = params.get("sessionId").and_then(Value::as_str).unwrap_or("");
                    if update_session != session_id {
                        continue;
                    }
                    let update = params.get("update").cloned().unwrap_or(Value::Null);
                    let variant = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
                    match variant {
                        "state_update" => {
                            let state = update.get("state").and_then(Value::as_str).unwrap_or("");
                            if state == "idle" || state == "requires_action" {
                                if let Some(state_usage) = update.get("usage") {
                                    usage = Some(parse_usage(state_usage));
                                }
                                break Ok(());
                            }
                        }
                        _ => handle_session_update(&update, &emit, &mut final_text),
                    }
                }
                prompt_result = &mut prompt_receiver => {
                    match prompt_result {
                        Ok(Ok(result)) => {
                            if let Some(result_usage) = result.get("usage") {
                                if usage.is_none() {
                                    usage = Some(parse_usage(result_usage));
                                }
                            }
                            break Ok(());
                        }
                        Ok(Err(error)) => break Err(anyhow::anyhow!("acp error: {error}")),
                        Err(_) => break Err(anyhow::anyhow!("acp connection closed")),
                    }
                }
            }
        };
        self.notification_receiver = Some(receiver);
        *runtime.current_emit.lock().await = None;
        *runtime.active_session.lock().await = None;

        match result {
            Ok(()) => {
                let _ = emit.send(TurnEvent::TurnCompleted {
                    final_text,
                    usage,
                    native_session_id: Some(session_id),
                    rollout_path: None,
                });
                Ok(())
            }
            Err(error) => {
                let _ = emit.send(TurnEvent::TurnFailed { error: error.to_string() });
                Err(error)
            }
        }
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
