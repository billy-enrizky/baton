#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::sync::mpsc;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
fn turn_event(thread_id: &str) -> baton::store::NewAuditEvent {
    baton::audit::event(
        baton::audit::HOST_ENGINE_ACTOR,
        "installation",
        Some(thread_id),
        Some(thread_id),
        baton::audit::AuditAction::TurnSubmit,
        &serde_json::json!({"thread_id": thread_id}),
    )
    .unwrap()
}

#[cfg(unix)]
struct ChildGuard(std::process::Child);

#[cfg(unix)]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
#[test]
fn serve_claims_once_and_releases_on_sigterm() {
    let home = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_baton");
    let mut child = Command::new(binary)
        .arg("serve")
        .env("BATON_HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("serve: {line}");
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut ready = false;
    while Instant::now() < deadline {
        if let Ok(line) = receiver.recv_timeout(Duration::from_millis(100)) {
            if line.contains("host ready") {
                ready = true;
                break;
            }
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
    }
    assert!(ready, "serve did not report readiness");

    let second = Command::new(binary)
        .arg("serve")
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(3), "{second:?}");
    let message = String::from_utf8_lossy(&second.stderr);
    assert!(message.contains("already claimed"), "{message}");

    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("serve did not stop after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
    let store = baton::store::BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
    assert!(store.get_host_claim().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn serve_stdio_stops_on_stdin_eof() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["serve", "--stdio"])
        .env("BATON_HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("host ready"), "{stderr}");
    assert!(stderr.contains("host stopped"), "{stderr}");
    let store = baton::store::BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
    assert!(store.get_host_claim().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn serve_stdio_protocol_round_trips_and_releases_on_eof() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["serve", "--stdio"])
        .env("BATON_HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut stderr_reader = BufReader::new(stderr);
    let mut ready = String::new();
    stderr_reader.read_line(&mut ready).unwrap();
    assert!(ready.contains("host ready"), "{ready}");
    let stdout_reader = BufReader::new(stdout);
    let mut responses = stdout_reader.lines();
    let mut stdin = stdin;

    writeln!(
        stdin,
        r#"{{"v":1,"kind":"request","id":1,"command":"ping"}}"#
    )
    .unwrap();
    writeln!(
        stdin,
        r#"{{"v":1,"kind":"request","id":2,"command":"status","args":{{"thread":null}}}}"#
    )
    .unwrap();
    writeln!(
        stdin,
        r#"{{"v":1,"kind":"request","id":3,"command":"claim-info"}}"#
    )
    .unwrap();
    writeln!(stdin, "not-json").unwrap();
    writeln!(
        stdin,
        r#"{{"v":2,"kind":"request","id":4,"command":"ping"}}"#
    )
    .unwrap();
    writeln!(
        stdin,
        r#"{{"v":1,"kind":"request","id":5,"command":"exec"}}"#
    )
    .unwrap();
    stdin.flush().unwrap();

    let read = |lines: &mut std::io::Lines<BufReader<std::process::ChildStdout>>| {
        let line = lines.next().unwrap().unwrap();
        serde_json::from_str::<serde_json::Value>(&line).unwrap()
    };
    let ping = read(&mut responses);
    assert_eq!(ping["id"], 1);
    assert_eq!(ping["result"]["pong"], true);
    let status = read(&mut responses);
    assert_eq!(status["result"]["scope"], "host");
    assert_eq!(status["result"]["threads"], 0);
    let claim = read(&mut responses);
    assert_eq!(claim["id"], 3);
    assert!(claim["result"]["host_id"].as_str().is_some());
    let malformed = read(&mut responses);
    assert_eq!(malformed["id"], serde_json::Value::Null);
    assert_eq!(malformed["error"]["code"], "malformed_line");
    let version = read(&mut responses);
    assert_eq!(version["id"], serde_json::Value::Null);
    assert_eq!(version["error"]["code"], "bad_version");
    let unknown = read(&mut responses);
    assert_eq!(unknown["id"], 5);
    assert_eq!(unknown["error"]["code"], "not_implemented");

    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "serve did not stop after EOF");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(status.success(), "{status:?}");
    let store = baton::store::BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
    assert!(store.get_host_claim().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn serve_shutdown_stops_owned_provider_children() {
    let home = tempfile::tempdir().unwrap();
    let mut host = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["serve", "--stdio"])
        .env("BATON_HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = host.stdin.take().unwrap();
    let stderr = host.stderr.take().unwrap();
    let mut stderr_reader = BufReader::new(stderr);
    let mut ready = String::new();
    stderr_reader.read_line(&mut ready).unwrap();
    assert!(ready.contains("host ready"), "{ready}");
    let tokens = ready.split_whitespace().collect::<Vec<_>>();
    let installation = tokens
        .iter()
        .position(|token| *token == "installation")
        .expect("installation token");
    let host_id = tokens[installation - 1];
    let host_pid = host.id();
    let owner_start = baton::process::process_start(host_pid);

    let mut provider = ChildGuard(
        Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn provider"),
    );
    let provider_pid = provider.0.id();
    let command = baton::process::command_line(provider_pid).unwrap();
    let start = baton::process::process_start(provider_pid).unwrap();
    let db = rusqlite::Connection::open(home.path().join("baton.sqlite")).unwrap();
    db.execute(
        "INSERT INTO provider_processes (pid, provider, port, command, process_start, cwd, owner_pid, owner_start, host_id, started_at) VALUES (?, 'codex', NULL, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            provider_pid,
            command,
            start,
            "/tmp/project",
            host_pid,
            owner_start,
            host_id,
            chrono::Utc::now().to_rfc3339()
        ],
    )
    .unwrap();
    drop(db);

    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = host.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "serve did not stop after EOF");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(status.success(), "{status:?}");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if provider.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "host shutdown did not stop its provider child"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let store = baton::store::BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
    assert!(store.list_provider_processes().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn serve_shutdown_settles_work_from_stopped_owners() {
    use baton::store::{BatonStore, NewThread};
    use baton::types::{ProviderId, TurnPhase};

    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("baton.sqlite");
    let mut owner = ChildGuard(Command::new("sleep").arg("60").spawn().unwrap());
    let owner_pid = owner.0.id();
    let owner_start = baton::process::process_start(owner_pid);

    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .args(["serve", "--stdio"])
            .env("BATON_HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stdin = child.0.stdin.take().unwrap();
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = String::new();
    let mut ready = false;
    while Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                output.push_str(&line);
                output.push('\n');
                if line.contains("host ready") {
                    ready = true;
                    break;
                }
            }
            Err(_) if child.0.try_wait().unwrap().is_some() => break,
            Err(_) => {}
        }
    }
    assert!(ready, "serve did not report readiness: {output}");

    // A trusted child writes through the host generation it received on the
    // command line, so the fixture takes the same host id from claim-info.
    writeln!(
        stdin,
        r#"{{"v":1,"kind":"request","id":1,"command":"claim-info"}}"#
    )
    .unwrap();
    let mut responses = BufReader::new(stdout).lines();
    let claim: serde_json::Value =
        serde_json::from_str(&responses.next().unwrap().unwrap()).unwrap();
    let host_id = claim["result"]["host_id"]
        .as_str()
        .expect("claim-info names the host")
        .to_string();
    let host_pid = claim["result"]["pid"]
        .as_u64()
        .expect("claim-info names the host pid") as u32;
    let host_start = claim["result"]["process_start"]
        .as_str()
        .expect("claim-info names the host start")
        .to_string();

    let (child_thread_id, host_thread_id) = {
        let store = BatonStore::open_hosted(&db, &host_id).unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "child work".into(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn_with_audit(
                &thread.id,
                ProviderId::Codex,
                None,
                "child work",
                false,
                &turn_event(&thread.id),
            )
            .unwrap();
        store
            .create_orchestration_run_owned(
                "run-child",
                "goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
                Some(owner_pid),
                owner_start.as_deref(),
            )
            .unwrap();
        store
            .add_orchestration_task(
                "task-1",
                "run-child",
                1,
                "task",
                ProviderId::Codex,
                "model",
                "prompt",
                None,
                None,
                None,
                None,
                "queued",
                None,
                None,
                "2026-11-26T00:00:00.000Z",
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let host_thread = store
            .create_thread(NewThread {
                title: "host work".into(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn_with_audit(
                &host_thread.id,
                ProviderId::Codex,
                None,
                "host work",
                false,
                &turn_event(&host_thread.id),
            )
            .unwrap();
        store
            .create_orchestration_run_owned(
                "run-host",
                "host goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
                Some(host_pid),
                Some(&host_start),
            )
            .unwrap();
        store
            .add_orchestration_task(
                "task-host",
                "run-host",
                1,
                "host task",
                ProviderId::Codex,
                "model",
                "prompt",
                Some("/tmp/host-worktree"),
                None,
                None,
                Some(&host_thread.id),
                "queued",
                None,
                None,
                "2026-11-26T00:00:00.000Z",
                None,
                None,
                None,
                None,
            )
            .unwrap();
        (thread.id, host_thread.id)
    };
    {
        // The child turn belongs to the helper so startup recovery left it
        // alone. The second turn belongs to the host, so D-5 cancels it.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE turns SET owner_pid = ?1, owner_start = ?2 WHERE thread_id = ?3",
            rusqlite::params![owner_pid, owner_start, child_thread_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE turns SET owner_pid = ?1, owner_start = ?2 WHERE thread_id = ?3",
            rusqlite::params![host_pid, host_start, host_thread_id],
        )
        .unwrap();
    }

    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.0.kill();
            panic!("serve did not stop after stdin EOF");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
    while let Ok(line) = receiver.recv_timeout(Duration::from_millis(100)) {
        output.push_str(&line);
        output.push('\n');
    }
    assert!(
        output.contains("settled 1 unfinished turn(s) on shutdown"),
        "{output}"
    );
    assert!(
        output.contains("cancelled 1 host turn(s), 1 task(s), and 1 run(s)"),
        "{output}"
    );
    assert!(output.contains("left 1 worktree(s) in place"), "{output}");
    assert!(output.contains("/tmp/host-worktree"), "{output}");

    let store = BatonStore::open_read_only(&db).unwrap();
    let child_turn = store.get_turn_record(&child_thread_id, 1).unwrap().unwrap();
    assert_eq!(child_turn.phase, TurnPhase::Unverified);
    assert!(
        child_turn.error.as_deref().unwrap_or("").contains("exited"),
        "{:?}",
        child_turn.error
    );
    let host_turn = store.get_turn_record(&host_thread_id, 1).unwrap().unwrap();
    assert_eq!(host_turn.phase, TurnPhase::Unverified);
    assert_eq!(host_turn.error.as_deref(), Some("host shut down"));
    let child_run = store
        .find_orchestration_runs("run-child")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(child_run.status, "failed");
    let host_run = store
        .find_orchestration_runs("run-host")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(host_run.status, "cancelled");
    let host_tasks = store.list_orchestration_tasks("run-host").unwrap();
    assert_eq!(host_tasks[0].status, "cancelled");
    assert_eq!(host_tasks[0].error.as_deref(), Some("host shut down"));
    assert!(store.get_host_claim().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn serve_socket_submit_runs_a_turn_through_the_host_engine() {
    use baton::store::BatonStore;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;

    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("fake-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let harness = bin.join("codex");
    std::fs::write(&harness, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let thread_id = {
        let store = BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
        let thread = store
            .create_thread(baton::store::NewThread {
                title: "remote submit".into(),
                cwd: root.path().to_string_lossy().to_string(),
                current_provider: baton::types::ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        thread.id
    };

    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("serve")
            .current_dir(root.path())
            .env("BATON_HOME", home.path())
            .env("PATH", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stdin = child.0.stdin.take().unwrap();
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let (out_sender, out_receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if out_sender.send(line).is_err() {
                break;
            }
        }
    });
    let (err_sender, err_receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if err_sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut output = String::new();
    let mut ready = false;
    while Instant::now() < deadline {
        match err_receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                output.push_str(&line);
                output.push('\n');
                if line.contains("host ready") {
                    ready = true;
                    break;
                }
            }
            Err(_) if child.0.try_wait().unwrap().is_some() => break,
            Err(_) => {}
        }
    }
    assert!(ready, "serve did not report readiness: {output}");

    writeln!(stdin, "pair --scope read --scope operate").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut url = String::new();
    while Instant::now() < deadline {
        match out_receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.starts_with("baton://local#") => {
                url = line;
                break;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(!url.is_empty(), "pair did not print a URL: {output}");
    let fragment = url.trim().strip_prefix("baton://local#").unwrap();
    let mut grant_id = None;
    let mut secret = None;
    for field in fragment.split('&') {
        let (key, value) = field.split_once('=').expect("key=value");
        match key {
            "grant" => grant_id = Some(value.to_string()),
            "secret" => secret = Some(value.to_string()),
            _ => {}
        }
    }
    let grant_id = grant_id.expect("grant id in the pairing URL");
    let secret = secret.expect("secret in the pairing URL");

    let socket = home.path().join("serve.sock");
    let mut stream = UnixStream::connect(&socket).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut next_id = 1u64;
    let mut ask = |value: serde_json::Value| -> serde_json::Value {
        let mut line = serde_json::to_string(&value).unwrap();
        line.push('\n');
        stream.write_all(line.as_bytes()).unwrap();
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    };
    let submit = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "attach",
            "--url",
            &url,
            "--thread",
            &thread_id,
            "--submit",
            "remote hello",
            "--idempotency-key",
            "remote-submit-1",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(submit.status.success(), "{submit:?}");
    let submit: serde_json::Value = serde_json::from_slice(&submit.stdout).unwrap();
    assert_eq!(submit["thread_id"], thread_id);
    assert_eq!(submit["accepted"], true);
    assert_eq!(submit["replayed"], false);

    let replay = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "attach",
            "--url",
            &url,
            "--thread",
            &thread_id,
            "--submit",
            "remote hello",
            "--idempotency-key",
            "remote-submit-1",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(replay.status.success(), "{replay:?}");
    let replay: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay["thread_id"], thread_id);
    assert_eq!(replay["accepted"], true);
    assert_eq!(replay["replayed"], true);

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut saw_message = false;
    while Instant::now() < deadline {
        let page = ask(serde_json::json!({
            "v":1,
            "kind":"request",
            "id": next_id,
            "command":"messages",
            "grant_id": grant_id,
            "secret": secret,
            "args": {"thread": thread_id, "after": 0, "limit": 50},
        }));
        next_id += 1;
        let messages = page["result"]["messages"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if messages
            .iter()
            .any(|message| message["text"] == "remote hello")
        {
            saw_message = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut tail = String::new();
    while let Ok(line) = err_receiver.try_recv() {
        tail.push_str(&line);
        tail.push('\n');
    }
    let state = BatonStore::open_read_only(&home.path().join("baton.sqlite"))
        .map(|store| {
            format!(
                "messages={:?} turn={:?} events={:?}",
                store
                    .list_messages(&thread_id)
                    .map(|rows| rows.iter().map(|row| row.text.clone()).collect::<Vec<_>>()),
                store
                    .get_turn_record(&thread_id, 1)
                    .map(|row| row.map(|row| row.phase)),
                store.list_audit_events(10).map(|rows| rows
                    .iter()
                    .map(|row| row.action.clone())
                    .collect::<Vec<_>>()),
            )
        })
        .unwrap_or_else(|error| format!("store read failed: {error}"));
    assert!(
        saw_message,
        "the engine did not run the remote turn: {output}\n{tail}\n{state}"
    );
    let verified = BatonStore::open_read_only(&home.path().join("baton.sqlite")).unwrap();
    let messages = verified.list_messages(&thread_id).unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|row| row.text == "remote hello")
            .count(),
        1,
        "a replay must not queue a second user message"
    );
    assert_eq!(
        verified
            .list_audit_events(10)
            .unwrap()
            .iter()
            .filter(|event| event.action == "turn.submit_request")
            .count(),
        1,
        "a replay must not write a second request row"
    );

    let audit = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["audit", "list", "--json", "--limit", "10"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(audit.status.success(), "{audit:?}");
    let audit: serde_json::Value = serde_json::from_slice(&audit.stdout).unwrap();
    let events = audit["events"].as_array().unwrap();
    assert!(
        events.iter().any(|event| {
            event["action"] == "turn.submit_request"
                && event["actor"] == grant_id
                && event["confirmation"] == "socket_grant"
                && event["target"] == thread_id
                && event["scope"] == "operate"
        }),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| {
            event["action"] == "turn.submit"
                && event["actor"] == "host_engine"
                && event["target"] == thread_id
        }),
        "{events:?}"
    );

    unsafe {
        libc::kill(child.0.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.0.kill();
            panic!("serve did not stop after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
    assert!(!socket.exists(), "shutdown removes the socket");
}

#[cfg(unix)]
#[test]
fn serve_socket_answers_ping_and_requires_a_read_grant() {
    use baton::grants::{mint, GrantRequest, Scope};
    use baton::store::BatonStore;
    use std::os::unix::net::UnixStream;

    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("baton.sqlite");
    let grant = {
        let store = BatonStore::open(&db).unwrap();
        mint(&store, home.path(), &GrantRequest::new(vec![Scope::Read])).unwrap()
    };
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("serve")
            .env("BATON_HOME", home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = child.0.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut ready = false;
    let mut output = String::new();
    while Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                output.push_str(&line);
                output.push('\n');
                if line.contains("host ready") {
                    ready = true;
                    break;
                }
            }
            Err(_) if child.0.try_wait().unwrap().is_some() => break,
            Err(_) => {}
        }
    }
    assert!(ready, "serve did not report readiness: {output}");
    assert!(output.contains("serve.sock"), "{output}");

    let socket = home.path().join("serve.sock");
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let mut stream = UnixStream::connect(&socket).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut ask = |value: serde_json::Value| -> serde_json::Value {
        writeln!(stream, "{}", value).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };

    let ping = ask(serde_json::json!({"v":1,"kind":"request","id":1,"command":"ping"}));
    assert_eq!(ping["result"]["pong"], true, "{ping}");

    let bare = ask(serde_json::json!({"v":1,"kind":"request","id":2,"command":"status"}));
    assert_eq!(bare["error"]["code"], "unauthenticated", "{bare}");

    let authenticated = ask(serde_json::json!({
        "v":1,
        "kind":"request",
        "id":3,
        "command":"status",
        "grant_id": grant.summary.grant_id,
        "secret": grant.secret,
    }));
    assert_eq!(authenticated["ok"], true, "{authenticated}");
    assert_eq!(authenticated["result"]["scope"], "host");

    let wrong = ask(serde_json::json!({
        "v":1,
        "kind":"request",
        "id":4,
        "command":"status",
        "grant_id": grant.summary.grant_id,
        "secret":"wrong",
    }));
    assert_eq!(wrong["error"]["code"], "bad_secret", "{wrong}");

    unsafe {
        libc::kill(child.0.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.0.kill();
            panic!("serve did not stop after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
    assert!(!socket.exists(), "shutdown removes the socket");
    let store = BatonStore::open(&db).unwrap();
    assert!(store.get_host_claim().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn serve_socket_refuses_a_regular_file_at_the_path() {
    use baton::store::BatonStore;

    let home = tempfile::tempdir().unwrap();
    let socket = home.path().join("serve.sock");
    std::fs::write(&socket, "keep").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("serve")
        .env("BATON_HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("not a socket"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&socket).unwrap(), "keep");
    let store = BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
    assert!(store.get_host_claim().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn serve_stdin_pair_and_attach_use_the_socket() {
    use baton::store::BatonStore;
    use std::os::unix::net::UnixStream;

    let home = tempfile::tempdir().unwrap();
    let (seeded_thread, first_message, second_message) = {
        let store = BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
        let thread = store
            .create_thread(baton::store::NewThread {
                title: "attach stream".into(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: baton::types::ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let first = store
            .add_message(&thread.id, 1, "user", Some("codex"), None, "first message")
            .unwrap();
        let second = store
            .add_message(
                &thread.id,
                1,
                "assistant",
                Some("codex"),
                None,
                "second message",
            )
            .unwrap();
        (thread.id, first, second)
    };
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("serve")
            .env("BATON_HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stdin = child.0.stdin.take().unwrap();
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let (out_sender, out_receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if out_sender.send(line).is_err() {
                break;
            }
        }
    });
    let (err_sender, err_receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if err_sender.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = String::new();
    let mut ready = false;
    while Instant::now() < deadline {
        match err_receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                output.push_str(&line);
                output.push('\n');
                if line.contains("host ready") {
                    ready = true;
                    break;
                }
            }
            Err(_) if child.0.try_wait().unwrap().is_some() => break,
            Err(_) => {}
        }
    }
    assert!(ready, "serve did not report readiness: {output}");

    writeln!(stdin, "pair --label test-socket --scope read").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut url = String::new();
    while Instant::now() < deadline {
        match out_receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.starts_with("baton://local#") => {
                url = line;
                break;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(!url.is_empty(), "pair did not print a URL: {output}");
    let fragment = url.trim().strip_prefix("baton://local#").unwrap();
    let mut grant_id = None;
    let mut secret = None;
    for field in fragment.split('&') {
        let (key, value) = field.split_once('=').expect("key=value");
        match key {
            "grant" => grant_id = Some(value.to_string()),
            "secret" => secret = Some(value.to_string()),
            _ => {}
        }
    }
    let grant_id = grant_id.expect("grant id in the pairing URL");
    let secret = secret.expect("secret in the pairing URL");

    let socket = home.path().join("serve.sock");
    let mut stream = UnixStream::connect(&socket).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut ask = |value: serde_json::Value| -> serde_json::Value {
        writeln!(stream, "{}", value).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let status = ask(serde_json::json!({
        "v":1,
        "kind":"request",
        "id":1,
        "command":"status",
        "grant_id": grant_id.clone(),
        "secret": secret.clone(),
    }));
    assert_eq!(status["ok"], true, "{status}");

    let audit = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["audit", "list", "--json"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(audit.status.success(), "{audit:?}");
    let audit: serde_json::Value = serde_json::from_slice(&audit.stdout).unwrap();
    assert_eq!(audit["events"].as_array().unwrap().len(), 1);
    assert_eq!(audit["events"][0]["action"], "grant.mint");

    let attached = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["attach", "--url", &url, "--json"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(attached.status.success(), "{attached:?}");
    let attached: serde_json::Value = serde_json::from_slice(&attached.stdout).unwrap();
    assert_eq!(attached["schema_version"], 1);
    assert_eq!(attached["scope"], "host");
    assert_eq!(attached["threads"], 1);
    assert_eq!(attached["running_turns"], 0);

    let attached_text = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["attach", "--url", &url])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    let attached_text_out = String::from_utf8_lossy(&attached_text.stdout);
    assert!(attached_text.status.success(), "{attached_text:?}");
    assert!(
        attached_text_out.contains("scope: host"),
        "{attached_text_out}"
    );
    assert!(
        attached_text_out.contains("threads: 1"),
        "{attached_text_out}"
    );

    let page = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "attach",
            "--url",
            &url,
            "--thread",
            &seeded_thread,
            "--messages",
            "--after",
            "0",
            "--limit",
            "1",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(page.status.success(), "{page:?}");
    let page: serde_json::Value = serde_json::from_slice(&page.stdout).unwrap();
    assert_eq!(page["thread_id"], seeded_thread);
    assert_eq!(page["cursor"], first_message);
    assert_eq!(page["running_turns"], 0);
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["messages"][0]["text"], "first message");

    let page = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "attach",
            "--url",
            &url,
            "--thread",
            &seeded_thread,
            "--messages",
            "--after",
            &first_message.to_string(),
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(page.status.success(), "{page:?}");
    let page: serde_json::Value = serde_json::from_slice(&page.stdout).unwrap();
    assert_eq!(page["cursor"], second_message);
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["messages"][0]["text"], "second message");

    let thread_status = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "attach",
            "--url",
            &url,
            "--thread",
            &seeded_thread,
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(thread_status.status.success(), "{thread_status:?}");
    let thread_status: serde_json::Value = serde_json::from_slice(&thread_status.stdout).unwrap();
    assert_eq!(thread_status["scope"], "thread");
    assert_eq!(thread_status["threads"], 1);

    let missing = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "attach",
            "--url",
            &url,
            "--thread",
            "missing",
            "--messages",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(!missing.status.success(), "{missing:?}");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("not_found"),
        "{missing:?}"
    );

    let missing_url = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("attach")
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(!missing_url.status.success(), "{missing_url:?}");
    assert!(
        String::from_utf8_lossy(&missing_url.stderr).contains("needs --url"),
        "{missing_url:?}"
    );

    let mut bad_url = url.clone();
    let last = bad_url.pop().unwrap();
    bad_url.push(if last == '0' { '1' } else { '0' });
    let refused = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["attach", "--url", &bad_url, "--json"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    let refused_err = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(refused_err.contains("bad_secret"), "{refused_err}");
    assert!(!refused_err.contains(&secret), "{refused_err}");

    writeln!(stdin, "pair --scope nope").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut error = String::new();
    while Instant::now() < deadline {
        match err_receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.contains("unknown scope") => {
                error = line;
                break;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(!error.is_empty(), "a bad pair line reports the error");
    let ping = ask(serde_json::json!({"v":1,"kind":"request","id":2,"command":"ping"}));
    assert_eq!(ping["result"]["pong"], true, "{ping}");

    writeln!(stdin, "renew {grant_id} --days 7").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut renewed = String::new();
    while Instant::now() < deadline {
        match err_receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.contains("renewed") => {
                renewed = line;
                break;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(!renewed.is_empty(), "the host renews a grant: {renewed}");
    assert!(renewed.contains(&grant_id), "{renewed}");

    writeln!(stdin, "renew {grant_id} --days 90").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut extended = String::new();
    while Instant::now() < deadline {
        match err_receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.contains("renewed") => {
                extended = line;
                break;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(
        !extended.is_empty(),
        "the host extends the window: {extended}"
    );

    unsafe {
        libc::kill(child.0.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.0.kill();
            panic!("serve did not stop after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
    assert!(!socket.exists(), "shutdown removes the socket");
    let store = BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
    assert!(store.get_host_claim().unwrap().is_none());
    let grants = baton::grants::list(&store).unwrap();
    assert_eq!(grants.len(), 1, "{grants:?}");
    assert_eq!(grants[0].grant_id, grant_id);
    let expiry = grants[0]
        .expires_at
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    assert!(
        expiry > chrono::Utc::now() + chrono::Duration::days(89),
        "{expiry}"
    );
    let events = store.list_audit_events(10).unwrap();
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0].action, "grant.renew");
    assert_eq!(events[1].action, "grant.renew");
    assert_eq!(events[2].action, "grant.mint");
}
