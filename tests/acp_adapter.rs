use baton::adapters::acp::AcpAdapter;
use baton::adapters::{duplex_halves, BatonAdapter, ModelListInput, TransportHalves, TurnEvent};
use baton::types::{ApprovalDecision, PermissionMode, ProviderId, QuestionAnswer};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
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
            "session/set_mode" | "session/set_config_option" => {
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "session/set_model" => {
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

async fn fake_acp_agent_reads_client_file(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    request_path: String,
) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    let mut client_fs = Value::Null;
    loop {
        line.clear();
        if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let frame: Value = match serde_json::from_str(line.trim()) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id = frame.get("id").cloned();
        match method.as_str() {
            "initialize" => {
                client_fs = frame
                    .pointer("/params/clientCapabilities/fs")
                    .cloned()
                    .unwrap_or(Value::Null);
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": 2,
                            "authMethods": []
                        }
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
                        "result": { "sessionId": "acp-session-1" }
                    }),
                )
                .await;
            }
            "session/set_mode" | "session/set_config_option" | "session/set_model" => {
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                )
                .await;
            }
            "session/prompt" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "acp-session-1",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {
                                    "type": "text",
                                    "text": format!(
                                        "cap:read={},write={}",
                                        client_fs.get("readTextFile").and_then(Value::as_bool).unwrap_or(false),
                                        client_fs.get("writeTextFile").and_then(Value::as_bool).unwrap_or(true)
                                    )
                                }
                            }
                        }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": "client-file-read",
                        "method": "fs/read_text_file",
                        "params": {
                            "sessionId": "acp-session-1",
                            "path": request_path,
                            "line": 2,
                            "limit": 2
                        }
                    }),
                )
                .await;
                let mut response_line = String::new();
                lines.read_line(&mut response_line).await.unwrap();
                let response: Value = serde_json::from_str(response_line.trim()).unwrap();
                let text = response
                    .get("result")
                    .and_then(|result| result.get("content"))
                    .and_then(Value::as_str)
                    .map(|content| format!("file:{content}"))
                    .or_else(|| {
                        response
                            .get("error")
                            .and_then(|error| error.get("message"))
                            .and_then(Value::as_str)
                            .map(|message| format!("error:{message}"))
                    })
                    .unwrap_or_else(|| "missing response".to_string());
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "acp-session-1",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": { "type": "text", "text": text }
                            }
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
                            "update": { "sessionUpdate": "state_update", "state": "idle", "stopReason": "end_turn" }
                        }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn" } }),
                )
                .await;
            }
            _ => {}
        }
    }
}

async fn fake_acp_agent_exits_with_question(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
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
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
        let id = frame.get("id").cloned();
        match method {
            "initialize" => {
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": 2 } }),
                )
                .await;
            }
            "session/new" => {
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "sessionId": "acp-exit" } }),
                )
                .await;
            }
            "session/set_model" | "session/set_config_option" => {
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                )
                .await;
            }
            "session/prompt" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": 900,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": "acp-exit",
                            "toolCall": {
                                "toolCallId": "interaction_exit_1",
                                "kind": "other",
                                "title": "Pick an exit option",
                                "status": "pending"
                            },
                            "options": [
                                { "optionId": "one", "name": "One", "kind": "allow_once" }
                            ]
                        }
                    }),
                )
                .await;
                return;
            }
            _ => {}
        }
    }
}

async fn fake_acp_session_error(read: tokio::io::DuplexStream, mut write: tokio::io::DuplexStream) {
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
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": 2 } })).await;
            }
            "session/new" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32000, "message": "Authentication required" }
                    }),
                )
                .await;
            }
            _ => {}
        }
    }
}

