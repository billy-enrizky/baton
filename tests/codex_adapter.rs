use baton::adapters::{duplex_halves, BatonAdapter, TransportHalves};
use baton::adapters::codex::CodexAdapter;
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

async fn fake_server_full_turn(read: tokio::io::DuplexStream, mut write: tokio::io::DuplexStream) {
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
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "thread/tokenUsage/updated",
                        "params": {
                            "threadId": "native-t1",
                            "tokenUsage": { "inputTokens": 50, "cachedInputTokens": 500, "cacheWriteInputTokens": 100, "outputTokens": 20 }
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
                            "turn": { "items": [ { "type": "agentMessage", "text": "hello there" } ] }
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
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
                send(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "turn/completed",
                        "params": { "threadId": "native-existing", "turn": { "items": [] } }
                    }),
                )
                .await;
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_adapter_runs_a_full_turn() {
    let spawner = Arc::new(move |_auth_env: &[(String, String)]| -> anyhow::Result<TransportHalves> {
        let (halves, server_read, server_write) = duplex_halves(64 * 1024);
        tokio::spawn(fake_server_full_turn(server_read, server_write));
        Ok(halves)
    });
    let mut adapter = CodexAdapter::new(spawner);

    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let result = tokio::time::timeout(Duration::from_secs(10), adapter.start_turn(
        baton::adapters::TurnStartInput {
            thread_id: "baton-thread".into(),
            cwd: "/work".into(),
            model: "gpt-6.1-sol".into(),
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
                send(&mut write, json!({ "jsonrpc": "2.0", "id": id, "result": {} })).await;
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
                            "turn": { "items": [ { "type": "agentMessage", "text": "streamed live" } ] }
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
                    thread_id: "baton-thread".into(),
                    cwd: "/work".into(),
                    model: "gpt-6.1-sol".into(),
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
                    thread_id: "bt".into(),
                    cwd: "/work".into(),
                    model: "m".into(),
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
