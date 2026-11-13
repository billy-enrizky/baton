use anyhow::Result;
use std::collections::HashMap;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::adapters::{child_halves, child_halves_with_cleanup, AdapterLifecycle, AdapterTurnGuard, BatonAdapter, LineWriter, SteerHandle, TransportHalves, TurnEvent, TurnEventSender, TurnStartInput};
use crate::process::{self, ProcessRecorder};
use crate::types::{ApprovalDecision, NativeSessionRef, PermissionMode, ProviderId, TurnUsage};

pub type ClaudeSpawner = Arc<dyn Fn(&TurnStartInput, Option<&NativeSessionRef>) -> Result<TransportHalves> + Send + Sync>;

type SharedWriter = Arc<AsyncMutex<Option<Box<dyn LineWriter>>>>;

pub struct ClaudeAdapter {
    interrupt: Arc<Mutex<Option<tokio::sync::watch::Sender<bool>>>>,
    writer: SharedWriter,
    active_turn: Arc<AtomicBool>,
    spawner: ClaudeSpawner,
    lifecycle: AdapterLifecycle,
}

async fn write_value(writer: &SharedWriter, value: &Value) -> Result<()> {
    let mut guard = writer.lock().await;
    let writer = guard
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("claude input stream is closed"))?;
    writer.write_line(&serde_json::to_string(value)?).await
}

impl ClaudeAdapter {
    pub fn new(spawner: ClaudeSpawner) -> ClaudeAdapter {
        ClaudeAdapter {
            interrupt: Arc::new(Mutex::new(None)),
            writer: Arc::new(AsyncMutex::new(None)),
            active_turn: Arc::new(AtomicBool::new(false)),
            spawner,
            lifecycle: AdapterLifecycle::new(),
        }
    }

    pub fn real_spawner() -> ClaudeSpawner {
        Arc::new(|input, session| Self::spawn_halves(input, session))
    }

    pub fn real_spawner_with_recorder(recorder: ProcessRecorder) -> ClaudeSpawner {
        Arc::new(move |input, session| Self::spawn_halves_tracked(input, session, &recorder))
    }

    pub fn command_args(input: &TurnStartInput, session: Option<&NativeSessionRef>) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--include-partial-messages".to_string(),
            "--model".to_string(),
            input.model.clone(),
        ];
        if let Some(effort) = input.effort.as_deref() {
            args.push("--effort".to_string());
            args.push(effort.to_string());
        }
        if let Some(session) = session {
            args.push("--resume".to_string());
            args.push(session.native_session_id.clone());
        }
        match input.permission_mode {
            PermissionMode::BypassPermissions => {
                args.push("--dangerously-skip-permissions".to_string());
            }
            mode => {
                args.push("--permission-mode".to_string());
                args.push(mode.as_str().to_string());
            }
        }
        args
    }

    pub fn spawn_halves(input: &TurnStartInput, session: Option<&NativeSessionRef>) -> Result<TransportHalves> {
        Self::spawn_halves_inner(input, session, None)
    }

    pub fn spawn_halves_tracked(
        input: &TurnStartInput,
        session: Option<&NativeSessionRef>,
        recorder: &ProcessRecorder,
    ) -> Result<TransportHalves> {
        Self::spawn_halves_inner(input, session, Some(recorder))
    }

    fn spawn_halves_inner(
        input: &TurnStartInput,
        session: Option<&NativeSessionRef>,
        recorder: Option<&ProcessRecorder>,
    ) -> Result<TransportHalves> {
        let mut command = tokio::process::Command::new("claude");
        command.args(Self::command_args(input, session));
        command.kill_on_drop(true);
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::null());
        for (key, value) in &input.auth_env {
            command.env(key, value);
        }
        command.current_dir(&input.cwd);
        let child = command.spawn()?;
        let Some(recorder) = recorder else {
            return child_halves(child);
        };
        let pid = child
            .id()
            .ok_or_else(|| anyhow::anyhow!("claude did not expose a process id"))?;
        let command_line = process::command_line(pid).unwrap_or_default();
        let process_start = process::process_start(pid);
        recorder.record(
            pid,
            ProviderId::Claude,
            None,
            &command_line,
            process_start.as_deref(),
        )?;
        let recorder = recorder.clone();
        child_halves_with_cleanup(
            child,
            Some(Box::new(move || recorder.forget(pid))),
        )
    }
}

