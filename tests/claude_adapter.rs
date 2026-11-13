use baton::adapters::{duplex_halves, BatonAdapter};
use baton::adapters::claude::ClaudeAdapter;
use baton::types::PermissionMode;
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

async fn fake_claude_gated_streaming(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    release: tokio::sync::oneshot::Receiver<()>,
) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    line.clear();
    if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    send(
        &mut write,
        json!({
            "type": "stream_event",
            "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "streamed " } }
        }),
    )
    .await;
    send(
        &mut write,
        json!({
            "type": "stream_event",
            "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "live" } }
        }),
    )
    .await;
    let _ = release.await;
    send(
        &mut write,
        json!({
            "type": "result",
            "subtype": "success",
            "result": "streamed live",
            "session_id": "claude-session-1",
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        }),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_adapter_streams_deltas_before_turn_completion() {
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel::<()>();
    let release_slot = std::sync::Mutex::new(Some(release_receiver));
    let spawner: baton::adapters::claude::ClaudeSpawner =
        Arc::new(move |input: &baton::adapters::TurnStartInput, _session: Option<&baton::types::NativeSessionRef>| {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            let release = release_slot.lock().unwrap().take().expect("spawner called once");
            tokio::spawn(fake_claude_gated_streaming(server_read, server_write, release));
            let _ = input;
            Ok(halves)
        });
    let mut adapter = ClaudeAdapter::new(spawner);

    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let turn_task = tokio::spawn(async move {
        adapter
            .start_turn(
                baton::adapters::TurnStartInput {
                    thread_id: "baton-thread".into(),
                    cwd: "/work".into(),
                    model: "claude-sonnet-4-5".into(),
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

    // The fake claude holds the result frame until released, so any delta observed
    // here proves the adapter forwards partial-message deltas mid-turn.
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
            baton::adapters::TurnEvent::TurnCompleted { final_text, native_session_id, .. } => {
                Some((final_text.clone(), native_session_id.clone()))
            }
            _ => None,
        })
        .expect("turn completed event");
    assert_eq!(completed.0, "streamed live");
    assert_eq!(completed.1.as_deref(), Some("claude-session-1"));
}


async fn fake_claude_tool_result(read: tokio::io::DuplexStream, mut write: tokio::io::DuplexStream) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    line.clear();
    if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    send(
        &mut write,
        json!({
            "type": "assistant",
            "message": {
                "content": [
                    { "type": "text", "text": "checking" },
                    { "type": "tool_use", "id": "tool-1", "name": "Bash", "input": { "command": "ls" } }
                ],
                "usage": { "input_tokens": 10, "output_tokens": 2 }
            }
        }),
    )
    .await;
    send(
        &mut write,
        json!({
            "type": "user",
            "message": {
                "content": [
                    { "type": "tool_result", "tool_use_id": "tool-1", "is_error": false, "content": "done" }
                ]
            }
        }),
    )
    .await;
    send(
        &mut write,
        json!({
            "type": "result",
            "subtype": "success",
            "result": "checked",
            "session_id": "claude-session-2",
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        }),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_adapter_pairs_tool_use_with_tool_result() {
    let spawner: baton::adapters::claude::ClaudeSpawner =
        Arc::new(move |_input: &baton::adapters::TurnStartInput, _session: Option<&baton::types::NativeSessionRef>| {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            tokio::spawn(fake_claude_tool_result(server_read, server_write));
            Ok(halves)
        });
    let mut adapter = ClaudeAdapter::new(spawner);

    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_turn(
            baton::adapters::TurnStartInput {
                thread_id: "baton-thread".into(),
                cwd: "/work".into(),
                model: "claude-sonnet-4-5".into(),
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
    .expect("turn completes within 10s")
    .expect("turn should complete");

    let events: Vec<baton::adapters::TurnEvent> = event_receiver.iter().collect();
    let started = events.iter().find_map(|event| match event {
        baton::adapters::TurnEvent::ToolStarted { tool, detail, .. } => Some((tool.clone(), detail.clone())),
        _ => None,
    });
    assert_eq!(started, Some(("Bash".to_string(), Some("ls".to_string()))));

    let completed = events.iter().find_map(|event| match event {
        baton::adapters::TurnEvent::ToolCompleted { tool, detail, exit_code, .. } => {
            Some((tool.clone(), detail.clone(), *exit_code))
        }
        _ => None,
    });
    assert_eq!(
        completed,
        Some(("Bash".to_string(), Some("ls".to_string()), Some(0))),
        "tool_result completes the matching tool_use with the same label"
    );
}


async fn fake_claude_steer(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    observed: std::sync::mpsc::Sender<String>,
) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    send(
        &mut write,
        json!({
            "type": "stream_event",
            "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "working" } }
        }),
    )
    .await;
    line.clear();
    if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let steer: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
    let text = steer["message"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let _ = observed.send(text);
    send(
        &mut write,
        json!({
            "type": "result",
            "subtype": "success",
            "result": "steered",
            "session_id": "claude-steer",
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        }),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_steer_writes_a_user_message_into_the_live_turn() {
    let (observed_sender, observed_receiver) = std::sync::mpsc::channel::<String>();
    let spawner: baton::adapters::claude::ClaudeSpawner =
        Arc::new(move |_input: &baton::adapters::TurnStartInput, _session: Option<&baton::types::NativeSessionRef>| {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            tokio::spawn(fake_claude_steer(server_read, server_write, observed_sender.clone()));
            Ok(halves)
        });
    let mut adapter = ClaudeAdapter::new(spawner);
    let steer = adapter.steer_handle().expect("claude exposes steering");
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let turn = tokio::spawn(async move {
        adapter
            .start_turn(
                baton::adapters::TurnStartInput {
                    thread_id: "baton-thread".into(),
                    cwd: "/work".into(),
                    model: "claude-sonnet-4-5".into(),
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

    assert!(steer("change direction".into()));
    let observed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = observed_receiver.try_recv() {
                break text;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("steer reaches the claude input stream");
    assert_eq!(observed, "change direction");

    let result = tokio::time::timeout(Duration::from_secs(10), turn).await.unwrap().unwrap();
    assert!(result.is_ok());
    assert!(!steer("too late".into()), "steering after completion is rejected");
}
