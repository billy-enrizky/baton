use baton::adapters::{duplex_halves, BatonAdapter, ModelListInput, TransportHalves};
use baton::adapters::acp::AcpAdapter;
use baton::types::{ApprovalDecision, PermissionMode, ProviderId};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn send(write: &mut tokio::io::DuplexStream, value: Value) {
    let mut serialized = serde_json::to_string(&value).unwrap();
    serialized.push('\n');
    write.write_all(serialized.as_bytes()).await.unwrap();
    write.flush().await.unwrap();
}

async fn fake_acp_agent(read: tokio::io::DuplexStream, mut write: tokio::io::DuplexStream) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    let mut prompt_id: Option<Value> = None;
    loop {
        line.clear();
        if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let frame: Value = match serde_json::from_str(line.trim()) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("").to_string();
        let id = frame.get("id").cloned();
        match method.as_str() {
            "initialize" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": 2,
                            "authMethods": [ { "id": "cached_token", "name": "Cached token", "type": "oauth" } ]
                        }
                    }),
                )
                .await;
            }
            "session/new" => {
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": { "sessionId": "acp-session-1" } })).await;
            }
            "session/resume" | "session/load" => {
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "session/set_model" => {
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "session/set_mode" | "session/set_config_option" => {
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "session/prompt" => {
                prompt_id = id;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "acp-session-1",
                            "update": { "sessionUpdate": "tool_call", "toolCallId": "tc-1", "title": "bash ls", "status": "in_progress" }
                        }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "acp-session-1",
                            "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hello " } }
                        }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "acp-session-1",
                            "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "acp" } }
                        }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "acp-session-1",
                            "update": {
                                "sessionUpdate": "state_update",
                                "state": "idle",
                                "stopReason": "end_turn",
                                "usage": { "inputTokens": 10, "outputTokens": 5, "cacheReadInputTokens": 100, "cacheWriteInputTokens": 20 }
                            }
                        }
                    }),
                )
                .await;
                if let Some(prompt_id) = prompt_id.take() {
                    send(&mut write, json!({ "jsonrpc": "2.0", "id": prompt_id, "result": { "stopReason": "end_turn" } })).await;
                }
            }
            _ => {}
        }
    }
}

