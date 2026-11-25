use baton::adapters::{duplex_halves, BatonAdapter};
use baton::adapters::claude::ClaudeAdapter;
use baton::types::{ApprovalDecision, PermissionMode};
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
            "session_id": "claude-session-1",
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
                    turn: 1,
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
    let mut early_events = Vec::new();
    let first_delta = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match event_receiver.try_recv() {
                Ok(baton::adapters::TurnEvent::TextDelta { text }) => break text,
                Ok(event) => early_events.push(event),
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

    let events: Vec<baton::adapters::TurnEvent> =
        early_events.into_iter().chain(event_receiver.iter()).collect();
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
    assert!(
        events.iter().any(|event| matches!(
            event,
            baton::adapters::TurnEvent::ProviderSession { native_session_id, .. }
                if native_session_id == "claude-session-1"
        )),
        "the reported session id is persisted before the turn completes"
    );
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
                turn: 1,
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
                    turn: 1,
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

    let request = |text: &str| baton::adapters::SteerRequest {
        thread_id: "baton-thread".into(),
        turn: 1,
        text: text.into(),
    };
    assert_eq!(
        steer(request("change direction")).await,
        baton::adapters::SteerOutcome::Delivered
    );
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
    assert_eq!(
        steer(request("too late")).await,
        baton::adapters::SteerOutcome::StaleTurn,
        "steering after completion is rejected"
    );
}

