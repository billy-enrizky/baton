use baton::adapters::opencode::{OpenCodeAdapter, OpenCodeChild, OpenCodeSpawner};
use baton::adapters::{BatonAdapter, ForkSessionInput, ModelListInput};
use baton::process::ProcessRecorder;
use baton::store::BatonStore;
use baton::types::PermissionMode;
use baton::types::{NativeSessionRef, ProviderId};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Minimal HTTP server speaking the opencode 1.x "next" flow: /api/info,
/// session create/model/prompt, and an SSE /api/event stream whose final
/// frames are held on a oneshot so the test can observe mid-turn deltas.
type Requests = Arc<Mutex<Vec<(String, String)>>>;

fn serve_opencode(
    listener: std::net::TcpListener,
    release: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    requests: Requests,
) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { break };
        let release = release.clone();
        let requests = requests.clone();
        std::thread::spawn(move || handle_connection(stream, &release, &requests));
    }
}

fn handle_connection(
    mut stream: std::net::TcpStream,
    release: &Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    requests: &Requests,
) {
    let mut buffer = [0u8; 8192];
    let mut head = Vec::new();
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        head.extend_from_slice(&buffer[..read]);
        if let Some(header_end) = find_header_end(&head) {
            let mut request = head.split_off(header_end);
            let head_text = String::from_utf8_lossy(&head).to_string();
            let content_length = head_text
                .to_ascii_lowercase()
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                .unwrap_or(0);
            while request.len() < content_length {
                let read = match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(read) => read,
                };
                request.extend_from_slice(&buffer[..read]);
            }
            let path = head_text.lines().next().unwrap_or("").split(' ').nth(1).unwrap_or("").to_string();
            let body = String::from_utf8_lossy(&request[..content_length.min(request.len())]).to_string();
            respond(stream, &path, release, requests, &body);
            return;
        }
    }
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n").map(|index| index + 4)
}