fn parse_usage(usage: &Value) -> TurnUsage {
    TurnUsage {
        input_tokens: usage.get("input_tokens").and_then(Value::as_i64).unwrap_or(0),
        output_tokens: usage.get("output_tokens").and_then(Value::as_i64).unwrap_or(0),
        cache_read_tokens: usage.get("cache_read_input_tokens").and_then(Value::as_i64).unwrap_or(0),
        cache_creation_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cost_usd: usage.get("total_cost_usd").and_then(Value::as_f64),
    }
}

fn tool_input_summary(input: &Value) -> Option<String> {
    let record = input.as_object()?;
    for key in ["command", "file_path", "path", "pattern"] {
        if let Some(value) = record.get(key).and_then(Value::as_str) {
            return Some(value.to_string());
        }
    }
    None
}

/// Builds the host-side answer for Claude's `can_use_tool` control request.
/// The CLI treats `AcceptAlways` as a permanent user decision only when the
/// answer carries its suggested permission updates.
fn permission_response(request: &Value, tool_input: &Value, decision: ApprovalDecision) -> Value {
    let (behavior, classification) = match decision {
        ApprovalDecision::Accept => ("allow", "user_temporary"),
        ApprovalDecision::AcceptAlways => ("allow", "user_permanent"),
        ApprovalDecision::Deny => ("deny", "user_reject"),
    };
    let mut result = json!({
        "behavior": behavior,
        "updatedInput": tool_input,
        "decisionClassification": classification,
    });
    if decision == ApprovalDecision::AcceptAlways {
        if let Some(suggestions) = request.get("permission_suggestions").and_then(Value::as_array) {
            result["updatedPermissions"] = Value::Array(suggestions.clone());
        }
    }
    if decision == ApprovalDecision::Deny {
        result["message"] = json!("User declined tool execution.");
    }
    if let Some(tool_use_id) = request.get("tool_use_id").and_then(Value::as_str) {
        result["toolUseID"] = json!(tool_use_id);
    }
    result
}

#[async_trait]
impl BatonAdapter for ClaudeAdapter {
    fn id(&self) -> ProviderId {
        ProviderId::Claude
    }

    fn lifecycle(&self) -> Option<AdapterLifecycle> {
        Some(self.lifecycle.clone())
    }

