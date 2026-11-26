#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::sync::mpsc;
#[cfg(unix)]
use std::time::{Duration, Instant};

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
