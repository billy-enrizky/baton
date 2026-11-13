use baton::adapters::{duplex_halves, BatonAdapter, ForkSessionInput, ModelListInput, RevertSessionInput, SteerOutcome, SteerRequest, TransportHalves};
use baton::adapters::codex::CodexAdapter;
use baton::types::{ApprovalDecision, NativeSessionRef, PermissionMode, ProviderId};
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

async fn fake_server_full_turn(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    params_sender: tokio::sync::mpsc::UnboundedSender<Value>,
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
            "initialize" => {
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "thread/start" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "thread": { "id": "native-t1", "path": "/rollout.jsonl" } }
                    }),
                )
                .await;
            }
            "turn/start" => {
                let _ = params_sender.send(frame.get("params").cloned().unwrap_or(Value::Null));
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "turn": { "id": "turn-1", "status": "inProgress" } }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "thread/tokenUsage/updated",
                        "params": {
                            "threadId": "native-t1",
                            "turnId": "turn-1",
                            "tokenUsage": {
                                "last": { "inputTokens": 50, "cachedInputTokens": 500, "cacheWriteInputTokens": 100, "outputTokens": 20 },
                                "total": { "inputTokens": 999, "cachedInputTokens": 999, "cacheWriteInputTokens": 999, "outputTokens": 999 }
                            }
                        }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "item/agentMessage/delta",
                        "params": { "threadId": "native-t1", "delta": "hel" }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "turn/completed",
                        "params": {
                            "threadId": "native-t1",
                            "turn": {
                                "id": "turn-1",
                                "status": "completed",
                                "items": [ { "type": "agentMessage", "text": "hello there" } ]
                            }
                        }
                    }),
                )
                .await;
            }
            _ => {}
        }
    }
}

async fn fake_server_resume_with_approval(read: tokio::io::DuplexStream, mut write: tokio::io::DuplexStream) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    let mut approval_sent = false;
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
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "thread/resume" => {
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
                if !approval_sent {
                    approval_sent = true;
                    send(
                        &mut write,
                        json!({
                            "jsonrpc": "2.0",
                            "id": 77,
                            "method": "item/commandExecution/requestApproval",
                            "params": { "threadId": "native-existing", "command": "cargo test" }
                        }),
                    )
                    .await;
                }
            }
            "turn/start" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "turn": { "id": "turn-approval", "status": "inProgress" } }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "turn/completed",
                        "params": {
                            "threadId": "native-existing",
                            "turn": { "id": "turn-approval", "status": "completed", "items": [] }
                        }
                    }),
                )
                .await;
            }
            _ => {}
        }
    }
}

async fn fake_server_fork(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    params_sender: tokio::sync::mpsc::UnboundedSender<Value>,
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
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "thread/fork" => {
                let _ = params_sender.send(frame.get("params").cloned().unwrap_or(Value::Null));
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "thread": { "id": "native-forked" } }
                    }),
                )
                .await;
            }
            _ => {}
        }
    }
}

async fn fake_server_revert(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    params_sender: tokio::sync::mpsc::UnboundedSender<Value>,
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
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "thread/resume" => {
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "thread/revert" => {
                let _ = params_sender.send(frame.get("params").cloned().unwrap_or(Value::Null));
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            _ => {}
        }
    }
}

