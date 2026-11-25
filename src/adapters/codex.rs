use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::adapters::{standard_approval_choices, ActiveTurn, AdapterLifecycle, AdapterTurnGuard, BatonAdapter, ForkSessionInput, LineReader, LineWriter, ModelListInput, RevertSessionInput, SteerFuture, SteerHandle, SteerOutcome, SteerRequest, TransportHalves, TurnEvent, TurnEventSender, TurnStartInput};
use crate::config::auth_signature;
use crate::types::{
    ApprovalDecision, NativeSessionRef, PermissionMode, ProviderId, QuestionAnswer, QuestionOption,
    QuestionPrompt, TurnUsage,
};

struct CodexRuntime {
    reader: AsyncMutex<Box<dyn LineReader>>,
    writer: AsyncMutex<Box<dyn LineWriter>>,
    pending: Mutex<HashMap<u32, oneshot::Sender<Result<Value, String>>>>,
    notifications: tokio::sync::mpsc::UnboundedSender<(String, Value)>,
    current_emit: AsyncMutex<Option<TurnEventSender>>,
    /// The baton turn and native ids published as one value so steering and
    /// interrupts never read a pair from two different turns.
    active_turn: AsyncMutex<Option<ActiveTurn>>,
}

/// Sends one JSON-RPC request and waits for its response. Free so the steer
/// handle can use it without borrowing the adapter.
async fn request_on(
    runtime: &Arc<CodexRuntime>,
    next_id: &AtomicU32,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    let id = next_id.fetch_add(1, Ordering::SeqCst);
    let (sender, receiver) = oneshot::channel();
    runtime.pending.lock().unwrap().insert(id, sender);
    let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    {
        let mut writer = runtime.writer.lock().await;
        writer.write_line(&serde_json::to_string(&frame)?).await?;
    }
    match tokio::time::timeout(timeout, receiver).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => anyhow::bail!("codex app-server error: {error}"),
        _ => anyhow::bail!("codex app-server request timed out: {method}"),
    }
}

pub type HalvesSpawner = Arc<dyn Fn(&[(String, String)]) -> Result<TransportHalves> + Send + Sync>;

pub struct CodexAdapter {
    spawner: HalvesSpawner,
    runtime: Arc<AsyncMutex<Option<Arc<CodexRuntime>>>>,
    notification_receiver: Option<tokio::sync::mpsc::UnboundedReceiver<(String, Value)>>,
    next_id: Arc<AtomicU32>,
    auth_signature: Arc<Mutex<Option<String>>>,
    lifecycle: AdapterLifecycle,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ApprovalKind {
    Command,
    FileChange,
    Permissions,
}

fn steer_debug(message: &str) {
    if std::env::var_os("BATON_DEBUG_STEER").is_some() {
        eprintln!("baton steer: {message}");
    }
}

fn approval_kind(method: &str) -> Option<ApprovalKind> {
    match method {
        "item/commandExecution/requestApproval" => Some(ApprovalKind::Command),
        "item/fileChange/requestApproval" => Some(ApprovalKind::FileChange),
        "item/permissions/requestApproval" => Some(ApprovalKind::Permissions),
        _ => None,
    }
}

/// Builds the response payload for a Codex approval request.
///
/// The command and file-change unions use `accept`, `acceptForSession`,
/// `decline`, and `cancel`. The permissions request is a different shape:
/// it returns the granted profile plus the grant scope.
fn approval_response(method: &str, params: &Value, decision: ApprovalDecision) -> Value {
    match approval_kind(method) {
        Some(ApprovalKind::Command) | Some(ApprovalKind::FileChange) => {
            let decision = match &decision {
                ApprovalDecision::Accept => "accept",
                ApprovalDecision::AcceptAlways => "acceptForSession",
                ApprovalDecision::Deny | ApprovalDecision::Choose(_) => "decline",
            };
            json!({ "decision": decision })
        }
        Some(ApprovalKind::Permissions) => {
            let granted = if matches!(&decision, ApprovalDecision::Accept | ApprovalDecision::AcceptAlways) {
                params.get("permissions").cloned().unwrap_or_else(|| json!({}))
            } else {
                json!({})
            };
            let scope = if decision == ApprovalDecision::AcceptAlways {
                "session"
            } else {
                "turn"
            };
            json!({ "permissions": granted, "scope": scope })
        }
        None => json!({}),
    }
}

async fn handle_server_request(
    runtime: &Arc<CodexRuntime>,
    id: Value,
    method: &str,
    params: Value,
) {
    if method == "item/tool/requestUserInput" {
        handle_user_input_request(runtime, id, &params).await;
        return;
    }
    let kind = match approval_kind(method) {
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
        .unwrap_or_else(|| {
            let label = match kind {
                ApprovalKind::Command => "command",
                ApprovalKind::FileChange => "fileChange",
                ApprovalKind::Permissions => "permissions",
            };
            format!("codex requests {label} approval")
        });
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
            let label = match kind {
                ApprovalKind::Command => "command",
                ApprovalKind::FileChange => "fileChange",
                ApprovalKind::Permissions => "permissions",
            };
            let _ = emit.send(TurnEvent::ApprovalRequest {
                id: request_id,
                kind: label.to_string(),
                summary,
                choices: standard_approval_choices(),
                respond,
            });
        }
    }
    let runtime = runtime.clone();
    let method = method.to_string();
    tokio::spawn(async move {
        let decision = receiver.await.unwrap_or(ApprovalDecision::Deny);
        let result = approval_response(&method, &params, decision);
        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        });
        if let Ok(serialized) = serde_json::to_string(&response) {
            let mut writer = runtime.writer.lock().await;
            let _ = writer.write_line(&serialized).await;
        }
    });
}