fn respond(
    mut stream: std::net::TcpStream,
    path: &str,
    release: &Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    requests: &Requests,
    body: &str,
) {
    let respond_json = |stream: &mut std::net::TcpStream, status: &str, body: &str| {
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    };
    if path.starts_with("/api/model") {
        respond_json(
            &mut stream,
            "200 OK",
            r#"{"data":[{"providerID":"myprovider","id":"model-a"},{"providerID":"myprovider","id":"model-b"}]}"#,
        );
        return;
    }
    match path {
        "/api/info" => respond_json(&mut stream, "200 OK", r#"{"version":"fake"}"#),
        "/api/session" => respond_json(&mut stream, "200 OK", r#"{"data":{"id":"ses_test"}}"#),
        "/api/session/ses_test/model" => respond_json(&mut stream, "204 No Content", ""),
        "/api/session/ses_test/prompt" => {
            requests
                .lock()
                .unwrap()
                .push((path.to_string(), body.to_string()));
            respond_json(&mut stream, "200 OK", r#"{"data":{"id":"msg_1"}}"#)
        }
        "/api/session/ses_test/interrupt" => {
            requests
                .lock()
                .unwrap()
                .push((path.to_string(), body.to_string()));
            respond_json(&mut stream, "204 No Content", "")
        }
        "/session/ses_test/fork" => {
            requests
                .lock()
                .unwrap()
                .push((path.to_string(), body.to_string()));
            respond_json(&mut stream, "200 OK", r#"{"id":"ses_fork"}"#)
        }
        "/api/event" => {
            if release.lock().unwrap().is_none() {
                respond_json(&mut stream, "503 Service Unavailable", "{\"error\":\"event stream disabled\"}");
                return;
            }
            let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: keep-alive\r\n\r\n";
            if stream.write_all(headers.as_bytes()).is_err() {
                return;
            }
            let send = |stream: &mut std::net::TcpStream, frame: &Value| {
                let payload = format!("data: {}\n\n", serde_json::to_string(frame).unwrap());
                let _ = stream.write_all(payload.as_bytes());
                let _ = stream.flush();
            };
            let session = "ses_test";
            send(&mut stream, &json!({"type": "session.next.text.delta", "data": {"sessionID": session, "delta": "streamed "}}));
            send(&mut stream, &json!({"type": "session.next.tool.input.started", "data": {"sessionID": session, "callID": "call-1", "name": "read"}}));
            send(&mut stream, &json!({"type": "session.next.tool.called", "data": {"sessionID": session, "callID": "call-1", "tool": "read", "input": {"path": "/work"}}}));
            // The real 1.19 server omits the tool name on success; only the callID identifies it.
            send(&mut stream, &json!({"type": "session.next.tool.success", "data": {"sessionID": session, "callID": "call-1"}}));
            send(&mut stream, &json!({"type": "session.next.step.ended", "data": {"sessionID": session, "finish": "tool-calls", "tokens": {"input": 10, "output": 2, "reasoning": 0, "cache": {"read": 0, "write": 0}}}}));
            send(&mut stream, &json!({"type": "session.next.text.delta", "data": {"sessionID": session, "delta": "live"}}));
            let release = release.lock().unwrap().take().expect("release channel present");
            let _ = release.blocking_recv();
            send(&mut stream, &json!({"type": "session.next.text.ended", "data": {"sessionID": session, "text": "streamed live"}}));
            send(&mut stream, &json!({"type": "session.next.step.ended", "data": {"sessionID": session, "finish": "stop", "tokens": {"input": 5, "output": 1, "reasoning": 0, "cache": {"read": 0, "write": 0}}}}));
        }
        _ => respond_json(&mut stream, "404 Not Found", "{}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_adapter_forks_a_whole_native_session() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_server = requests.clone();
    std::thread::spawn(move || serve_opencode(listener, Arc::new(Mutex::new(None)), requests_for_server));

    let spawner: OpenCodeSpawner = Arc::new(move |_auth_env: &[(String, String)]| {
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .stdout(std::process::Stdio::null())
            .spawn()?;
        Ok(OpenCodeChild { port, password: "pw".to_string(), child })
    });
    let mut adapter = OpenCodeAdapter::new(spawner);
    let forked = adapter
        .fork_session(ForkSessionInput {
            cwd: "/work".into(),
            model: "myprovider/model-a".into(),
            session: NativeSessionRef {
                native_session_id: "ses_test".into(),
                rollout_path: None,
            },
            auth_env: vec![],
            last_turn_id: None,
        })
        .await
        .unwrap();
    assert_eq!(forked.as_deref(), Some("ses_fork"));
    assert!(requests
        .lock()
        .unwrap()
        .iter()
        .any(|(path, _)| path == "/session/ses_test/fork"));

    let error = adapter
        .fork_session(ForkSessionInput {
            cwd: "/work".into(),
            model: "myprovider/model-a".into(),
            session: NativeSessionRef {
                native_session_id: "ses_test".into(),
                rollout_path: None,
            },
            auth_env: vec![],
            last_turn_id: Some("turn-1".into()),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("whole sessions only"));
    adapter.dispose().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_dispose_removes_the_tracked_process() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_server = requests.clone();
    std::thread::spawn(move || serve_opencode(listener, Arc::new(Mutex::new(None)), requests_for_server));

    let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
    let recorder = ProcessRecorder::new(store.clone(), "/work");
    let spawner: OpenCodeSpawner = {
        let recorder = recorder.clone();
        Arc::new(move |_auth_env: &[(String, String)]| {
            let child = tokio::process::Command::new("sleep")
                .arg("60")
                .stdout(std::process::Stdio::null())
                .spawn()?;
            let pid = child.id().expect("sleep child pid");
            let command = baton::process::command_line(pid).unwrap_or_else(|| "sleep 60".to_string());
            let start = baton::process::process_start(pid);
            recorder.record(pid, ProviderId::OpenCode, Some(port), &command, start.as_deref())?;
            Ok(OpenCodeChild { port, password: "pw".to_string(), child })
        })
    };
    let mut adapter = OpenCodeAdapter::with_recorder(spawner, recorder);
    adapter
        .list_models(ModelListInput {
            cwd: "/work".into(),
            auth_env: vec![],
        })
        .await
        .unwrap();
    assert_eq!(store.lock().unwrap().list_provider_processes().unwrap().len(), 1);

    adapter.dispose().await.unwrap();

    assert!(store.lock().unwrap().list_provider_processes().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_adapter_streams_deltas_and_completes_on_next_flow() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel::<()>();
    let release_slot = Arc::new(Mutex::new(Some(release_receiver)));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_server = requests.clone();
    std::thread::spawn(move || serve_opencode(listener, release_slot, requests_for_server));

    let spawner: OpenCodeSpawner = Arc::new(move |_auth_env: &[(String, String)]| {
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .stdout(std::process::Stdio::null())
            .spawn()?;
        Ok(OpenCodeChild { port, password: "pw".to_string(), child })
    });
    let mut adapter = OpenCodeAdapter::new(spawner);
    let models = adapter
        .list_models(ModelListInput {
            cwd: "/work".into(),
            auth_env: vec![],
        })
        .await
        .unwrap();
    assert_eq!(models, vec!["myprovider/model-a", "myprovider/model-b"]);
    let steer = adapter.steer_handle().expect("opencode exposes steering");
    let interrupt = adapter.interrupt_handle();
    assert!(!steer("too early".into()), "no runtime means no active turn");

    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let turn_task = tokio::spawn(async move {
        adapter
            .start_turn(
                baton::adapters::TurnStartInput {
                    thread_id: "baton-thread".into(),
                    cwd: "/work".into(),
                    model: "myprovider/rits-zai-org/glm-5-3".into(),
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

    // Deltas (and the tool-calls step end) must arrive while the turn is held open.
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
    assert!(!turn_task.is_finished(), "a tool-calls step must not complete the turn");

    assert!(steer("change direction".into()), "steering is accepted during the live turn");
    let steer_body = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let bodies = requests.lock().unwrap().clone();
            if let Some((_, body)) = bodies
                .iter()
                .find(|(path, body)| path == "/api/session/ses_test/prompt" && body.contains("delivery"))
            {
                break body.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("steer request reaches opencode");
    let steer_json: Value = serde_json::from_str(&steer_body).unwrap();
    assert_eq!(steer_json["delivery"], "steer");
    assert_eq!(steer_json["prompt"]["text"], "change direction");
    assert!(
        steer_json["id"]
            .as_str()
            .unwrap_or("")
            .starts_with("msg_baton_steer_"),
        "steer uses a stable native inbox id"
    );

    interrupt();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if requests
                .lock()
                .unwrap()
                .iter()
                .any(|(path, _)| path == "/api/session/ses_test/interrupt")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("interrupt request reaches opencode");

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
    assert!(events.iter().any(|event| matches!(event, baton::adapters::TurnEvent::ToolStarted { tool, .. } if tool == "read")));
    assert!(
        events.iter().any(|event| matches!(
            event,
            baton::adapters::TurnEvent::ToolStarted { detail: Some(detail), .. }
                if detail == "/work"
        )),
        "the called frame upgrades the tool row with its input"
    );
    assert!(events.iter().any(|event| matches!(event, baton::adapters::TurnEvent::ToolCompleted { tool, .. } if tool == "read")));
    let completed = events
        .iter()
        .find_map(|event| match event {
            baton::adapters::TurnEvent::TurnCompleted { final_text, usage, native_session_id, .. } => {
                Some((final_text.clone(), usage.clone(), native_session_id.clone()))
            }
            _ => None,
        })
        .expect("turn completed event");
    assert_eq!(completed.0, "streamed live", "final text comes from session.next.text.ended");
    assert_eq!(completed.1.unwrap().input_tokens, 15, "usage sums across steps");
    assert_eq!(completed.2.as_deref(), Some("ses_test"));
    assert!(!steer("too late".into()), "steering after completion is rejected");
}