async fn fake_claude_subagent(read: tokio::io::DuplexStream, mut write: tokio::io::DuplexStream) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    send(
        &mut write,
        json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_task_1",
                    "name": "Task",
                    "input": {
                        "subagent_type": "Explore",
                        "description": "Find the parser"
                    }
                }]
            }
        }),
    )
    .await;
    send(
        &mut write,
        json!({
            "type": "user",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_task_1",
                    "is_error": false
                }]
            }
        }),
    )
    .await;
    send(
        &mut write,
        json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "done" }] } }),
    )
    .await;
    send(
        &mut write,
        json!({
            "type": "result",
            "subtype": "success",
            "result": "done",
            "session_id": "claude-session-subagent"
        }),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_adapter_reports_task_subagents() {
    let spawner: baton::adapters::claude::ClaudeSpawner = Arc::new(
        |_input: &baton::adapters::TurnStartInput,
         _session: Option<&baton::types::NativeSessionRef>| {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            tokio::spawn(fake_claude_subagent(server_read, server_write));
            Ok(halves)
        },
    );
    let mut adapter = ClaudeAdapter::new(spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_turn(
            baton::adapters::TurnStartInput {
                turn: 1,
                thread_id: "baton-thread".into(),
                cwd: "/work".into(),
                model: "claude-sonnet-4-5".into(),
                effort: None,
                prompt: "delegate this".into(),
                permission_mode: PermissionMode::Default,
                auth_env: vec![],
            },
            None,
            event_sender,
        ),
    )
    .await
    .unwrap()
    .unwrap();

    let events: Vec<baton::adapters::TurnEvent> = event_receiver.iter().collect();
    let started = events.iter().find_map(|event| match event {
        baton::adapters::TurnEvent::SubagentStarted { id, name, detail } => {
            Some((id.clone(), name.clone(), detail.clone()))
        }
        _ => None,
    });
    let started = started.expect("Task emits a subagent start");
    assert_eq!(started.0.as_deref(), Some("toolu_task_1"));
    assert_eq!(started.1, "Explore");
    assert_eq!(started.2.as_deref(), Some("Find the parser"));
    let finished = events.iter().find_map(|event| match event {
        baton::adapters::TurnEvent::SubagentFinished {
            id, name, status, ..
        } => Some((id.clone(), name.clone(), status.clone())),
        _ => None,
    });
    let finished = finished.expect("the tool result emits a subagent finish");
    assert_eq!(finished.0.as_deref(), Some("toolu_task_1"));
    assert_eq!(finished.1, "Explore");
    assert_eq!(finished.2, "done");
}

async fn fake_claude_approval(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    captured: tokio::sync::mpsc::UnboundedSender<Value>,
) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    send(
        &mut write,
        json!({
            "type": "control_request",
            "request_id": "req-1",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "input": { "command": "ls" },
                "tool_use_id": "toolu_9"
            }
        }),
    )
    .await;
    let mut answer = String::new();
    if lines.read_line(&mut answer).await.unwrap_or(0) == 0 {
        return;
    }
    let _ = captured.send(serde_json::from_str(&answer).unwrap_or(Value::Null));
    send(
        &mut write,
        json!({
            "type": "result",
            "subtype": "success",
            "result": "approved",
            "session_id": "claude-session-approval"
        }),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_adapter_answers_the_control_request_id_the_frame_carried() {
    let (captured, mut captured_receiver) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let spawner: baton::adapters::claude::ClaudeSpawner = Arc::new(
        move |_input: &baton::adapters::TurnStartInput,
              _session: Option<&baton::types::NativeSessionRef>| {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            tokio::spawn(fake_claude_approval(
                server_read,
                server_write,
                captured.clone(),
            ));
            Ok(halves)
        },
    );
    let mut adapter = ClaudeAdapter::new(spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let turn = tokio::spawn(async move {
        adapter
            .start_turn(
                baton::adapters::TurnStartInput {
                    turn: 1,
                    thread_id: "baton-thread".into(),
                    cwd: "/work".into(),
                    model: "claude-sonnet-4-5".into(),
                    effort: None,
                    prompt: "list files".into(),
                    permission_mode: PermissionMode::Default,
                    auth_env: vec![],
                },
                None,
                event_sender,
            )
            .await
            .unwrap();
    });

    let approval = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            while let Ok(event) = event_receiver.try_recv() {
                if let baton::adapters::TurnEvent::ApprovalRequest {
                    summary, respond, ..
                } = event
                {
                    return (summary, respond);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("approval event");
    assert_eq!(approval.0, "ls");
    approval.1.send(ApprovalDecision::Accept).unwrap();

    tokio::time::timeout(Duration::from_secs(10), turn)
        .await
        .unwrap()
        .unwrap();
    let answer = tokio::time::timeout(Duration::from_secs(10), captured_receiver.recv())
        .await
        .expect("the adapter wrote a control response")
        .expect("control response value");
    assert_eq!(answer["type"], "control_response");
    assert_eq!(
        answer["response"]["request_id"], "req-1",
        "the response must carry the id the frame carried: {answer}"
    );
    assert_eq!(answer["response"]["response"]["behavior"], "allow");
    assert_eq!(answer["response"]["response"]["toolUseID"], "toolu_9");
    assert_eq!(
        answer["response"]["response"]["updatedInput"]["command"],
        "ls"
    );
}

async fn fake_claude_question(
    read: tokio::io::DuplexStream,
    mut write: tokio::io::DuplexStream,
    captured: tokio::sync::mpsc::UnboundedSender<Value>,
) {
    let mut lines = BufReader::new(read);
    let mut line = String::new();
    if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    send(
        &mut write,
        json!({
            "type": "control_request",
            "request_id": "req-7",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "AskUserQuestion",
                "tool_use_id": "toolu_ask_1",
                "input": {
                    "questions": [
                        {
                            "header": "Approach",
                            "question": "Which approach?",
                            "multiSelect": true,
                            "options": [
                                { "label": "Simple", "description": "fewer parts" },
                                { "label": "Safe", "description": "keep the current setup" }
                            ]
                        },
                        {
                            "header": "Scope",
                            "question": "Ship what?",
                            "options": [
                                { "label": "Installer", "description": "ship the installer" },
                                { "label": "Docs", "description": "ship the docs" }
                            ]
                        }
                    ]
                }
            }
        }),
    )
    .await;
    let mut answer = String::new();
    if lines.read_line(&mut answer).await.unwrap_or(0) == 0 {
        return;
    }
    let _ = captured.send(serde_json::from_str(&answer).unwrap_or(Value::Null));
    send(
        &mut write,
        json!({
            "type": "result",
            "subtype": "success",
            "result": "answered",
            "session_id": "claude-session-question"
        }),
    )
    .await;
}

async fn run_claude_question(
    answers: Option<Vec<baton::types::QuestionAnswer>>,
) -> (Vec<baton::adapters::TurnEvent>, Value) {
    let (captured, mut captured_receiver) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let spawner: baton::adapters::claude::ClaudeSpawner = Arc::new(
        move |_input: &baton::adapters::TurnStartInput,
              _session: Option<&baton::types::NativeSessionRef>| {
            let (halves, server_read, server_write) = duplex_halves(64 * 1024);
            tokio::spawn(fake_claude_question(
                server_read,
                server_write,
                captured.clone(),
            ));
            Ok(halves)
        },
    );
    let mut adapter = ClaudeAdapter::new(spawner);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let turn = tokio::spawn(async move {
        adapter
            .start_turn(
                baton::adapters::TurnStartInput {
                    turn: 1,
                    thread_id: "baton-thread".into(),
                    cwd: "/work".into(),
                    model: "claude-sonnet-4-5".into(),
                    effort: None,
                    prompt: "ask me".into(),
                    permission_mode: PermissionMode::Default,
                    auth_env: vec![],
                },
                None,
                event_sender,
            )
            .await
            .unwrap();
    });

    let request = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            while let Ok(event) = event_receiver.try_recv() {
                if let baton::adapters::TurnEvent::QuestionRequest {
                    id,
                    questions,
                    respond,
                } = event
                {
                    return (id, questions, respond);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("question request");
    assert_eq!(request.0, "claude:req-7");
    assert_eq!(request.1.len(), 2, "both questions reach the overlay");
    assert_eq!(request.1[0].id, "Which approach?");
    assert_eq!(request.1[0].header, "Approach");
    assert!(request.1[0].multi_select, "multiSelect reaches the overlay");
    assert!(
        !request.1[1].allow_custom,
        "Claude draws only the labels it offered, so the overlay must not accept other text"
    );
    assert_eq!(request.1[1].options.len(), 2);
    let _ = request.2.send(answers.unwrap_or_default());

    tokio::time::timeout(Duration::from_secs(10), turn)
        .await
        .unwrap()
        .unwrap();
    let answer = tokio::time::timeout(Duration::from_secs(10), captured_receiver.recv())
        .await
        .expect("the adapter wrote a control response")
        .expect("control response value");
    let events: Vec<baton::adapters::TurnEvent> = event_receiver.iter().collect();
    (events, answer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_adapter_answers_ask_user_question_with_joined_labels() {
    let (_events, answer) = run_claude_question(Some(vec![
        baton::types::QuestionAnswer {
            id: "Which approach?".into(),
            answers: vec!["Simple".into(), "Safe".into()],
        },
        baton::types::QuestionAnswer {
            id: "Ship what?".into(),
            answers: vec!["Installer".into()],
        },
    ]))
    .await;

    assert_eq!(answer["response"]["request_id"], "req-7");
    let result = &answer["response"]["response"];
    assert_eq!(result["behavior"], "allow");
    assert_eq!(result["toolUseID"], "toolu_ask_1");
    assert_eq!(
        result["updatedInput"]["answers"]["Which approach?"], "Simple, Safe",
        "multi-select answers join into one comma-separated string: {answer}"
    );
    assert_eq!(result["updatedInput"]["answers"]["Ship what?"], "Installer");
    assert_eq!(
        result["updatedInput"]["questions"].as_array().map(Vec::len),
        Some(2),
        "the CLI receives the questions it asked: {answer}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_adapter_denies_an_answered_with_nothing_question() {
    let (_events, answer) = run_claude_question(None).await;

    let result = &answer["response"]["response"];
    assert_eq!(
        result["behavior"], "deny",
        "a dismissed question must not hand the model an empty allow: {answer}"
    );
    assert_eq!(result["message"], "User cancelled tool execution.");
    assert_eq!(result["toolUseID"], "toolu_ask_1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_question_answers_ignore_ids_the_questions_never_offered() {
    let (_events, answer) = run_claude_question(Some(vec![
        baton::types::QuestionAnswer {
            id: "Which approach?".into(),
            answers: vec!["Simple".into()],
        },
        baton::types::QuestionAnswer {
            id: "Made up".into(),
            answers: vec!["Injected".into()],
        },
    ]))
    .await;

    let answers = &answer["response"]["response"]["updatedInput"]["answers"];
    assert_eq!(answers["Which approach?"], "Simple");
    assert!(
        answers.get("Made up").is_none(),
        "only offered questions may carry an answer: {answer}"
    );
}