async fn fake_acp_plan(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    protocol_version: u64,
    updates: Vec<Value>,
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
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
        let id = frame.get("id").cloned();
        match method {
            "initialize" => {
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": protocol_version } }),
                )
                .await;
            }
            "session/new" => {
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "sessionId": "acp-plan" } }),
                )
                .await;
            }
            "session/set_model" | "session/set_config_option" => {
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "session/prompt" => {
                for update in &updates {
                    let session_id = update
                        .get("_sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or("acp-plan");
                    let mut update = update.clone();
                    if let Some(object) = update.as_object_mut() {
                        object.remove("_sessionId");
                    }
                    send(
                        &mut write,
                        json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": { "sessionId": session_id, "update": update }
                        }),
                    )
                    .await;
                }
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn" } }),
                )
                .await;
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_acp_agent_exit_fails_the_turn_and_restarts_cleanly() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let spawner_attempts = attempts.clone();
    let spawner = Arc::new(
        move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
            let attempt = spawner_attempts.fetch_add(1, Ordering::SeqCst);
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            if attempt == 0 {
                tokio::spawn(fake_acp_agent_exits_with_question(
                    server_read,
                    server_write,
                ));
            } else {
                tokio::spawn(fake_acp_agent(server_read, server_write));
            }
            Ok(halves)
        },
    );
    let mut adapter = AcpAdapter::new(ProviderId::Antigravity, spawner);
    let input = || baton::adapters::TurnStartInput {
        turn: 1,
        thread_id: "thread".into(),
        cwd: "/work".into(),
        model: "model".into(),
        effort: None,
        prompt: "hello".into(),
        permission_mode: PermissionMode::Default,
        auth_env: vec![],
    };
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        adapter.start_turn(input(), None, event_sender),
    )
    .await
    .expect("an agent exit returns instead of hanging");
    assert!(result.is_err(), "the runtime exit fails the turn");
    let events: Vec<TurnEvent> = event_receiver.try_iter().collect();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, TurnEvent::QuestionRequest { .. })),
        "the open question was reported: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, TurnEvent::TurnFailed { .. })),
        "the turn failed after the runtime exit: {events:?}"
    );

    let (restart_sender, _restart_receiver) = std::sync::mpsc::channel();
    tokio::time::timeout(
        Duration::from_secs(3),
        adapter.start_turn(input(), None, restart_sender),
    )
    .await
    .expect("the adapter restarts after an exit")
    .expect("the next turn completes");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_acp_error_before_the_prompt_releases_the_event_sender() {
    let spawner: Arc<dyn Fn(&[(String, String)]) -> anyhow::Result<TransportHalves> + Send + Sync> =
        Arc::new(|_auth_env: &[(String, String)]| {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            tokio::spawn(fake_acp_session_error(server_read, server_write));
            Ok(halves)
        });
    let mut adapter = AcpAdapter::new(ProviderId::Grok, spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_turn(
            baton::adapters::TurnStartInput {
                thread_id: "thread".into(),
                turn: 1,
                cwd: "/work".into(),
                model: "model".into(),
                effort: None,
                prompt: "hello".into(),
                permission_mode: PermissionMode::Default,
                auth_env: vec![],
            },
            None,
            event_sender,
        ),
    )
    .await
    .expect("the adapter returns instead of hanging");
    assert!(result.is_err(), "the session error is reported");
    assert!(
        matches!(
            event_receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ),
        "the adapter must drop its event sender on the error path"
    );
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
            turn: 1,
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
    assert!(
        events.iter().any(|event| matches!(
            event,
            baton::adapters::TurnEvent::TurnCompleted { final_text, .. } if final_text == "hello acp"
        )),
        "streamed ACP text is kept as the final answer"
    );
    adapter.dispose().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_client_file_reads_use_relative_cwd_and_advertise_read_only() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("notes.txt"), "one\ntwo\nthree\nfour").unwrap();
    let spawner = Arc::new(
        move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            tokio::spawn(fake_acp_agent_reads_client_file(
                server_read,
                server_write,
                "notes.txt".to_string(),
            ));
            Ok(halves)
        },
    );
    let mut adapter = AcpAdapter::new(ProviderId::Antigravity, spawner);
    let input = baton::adapters::TurnStartInput {
        turn: 1,
        thread_id: "thread".into(),
        cwd: workspace.path().to_string_lossy().to_string(),
        model: "model".into(),
        effort: None,
        prompt: "read the notes".into(),
        permission_mode: PermissionMode::Default,
        auth_env: vec![],
    };
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    tokio::time::timeout(
        Duration::from_secs(3),
        adapter.start_turn(input, None, event_sender),
    )
    .await
    .expect("the client file read completes")
    .expect("the turn completes");
    let events: Vec<TurnEvent> = event_receiver.try_iter().collect();
    assert!(
        events.iter().any(|event| matches!(
            event,
            TurnEvent::TurnCompleted { final_text, .. }
                if final_text.contains("cap:read=true,write=false")
                    && final_text.contains("file:two\nthree")
        )),
        "the client file response and capability reach the turn: {events:?}"
    );
    adapter.dispose().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_adapter_maps_plan_updates() {
    let updates = Arc::new(vec![
        json!({
            "sessionUpdate": "plan_update",
            "plan": {
                "type": "items",
                "planId": "plan-1",
                "entries": [
                    { "content": "Read the request", "priority": "high", "status": "completed" },
                    { "content": "Write the answer", "priority": "medium", "status": "in_progress" }
                ]
            }
        }),
        json!({ "sessionUpdate": "plan_removed", "planId": "plan-1" }),
        json!({
            "_sessionId": "another-session",
            "sessionUpdate": "plan_update",
            "plan": {
                "type": "items",
                "planId": "foreign-plan",
                "entries": [
                    { "content": "Foreign step", "priority": "high", "status": "pending" }
                ]
            }
        }),
    ]);
    let spawner_updates = updates.clone();
    let spawner = Arc::new(
        move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            tokio::spawn(fake_acp_plan(
                server_read,
                server_write,
                2,
                (*spawner_updates).clone(),
            ));
            Ok(halves)
        },
    );
    let mut adapter = AcpAdapter::new(ProviderId::Antigravity, spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_turn(
            baton::adapters::TurnStartInput {
                turn: 1,
                thread_id: "bt".into(),
                cwd: "/work".into(),
                model: "gemini-3.7-flash".into(),
                effort: None,
                prompt: "plan".into(),
                permission_mode: PermissionMode::Default,
                auth_env: vec![],
            },
            None,
            event_sender,
        ),
    )
    .await
    .expect("plan turn completes")
    .expect("plan turn succeeds");

    let events: Vec<baton::adapters::TurnEvent> = event_receiver.iter().collect();
    assert!(
        events.iter().any(|event| matches!(
            event,
            baton::adapters::TurnEvent::PlanUpdated { provider, plan_id, turn_id, entries, explanation }
                if *provider == ProviderId::Antigravity
                    && plan_id.as_deref() == Some("plan-1")
                    && turn_id.is_none()
                    && explanation.is_none()
                    && entries.len() == 2
                    && entries[0].content == "Read the request"
                    && entries[0].priority.as_deref() == Some("high")
                    && entries[1].status == "in_progress"
        )),
        "plan event missing: {events:?}"
    );
    let plans: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            baton::adapters::TurnEvent::PlanUpdated { entries, .. } => Some(entries),
            _ => None,
        })
        .collect();
    assert_eq!(plans.len(), 1);
    assert!(
        events.iter().any(|event| matches!(
            event,
            baton::adapters::TurnEvent::PlanRemoved { provider, plan_id }
                if *provider == ProviderId::Antigravity && plan_id == "plan-1"
        )),
        "plan_removed keeps its identity: {events:?}"
    );
    adapter.dispose().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_adapter_preserves_markdown_file_and_unknown_plan_kinds() {
    let updates = Arc::new(vec![
        json!({
            "sessionUpdate": "plan_update",
            "plan": { "type": "markdown", "planId": "plan-markdown", "content": "# Ship it\n\n- run checks" }
        }),
        json!({
            "sessionUpdate": "plan_update",
            "plan": { "type": "file", "planId": "plan-file", "uri": "file:///workspace/PLAN.md" }
        }),
        json!({
            "sessionUpdate": "plan_update",
            "plan": { "type": "diagram", "planId": "plan-future", "nodes": [] }
        }),
        json!({
            "sessionUpdate": "plan_update",
            "plan": { "type": "markdown", "planId": "plan-missing-content" }
        }),
        json!({
            "sessionUpdate": "plan_update",
            "plan": { "type": "file", "planId": "plan-missing-uri" }
        }),
    ]);
    let spawner_updates = updates.clone();
    let spawner = Arc::new(
        move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            tokio::spawn(fake_acp_plan(
                server_read,
                server_write,
                2,
                (*spawner_updates).clone(),
            ));
            Ok(halves)
        },
    );
    let mut adapter = AcpAdapter::new(ProviderId::Antigravity, spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_turn(
            baton::adapters::TurnStartInput {
                turn: 1,
                thread_id: "bt".into(),
                cwd: "/work".into(),
                model: "gemini-3.7-flash".into(),
                effort: None,
                prompt: "plan".into(),
                permission_mode: PermissionMode::Default,
                auth_env: vec![],
            },
            None,
            event_sender,
        ),
    )
    .await
    .expect("plan turn completes")
    .expect("plan turn succeeds");

    let events: Vec<baton::adapters::TurnEvent> = event_receiver.iter().collect();
    let plans: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            baton::adapters::TurnEvent::PlanUpdated {
                plan_id, entries, ..
            } => Some((plan_id.as_deref(), entries)),
            _ => None,
        })
        .collect();
    assert_eq!(plans.len(), 3, "plan kinds: {events:?}");
    assert!(!plans
        .iter()
        .any(|(id, _)| matches!(*id, Some("plan-missing-content") | Some("plan-missing-uri"))));
    assert_eq!(plans[0].0, Some("plan-markdown"));
    assert_eq!(plans[0].1[0].kind, baton::types::PlanKind::Markdown);
    assert_eq!(plans[0].1[0].content, "# Ship it\n\n- run checks");
    assert_eq!(plans[0].1[0].status, "pending");
    assert_eq!(plans[1].0, Some("plan-file"));
    assert_eq!(plans[1].1[0].kind, baton::types::PlanKind::File);
    assert_eq!(
        plans[1].1[0].uri.as_deref(),
        Some("file:///workspace/PLAN.md")
    );
    assert_eq!(plans[2].0, Some("plan-future"));
    assert_eq!(plans[2].1[0].kind, baton::types::PlanKind::Items);
    assert_eq!(plans[2].1[0].content, "[unsupported ACP plan: diagram]");
    adapter.dispose().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_adapter_maps_the_legacy_plan_frame() {
    let updates = Arc::new(vec![json!({
        "sessionUpdate": "plan",
        "entries": [
            { "content": "Read the request", "priority": "high", "status": "completed" }
        ]
    })]);
    let spawner_updates = updates.clone();
    let spawner = Arc::new(
        move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            tokio::spawn(fake_acp_plan(
                server_read,
                server_write,
                1,
                (*spawner_updates).clone(),
            ));
            Ok(halves)
        },
    );
    let mut adapter = AcpAdapter::new(ProviderId::Grok, spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_turn(
            baton::adapters::TurnStartInput {
                turn: 1,
                thread_id: "bt".into(),
                cwd: "/work".into(),
                model: "grok-build".into(),
                effort: None,
                prompt: "plan".into(),
                permission_mode: PermissionMode::Default,
                auth_env: vec![],
            },
            None,
            event_sender,
        ),
    )
    .await
    .expect("plan turn completes")
    .expect("plan turn succeeds");
    let events: Vec<baton::adapters::TurnEvent> = event_receiver.iter().collect();
    assert!(
        events.iter().any(|event| matches!(
            event,
            baton::adapters::TurnEvent::PlanUpdated { provider, entries, .. }
                if *provider == ProviderId::Grok
                    && entries.len() == 1
                    && entries[0].content == "Read the request"
        )),
        "legacy plan event missing: {events:?}"
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
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                )
                .await;
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

