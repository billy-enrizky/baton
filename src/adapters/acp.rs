use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::adapters::{
    standard_approval_choices, AdapterLifecycle, AdapterTurnGuard, BatonAdapter, LineReader,
    LineWriter, ModelListInput, TransportHalves, TurnEvent, TurnEventSender, TurnStartInput,
};
use crate::config::auth_signature;
use crate::types::{
    ApprovalChoice, ApprovalDecision, NativeSessionRef, ProviderId, QuestionAnswer, QuestionOption,
    QuestionPrompt, TurnUsage,
};

struct AcpRuntime {
    reader: AsyncMutex<Box<dyn LineReader>>,
    writer: AsyncMutex<Box<dyn LineWriter>>,
    pending: Mutex<HashMap<u32, oneshot::Sender<Result<Value, String>>>>,
    notifications: tokio::sync::mpsc::UnboundedSender<(String, Value)>,
    current_emit: AsyncMutex<Option<TurnEventSender>>,
    active_session: AsyncMutex<Option<String>>,
}

pub type HalvesSpawner = Arc<dyn Fn(&[(String, String)]) -> Result<TransportHalves> + Send + Sync>;

/// Clears the live event sender on every exit path, including an error before
/// the prompt exists. A leaked sender clone makes the engine's forwarder wait
/// forever for a turn that already failed.
struct EmitScope {
    runtime: Arc<AcpRuntime>,
}

impl EmitScope {
    async fn set(runtime: &Arc<AcpRuntime>, emit: TurnEventSender) -> Self {
        *runtime.current_emit.lock().await = Some(emit);
        Self { runtime: runtime.clone() }
    }
}

impl Drop for EmitScope {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.runtime.current_emit.try_lock() {
            *slot = None;
        }
    }
}

pub struct AcpAdapter {
    provider: ProviderId,
    spawner: HalvesSpawner,
    runtime: Arc<AsyncMutex<Option<Arc<AcpRuntime>>>>,
    notification_receiver: Option<tokio::sync::mpsc::UnboundedReceiver<(String, Value)>>,
    next_id: Arc<AtomicU32>,
    auth_signature: Arc<Mutex<Option<String>>>,
    lifecycle: AdapterLifecycle,
}