async fn handle_user_input_request(runtime: &Arc<CodexRuntime>, id: Value, params: &Value) {
    let questions = question_prompts(params);
    if questions.is_empty() {
        send_server_result(runtime, id, json!({ "answers": {} })).await;
        return;
    }
    let request_id = format!(
        "codex:{}",
        params.get("itemId").and_then(Value::as_str).unwrap_or("")
    );
    let (respond, receiver) = oneshot::channel::<Vec<QuestionAnswer>>();
    {
        let emit = runtime.current_emit.lock().await;
        if let Some(emit) = emit.as_ref() {
            let _ = emit.send(TurnEvent::QuestionRequest {
                id: request_id,
                questions: questions.clone(),
                respond,
            });
        }
    }
    let runtime = runtime.clone();
    tokio::spawn(async move {
        let answers = receiver.await.unwrap_or_default();
        send_server_result(&runtime, id, question_response(&questions, &answers)).await;
    });
}

async fn send_server_result(runtime: &Arc<CodexRuntime>, id: Value, result: Value) {
    let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    if let Ok(serialized) = serde_json::to_string(&response) {
        let mut writer = runtime.writer.lock().await;
        let _ = writer.write_line(&serialized).await;
    }
}

fn question_prompts(params: &Value) -> Vec<QuestionPrompt> {
    let Some(questions) = params.get("questions").and_then(Value::as_array) else {
        return Vec::new();
    };
    questions
        .iter()
        .enumerate()
        .filter_map(|(index, question)| {
            let text = question.get("question").and_then(Value::as_str)?.trim();
            if text.is_empty() {
                return None;
            }
            let id = question
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("question-{}", index + 1));
            let header = question
                .get("header")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|header| !header.is_empty())
                .unwrap_or("Question")
                .to_string();
            let options: Vec<QuestionOption> = question
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| {
                            let label = option.get("label").and_then(Value::as_str)?.trim();
                            if label.is_empty() {
                                return None;
                            }
                            let description = option
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or(label)
                                .to_string();
                            Some(QuestionOption {
                                label: label.to_string(),
                                description,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let allow_custom = question
                .get("isOther")
                .and_then(Value::as_bool)
                .unwrap_or(true)
                || options.is_empty();
            Some(QuestionPrompt {
                id,
                header,
                question: text.to_string(),
                options,
                allow_custom,
                multi_select: question
                    .get("multiSelect")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                secret: question
                    .get("isSecret")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn question_response(questions: &[QuestionPrompt], answers: &[QuestionAnswer]) -> Value {
    let mut map = serde_json::Map::new();
    for prompt in questions {
        if let Some(answer) = answers.iter().find(|answer| answer.id == prompt.id) {
            if !answer.answers.is_empty() {
                map.insert(prompt.id.clone(), json!({ "answers": answer.answers }));
            }
        }
    }
    json!({ "answers": map })
}

impl CodexAdapter {
    pub fn new(spawner: HalvesSpawner) -> Self {
        CodexAdapter {
            spawner,
            runtime: Arc::new(AsyncMutex::new(None)),
            notification_receiver: None,
            next_id: Arc::new(AtomicU32::new(1)),
            auth_signature: Arc::new(Mutex::new(None)),
            lifecycle: AdapterLifecycle::new(),
        }
    }

    async fn ensure_runtime(&mut self, auth_env: &[(String, String)]) -> Result<Arc<CodexRuntime>> {
        steer_debug(&format!("ensure adapter={:x} slot={:x}", self as *const Self as usize, Arc::as_ptr(&self.runtime) as usize));
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
                active_turn: AsyncMutex::new(None),
            });
            spawn_reader(runtime.clone());
            *slot = Some(runtime.clone());
            steer_debug(&format!("runtime spawned adapter={:x} slot={:x} runtime={:p}", self as *const Self as usize, Arc::as_ptr(&self.runtime) as usize, Arc::as_ptr(&runtime)));
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
        request_on(runtime, &self.next_id, method, params, Duration::from_secs(60)).await
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
            // v2 nests per-turn usage under `last` and cumulative thread usage
            // under `total`. Older servers sent the breakdown at the top level.
            let token_usage = params.get("tokenUsage").cloned().unwrap_or(Value::Null);
            let breakdown = token_usage.get("last").unwrap_or(&token_usage);
            let read = |key: &str| breakdown.get(key).and_then(Value::as_i64).unwrap_or(0);
            if thread_id == active_thread {
                let usage = TurnUsage {
                    input_tokens: read("inputTokens"),
                    output_tokens: read("outputTokens"),
                    cache_read_tokens: read("cachedInputTokens"),
                    cache_creation_tokens: read("cacheWriteInputTokens"),
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
                        let call_id = item.get("id").and_then(Value::as_str).map(str::to_string);
                        if !completed {
                            let _ = emit.send(TurnEvent::ToolStarted { id: call_id, tool: "shell".into(), detail: Some(command) });
                        } else {
                            let status = item.get("status").and_then(Value::as_str).unwrap_or("");
                            if status == "completed" || status == "failed" {
                                let _ = emit.send(TurnEvent::ToolCompleted {
                                    id: call_id,
                                    tool: "shell".into(),
                                    detail: Some(command),
                                    exit_code: item.get("exitCode").and_then(Value::as_i64),
                                });
                            }
                        }
                    }
                    "collabAgentToolCall" => {
                        let tool = item.get("tool").and_then(Value::as_str).unwrap_or("");
                        if tool == "spawnAgent" {
                            let call_id = item
                                .get("id")
                                .and_then(Value::as_str)
                                .filter(|id| !id.is_empty())
                                .map(str::to_string);
                            let name = item
                                .get("model")
                                .and_then(Value::as_str)
                                .filter(|model| !model.is_empty())
                                .unwrap_or("spawnAgent")
                                .to_string();
                            let detail = item
                                .get("prompt")
                                .and_then(Value::as_str)
                                .map(|prompt| prompt.chars().take(200).collect::<String>());
                            if completed {
                                let status = item
                                    .get("status")
                                    .and_then(Value::as_str)
                                    .unwrap_or("completed");
                                let status = match status {
                                    "failed" => "failed",
                                    "interrupted" => "interrupted",
                                    _ => "done",
                                };
                                let _ = emit.send(TurnEvent::SubagentFinished {
                                    id: call_id,
                                    name,
                                    status: status.to_string(),
                                    detail,
                                });
                            } else {
                                let _ = emit.send(TurnEvent::SubagentStarted {
                                    id: call_id,
                                    name,
                                    detail,
                                });
                            }
                        }
                    }
                    "subAgentActivity" => {
                        let id = item
                            .get("agentThreadId")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                            .map(str::to_string);
                        let name = item
                            .get("agentPath")
                            .and_then(Value::as_str)
                            .filter(|path| !path.is_empty())
                            .unwrap_or("subagent")
                            .to_string();
                        match item.get("kind").and_then(Value::as_str).unwrap_or("") {
                            "started" => {
                                let _ = emit.send(TurnEvent::SubagentStarted {
                                    id,
                                    name,
                                    detail: None,
                                });
                            }
                            "completed" | "interrupted" => {
                                let status = if item.get("kind").and_then(Value::as_str)
                                    == Some("interrupted")
                                {
                                    "interrupted"
                                } else {
                                    "done"
                                };
                                let _ = emit.send(TurnEvent::SubagentFinished {
                                    id,
                                    name,
                                    status: status.to_string(),
                                    detail: None,
                                });
                            }
                            _ => {}
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
                        let call_id = item.get("id").and_then(Value::as_str).map(str::to_string);
                        if !completed {
                            let _ = emit.send(TurnEvent::ToolStarted { id: call_id, tool: "edit".into(), detail: Some(paths) });
                        } else {
                            let _ = emit.send(TurnEvent::ToolCompleted { id: call_id, tool: "edit".into(), detail: Some(paths), exit_code: None });
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

    fn lifecycle(&self) -> Option<AdapterLifecycle> {
        Some(self.lifecycle.clone())
    }

    fn interrupt_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        let runtime = self.runtime.clone();
        let next_id = self.next_id.clone();
        let lifecycle = self.lifecycle.clone();
        Arc::new(move || {
            lifecycle.cancelling();
            let runtime = runtime.clone();
            let next_id = next_id.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let runtime = match runtime.lock().await.clone() {
                        Some(runtime) => runtime,
                        None => return,
                    };
                    let active = runtime.active_turn.lock().await.clone();
                    let active = match active {
                        Some(active) => active,
                        None => return,
                    };
                    let turn_id = match active.native_turn_id {
                        Some(turn_id) => turn_id,
                        None => return,
                    };
                    let thread_id = active.native_thread_id;
                    let id = next_id.fetch_add(1, Ordering::SeqCst);
                    let frame = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "turn/interrupt",
                        "params": { "threadId": thread_id, "turnId": turn_id }
                    });
                    if let Ok(serialized) = serde_json::to_string(&frame) {
                        let mut writer = runtime.writer.lock().await;
                        let _ = writer.write_line(&serialized).await;
                    }
                });
            }
        })
    }

    fn steer_handle(&self) -> Option<SteerHandle> {
        let runtime = self.runtime.clone();
        let next_id = self.next_id.clone();
        Some(Arc::new(move |request: SteerRequest| -> SteerFuture {
            let runtime = runtime.clone();
            let next_id = next_id.clone();
            Box::pin(async move {
                let runtime = match runtime.try_lock() {
                    Ok(slot) => slot.clone(),
                    Err(_) => return SteerOutcome::Unavailable("codex adapter is busy".into()),
                };
                let Some(runtime) = runtime else {
                    return SteerOutcome::Unavailable("codex is not running".into());
                };
                let active = match runtime.active_turn.try_lock() {
                    Ok(active) => active.clone(),
                    Err(_) => return SteerOutcome::StaleTurn,
                };
                let Some(active) = active else {
                    return SteerOutcome::StaleTurn;
                };
                if active.thread_id != request.thread_id || active.turn != request.turn {
                    return SteerOutcome::StaleTurn;
                }
                let Some(native_turn_id) = active.native_turn_id else {
                    return SteerOutcome::StaleTurn;
                };
                let params = json!({
                    "threadId": active.native_thread_id,
                    "expectedTurnId": native_turn_id,
                    "input": [ { "type": "text", "text": request.text } ]
                });
                match request_on(&runtime, &next_id, "turn/steer", params, Duration::from_secs(10)).await {
                    Ok(_) => SteerOutcome::Delivered,
                    Err(error) => SteerOutcome::Unavailable(error.to_string()),
                }
            })
        }))
    }

    async fn list_models(&mut self, input: ModelListInput) -> Result<Vec<String>> {
        let _turn_guard = AdapterTurnGuard::new(self.lifecycle.clone());
        self.lifecycle.starting("model-list");
        let runtime = self.ensure_runtime(&input.auth_env).await?;
        self.ensure_initialized(&runtime).await?;
        let mut models: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut params = json!({ "limit": 200 });
            if let Some(cursor) = cursor.as_ref() {
                params["cursor"] = json!(cursor);
            }
            let response = self.request(&runtime, "model/list", params).await?;
            if let Some(data) = response.get("data").and_then(Value::as_array) {
                for model in data {
                    if model.get("hidden").and_then(Value::as_bool).unwrap_or(false) {
                        continue;
                    }
                    let slug = model
                        .get("model")
                        .or_else(|| model.get("id"))
                        .and_then(Value::as_str)
                        .filter(|slug| !slug.is_empty());
                    if let Some(slug) = slug {
                        if !models.iter().any(|known| known == slug) {
                            models.push(slug.to_string());
                        }
                    }
                }
            }
            let next = response
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|next| !next.is_empty())
                .map(str::to_string);
            match next {
                Some(next) if cursor.as_deref() != Some(next.as_str()) => cursor = Some(next),
                _ => break,
            }
        }
        Ok(models)
    }

    async fn fork_session(&mut self, input: ForkSessionInput) -> Result<Option<String>> {
        let _turn_guard = AdapterTurnGuard::new(self.lifecycle.clone());
        self.lifecycle.starting("native-fork");
        let runtime = self.ensure_runtime(&input.auth_env).await?;
        self.ensure_initialized(&runtime).await?;
        let mut params = json!({
            "threadId": input.session.native_session_id,
            "cwd": input.cwd,
            "model": input.model,
            "excludeTurns": true,
        });
        if let Some(last_turn_id) = input.last_turn_id {
            params["lastTurnId"] = json!(last_turn_id);
        }
        let response = self.request(&runtime, "thread/fork", params).await?;
        let forked = response
            .pointer("/thread/id")
            .or_else(|| response.get("threadId"))
            .and_then(Value::as_str)
            .map(str::to_string);
        match forked {
            Some(id) if !id.is_empty() => Ok(Some(id)),
            _ => anyhow::bail!("codex thread/fork did not return a thread id"),
        }
    }

    async fn revert_session(&mut self, input: RevertSessionInput) -> Result<bool> {
        let _turn_guard = AdapterTurnGuard::new(self.lifecycle.clone());
        self.lifecycle.starting("native-revert");
        let runtime = self.ensure_runtime(&input.auth_env).await?;
        self.ensure_initialized(&runtime).await?;
        self.request(
            &runtime,
            "thread/resume",
            json!({
                "threadId": input.session.native_session_id,
                "excludeTurns": true
            }),
        )
        .await?;
        self.request(
            &runtime,
            "thread/revert",
            json!({
                "threadId": input.session.native_session_id,
                "beforeTurnId": input.before_turn_id
            }),
        )
        .await?;
        Ok(true)
    }

    async fn start_turn(
        &mut self,
        input: TurnStartInput,
        session: Option<NativeSessionRef>,
        emit: TurnEventSender,
    ) -> Result<()> {
        let _turn_guard = AdapterTurnGuard::new(self.lifecycle.clone());
        self.lifecycle.starting(&input.thread_id);
        let runtime = self.ensure_runtime(&input.auth_env).await?;
        self.ensure_initialized(&runtime).await?;
        self.lifecycle.turn_running(&input.thread_id);
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
        let _ = emit.send(TurnEvent::ProviderSession {
            native_session_id: thread_id.clone(),
            rollout_path: rollout_path.clone(),
        });

        let (approval_policy, sandbox_policy) = match input.permission_mode {
            PermissionMode::BypassPermissions => ("never".to_string(), json!({ "type": "dangerFullAccess" })),
            PermissionMode::Plan => ("on-request".to_string(), json!({ "type": "readOnly", "networkAccess": false })),
            _ => (
                "on-request".to_string(),
                json!({ "type": "workspaceWrite", "writableRoots": [], "networkAccess": false }),
            ),
        };
        let mut turn_params = json!({
            "threadId": thread_id,
            "cwd": input.cwd,
            "model": input.model,
            "input": [ { "type": "text", "text": input.prompt } ],
            "approvalPolicy": approval_policy,
            "sandboxPolicy": sandbox_policy,
            "approvalsReviewer": "user",
            "summary": "detailed"
        });
        if let Some(effort) = input.effort.as_deref() {
            turn_params["effort"] = json!(effort);
        }
        let started = self.request(&runtime, "turn/start", turn_params).await?;
        let turn_id = started
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| started.get("turnId").and_then(Value::as_str).map(str::to_string));
        *runtime.active_turn.lock().await = Some(ActiveTurn {
            thread_id: input.thread_id.clone(),
            turn: input.turn,
            native_thread_id: thread_id.clone(),
            native_turn_id: turn_id.clone(),
        });
        if let Some(turn_id) = turn_id.as_ref() {
            let _ = emit.send(TurnEvent::ProviderTurnId { id: turn_id.clone() });
        }

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
                "turn/started" => {
                    let started_thread = params.get("threadId").and_then(Value::as_str).unwrap_or("");
                    if started_thread == thread_id {
                        if let Some(id) = params
                            .get("turn")
                            .and_then(|turn| turn.get("id"))
                            .and_then(Value::as_str)
                        {
                            let _ = emit.send(TurnEvent::ProviderTurnId { id: id.to_string() });
                            if let Some(active) = runtime.active_turn.lock().await.as_mut() {
                                if active.native_thread_id == started_thread {
                                    active.native_turn_id = Some(id.to_string());
                                }
                            }
                        }
                    }
                }
                "turn/completed" => {
                    let completed_thread = params.get("threadId").and_then(Value::as_str).unwrap_or("");
                    if completed_thread == thread_id {
                        let turn = params.get("turn").cloned().unwrap_or(Value::Null);
                        let status = turn
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("completed");
                        let error = turn
                            .get("error")
                            .and_then(|error| error.get("message"))
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        if let Some(id) = turn.get("id").and_then(Value::as_str) {
                            if let Some(active) = runtime.active_turn.lock().await.as_mut() {
                                if active.native_thread_id == completed_thread {
                                    active.native_turn_id = Some(id.to_string());
                                }
                            }
                        }
                        match status {
                            "completed" => {
                                let final_message = turn
                                    .get("items")
                                    .and_then(Value::as_array)
                                    .and_then(|items| {
                                        items.iter().rev().find(|item| {
                                            item.get("type").and_then(Value::as_str)
                                                == Some("agentMessage")
                                        })
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
                            "interrupted" => {
                                let reason = error;
                                let message = reason
                                    .clone()
                                    .unwrap_or_else(|| "turn interrupted".to_string());
                                let _ = emit.send(TurnEvent::TurnInterrupted { reason });
                                break Err(anyhow::anyhow!(message));
                            }
                            "failed" => {
                                let message = error.unwrap_or_else(|| "codex turn failed".to_string());
                                let _ = emit.send(TurnEvent::TurnFailed { error: message.clone() });
                                break Err(anyhow::anyhow!(message));
                            }
                            other => {
                                let message = format!("codex turn ended with status {other}");
                                let _ = emit.send(TurnEvent::TurnFailed { error: message.clone() });
                                break Err(anyhow::anyhow!(message));
                            }
                        }
                    }
                }
                "turn/failed" => {
                    let failed_thread = params.get("threadId").and_then(Value::as_str).unwrap_or("");
                    if failed_thread == thread_id {
                        let error = params
                            .get("error")
                            .and_then(|error| error.get("message"))
                            .and_then(Value::as_str)
                            .or_else(|| params.get("error").and_then(Value::as_str))
                            .unwrap_or("codex turn failed");
                        let _ = emit.send(TurnEvent::TurnFailed { error: error.to_string() });
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
        *runtime.active_turn.lock().await = None;
        result
    }

    async fn dispose(&mut self) -> Result<()> {
        self.lifecycle.disposing();
        let runtime = self.runtime.lock().await.clone();
        if let Some(runtime) = runtime {
            let mut writer = runtime.writer.lock().await;
            let _ = writer.shutdown().await;
        }
        self.lifecycle.disposed();
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_and_file_approvals_use_v2_decisions() {
        let params = json!({ "command": "cargo test" });
        let accept = approval_response(
            "item/commandExecution/requestApproval",
            &params,
            ApprovalDecision::Accept,
        );
        assert_eq!(accept["decision"], "accept");
        let always = approval_response(
            "item/fileChange/requestApproval",
            &params,
            ApprovalDecision::AcceptAlways,
        );
        assert_eq!(always["decision"], "acceptForSession");
        let deny = approval_response(
            "item/commandExecution/requestApproval",
            &params,
            ApprovalDecision::Deny,
        );
        assert_eq!(deny["decision"], "decline", "deny is not a v2 decision value");
    }

    #[test]
    fn user_input_questions_keep_labels_custom_and_secret() {
        let params = json!({
            "questions": [
                {
                    "id": "schema_preference",
                    "header": "Fixture",
                    "question": "Strict schemas or UI flexibility?",
                    "isOther": true,
                    "isSecret": false,
                    "options": [
                        { "label": "Strict schemas", "description": "Tight validation" },
                        { "label": "UI flexibility", "description": "Looser validation" }
                    ]
                },
                {
                    "header": "Secret",
                    "question": "Token?",
                    "isOther": false,
                    "isSecret": true,
                    "options": [
                        { "label": "Use token", "description": "Continue with the token" }
                    ]
                }
            ]
        });
        let questions = question_prompts(&params);
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].id, "schema_preference");
        assert_eq!(questions[0].options[0].label, "Strict schemas");
        assert_eq!(questions[0].options[1].description, "Looser validation");
        assert!(questions[0].allow_custom);
        assert!(!questions[0].multi_select);
        assert!(!questions[1].allow_custom);
        assert!(questions[1].secret);
        assert_eq!(questions[1].id, "question-2");
    }

    #[test]
    fn user_input_question_ids_keep_surrounding_whitespace() {
        let params = json!({
            "questions": [
                { "id": " choice ", "header": "One", "question": "Pick", "isOther": false }
            ]
        });
        let questions = question_prompts(&params);
        assert_eq!(questions[0].id, " choice ");
        assert!(questions[0].allow_custom, "an empty option list needs the custom row");
    }

    #[test]
    fn user_input_response_maps_native_ids_and_custom_answers() {
        let questions = vec![
            QuestionPrompt {
                id: "first".into(),
                header: "One".into(),
                question: "Pick".into(),
                options: Vec::new(),
                allow_custom: true,
                multi_select: false,
                secret: false,
            },
            QuestionPrompt {
                id: "second".into(),
                header: "Two".into(),
                question: "Pick again".into(),
                options: Vec::new(),
                allow_custom: true,
                multi_select: false,
                secret: false,
            },
        ];
        let answers = vec![
            QuestionAnswer {
                id: "first".into(),
                answers: vec!["option one".into()],
            },
            QuestionAnswer {
                id: "second".into(),
                answers: vec!["custom answer".into()],
            },
        ];
        let response = question_response(&questions, &answers);
        assert_eq!(response["answers"]["first"]["answers"][0], "option one");
        assert_eq!(response["answers"]["second"]["answers"][0], "custom answer");
    }

    #[test]
    fn permissions_approval_returns_granted_profile_and_scope() {
        let params = json!({
            "permissions": {
                "network": { "enabled": true },
                "fileSystem": { "read": ["/tmp"], "write": [] }
            }
        });
        let accept = approval_response(
            "item/permissions/requestApproval",
            &params,
            ApprovalDecision::Accept,
        );
        assert_eq!(accept["permissions"], params["permissions"]);
        assert_eq!(accept["scope"], "turn");
        assert!(accept.get("decision").is_none());

        let always = approval_response(
            "item/permissions/requestApproval",
            &params,
            ApprovalDecision::AcceptAlways,
        );
        assert_eq!(always["scope"], "session");

        let deny = approval_response(
            "item/permissions/requestApproval",
            &params,
            ApprovalDecision::Deny,
        );
        assert_eq!(deny["permissions"], json!({}));
        assert_eq!(deny["scope"], "turn");
    }
}