async fn fake_acp_agent_with_question(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    responses: tokio::sync::mpsc::UnboundedSender<String>,
) {
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
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
        let id = frame.get("id").cloned();
        match method {
            "initialize" | "session/resume" | "session/load" | "session/set_model" => {
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                )
                .await;
            }
            "session/new" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "sessionId": "acp-question-1" }
                    }),
                )
                .await;
            }
            "session/set_mode" | "session/set_config_option" => {
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                )
                .await;
            }
            "session/prompt" => {
                prompt_id = id.clone();
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": 92,
                        "method": "x.ai/ask_user_question",
                        "params": {
                            "sessionId": "acp-question-1",
                            "toolCallId": "tc-question",
                            "mode": "default",
                            "questions": [{
                                "id": "scope",
                                "question": "Which scope should Grok use?",
                                "options": [
                                    { "label": "Workspace", "description": "Use the current workspace" },
                                    { "label": "Session", "description": "Only use this session" }
                                ]
                            }]
                        }
                    }),
                )
                .await;
            }
            _ => {
                if id == Some(Value::from(92)) {
                    let _ = responses.send(line.trim().to_string());
                    send(
                        &mut write,
                        json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": "acp-question-1",
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": { "type": "text", "text": "question answered" }
                                }
                            }
                        }),
                    )
                    .await;
                    if let Some(prompt_id) = prompt_id.take() {
                        send(
                            &mut write,
                            json!({ "jsonrpc": "2.0", "id": prompt_id, "result": { "stopReason": "end_turn" } }),
                        )
                        .await;
                    }
                }
            }
        }
    }
}