fn acp_debug(message: &str) {
    if std::env::var_os("BATON_DEBUG_ACP").is_some() {
        eprintln!("baton acp: {message}");
    }
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
            acp_debug(&format!("frame method={method:?} id={:?}", frame.get("id")));
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
    if matches!(method, "x.ai/ask_user_question" | "_x.ai/ask_user_question") {
        handle_xai_question(runtime, id, &params).await;
        return;
    }
    if method == "session/request_permission" {
        let questions = antigravity_question_prompts(&params);
        if !questions.is_empty() {
            handle_antigravity_question(runtime, id, &params, questions).await;
            return;
        }
    }
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
    let choices = permission_choices(&options);
    let (respond, receiver) = oneshot::channel::<ApprovalDecision>();
    {
        let emit = runtime.current_emit.lock().await;
        if let Some(emit) = emit.as_ref() {
            let _ = emit.send(TurnEvent::ApprovalRequest {
                id: format!("acp:{summary}"),
                kind: "permissions".into(),
                summary,
                choices,
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

fn xai_question_params(params: &Value) -> &Value {
    params
        .get("params")
        .filter(|_| params.get("method").is_some())
        .unwrap_or(params)
}

fn xai_question_prompts(params: &Value) -> Vec<QuestionPrompt> {
    let params = xai_question_params(params);
    let Some(questions) = params.get("questions").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut used_ids = HashSet::new();
    questions
        .iter()
        .filter_map(|question| {
            let text = question.get("question").and_then(Value::as_str)?.trim();
            if text.is_empty() {
                return None;
            }
            let mut id = question
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| text.to_string());
            if !used_ids.insert(id.clone()) {
                let base = id.clone();
                let mut suffix = 2;
                loop {
                    let candidate = format!("{base}:{suffix}");
                    if used_ids.insert(candidate.clone()) {
                        id = candidate;
                        break;
                    }
                    suffix += 1;
                }
            }
            let mut options: Vec<QuestionOption> = question
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
            if options.is_empty() {
                options.push(QuestionOption {
                    label: "OK".into(),
                    description: "Continue".into(),
                });
            }
            Some(QuestionPrompt {
                id,
                header: "Question".into(),
                question: text.to_string(),
                options,
                allow_custom: true,
                multi_select: question
                    .get("multiSelect")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                secret: false,
            })
        })
        .collect()
}

fn xai_question_response(questions: &[QuestionPrompt], answers: &[QuestionAnswer]) -> Value {
    let mut map = serde_json::Map::new();
    for question in questions {
        if let Some(answer) = answers.iter().find(|answer| answer.id == question.id) {
            if !answer.answers.is_empty() {
                map.insert(question.question.clone(), json!(answer.answers));
            }
        }
    }
    if map.is_empty() {
        json!({ "outcome": "cancelled" })
    } else {
        json!({ "outcome": "accepted", "answers": map })
    }
}

async fn handle_xai_question(runtime: &Arc<AcpRuntime>, id: Value, params: &Value) {
    let questions = xai_question_prompts(params);
    if questions.is_empty() {
        send_acp_result(runtime, id, json!({ "outcome": "cancelled" })).await;
        return;
    }
    let params = xai_question_params(params);
    let request_id = format!(
        "xai:{}",
        params
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or("")
    );
    handle_question_request(runtime, id, request_id, questions, xai_question_response).await;
}

fn antigravity_question_prompts(params: &Value) -> Vec<QuestionPrompt> {
    let Some(tool_call_id) = params
        .get("toolCall")
        .and_then(|tool_call| tool_call.get("toolCallId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| id.starts_with("interaction_"))
    else {
        return Vec::new();
    };
    let question = params
        .get("toolCall")
        .and_then(|tool_call| tool_call.get("title"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("Choose an option.");
    let options: Vec<QuestionOption> = params
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    let id = option.get("optionId").and_then(Value::as_str)?.trim();
                    if id.is_empty() {
                        return None;
                    }
                    let label = option
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .unwrap_or(id);
                    Some(QuestionOption {
                        label: label.to_string(),
                        description: label.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if options.is_empty() {
        return Vec::new();
    }
    vec![QuestionPrompt {
        id: tool_call_id.to_string(),
        header: "Question".into(),
        question: question.to_string(),
        options,
        allow_custom: false,
        multi_select: false,
        secret: false,
    }]
}

fn antigravity_question_response(
    params: &Value,
    questions: &[QuestionPrompt],
    answers: &[QuestionAnswer],
) -> Value {
    let cancelled = json!({ "outcome": { "outcome": "cancelled" } });
    let Some(question) = questions.first() else {
        return cancelled;
    };
    let Some(answer) = answers
        .iter()
        .find(|answer| answer.id == question.id)
        .and_then(|answer| answer.answers.first())
        .map(String::as_str)
    else {
        return cancelled;
    };
    let Some(options) = params.get("options").and_then(Value::as_array) else {
        return cancelled;
    };
    let mut matched = options.iter().filter_map(|option| {
        let id = option.get("optionId").and_then(Value::as_str)?.trim();
        if id.is_empty() {
            return None;
        }
        let label = option
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(id);
        (label == answer || id == answer).then(|| id.to_string())
    });
    let Some(option_id) = matched.next() else {
        return cancelled;
    };
    if matched.next().is_some() {
        return cancelled;
    }
    json!({ "outcome": { "outcome": "selected", "optionId": option_id } })
}

async fn handle_antigravity_question(
    runtime: &Arc<AcpRuntime>,
    id: Value,
    params: &Value,
    questions: Vec<QuestionPrompt>,
) {
    let request_id = format!(
        "antigravity:{}",
        questions
            .first()
            .map(|question| question.id.as_str())
            .unwrap_or("")
    );
    let params = params.clone();
    handle_question_request(
        runtime,
        id,
        request_id,
        questions,
        move |questions, answers| antigravity_question_response(&params, questions, answers),
    )
    .await;
}

async fn handle_question_request<F>(
    runtime: &Arc<AcpRuntime>,
    id: Value,
    request_id: String,
    questions: Vec<QuestionPrompt>,
    response: F,
) where
    F: FnOnce(&[QuestionPrompt], &[QuestionAnswer]) -> Value + Send + 'static,
{
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
        send_acp_result(&runtime, id, response(&questions, &answers)).await;
    });
}

async fn send_acp_result(runtime: &Arc<AcpRuntime>, id: Value, result: Value) {
    let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    if let Ok(serialized) = serde_json::to_string(&response) {
        let mut writer = runtime.writer.lock().await;
        let _ = writer.write_line(&serialized).await;
    }
}

fn pick_permission_option(options: &[Value], decision: Option<ApprovalDecision>) -> Option<String> {
    if let Some(ApprovalDecision::Choose(id)) = &decision {
        return options
            .iter()
            .filter_map(|option| option.get("optionId").and_then(Value::as_str))
            .any(|candidate| candidate == id)
            .then(|| id.clone());
    }
    let wanted_kind = match &decision {
        Some(ApprovalDecision::Accept) | None => "allow_once",
        Some(ApprovalDecision::AcceptAlways) => "allow_always",
        Some(ApprovalDecision::Deny) => "reject_once",
        Some(ApprovalDecision::Choose(_)) => return None,
    };
    let fallback_kind = match &decision {
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

fn permission_choices(options: &[Value]) -> Vec<ApprovalChoice> {
    let choices: Vec<ApprovalChoice> = options
        .iter()
        .filter_map(|option| {
            let id = option.get("optionId").and_then(Value::as_str)?.to_string();
            let kind = option
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let label = option
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| (!kind.is_empty()).then(|| kind.clone()))
                .unwrap_or_else(|| id.clone());
            Some(ApprovalChoice { id, label, kind })
        })
        .collect();
    if choices.is_empty() {
        standard_approval_choices()
    } else {
        choices
    }
}

fn collect_model_values(value: &Value, models: &mut Vec<String>) {
    if let Some(model) = value
        .get("value")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
    {
        if !models.iter().any(|known| known == model) {
            models.push(model.to_string());
        }
        return;
    }
    if let Some(options) = value.get("options").and_then(Value::as_array) {
        for option in options {
            collect_model_values(option, models);
        }
    }
}

/// Antigravity exposes model families as config values and encodes the
/// thinking level in the variant id. The ACP config option accepts the family.
fn antigravity_model_family(model: &str) -> &str {
    for suffix in ["-xhigh", "-high", "-medium", "-low"] {
        if let Some(family) = model.strip_suffix(suffix) {
            return family;
        }
    }
    model
}

fn model_options(setup: &Value) -> Vec<String> {
    let mut models = Vec::new();
    let Some(options) = setup.get("configOptions").and_then(Value::as_array) else {
        return models;
    };
    for option in options {
        let category = option.get("category").and_then(Value::as_str).unwrap_or("");
        let id = option.get("id").and_then(Value::as_str).unwrap_or("");
        let name = option.get("name").and_then(Value::as_str).unwrap_or("");
        let is_model = category == "model"
            || id.eq_ignore_ascii_case("model")
            || name.to_ascii_lowercase().contains("model");
        if !is_model || option.get("type").and_then(Value::as_str) != Some("select") {
            continue;
        }
        if let Some(values) = option.get("options").and_then(Value::as_array) {
            for value in values {
                collect_model_values(value, &mut models);
            }
        }
    }
    models
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

fn plan_entries(value: Option<&Value>) -> Vec<crate::types::PlanEntry> {
    value
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    Some(crate::types::PlanEntry {
                        content: entry.get("content")?.as_str()?.to_string(),
                        status: crate::types::normalize_plan_status(entry.get("status")?.as_str()?),
                        priority: entry
                            .get("priority")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn handle_session_update(
    update: &Value,
    provider: crate::types::ProviderId,
    emit: &TurnEventSender,
    final_text: &mut String,
) {
    let variant = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
    match variant {
        "agent_message_chunk" => {
            let text = update
                .get("content")
                .and_then(|content| content.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !text.is_empty() {
                final_text.push_str(text);
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
        "plan" => {
            let _ = emit.send(TurnEvent::PlanUpdated {
                provider,
                plan_id: None,
                entries: plan_entries(update.get("entries")),
                explanation: None,
            });
        }
        "plan_update" => {
            let plan = update.get("plan");
            if plan
                .and_then(|plan| plan.get("type"))
                .and_then(Value::as_str)
                == Some("items")
            {
                let _ = emit.send(TurnEvent::PlanUpdated {
                    provider,
                    plan_id: plan
                        .and_then(|plan| plan.get("planId"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    entries: plan_entries(plan.and_then(|plan| plan.get("entries"))),
                    explanation: None,
                });
            }
        }
        "plan_removed" => {
            if let Some(plan_id) = update.get("planId").and_then(Value::as_str) {
                let _ = emit.send(TurnEvent::PlanRemoved {
                    provider,
                    plan_id: plan_id.to_string(),
                });
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
                        id: (!tool_call_id.is_empty()).then_some(tool_call_id.clone()),
                        tool: title,
                        detail: Some(tool_call_id),
                        exit_code: if status == "failed" { Some(1) } else { Some(0) },
                    });
                }
                _ => {
                    let _ = emit.send(TurnEvent::ToolStarted {
                        id: (!tool_call_id.is_empty()).then_some(tool_call_id.clone()),
                        tool: title,
                        detail: Some(tool_call_id),
                    });
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
            lifecycle: AdapterLifecycle::new(),
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
        acp_debug(&format!("request {method}"));
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        runtime.pending.lock().unwrap().insert(id, sender);
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        {
            let mut writer = runtime.writer.lock().await;
            writer.write_line(&serde_json::to_string(&frame)?).await?;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(120), receiver).await {
            Ok(Ok(Ok(value))) => {
                acp_debug(&format!("response {method} ok"));
                Ok(value)
            }
            Ok(Ok(Err(error))) => {
                acp_debug(&format!("response {method} error={error}"));
                anyhow::bail!("acp error: {error}")
            }
            _ => {
                acp_debug(&format!("response {method} timeout"));
                anyhow::bail!("acp request timed out: {method}")
            }
        }
    }

    async fn ensure_initialized(
        &self,
        runtime: &Arc<AcpRuntime>,
        auth_env: &[(String, String)],
    ) -> Result<()> {
        // The standard ACP shape. Cursor's schema rejects extra clientInfo
        // fields and duplicate info/capabilities blocks with Invalid params.
        let params = json!({
            "protocolVersion": 2,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true },
                "terminal": false
            },
            "clientInfo": { "name": "baton", "version": env!("CARGO_PKG_VERSION") }
        });
        self.request(runtime, "initialize", params).await?;
        // Antigravity's ACP server keeps credentials per profile. An API-key
        // run must select the gemini-api-key method explicitly; a stored OAuth
        // login (no auth env) needs no call.
        if self.provider == ProviderId::Antigravity
            && auth_env.iter().any(|(key, _)| key == "GEMINI_API_KEY")
        {
            self.request(
                runtime,
                "authenticate",
                json!({ "methodId": "gemini-api-key" }),
            )
            .await?;
        }
        // Cursor's ACP server uses the stored `cursor-agent login` session.
        // API-key runs authenticate through the environment and skip this.
        if self.provider == ProviderId::Cursor && auth_env.is_empty() {
            self.request(
                runtime,
                "authenticate",
                json!({ "methodId": "cursor_login" }),
            )
            .await?;
        }
        Ok(())
    }

    async fn select_model(&self, runtime: &Arc<AcpRuntime>, session_id: &str, model: &str) -> Result<()> {
        if self.provider == ProviderId::Antigravity {
            // Antigravity models are config families (`gemini-3.7-flash`), and
            // `session/set_model` silently ignores the `-high`/`-low` ids that
            // the print-mode catalog advertises.
            let set_option = json!({
                "sessionId": session_id,
                "configId": "model",
                "value": antigravity_model_family(model),
            });
            self.request(runtime, "session/set_config_option", set_option).await?;
            return Ok(());
        }
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

    async fn list_models(&mut self, input: ModelListInput) -> Result<Vec<String>> {
        let _turn_guard = AdapterTurnGuard::new(self.lifecycle.clone());
        self.lifecycle.starting("model-list");
        let runtime = self.ensure_runtime(&input.auth_env).await?;
        self.ensure_initialized(&runtime, &input.auth_env).await?;
        let created = self
            .request(
                &runtime,
                "session/new",
                json!({ "cwd": input.cwd, "mcpServers": [] }),
            )
            .await?;
        Ok(model_options(&created))
    }

    async fn start_turn(
        &mut self,
        input: TurnStartInput,
        session: Option<NativeSessionRef>,
        emit: TurnEventSender,
    ) -> Result<()> {
        let _turn_guard = AdapterTurnGuard::new(self.lifecycle.clone());
        self.lifecycle.starting(&input.thread_id);
        if input.effort.is_some() {
            anyhow::bail!("{} does not support reasoning effort; use codex or claude", self.provider);
        }
        let runtime = self.ensure_runtime(&input.auth_env).await?;
        self.ensure_initialized(&runtime, &input.auth_env).await?;
        self.lifecycle.turn_running(&input.thread_id);
        let _emit_scope = EmitScope::set(&runtime, emit.clone()).await;

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
        let _ = emit.send(TurnEvent::ProviderSession {
            native_session_id: session_id.clone(),
            rollout_path: None,
        });
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
                        _ => handle_session_update(&update, self.provider, &emit, &mut final_text),
                    }
                }
                // The prompt RPC reply is the authoritative turn end. Every
                // branch below breaks, so the oneshot is never polled again.
                prompt_result = &mut prompt_receiver => {
                    match prompt_result {
                        Ok(Ok(result)) => {
                            if let Some(result_usage) = result.get("usage") {
                                if usage.is_none() {
                                    usage = Some(parse_usage(result_usage));
                                }
                            }
                            // Drain updates the agent sent before the reply
                            // (usage, trailing message chunks), then end.
                            while let Ok((method, params)) = receiver.try_recv() {
                                if method != "session/update" {
                                    continue;
                                }
                                if params.get("sessionId").and_then(Value::as_str)
                                    != Some(session_id.as_str())
                                {
                                    continue;
                                }
                                let update = params.get("update").cloned().unwrap_or(Value::Null);
                                let variant = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
                                if variant == "state_update" {
                                    if let Some(state_usage) = update.get("usage") {
                                        usage = Some(parse_usage(state_usage));
                                    }
                                } else {
                                    handle_session_update(
                                        &update,
                                        self.provider,
                                        &emit,
                                        &mut final_text,
                                    );
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
    use super::{
        antigravity_model_family, antigravity_question_prompts, antigravity_question_response,
        permission_choices, pick_permission_option, xai_question_prompts, xai_question_response,
    };
    use crate::types::{ApprovalDecision, QuestionAnswer, QuestionPrompt};
    use serde_json::json;

    #[test]
    fn antigravity_model_families_strip_thinking_suffixes() {
        assert_eq!(antigravity_model_family("gemini-3.8-flash-high"), "gemini-3.8-flash");
        assert_eq!(antigravity_model_family("gemini-4-hydrogen-xhigh"), "gemini-4-hydrogen");
        assert_eq!(antigravity_model_family("gemini-3.7-flash"), "gemini-3.7-flash");
    }

    #[test]
    fn permission_choices_keep_the_provider_labels_and_ids() {
        let options = vec![
            json!({ "optionId": "opt-allow", "name": "Allow", "kind": "allow_once" }),
            json!({ "optionId": "opt-always", "name": "Always allow", "kind": "allow_always" }),
            json!({ "optionId": "opt-reject", "name": "Reject", "kind": "reject_once" }),
        ];
        let choices = permission_choices(&options);
        assert_eq!(choices.len(), 3);
        assert_eq!(choices[0].id, "opt-allow");
        assert_eq!(choices[0].label, "Allow");
        assert_eq!(choices[1].kind, "allow_always");
        assert_eq!(choices[2].label, "Reject");
    }

    #[test]
    fn antigravity_interactions_become_question_prompts() {
        let params = json!({
            "sessionId": "session-1",
            "toolCall": {
                "toolCallId": "interaction_9960062f",
                "title": "Which database should the service use?"
            },
            "options": [
                { "optionId": "opt-postgres", "name": "Postgres", "kind": "allow_once" },
                { "optionId": "opt-sqlite", "name": "SQLite", "kind": "allow_once" },
                { "optionId": "opt-cancel", "name": "Cancel", "kind": "reject_once" }
            ]
        });
        let questions = antigravity_question_prompts(&params);
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].id, "interaction_9960062f");
        assert_eq!(
            questions[0].question,
            "Which database should the service use?"
        );
        assert_eq!(questions[0].options.len(), 3);
        assert_eq!(questions[0].options[1].label, "SQLite");
        assert!(!questions[0].allow_custom);

        let permission = json!({
            "toolCall": { "toolCallId": "tc-1", "title": "bash rm" },
            "options": [{ "optionId": "opt-allow", "name": "Allow" }]
        });
        assert!(antigravity_question_prompts(&permission).is_empty());
    }

    #[test]
    fn antigravity_question_response_maps_a_unique_label_to_its_option_id() {
        let params = json!({
            "options": [
                { "optionId": "opt-postgres", "name": "Postgres" },
                { "optionId": "opt-sqlite", "name": "SQLite" }
            ]
        });
        let questions = vec![QuestionPrompt {
            id: "interaction_1".into(),
            header: "Question".into(),
            question: "Which database?".into(),
            options: Vec::new(),
            allow_custom: false,
            multi_select: false,
            secret: false,
        }];
        let answers = vec![QuestionAnswer {
            id: "interaction_1".into(),
            answers: vec!["SQLite".into()],
        }];
        let response = antigravity_question_response(&params, &questions, &answers);
        assert_eq!(response["outcome"]["outcome"], "selected");
        assert_eq!(response["outcome"]["optionId"], "opt-sqlite");

        let unknown = vec![QuestionAnswer {
            id: "interaction_1".into(),
            answers: vec!["MySQL".into()],
        }];
        assert_eq!(
            antigravity_question_response(&params, &questions, &unknown)["outcome"]["outcome"],
            "cancelled"
        );
    }

    #[test]
    fn antigravity_question_response_refuses_an_ambiguous_label() {
        let params = json!({
            "options": [
                { "optionId": "opt-one", "name": "Same label" },
                { "optionId": "opt-two", "name": "Same label" }
            ]
        });
        let questions = vec![QuestionPrompt {
            id: "interaction_1".into(),
            header: "Question".into(),
            question: "Pick".into(),
            options: Vec::new(),
            allow_custom: false,
            multi_select: false,
            secret: false,
        }];
        let answers = vec![QuestionAnswer {
            id: "interaction_1".into(),
            answers: vec!["Same label".into()],
        }];
        assert_eq!(
            antigravity_question_response(&params, &questions, &answers)["outcome"]["outcome"],
            "cancelled"
        );
    }

    #[test]
    fn a_typed_choice_must_be_one_of_the_offered_options() {
        let options = vec![
            json!({ "optionId": "opt-allow", "name": "Allow", "kind": "allow_once" }),
            json!({ "optionId": "opt-reject", "name": "Reject", "kind": "reject_once" }),
        ];
        assert_eq!(
            pick_permission_option(
                &options,
                Some(ApprovalDecision::Choose("opt-reject".into()))
            ),
            Some("opt-reject".into())
        );
        assert_eq!(
            pick_permission_option(
                &options,
                Some(ApprovalDecision::Choose("opt-unknown".into()))
            ),
            None
        );
        assert_eq!(
            pick_permission_option(&options, Some(ApprovalDecision::Accept)),
            Some("opt-allow".into())
        );
    }

    #[test]
    fn xai_question_prompts_keep_labels_and_fallbacks() {
        let direct = json!({
            "sessionId": "session-1",
            "toolCallId": "tool-call-1",
            "mode": "default",
            "questions": [{
                "id": "scope",
                "question": "Which scope should Grok use?",
                "multiSelect": true,
                "options": [
                    { "label": "Workspace", "description": "Use the current workspace" },
                    { "label": "Session", "description": "Only use this session" }
                ]
            }]
        });
        let questions = xai_question_prompts(&direct);
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].id, "scope");
        assert_eq!(questions[0].header, "Question");
        assert_eq!(questions[0].options[0].label, "Workspace");
        assert_eq!(questions[0].options[1].description, "Only use this session");
        assert!(questions[0].allow_custom);
        assert!(questions[0].multi_select);

        let wrapped = json!({
            "method": "_x.ai/ask_user_question",
            "params": {
                "sessionId": "session-1",
                "toolCallId": "tool-call-2",
                "mode": "plan",
                "questions": [{ "question": "Pick one", "options": [] }]
            }
        });
        let questions = xai_question_prompts(&wrapped);
        assert_eq!(questions[0].id, "Pick one");
        assert_eq!(questions[0].options[0].label, "OK");
        assert!(!questions[0].multi_select);
    }

    #[test]
    fn xai_question_response_uses_question_text_keys() {
        let questions = vec![QuestionPrompt {
            id: "scope".into(),
            header: "Question".into(),
            question: "Which scope should Grok use?".into(),
            options: Vec::new(),
            allow_custom: true,
            multi_select: false,
            secret: false,
        }];
        let answers = vec![QuestionAnswer {
            id: "scope".into(),
            answers: vec!["Workspace".into()],
        }];
        let response = xai_question_response(&questions, &answers);
        assert_eq!(response["outcome"], "accepted");
        assert_eq!(
            response["answers"]["Which scope should Grok use?"][0],
            "Workspace"
        );
        let cancelled = xai_question_response(&questions, &[]);
        assert_eq!(cancelled["outcome"], "cancelled");
    }

    #[test]
    fn xai_multi_select_response_keeps_every_choice() {
        let questions = vec![QuestionPrompt {
            id: "features".into(),
            header: "Question".into(),
            question: "Which features?".into(),
            options: Vec::new(),
            allow_custom: true,
            multi_select: true,
            secret: false,
        }];
        let answers = vec![QuestionAnswer {
            id: "features".into(),
            answers: vec!["CI".into(), "Installer".into()],
        }];
        let response = xai_question_response(&questions, &answers);
        assert_eq!(response["outcome"], "accepted");
        assert_eq!(response["answers"]["Which features?"][0], "CI");
        assert_eq!(response["answers"]["Which features?"][1], "Installer");
    }

    #[test]
    fn xai_question_prompts_make_prompt_ids_unique() {
        let params = json!({
            "sessionId": "session-1",
            "toolCallId": "tool-call-1",
            "mode": "default",
            "questions": [
                { "id": "Pick one", "question": "First question" },
                { "question": "Pick one" }
            ]
        });
        let questions = xai_question_prompts(&params);
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].id, "Pick one");
        assert_eq!(questions[1].id, "Pick one:2");
    }
}