async fn fake_server_models(read: tokio::io::DuplexStream, mut write: tokio::io::DuplexStream) {
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
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "model/list" => {
                let cursor = frame
                    .get("params")
                    .and_then(|params| params.get("cursor"))
                    .and_then(Value::as_str);
                let result = if cursor.is_none() {
                    json!({
                        "data": [
                            { "id": "alpha-id", "model": "alpha-model", "hidden": false },
                            { "id": "hidden-id", "model": "hidden-model", "hidden": true }
                        ],
                        "nextCursor": "page-2"
                    })
                } else {
                    json!({
                        "data": [
                            { "id": "beta-id", "model": "beta-model", "hidden": false }
                        ],
                        "nextCursor": null
                    })
                };
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": result })).await;
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_adapter_lists_visible_models_across_pages() {
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        tokio::spawn(fake_server_models(server_read, server_write));
        Ok(halves)
    });
    let mut adapter = CodexAdapter::new(spawner);
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
    assert_eq!(models, vec!["alpha-model", "beta-model"]);
    adapter.dispose().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_adapter_reverts_a_native_turn() {
    let (params_sender, mut params_receiver) = tokio::sync::mpsc::unbounded_channel();
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        tokio::spawn(fake_server_revert(server_read, server_write, params_sender.clone()));
        Ok(halves)
    });
    let mut adapter = CodexAdapter::new(spawner);
    let reverted = tokio::time::timeout(
        Duration::from_secs(10),
        adapter.revert_session(RevertSessionInput {
            session: NativeSessionRef {
                native_session_id: "native-source".into(),
                rollout_path: None,
            },
            before_turn_id: "turn-remove".into(),
            auth_env: vec![],
        }),
    )
    .await
    .expect("revert completes")
    .expect("revert succeeds");
    assert!(reverted);
    let params = tokio::time::timeout(Duration::from_secs(2), params_receiver.recv())
        .await
        .expect("revert params arrive")
        .expect("revert params channel");
    assert_eq!(params["threadId"], "native-source");
    assert_eq!(params["beforeTurnId"], "turn-remove");
    adapter.dispose().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_adapter_forks_a_native_thread() {
    let (params_sender, mut params_receiver) = tokio::sync::mpsc::unbounded_channel();
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        tokio::spawn(fake_server_fork(server_read, server_write, params_sender.clone()));
        Ok(halves)
    });
    let mut adapter = CodexAdapter::new(spawner);
    let forked = tokio::time::timeout(
        Duration::from_secs(10),
        adapter.fork_session(ForkSessionInput {
            cwd: "/work".into(),
            model: "gpt-6.1-sol".into(),
            session: NativeSessionRef {
                native_session_id: "native-source".into(),
                rollout_path: None,
            },
            auth_env: vec![],
            last_turn_id: Some("turn-source".into()),
        }),
    )
    .await
    .expect("fork completes")
    .expect("fork succeeds");
    assert_eq!(forked.as_deref(), Some("native-forked"));
    let params = tokio::time::timeout(Duration::from_secs(2), params_receiver.recv())
        .await
        .expect("fork params arrive")
        .expect("fork params channel");
    assert_eq!(params["threadId"], "native-source");
    assert_eq!(params["lastTurnId"], "turn-source");
    assert_eq!(params["excludeTurns"], true);
    assert_eq!(params["cwd"], "/work");
    adapter.dispose().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_adapter_runs_a_full_turn() {
    let (params_sender, mut params_receiver) = tokio::sync::mpsc::unbounded_channel();
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        tokio::spawn(fake_server_full_turn(
            server_read,
            server_write,
            params_sender.clone(),
        ));
        Ok(halves)
    });
    let mut adapter = CodexAdapter::new(spawner);

    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let result = tokio::time::timeout(Duration::from_secs(10), adapter.start_turn(
        baton::adapters::TurnStartInput {
            turn: 1,
            thread_id: "baton-thread".into(),
            cwd: "/work".into(),
            model: "gpt-6.1-sol".into(),
            effort: Some("high".into()),
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

    let _ = result;
    let params = tokio::time::timeout(Duration::from_secs(2), params_receiver.recv())
        .await
        .expect("turn/start params arrive")
        .expect("turn/start params channel");
    assert_eq!(params["effort"], "high");
    let events: Vec<baton::adapters::TurnEvent> = event_receiver.iter().collect();
    let completed = events
        .iter()
        .find_map(|event| match event {
            baton::adapters::TurnEvent::TurnCompleted { final_text, usage, native_session_id, rollout_path } => {
                Some((final_text.clone(), usage.clone(), native_session_id.clone(), rollout_path.clone()))
            }
            _ => None,
        })
        .expect("turn completed event");
    assert_eq!(completed.0, "hello there");
    assert_eq!(completed.2.as_deref(), Some("native-t1"));
    assert_eq!(completed.3.as_deref(), Some("/rollout.jsonl"));
    assert_eq!(completed.1.unwrap().cache_read_tokens, 500);
    let deltas: String = events
        .iter()
        .filter_map(|event| match event {
            baton::adapters::TurnEvent::TextDelta { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "hel");
    let provider_turn = events.iter().find_map(|event| match event {
        baton::adapters::TurnEvent::ProviderTurnId { id } => Some(id.clone()),
        _ => None,
    });
    assert_eq!(provider_turn.as_deref(), Some("turn-1"));
    adapter.dispose().await.unwrap();
}

async fn fake_server_gated_streaming(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    release: tokio::sync::oneshot::Receiver<()>,
) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    let mut release = Some(release);
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
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
            }
            "thread/start" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "thread": { "id": "native-stream", "path": "/rollout.jsonl" } }
                    }),
                )
                .await;
            }
            "turn/start" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "turn": { "id": "turn-stream", "status": "inProgress" } }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "item/agentMessage/delta",
                        "params": { "threadId": "native-stream", "delta": "streamed " }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "item/agentMessage/delta",
                        "params": { "threadId": "native-stream", "delta": "live" }
                    }),
                )
                .await;
                if let Some(release) = release.take() {
                    let _ = release.await;
                }
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "turn/completed",
                        "params": {
                            "threadId": "native-stream",
                            "turn": {
                                "id": "turn-stream",
                                "status": "completed",
                                "items": [ { "type": "agentMessage", "text": "streamed live" } ]
                            }
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
async fn codex_adapter_streams_deltas_before_turn_completion() {
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel::<()>();
    let release_slot = std::sync::Mutex::new(Some(release_receiver));
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        let release = release_slot.lock().unwrap().take().expect("spawner called once");
        tokio::spawn(fake_server_gated_streaming(server_read, server_write, release));
        Ok(halves)
    });
    let mut adapter = CodexAdapter::new(spawner);

    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let turn_task = tokio::spawn(async move {
        adapter
            .start_turn(
                baton::adapters::TurnStartInput {
                    turn: 1,
                    thread_id: "baton-thread".into(),
                    cwd: "/work".into(),
                    model: "gpt-6.1-sol".into(),
                    effort: None,
                    prompt: "hello".into(),
                    permission_mode: PermissionMode::Default,
                    auth_env: vec![],
                },
                None,
                event_sender,
            )
            .await
            .unwrap();
    });

    // The fake server holds turn/completed until released, so any delta observed
    // here proves the adapter forwards deltas while the turn is still running.
    let first_delta = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match event_receiver.try_recv() {
                Ok(baton::adapters::TurnEvent::TextDelta { text }) => break text,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("text delta arrives while the turn is in flight");
    assert_eq!(first_delta, "streamed ");
    assert!(!turn_task.is_finished(), "turn must not complete before release");

    let _ = release_sender.send(());
    tokio::time::timeout(Duration::from_secs(10), turn_task).await.unwrap().unwrap();

    let events: Vec<baton::adapters::TurnEvent> = event_receiver.iter().collect();
    let deltas: String = std::iter::once(first_delta)
        .chain(events.iter().filter_map(|event| match event {
            baton::adapters::TurnEvent::TextDelta { text } => Some(text.clone()),
            _ => None,
        }))
        .collect();
    assert_eq!(deltas, "streamed live");
    let completed = events
        .iter()
        .find_map(|event| match event {
            baton::adapters::TurnEvent::TurnCompleted { final_text, .. } => Some(final_text.clone()),
            _ => None,
        })
        .expect("turn completed event");
    assert_eq!(completed, "streamed live");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_adapter_resumes_sessions_and_maps_approvals() {
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        tokio::spawn(fake_server_resume_with_approval(server_read, server_write));
        Ok(halves)
    });
    let mut adapter = CodexAdapter::new(spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let mut adapter_for_interrupt = adapter;
    let turn = tokio::spawn(async move {
        adapter_for_interrupt
            .start_turn(
                baton::adapters::TurnStartInput {
                    turn: 1,
                    thread_id: "bt".into(),
                    cwd: "/work".into(),
                    model: "m".into(),
                    effort: None,
                    prompt: "p".into(),
                    permission_mode: PermissionMode::Default,
                    auth_env: vec![],
                },
                Some(baton::types::NativeSessionRef {
                    native_session_id: "native-existing".into(),
                    rollout_path: None,
                }),
                event_sender,
            )
            .await
            .unwrap();
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
    assert_eq!(approval.0, "cargo test");
    approval.1.send(ApprovalDecision::AcceptAlways).unwrap();

    tokio::time::timeout(Duration::from_secs(10), turn).await.unwrap().unwrap();
    let _ = ProviderId::Codex;
}


async fn fake_server_failed_turn(read: tokio::io::DuplexStream, mut write: tokio::io::DuplexStream) {
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
            "initialize" => send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await,
            "thread/start" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "thread": { "id": "native-fail", "path": null } }
                    }),
                )
                .await;
            }
            "turn/start" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "turn": { "id": "turn-fail", "status": "inProgress" } }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "turn/completed",
                        "params": {
                            "threadId": "native-fail",
                            "turn": {
                                "id": "turn-fail",
                                "status": "failed",
                                "error": { "message": "model exploded" },
                                "items": []
                            }
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
async fn codex_adapter_reports_failed_turn_status() {
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        tokio::spawn(fake_server_failed_turn(server_read, server_write));
        Ok(halves)
    });
    let mut adapter = CodexAdapter::new(spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_turn(
            baton::adapters::TurnStartInput {
                turn: 1,
                thread_id: "baton-thread".into(),
                cwd: "/work".into(),
                model: "m".into(),
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
    .expect("turn returns within 10s");
    assert!(result.is_err(), "a failed turn must not return Ok");

    let events: Vec<baton::adapters::TurnEvent> = event_receiver.iter().collect();
    let failure = events.iter().find_map(|event| match event {
        baton::adapters::TurnEvent::TurnFailed { error } => Some(error.clone()),
        _ => None,
    });
    assert_eq!(failure.as_deref(), Some("model exploded"));
    assert!(
        !events.iter().any(|event| matches!(event, baton::adapters::TurnEvent::TurnCompleted { .. })),
        "a failed turn must not emit TurnCompleted"
    );
}

async fn fake_server_interrupt(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    observed: std::sync::mpsc::Sender<Value>,
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
            "initialize" => send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await,
            "thread/start" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "thread": { "id": "native-int", "path": null } }
                    }),
                )
                .await;
            }
            "turn/start" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "turn": { "id": "turn-int", "status": "inProgress" } }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "item/agentMessage/delta",
                        "params": { "threadId": "native-int", "delta": "working" }
                    }),
                )
                .await;
            }
            "turn/interrupt" => {
                let params = frame.get("params").cloned().unwrap_or(Value::Null);
                let _ = observed.send(params);
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "turn/completed",
                        "params": {
                            "threadId": "native-int",
                            "turn": {
                                "id": "turn-int",
                                "status": "interrupted",
                                "error": { "message": "interrupted by user" },
                                "items": []
                            }
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
async fn codex_interrupt_sends_turn_id_and_reports_interruption() {
    let (observed_sender, observed_receiver) = std::sync::mpsc::channel::<Value>();
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        tokio::spawn(fake_server_interrupt(server_read, server_write, observed_sender.clone()));
        Ok(halves)
    });
    let mut adapter = CodexAdapter::new(spawner);
    let interrupt = adapter.interrupt_handle();
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let turn = tokio::spawn(async move {
        adapter
            .start_turn(
                baton::adapters::TurnStartInput {
                    turn: 1,
                    thread_id: "baton-thread".into(),
                    cwd: "/work".into(),
                    model: "m".into(),
                    effort: None,
                    prompt: "hello".into(),
                    permission_mode: PermissionMode::Default,
                    auth_env: vec![],
                },
                None,
                event_sender,
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(event_receiver.try_recv(), Ok(baton::adapters::TurnEvent::TextDelta { .. })) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("turn is running");

    interrupt();

    let params = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(params) = observed_receiver.try_recv() {
                break params;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("interrupt request reaches the server");
    assert_eq!(params.get("threadId").and_then(Value::as_str), Some("native-int"));
    assert_eq!(
        params.get("turnId").and_then(Value::as_str),
        Some("turn-int"),
        "turn/interrupt requires the active turn id"
    );

    let result = tokio::time::timeout(Duration::from_secs(10), turn).await.unwrap().unwrap();
    assert!(result.is_err(), "an interrupted turn must not return Ok");
    let events: Vec<baton::adapters::TurnEvent> = event_receiver.iter().collect();
    let interrupted = events.iter().find_map(|event| match event {
        baton::adapters::TurnEvent::TurnInterrupted { reason } => reason.clone(),
        _ => None,
    });
    assert_eq!(interrupted.as_deref(), Some("interrupted by user"));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, baton::adapters::TurnEvent::TurnFailed { .. })),
        "an interruption is not a generic failure"
    );
}


async fn fake_server_steer(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    observed: std::sync::mpsc::Sender<Value>,
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
            "initialize" => send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await,
            "thread/start" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "thread": { "id": "native-steer", "path": null } }
                    }),
                )
                .await;
            }
            "turn/start" => {
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "turn": { "id": "turn-steer", "status": "inProgress" } }
                    }),
                )
                .await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "item/agentMessage/delta",
                        "params": { "threadId": "native-steer", "delta": "working" }
                    }),
                )
                .await;
            }
            "turn/steer" => {
                let params = frame.get("params").cloned().unwrap_or(Value::Null);
                let _ = observed.send(params);
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": { "turnId": "turn-steer" } })).await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "turn/completed",
                        "params": {
                            "threadId": "native-steer",
                            "turn": {
                                "id": "turn-steer",
                                "status": "completed",
                                "items": [ { "type": "agentMessage", "text": "steered" } ]
                            }
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
async fn codex_steer_injects_into_the_active_turn() {
    let (observed_sender, observed_receiver) = std::sync::mpsc::channel::<Value>();
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        tokio::spawn(fake_server_steer(server_read, server_write, observed_sender.clone()));
        Ok(halves)
    });
    let mut adapter = CodexAdapter::new(spawner);
    let steer = adapter.steer_handle().expect("codex exposes steering");
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let turn = tokio::spawn(async move {
        adapter
            .start_turn(
                baton::adapters::TurnStartInput {
                    turn: 1,
                    thread_id: "baton-thread".into(),
                    cwd: "/work".into(),
                    model: "m".into(),
                    effort: None,
                    prompt: "hello".into(),
                    permission_mode: PermissionMode::Default,
                    auth_env: vec![],
                },
                None,
                event_sender,
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(event_receiver.try_recv(), Ok(baton::adapters::TurnEvent::TextDelta { .. })) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("turn is running");

    let request = |text: &str| SteerRequest {
        thread_id: "baton-thread".into(),
        turn: 1,
        text: text.into(),
    };
    assert_eq!(
        steer(request("change direction")).await,
        SteerOutcome::Delivered,
        "steer is delivered while the turn runs"
    );
    let params = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(params) = observed_receiver.try_recv() {
                break params;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("steer frame reaches the server");
    assert_eq!(params["threadId"], "native-steer");
    assert_eq!(params["expectedTurnId"], "turn-steer");
    assert_eq!(params["input"][0]["text"], "change direction");

    let result = tokio::time::timeout(Duration::from_secs(10), turn).await.unwrap().unwrap();
    assert!(result.is_ok());
    assert_eq!(
        steer(request("too late")).await,
        SteerOutcome::StaleTurn,
        "steering after the turn ends is rejected"
    );
    assert_eq!(
        steer(SteerRequest {
            thread_id: "baton-thread".into(),
            turn: 7,
            text: "wrong turn".into(),
        })
        .await,
        SteerOutcome::StaleTurn,
        "a steer scoped to another baton turn is rejected"
    );
}