async fn fake_acp_agent_with_antigravity_question(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    responses: tokio::sync::mpsc::UnboundedSender<String>,
) {
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
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
        let id = frame.get("id").cloned();
        match method {
            "initialize" | "session/resume" | "session/load" | "session/set_model" => {
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                )
                .await;
            }
            "session/new" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "sessionId": "acp-question-2" }
                    }),
                )
                .await;
            }
            "session/set_mode" | "session/set_config_option" => {
                send(
                    &mut write,
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                )
                .await;
            }
            "session/prompt" => {
                prompt_id = id;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": 93,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": "acp-question-2",
                            "toolCall": {
                                "toolCallId": "interaction_9960062f",
                                "title": "Which database should the service use?"
                            },
                            "options": [
                                { "optionId": "opt-postgres", "name": "Postgres", "kind": "allow_once" },
                                { "optionId": "opt-sqlite", "name": "SQLite", "kind": "allow_once" }
                            ]
                        }
                    }),
                )
                .await;
            }
            _ => {
                if id == Some(Value::from(93)) {
                    let _ = responses.send(line.trim().to_string());
                    send(
                        &mut write,
                        json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": "acp-question-2",
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": { "type": "text", "text": "question answered" }
                                }
                            }
                        }),
                    )
                    .await;
                    if let Some(prompt_id) = prompt_id.take() {
                        send(
                            &mut write,
                            json!({ "jsonrpc": "2.0", "id": prompt_id, "result": { "stopReason": "end_turn" } }),
                        )
                        .await;
                    }
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
                    turn: 1,
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
                if let baton::adapters::TurnEvent::ApprovalRequest {
                    summary,
                    choices,
                    respond,
                    ..
                } = event
                {
                    return (summary, choices, respond);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("approval event");
    assert!(approval.0.contains("rm -rf"));
    assert_eq!(approval.1.len(), 3);
    assert_eq!(approval.1[0].label, "Allow");
    assert_eq!(approval.1[1].kind, "allow_always");
    approval.2.send(ApprovalDecision::AcceptAlways).unwrap();

    let response = tokio::time::timeout(Duration::from_secs(10), response_receiver.recv())
        .await
        .expect("server receives permission response")
        .expect("response present");
    assert!(response.contains("\"id\":41"), "response addressed to request 41: {response}");
    assert!(response.contains("opt-always"), "acceptAlways maps to allow_always option: {response}");
    let _ = turn.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_adapter_routes_xai_questions_to_the_tui_surface() {
    let (responses, mut response_receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
    let spawner = Arc::new(
        move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            let responses = responses.clone();
            tokio::spawn(fake_acp_agent_with_question(
                server_read,
                server_write,
                responses,
            ));
            Ok(halves)
        },
    );
    let mut adapter = AcpAdapter::new(ProviderId::Grok, spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let turn = tokio::spawn(async move {
        adapter
            .start_turn(
                baton::adapters::TurnStartInput {
                    turn: 1,
                    thread_id: "bt".into(),
                    cwd: "/work".into(),
                    model: "grok-build".into(),
                    effort: None,
                    prompt: "p".into(),
                    permission_mode: PermissionMode::Default,
                    auth_env: vec![],
                },
                None,
                event_sender,
            )
            .await
    });

    let question = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            while let Ok(event) = event_receiver.try_recv() {
                if let baton::adapters::TurnEvent::QuestionRequest {
                    questions, respond, ..
                } = event
                {
                    return (questions, respond);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("question event");
    assert_eq!(question.0.len(), 1);
    assert_eq!(question.0[0].id, "scope");
    assert_eq!(question.0[0].question, "Which scope should Grok use?");
    assert_eq!(question.0[0].options[0].label, "Workspace");
    question
        .1
        .send(vec![QuestionAnswer {
            id: "scope".into(),
            answers: vec!["Workspace".into()],
        }])
        .unwrap();

    let response = tokio::time::timeout(Duration::from_secs(10), response_receiver.recv())
        .await
        .expect("server receives question response")
        .expect("response present");
    assert!(
        response.contains("\"id\":92"),
        "response addressed to request 92: {response}"
    );
    assert!(response.contains("\"outcome\":\"accepted\""), "{response}");
    assert!(
        response.contains("\"Which scope should Grok use?\":[\"Workspace\"]"),
        "{response}"
    );
    let _ = tokio::time::timeout(Duration::from_secs(10), turn)
        .await
        .expect("turn completes")
        .expect("turn succeeds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_adapter_routes_antigravity_interactions_to_the_tui_surface() {
    let (responses, mut response_receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
    let spawner = Arc::new(
        move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            let responses = responses.clone();
            tokio::spawn(fake_acp_agent_with_antigravity_question(
                server_read,
                server_write,
                responses,
            ));
            Ok(halves)
        },
    );
    let mut adapter = AcpAdapter::new(ProviderId::Antigravity, spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let turn = tokio::spawn(async move {
        adapter
            .start_turn(
                baton::adapters::TurnStartInput {
                    turn: 1,
                    thread_id: "bt".into(),
                    cwd: "/work".into(),
                    model: "gemini-3.7-flash".into(),
                    effort: None,
                    prompt: "p".into(),
                    permission_mode: PermissionMode::Default,
                    auth_env: vec![],
                },
                None,
                event_sender,
            )
            .await
    });

    let question = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            while let Ok(event) = event_receiver.try_recv() {
                if let baton::adapters::TurnEvent::QuestionRequest {
                    questions, respond, ..
                } = event
                {
                    return (questions, respond);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("question event");
    assert_eq!(question.0.len(), 1);
    assert_eq!(question.0[0].id, "interaction_9960062f");
    assert_eq!(
        question.0[0].question,
        "Which database should the service use?"
    );
    assert_eq!(question.0[0].options[1].label, "SQLite");
    question
        .1
        .send(vec![QuestionAnswer {
            id: "interaction_9960062f".into(),
            answers: vec!["SQLite".into()],
        }])
        .unwrap();

    let response = tokio::time::timeout(Duration::from_secs(10), response_receiver.recv())
        .await
        .expect("server receives question response")
        .expect("response present");
    assert!(
        response.contains("\"id\":93"),
        "response addressed to request 93: {response}"
    );
    assert!(
        response.contains("\"optionId\":\"opt-sqlite\""),
        "{response}"
    );
    let _ = tokio::time::timeout(Duration::from_secs(10), turn)
        .await
        .expect("turn completes")
        .expect("turn succeeds");
}
