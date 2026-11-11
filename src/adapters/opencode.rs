use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncBufReadExt;
use tokio::process::Child;
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use uuid::Uuid;

use crate::adapters::{BatonAdapter, ModelListInput, SteerHandle, TurnEvent, TurnEventSender, TurnStartInput};
use crate::config::auth_signature;
use crate::types::{ApprovalDecision, NativeSessionRef, ProviderId, TurnUsage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenCodeVersion {
    V2,
    V1,
}

pub struct OpenCodeChild {
    pub port: u16,
    pub password: String,
    pub child: Child,
}

pub type OpenCodeSpawner = Arc<dyn Fn(&[(String, String)]) -> Result<OpenCodeChild> + Send + Sync>;

struct OpenCodeRuntime {
    http: reqwest::Client,
    base_url: String,
    auth_header: String,
    version: OpenCodeVersion,
    events: AsyncMutex<tokio::sync::mpsc::UnboundedReceiver<(String, Value)>>,
    child: Arc<AsyncMutex<Child>>,
    current_emit: AsyncMutex<Option<TurnEventSender>>,
    active_session: AsyncMutex<Option<String>>,
    /// True only after the prompt request was accepted. Steering before that
    /// would race session admission and can be rejected by opencode.
    active_turn: AtomicBool,
}

pub struct OpenCodeAdapter {
    spawner: OpenCodeSpawner,
    runtime: Arc<AsyncMutex<Option<Arc<OpenCodeRuntime>>>>,
    auth_signature: Arc<Mutex<Option<String>>>,
    current_model: AsyncMutex<Option<String>>,
}

fn parse_model_slug(slug: &str) -> Result<(String, String)> {
    match slug.split_once('/') {
        Some((provider_id, model_id)) if !provider_id.is_empty() && !model_id.is_empty() => {
            Ok((provider_id.to_string(), model_id.to_string()))
        }
        _ => anyhow::bail!("opencode models are provider/model slugs, e.g. anthropic/claude-sonnet-4-5 (got {slug})"),
    }
}

impl OpenCodeAdapter {
    pub fn new(spawner: OpenCodeSpawner) -> Self {
        OpenCodeAdapter {
            spawner,
            runtime: Arc::new(AsyncMutex::new(None)),
            auth_signature: Arc::new(Mutex::new(None)),
            current_model: AsyncMutex::new(None),
        }
    }

    async fn ensure_runtime(&mut self, auth_env: &[(String, String)]) -> Result<Arc<OpenCodeRuntime>> {
        let signature = auth_signature(auth_env);
        let need_spawn = self.auth_signature.lock().unwrap().as_deref() != Some(signature.as_str());
        if need_spawn {
            let mut slot = self.runtime.lock().await;
            if let Some(runtime) = slot.as_ref() {
                let mut child = runtime.child.lock().await;
                let _ = child.start_kill();
            }
            let mut spawned = (self.spawner)(auth_env)?;
            let auth_header = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("opencode:{}", spawned.password))
            );
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()?;
            let base_url = match spawned.child.stdout.take() {
                Some(stdout) => {
                    let expected = format!("http://127.0.0.1:{}", spawned.port);
                    let detected = wait_for_server_url(stdout).await.unwrap_or(expected);
                    detected.trim_end_matches('/').to_string()
                }
                None => format!("http://127.0.0.1:{}", spawned.port),
            };
            let runtime_base_url = base_url.clone();
            let version = match http
                .get(format!("{base_url}/api/info"))
                .header("Authorization", &auth_header)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => OpenCodeVersion::V2,
                _ => OpenCodeVersion::V1,
            };
            let (event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
            let runtime = Arc::new(OpenCodeRuntime {
                http: http.clone(),
                base_url,
                auth_header: auth_header.clone(),
                version,
                events: AsyncMutex::new(event_receiver),
                child: Arc::new(AsyncMutex::new(spawned.child)),
                current_emit: AsyncMutex::new(None),
                active_session: AsyncMutex::new(None),
                active_turn: AtomicBool::new(false),
            });
            spawn_sse_reader(http, runtime_base_url.clone(), auth_header, event_sender);
            *slot = Some(runtime.clone());
            *self.auth_signature.lock().unwrap() = Some(signature);
        }
        Ok(self.runtime.lock().await.clone().expect("opencode runtime spawned"))
    }

    async fn post(&self, runtime: &Arc<OpenCodeRuntime>, path: &str, body: Value) -> Result<Value> {
        let response = runtime
            .http
            .post(format!("{}{}", runtime.base_url, path))
            .header("Authorization", &runtime.auth_header)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("opencode {path} failed ({status}): {text}");
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

fn spawn_sse_reader(
    http: reqwest::Client,
    base_url: String,
    auth_header: String,
    sender: tokio::sync::mpsc::UnboundedSender<(String, Value)>,
) {
    tokio::spawn(async move {
        let mut backoff = std::time::Duration::from_secs(1);
        loop {
            let attempt = http
                .get(format!("{base_url}/api/event"))
                .header("Authorization", &auth_header)
                .header("accept", "text/event-stream")
                .send()
                .await;
            let response = match attempt {
                Ok(response) if response.status().is_success() => response,
                _ => {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                    continue;
                }
            };
            backoff = std::time::Duration::from_secs(1);
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(_) => break,
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(newline) = buffer.find('\n') {
                    let line: String = buffer.drain(..=newline).collect();
                    let line = line.trim();
                    if let Some(payload) = line.strip_prefix("data: ") {
                        if let Ok(frame) = serde_json::from_str::<Value>(payload) {
                            let event_type = frame.get("type").and_then(Value::as_str).unwrap_or("").to_string();
                            let data = frame.get("data").cloned().unwrap_or(Value::Null);
                            let _ = sender.send((event_type, data));
                        }
                    }
                }
            }
            tokio::time::sleep(backoff).await;
        }
    });
}

