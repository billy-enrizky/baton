use anyhow::Result;
use std::collections::HashMap;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

use crate::adapters::{child_halves, BatonAdapter, LineReader, LineWriter, TransportHalves, TurnEvent, TurnEventSender, TurnStartInput};
use crate::types::{ApprovalDecision, NativeSessionRef, PermissionMode, ProviderId, TurnUsage};

pub type ClaudeSpawner = Arc<dyn Fn(&TurnStartInput, Option<&NativeSessionRef>) -> Result<TransportHalves> + Send + Sync>;

pub struct ClaudeAdapter {
    interrupt: Arc<Mutex<Option<tokio::sync::watch::Sender<bool>>>>,
    spawner: ClaudeSpawner,
}

impl ClaudeAdapter {
    pub fn new(spawner: ClaudeSpawner) -> ClaudeAdapter {
        ClaudeAdapter {
            interrupt: Arc::new(Mutex::new(None)),
            spawner,
        }
    }

    pub fn real_spawner() -> ClaudeSpawner {
        Arc::new(|input, session| Self::spawn_halves(input, session))
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
        let mut command = tokio::process::Command::new("claude");
        command.args(Self::command_args(input, session));
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::null());
        for (key, value) in &input.auth_env {
            command.env(key, value);
        }
        command.current_dir(&input.cwd);
        child_halves(command.spawn()?)
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

#[async_trait]
impl BatonAdapter for ClaudeAdapter {
    fn id(&self) -> ProviderId {
        ProviderId::Claude
    }

    fn interrupt_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        let interrupt = self.interrupt.clone();
        Arc::new(move || {
            if let Some(sender) = interrupt.lock().unwrap().as_ref() {
                let _ = sender.send(true);
            }
        })
    }

    async fn start_turn(
        &mut self,
        input: TurnStartInput,
        session: Option<NativeSessionRef>,
        emit: TurnEventSender,
    ) -> Result<()> {
        let (mut reader, mut writer) = (self.spawner)(&input, session.as_ref())?;
        let user_message = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [ { "type": "text", "text": input.prompt } ]
            }
        });
        writer.write_line(&serde_json::to_string(&user_message)?).await?;

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
                                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                                        if !id.is_empty() {
                                            tool_calls.insert(id.to_string(), (tool.clone(), detail.clone()));
                                        }
                                    }
                                    let _ = emit.send(TurnEvent::ToolStarted { tool, detail });
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
                        let decision = match receiver.await {
                            Ok(ApprovalDecision::Accept) | Ok(ApprovalDecision::AcceptAlways) => "allow",
                            Ok(ApprovalDecision::Deny) | Err(_) => "deny",
                        };
                        let response = json!({
                            "type": "control_response",
                            "response": {
                                "subtype": "success",
                                "request_id": request_id,
                                "response": {
                                    "behavior": decision,
                                    "updatedInput": tool_input
                                }
                            }
                        });
                        writer.write_line(&serde_json::to_string(&response)?).await?;
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

        let _ = writer.shutdown().await;
        *self.interrupt.lock().unwrap() = None;

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
            prompt: "p".into(),
            permission_mode: PermissionMode::Default,
            auth_env: vec![],
        };
        let args = ClaudeAdapter::command_args(&input, None);
        assert!(args.contains(&"--include-partial-messages".to_string()));
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
}