    fn interrupt_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        let interrupt = self.interrupt.clone();
        let lifecycle = self.lifecycle.clone();
        Arc::new(move || {
            lifecycle.cancelling();
            if let Some(sender) = interrupt.lock().unwrap().as_ref() {
                let _ = sender.send(true);
            }
        })
    }

    fn steer_handle(&self) -> Option<SteerHandle> {
        let writer = self.writer.clone();
        let active = self.active_turn.clone();
        Some(Arc::new(move |text: String| -> bool {
            if !active.load(Ordering::SeqCst) {
                return false;
            }
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                return false;
            };
            let writer = writer.clone();
            handle.spawn(async move {
                let message = json!({
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": [ { "type": "text", "text": text } ]
                    }
                });
                let _ = write_value(&writer, &message).await;
            });
            true
        }))
    }

    async fn start_turn(
        &mut self,
        input: TurnStartInput,
        session: Option<NativeSessionRef>,
        emit: TurnEventSender,
    ) -> Result<()> {
        let _turn_guard = AdapterTurnGuard::new(self.lifecycle.clone());
        self.lifecycle.starting(&input.thread_id);
        let (mut reader, writer) = (self.spawner)(&input, session.as_ref())?;
        let writer_handle = self.writer.clone();
        *writer_handle.lock().await = Some(writer);
        self.active_turn.store(true, Ordering::SeqCst);
        self.lifecycle.turn_running(&input.thread_id);
        let writer = writer_handle;
        let user_message = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [ { "type": "text", "text": input.prompt } ]
            }
        });
        if let Err(error) = write_value(&writer, &user_message).await {
            self.active_turn.store(false, Ordering::SeqCst);
            let mut guard = self.writer.lock().await;
            if let Some(mut writer) = guard.take() {
                let _ = writer.shutdown().await;
            }
            return Err(error);
        }

        let (interrupt_sender, interrupt_receiver) = tokio::sync::watch::channel(false);
        *self.interrupt.lock().unwrap() = Some(interrupt_sender);

        let mint_session_id = uuid::Uuid::new_v4().to_string();
        let mut session_id = session
            .as_ref()
            .map(|session| session.native_session_id.clone())
            .unwrap_or_else(|| mint_session_id.clone());
        let mut final_text = String::new();
        let mut usage: Option<TurnUsage> = None;
        let mut saw_stream_events = false;
        let mut interrupted = false;
        let mut result: Result<()> = Ok(());
        // Claude starts a tool with `tool_use.id` and finishes it with a
        // `tool_result.tool_use_id`. Keep the label so both events match.
        let mut tool_calls: HashMap<String, (String, Option<String>)> = HashMap::new();

        loop {
            let line = tokio::select! {
                line = reader.read_line() => match line {
                    Ok(Some(line)) => line,
                    _ => break,
                },
                _ = async {
                    let mut receiver = interrupt_receiver.clone();
                    while !*receiver.borrow() {
                        if receiver.changed().await.is_err() {
                            break;
                        }
                    }
                } => {
                    interrupted = true;
                    result = Err(anyhow::anyhow!("interrupted"));
                    break;
                }
            };
            let message: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let message_type = message.get("type").and_then(Value::as_str).unwrap_or("");
            match message_type {
                "stream_event" => {
                    let event = message.get("event").cloned().unwrap_or(Value::Null);
                    if event.get("type").and_then(Value::as_str) == Some("content_block_delta") {
                        let delta = event.get("delta").cloned().unwrap_or(Value::Null);
                        if delta.get("type").and_then(Value::as_str) == Some("text_delta") {
                            saw_stream_events = true;
                            let parent = message.get("parent_tool_use_id").and_then(Value::as_str);
                            if parent.is_none() {
                                if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                    if !text.is_empty() {
                                        let _ = emit.send(TurnEvent::TextDelta { text: text.to_string() });
                                    }
                                }
                            }
                        }
                    }
                }
                "assistant" => {
                    if let Some(inner) = message.get("message") {
                        if let Some(content) = inner.get("content").and_then(Value::as_array) {
                            for block in content {
                                let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
                                if block_type == "text" {
                                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                                        if !text.is_empty() && !saw_stream_events {
                                            let _ = emit.send(TurnEvent::TextDelta { text: text.to_string() });
                                        }
                                    }
                                } else if block_type == "tool_use" {
                                    let tool = block.get("name").and_then(Value::as_str).unwrap_or("tool").to_string();
                                    let detail = block.get("input").and_then(tool_input_summary);
                                    let call_id = block
                                        .get("id")
                                        .and_then(Value::as_str)
                                        .filter(|id| !id.is_empty())
                                        .map(str::to_string);
                                    if let Some(id) = call_id.as_ref() {
                                        tool_calls.insert(id.clone(), (tool.clone(), detail.clone()));
                                    }
                                    let _ = emit.send(TurnEvent::ToolStarted { id: call_id, tool, detail });
                                }
                            }
                        }
                        if let Some(inner_usage) = inner.get("usage") {
                            usage = Some(parse_usage(inner_usage));
                            if let Some(usage) = usage {
                                let _ = emit.send(TurnEvent::Usage { usage });
                            }
                        }
                    }
                }
                "user" => {
                    if let Some(inner) = message.get("message") {
                        if let Some(content) = inner.get("content").and_then(Value::as_array) {
                            for block in content {
                                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                                    continue;
                                }
                                let id = block.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                                let is_error = block.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                                if let Some((tool, detail)) = tool_calls.remove(id) {
                                    let _ = emit.send(TurnEvent::ToolCompleted {
                                        id: (!id.is_empty()).then_some(id.to_string()),
                                        tool,
                                        detail,
                                        exit_code: Some(if is_error { 1 } else { 0 }),
                                    });
                                }
                            }
                        }
                    }
                }
                "control_request" => {
                    let request = message.get("request").cloned().unwrap_or(Value::Null);
                    if request.get("subtype").and_then(Value::as_str) == Some("can_use_tool") {
                        let request_id = request.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                        let tool_name = request
                            .get("tool_name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string();
                        let tool_input = request.get("input").cloned().unwrap_or(Value::Null);
                        let summary = tool_input_summary(&tool_input)
                            .unwrap_or_else(|| tool_name.clone());
                        let (respond, receiver) = oneshot::channel::<ApprovalDecision>();
                        let _ = emit.send(TurnEvent::ApprovalRequest {
                            id: format!("claude:{request_id}"),
                            kind: "command".into(),
                            summary,
                            respond,
                        });
                        let decision = receiver.await.unwrap_or(ApprovalDecision::Deny);
                        let result = permission_response(&request, &tool_input, decision);
                        let response = json!({
                            "type": "control_response",
                            "response": {
                                "subtype": "success",
                                "request_id": request_id,
                                "response": result
                            }
                        });
                        write_value(&writer, &response).await?;
                    }
                }
                "result" => {
                    if let Some(text) = message.get("result").and_then(Value::as_str) {
                        final_text = text.to_string();
                    }
                    if let Some(result_session) = message.get("session_id").and_then(Value::as_str) {
                        if !result_session.is_empty() {
                            session_id = result_session.to_string();
                        }
                    }
                    if let Some(result_usage) = message.get("usage") {
                        let mut parsed = parse_usage(result_usage);
                        if parsed.cost_usd.is_none() {
                            parsed.cost_usd = message.get("total_cost_usd").and_then(Value::as_f64);
                        }
                        usage = Some(parsed);
                    } else if usage.is_none() {
                        usage = Some(TurnUsage {
                            cost_usd: message.get("total_cost_usd").and_then(Value::as_f64),
                            ..Default::default()
                        });
                    }
                    if message.get("is_error").and_then(Value::as_bool) == Some(true) {
                        result = Err(anyhow::anyhow!(if final_text.is_empty() {
                            "claude turn ended with an error".to_string()
                        } else {
                            final_text.clone()
                        }));
                    }
                    break;
                }
                _ => {}
            }
        }

        self.active_turn.store(false, Ordering::SeqCst);
        *self.interrupt.lock().unwrap() = None;
        {
            let mut guard = self.writer.lock().await;
            if let Some(mut writer) = guard.take() {
                let _ = writer.shutdown().await;
            }
        }

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
                if interrupted {
                    let _ = emit.send(TurnEvent::TurnInterrupted {
                        reason: Some(error.to_string()),
                    });
                } else {
                    let _ = emit.send(TurnEvent::TurnFailed { error: error.to_string() });
                }
                Err(error)
            }
        }
    }

    async fn dispose(&mut self) -> Result<()> {
        self.lifecycle.disposing();
        self.active_turn.store(false, Ordering::SeqCst);
        let mut guard = self.writer.lock().await;
        if let Some(mut writer) = guard.take() {
            let _ = writer.shutdown().await;
        }
        drop(guard);
        self.lifecycle.disposed();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_args_request_partial_stream_messages() {
        let input = TurnStartInput {
            thread_id: "t".into(),
            cwd: "/work".into(),
            model: "claude-sonnet-4-5".into(),
            effort: Some("max".into()),
            prompt: "p".into(),
            permission_mode: PermissionMode::Default,
            auth_env: vec![],
        };
        let args = ClaudeAdapter::command_args(&input, None);
        assert!(args.contains(&"--include-partial-messages".to_string()));
        let effort = args.iter().position(|arg| arg == "--effort").expect("effort flag");
        assert_eq!(args[effort + 1], "max");
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"stream-json".to_string()));

        let resumed = ClaudeAdapter::command_args(
            &input,
            Some(&NativeSessionRef { native_session_id: "s-1".into(), rollout_path: None }),
        );
        let position = resumed.iter().position(|arg| arg == "--resume").expect("resume flag");
        assert_eq!(resumed[position + 1], "s-1");
        assert!(resumed.contains(&"--include-partial-messages".to_string()));
    }

    #[test]
    fn accept_always_grants_suggested_session_permissions() {
        let request = json!({
            "tool_use_id": "tool-1",
            "permission_suggestions": [
                {
                    "type": "addRules",
                    "rules": [{ "toolName": "Bash", "ruleContent": "ls" }],
                    "behavior": "allow",
                    "destination": "session"
                }
            ]
        });
        let tool_input = json!({ "command": "ls" });

        let always = permission_response(&request, &tool_input, ApprovalDecision::AcceptAlways);
        assert_eq!(always["behavior"], "allow");
        assert_eq!(always["decisionClassification"], "user_permanent");
        assert_eq!(always["toolUseID"], "tool-1");
        assert_eq!(always["updatedInput"], tool_input);
        assert_eq!(always["updatedPermissions"], request["permission_suggestions"]);

        let once = permission_response(&request, &tool_input, ApprovalDecision::Accept);
        assert_eq!(once["decisionClassification"], "user_temporary");
        assert!(once.get("updatedPermissions").is_none());

        let denied = permission_response(&request, &tool_input, ApprovalDecision::Deny);
        assert_eq!(denied["behavior"], "deny");
        assert_eq!(denied["decisionClassification"], "user_reject");
        assert_eq!(denied["message"], "User declined tool execution.");
    }
}