fn handle_event(
    event_type: &str,
    data: &Value,
    emit: &TurnEventSender,
    active_session: &str,
    final_text: &mut String,
    usage: &mut Option<TurnUsage>,
    tool_names: &mut std::collections::HashMap<String, String>,
) -> bool {
    let session_matches = data.get("sessionID").and_then(Value::as_str) == Some(active_session);
    match event_type {
        "session.text.delta" | "session.next.text.delta" | "message.part.delta" if session_matches => {
            if event_type == "message.part.delta" && data.get("field").and_then(Value::as_str) != Some("text") {
                return false;
            }
            if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                let _ = emit.send(TurnEvent::TextDelta { text: delta.to_string() });
            }
            false
        }
        "session.next.text.ended" if session_matches => {
            if let Some(text) = data.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    *final_text = text.to_string();
                }
            }
            false
        }
        "session.tool.input.started" | "session.next.tool.input.started" if session_matches => {
            let name = data.get("name").and_then(Value::as_str).unwrap_or("tool").to_string();
            if let Some(call_id) = data.get("callID").and_then(Value::as_str) {
                tool_names.insert(call_id.to_string(), name.clone());
            }
            let id = data.get("callID").and_then(Value::as_str).map(str::to_string);
            let _ = emit.send(TurnEvent::ToolStarted { id, tool: name, detail: None });
            false
        }
        // The called frame carries the authoritative tool name for the call.
        "session.next.tool.called" if session_matches => {
            if let Some(call_id) = data.get("callID").and_then(Value::as_str) {
                if let Some(tool) = data.get("tool").and_then(Value::as_str) {
                    tool_names.insert(call_id.to_string(), tool.to_string());
                }
            }
            false
        }
        // Success/failure frames carry only the callID, not the name.
        "session.tool.success" | "session.tool.failed" | "session.next.tool.success" | "session.next.tool.failed"
            if session_matches =>
        {
            let fallback = data.get("name").and_then(Value::as_str).unwrap_or("tool").to_string();
            let name = data
                .get("callID")
                .and_then(Value::as_str)
                .and_then(|call_id| tool_names.get(call_id))
                .cloned()
                .unwrap_or(fallback);
            let id = data.get("callID").and_then(Value::as_str).map(str::to_string);
            let _ = emit.send(TurnEvent::ToolCompleted {
                id,
                tool: name,
                detail: None,
                exit_code: Some(if event_type.ends_with("failed") { 1 } else { 0 }),
            });
            false
        }
        // opencode >= 2 ends a turn with session.execution.*; 1.x "next" servers
        // end each step instead: finish "tool-calls" means another step follows.
        "session.next.step.ended" if session_matches => {
            if let Some(tokens) = data.get("tokens") {
                let step = TurnUsage {
                    input_tokens: tokens.get("input").and_then(Value::as_i64).unwrap_or(0)
                        + tokens.get("cache").and_then(|cache| cache.get("read")).and_then(Value::as_i64).unwrap_or(0)
                        + tokens.get("cache").and_then(|cache| cache.get("write")).and_then(Value::as_i64).unwrap_or(0),
                    output_tokens: tokens.get("output").and_then(Value::as_i64).unwrap_or(0)
                        + tokens.get("reasoning").and_then(Value::as_i64).unwrap_or(0),
                    cache_read_tokens: tokens.get("cache").and_then(|cache| cache.get("read")).and_then(Value::as_i64).unwrap_or(0),
                    cache_creation_tokens: tokens.get("cache").and_then(|cache| cache.get("write")).and_then(Value::as_i64).unwrap_or(0),
                    cost_usd: None,
                };
                let accumulated = usage.get_or_insert_with(TurnUsage::default);
                accumulated.input_tokens += step.input_tokens;
                accumulated.output_tokens += step.output_tokens;
                accumulated.cache_read_tokens += step.cache_read_tokens;
                accumulated.cache_creation_tokens += step.cache_creation_tokens;
                accumulated.cost_usd = None;
            }
            data.get("finish").and_then(Value::as_str) != Some("tool-calls")
        }
        "session.next.step.failed" if session_matches => {
            let message = data
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("opencode step failed");
            let _ = emit.send(TurnEvent::TurnFailed { error: message.to_string() });
            true
        }
        "session.execution.succeeded" | "session.execution.failed" | "session.execution.interrupted"
            if session_matches =>
        {
            if event_type == "session.execution.failed" {
                if let Some(error) = data.get("error") {
                    let message = error.get("message").and_then(Value::as_str).unwrap_or("opencode turn failed");
                    let _ = emit.send(TurnEvent::TurnFailed { error: message.to_string() });
                }
            }
            true
        }
        "session.idle" | "session.status" if session_matches => {
            let status = data
                .get("status")
                .and_then(|status| status.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("idle");
            status == "idle"
        }
        "message.updated" if session_matches => {
            if let Some(info) = data.get("info") {
                if info.get("time").and_then(|time| time.get("completed")).is_some() {
                    if let Some(parts) = info.get("parts").and_then(Value::as_array) {
                        let text: String = parts
                            .iter()
                            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                            .filter_map(|part| part.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("");
                        *final_text = text;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

#[async_trait]
impl BatonAdapter for OpenCodeAdapter {
    fn id(&self) -> ProviderId {
        ProviderId::OpenCode
    }

    fn interrupt_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        let runtime = self.runtime.clone();
        Arc::new(move || {
            let runtime = runtime.clone();
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
                    let path = match runtime.version {
                        OpenCodeVersion::V2 => format!("/api/session/{session_id}/interrupt"),
                        OpenCodeVersion::V1 => format!("/session/{session_id}/abort"),
                    };
                    let _ = runtime
                        .http
                        .post(format!("{}{}", runtime.base_url, path))
                        .header("Authorization", &runtime.auth_header)
                        .send()
                        .await;
                });
            }
        })
    }

    fn steer_handle(&self) -> Option<SteerHandle> {
        let runtime = self.runtime.clone();
        Some(Arc::new(move |text: String| -> bool {
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                return false;
            };
            let runtime = match runtime.try_lock() {
                Ok(slot) => slot.clone(),
                Err(_) => return false,
            };
            let runtime = match runtime {
                Some(runtime) => runtime,
                None => return false,
            };
            if runtime.version != OpenCodeVersion::V2
                || !runtime.active_turn.load(Ordering::SeqCst)
            {
                return false;
            }
            let session_id = match runtime.active_session.try_lock() {
                Ok(session_id) => session_id.clone(),
                Err(_) => return false,
            };
            let Some(session_id) = session_id else {
                return false;
            };
            let id = format!("msg_baton_steer_{}", Uuid::new_v4().simple());
            handle.spawn(async move {
                let body = json!({
                    "id": id,
                    "prompt": { "text": text },
                    "delivery": "steer"
                });
                let _ = runtime
                    .http
                    .post(format!(
                        "{}/api/session/{session_id}/prompt",
                        runtime.base_url
                    ))
                    .header("Authorization", &runtime.auth_header)
                    .json(&body)
                    .send()
                    .await;
            });
            true
        }))
    }

    async fn list_models(&mut self, input: ModelListInput) -> Result<Vec<String>> {
        let runtime = self.ensure_runtime(&input.auth_env).await?;
        if runtime.version != OpenCodeVersion::V2 {
            return Ok(Vec::new());
        }
        let response = runtime
            .http
            .get(format!("{}/api/model", runtime.base_url))
            .header("Authorization", &runtime.auth_header)
            .query(&[("location[directory]", input.cwd.as_str())])
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("opencode /api/model failed ({status}): {text}");
        }
        let frame: Value = serde_json::from_str(&text)?;
        let mut models: Vec<String> = Vec::new();
        if let Some(data) = frame.get("data").and_then(Value::as_array) {
            for model in data {
                let provider = model
                    .get("providerID")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty());
                let id = model
                    .get("id")
                    .or_else(|| model.get("modelID"))
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty());
                if let (Some(provider), Some(id)) = (provider, id) {
                    let slug = format!("{provider}/{id}");
                    if !models.contains(&slug) {
                        models.push(slug);
                    }
                }
            }
        }
        Ok(models)
    }

    async fn start_turn(
        &mut self,
        input: TurnStartInput,
        session: Option<NativeSessionRef>,
        emit: TurnEventSender,
    ) -> Result<()> {
        let runtime = self.ensure_runtime(&input.auth_env).await?;
        *runtime.current_emit.lock().await = Some(emit.clone());
        let _ = &self.current_model;

        let session_id = match session {
            Some(session) => session.native_session_id,
            None => match runtime.version {
                OpenCodeVersion::V2 => {
                    let (provider_id, model_id) = parse_model_slug(&input.model)?;
                    let body = json!({
                        "location": { "directory": input.cwd },
                        "model": { "providerID": provider_id, "id": model_id }
                    });
                    let created = self.post(&runtime, "/api/session", body).await?;
                    match created
                        .pointer("/data/id")
                        .or_else(|| created.get("id"))
                        .and_then(Value::as_str)
                    {
                        Some(session_id) => session_id.to_string(),
                        None => anyhow::bail!("opencode did not return a session id"),
                    }
                }
                OpenCodeVersion::V1 => {
                    let body = json!({ "permission": [] });
                    let created = self.post(&runtime, "/session", body).await?;
                    match created
                        .pointer("/data/id")
                        .or_else(|| created.get("id"))
                        .and_then(Value::as_str)
                    {
                        Some(session_id) => session_id.to_string(),
                        None => anyhow::bail!("opencode did not return a session id"),
                    }
                }
            },
        };
        *runtime.active_session.lock().await = Some(session_id.clone());

        match runtime.version {
            OpenCodeVersion::V2 => {
                let (provider_id, model_id) = parse_model_slug(&input.model)?;
                let body = json!({ "model": { "providerID": provider_id, "id": model_id } });
                let _ = self
                    .post(&runtime, &format!("/api/session/{session_id}/model"), body)
                    .await;
                // opencode >= 2 wraps the prompt text: {"prompt": {"text": ...}}.
                let prompt_body = json!({ "prompt": { "text": input.prompt } });
                self.post(&runtime, &format!("/api/session/{session_id}/prompt"), prompt_body)
                    .await?;
            }
            OpenCodeVersion::V1 => {
                let (provider_id, model_id) = parse_model_slug(&input.model)?;
                let body = json!({
                    "model": { "providerID": provider_id, "modelID": model_id },
                    "parts": [ { "type": "text", "text": input.prompt } ]
                });
                let _ = self
                    .post(&runtime, &format!("/session/{session_id}/prompt_async"), body)
                    .await;
            }
        }
        runtime.active_turn.store(true, Ordering::SeqCst);

        let mut receiver = runtime.events.lock().await;
        let mut final_text = String::new();
        let mut tool_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut usage: Option<TurnUsage> = None;
        let pending_approvals: Vec<tokio::sync::oneshot::Receiver<ApprovalDecision>> = Vec::new();
        let result: Result<()> = loop {
            let (event_type, data) = match receiver.recv().await {
                Some(event) => event,
                None => break Err(anyhow::anyhow!("opencode event stream closed")),
            };
            if event_type == "permission.asked" {
                let request_id = data.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                let session_matches = data.get("sessionID").and_then(Value::as_str) == Some(session_id.as_str());
                if session_matches {
                    let (respond, receiver_approval) = oneshot::channel::<ApprovalDecision>();
                    let summary = data
                        .get("action")
                        .and_then(Value::as_str)
                        .unwrap_or("opencode requests permission")
                        .to_string();
                    let _ = emit.send(TurnEvent::ApprovalRequest {
                        id: format!("opencode:{request_id}"),
                        kind: "permissions".into(),
                        summary,
                        respond,
                    });
                    let runtime_for_reply = runtime.clone();
                    let session_id_for_reply = session_id.clone();
                    let request_id_for_reply = request_id;
                    tokio::spawn(async move {
                        let decision = receiver_approval.await.ok();
                        let decision_label = match decision {
                            Some(ApprovalDecision::Accept) => "once",
                            Some(ApprovalDecision::AcceptAlways) => "always",
                            _ => "reject",
                        };
                        let path = format!("/api/session/{session_id_for_reply}/permission/{request_id_for_reply}/reply");
                        let body = json!({ "decision": decision_label });
                        let _ = runtime_for_reply
                            .http
                            .post(format!("{}{}", runtime_for_reply.base_url, path))
                            .header("Authorization", &runtime_for_reply.auth_header)
                            .json(&body)
                            .send()
                            .await;
                    });
                    let _ = pending_approvals;
                    continue;
                }
            }
            let mut finished = handle_event(&event_type, &data, &emit, &session_id, &mut final_text, &mut usage, &mut tool_names);
            if std::env::var("BATON_DEBUG_EVENTS").is_ok() {
                eprintln!("[opencode] {event_type} finished={finished} data={}", serde_json::to_string(&data).unwrap_or_default());
            }
            if event_type == "session.execution.failed"
                && data.get("sessionID").and_then(Value::as_str) == Some(session_id.as_str())
            {
                finished = true;
            }
            if finished {
                break Ok(());
            }
        };
        drop(receiver);
        runtime.active_turn.store(false, Ordering::SeqCst);
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
            runtime.active_turn.store(false, Ordering::SeqCst);
            let mut child = runtime.child.lock().await;
            let _ = child.start_kill();
        }
        Ok(())
    }
}

pub async fn wait_for_server_url(child_stdout: tokio::process::ChildStdout) -> Result<String> {
    let mut reader = tokio::io::BufReader::new(child_stdout);
    let mut line = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if std::time::Instant::now() > deadline {
            anyhow::bail!("opencode server did not report a listening url within 30s");
        }
        line.clear();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            anyhow::bail!("opencode server exited before listening");
        }
        if let Some(index) = line.find("server listening on ") {
            let url = line[index + "server listening on ".len()..].trim().to_string();
            if url.starts_with("http") {
                return Ok(url);
            }
        }
    }
}