async fn fake_acp_models(read: tokio::io::DuplexStream, mut write: tokio::io::DuplexStream) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    loop {
        line.clear();
        if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let frame: Value = match serde_json::from_str(line.trim()) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
        let id = frame.get("id").cloned();
        match method {
            "initialize" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "protocolVersion": 2 }
                    }),
                )
                .await;
            }
            "session/new" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "sessionId": "acp-models-1",
                            "configOptions": [
                                {
                                    "id": "model",
                                    "name": "Model",
                                    "category": "model",
                                    "type": "select",
                                    "currentValue": "grok-4.7",
                                    "options": [
                                        { "value": "grok-4.7", "name": "Grok 4.7" },
                                        {
                                            "group": "legacy",
                                            "name": "Legacy",
                                            "options": [
                                                { "value": "grok-4.5", "name": "Grok 4.5" }
                                            ]
                                        }
                                    ]
                                },
                                {
                                    "id": "mode",
                                    "name": "Mode",
                                    "type": "select",
                                    "currentValue": "ask",
                                    "options": [ { "value": "ask", "name": "Ask" } ]
                                }
                            ]
                        }
                    }),
                )
                .await;
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_adapter_lists_session_model_options() {
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        tokio::spawn(fake_acp_models(server_read, server_write));
        Ok(halves)
    });
    let mut adapter = AcpAdapter::new(ProviderId::Grok, spawner);
    let models = tokio::time::timeout(
        Duration::from_secs(10),
        adapter.list_models(ModelListInput {
            cwd: "/work".into(),
            auth_env: vec![],
        }),
    )
    .await
    .expect("model list completes")
    .expect("model list succeeds");
    assert_eq!(models, vec!["grok-4.7", "grok-4.5"]);
    adapter.dispose().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_adapter_runs_a_full_turn_with_streaming() {
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        tokio::spawn(fake_acp_agent(server_read, server_write));
        Ok(halves)
    });
    let mut adapter = AcpAdapter::new(ProviderId::Grok, spawner);

    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    tokio::time::timeout(Duration::from_secs(10), adapter.start_turn(
        baton::adapters::TurnStartInput {
            thread_id: "bt".into(),
            cwd: "/work".into(),
            model: "grok-build".into(),
            effort: None,
            prompt: "hello".into(),
            permission_mode: PermissionMode::Default,
            auth_env: vec![],
        },
        None,
        event_sender,
    ))
    .await
    .expect("turn completes within 10s")
    .expect("turn should complete");

    let events: Vec<baton::adapters::TurnEvent> = event_receiver.iter().collect();
    let completed = events
        .iter()
        .find_map(|event| match event {
            baton::adapters::TurnEvent::TurnCompleted { final_text, usage, native_session_id, .. } => {
                Some((final_text.clone(), usage.clone(), native_session_id.clone()))
            }
            _ => None,
        })
        .expect("turn completed event");
    assert_eq!(completed.2.as_deref(), Some("acp-session-1"));
    let usage = completed.1.unwrap();
    assert_eq!(usage.cache_read_tokens, 100);
    assert_eq!(usage.input_tokens, 10);
    let deltas: String = events
        .iter()
        .filter_map(|event| match event {
            baton::adapters::TurnEvent::TextDelta { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "hello acp");
    assert!(events.iter().any(|event| matches!(event, baton::adapters::TurnEvent::ToolStarted { tool, .. } if tool == "bash ls")));
    assert!(
        events.iter().any(|event| matches!(
            event,
            baton::adapters::TurnEvent::ProviderSession { native_session_id, .. }
                if native_session_id == "acp-session-1"
        )),
        "the ACP session id is persisted as soon as the session is established"
    );
    adapter.dispose().await.unwrap();
}

async fn fake_acp_agent_with_permission(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    responses: tokio::sync::mpsc::UnboundedSender<String>,
) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    loop {
        line.clear();
        if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let frame: Value = match serde_json::from_str(line.trim()) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("").to_string();
        let id = frame.get("id").cloned();
        match method.as_str() {
            "initialize" | "session/resume" | "session/load" | "session/set_model" => {
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "session/prompt" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": 41,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": "acp-perm-1",
                            "toolCall": { "toolCallId": "tc-9", "title": "bash rm -rf /tmp/x" },
                            "options": [
                                { "optionId": "opt-allow", "name": "Allow", "kind": "allow_once" },
                                { "optionId": "opt-always", "name": "Always allow", "kind": "allow_always" },
                                { "optionId": "opt-reject", "name": "Reject", "kind": "reject_once" }
                            ]
                        }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "stopReason": "end_turn" }
                    }),
                )
                .await;
            }
            _ => {
                if frame.get("result").is_some() {
                    let _ = responses.send(line.trim().to_string());
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_adapter_maps_permission_decisions_to_options() {
    let (responses, mut response_receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        let responses = responses.clone();
        tokio::spawn(fake_acp_agent_with_permission(server_read, server_write, responses));
        Ok(halves)
    });
    let mut adapter = AcpAdapter::new(ProviderId::Grok, spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let turn = tokio::spawn(async move {
        adapter
            .start_turn(
                baton::adapters::TurnStartInput {
                    thread_id: "bt".into(),
                    cwd: "/work".into(),
                    model: "grok-build".into(),
                    effort: None,
                    prompt: "p".into(),
                    permission_mode: PermissionMode::Default,
                    auth_env: vec![],
                },
                Some(baton::types::NativeSessionRef {
                    native_session_id: "acp-perm-1".into(),
                    rollout_path: None,
                }),
                event_sender,
            )
            .await
    });

    let approval = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            while let Ok(event) = event_receiver.try_recv() {
                if let baton::adapters::TurnEvent::ApprovalRequest { summary, respond, .. } = event {
                    return (summary, respond);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("approval event");
    assert!(approval.0.contains("rm -rf"));
    approval.1.send(ApprovalDecision::AcceptAlways).unwrap();

    let response = tokio::time::timeout(Duration::from_secs(10), response_receiver.recv())
        .await
        .expect("server receives permission response")
        .expect("response present");
    assert!(response.contains("\"id\":41"), "response addressed to request 41: {response}");
    assert!(response.contains("opt-always"), "acceptAlways maps to allow_always option: {response}");
    let _ = turn.await;
}
