use baton::claim::{acquire, ClaimOutcome, SystemProbe};
use baton::store::BatonStore;

#[cfg(unix)]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::process::{Child, ChildStdin, Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
#[test]
fn a_file_store_claim_is_visible_and_releasable() {
    let dir = tempfile::tempdir().unwrap();
    let host_id = uuid::Uuid::new_v4().to_string();
    let store = BatonStore::open_hosted(&dir.path().join("baton.sqlite"), &host_id).unwrap();
    let claim = match acquire(&store, dir.path(), &host_id, &SystemProbe).unwrap() {
        ClaimOutcome::Claimed(claim) => claim,
        other => panic!("expected claim, got {other:?}"),
    };
    assert_eq!(claim.host_id, host_id);
    assert_eq!(store.get_host_claim().unwrap().as_ref(), Some(&claim));
    assert!(store.release_host_claim(&host_id).unwrap());
    assert!(store.get_host_claim().unwrap().is_none());
}

#[cfg(unix)]
struct HostProcess {
    child: Child,
    _stdin: ChildStdin,
}

#[cfg(unix)]
struct ChildGuard(Child);

#[cfg(unix)]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
impl Drop for HostProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
fn start_host(home: &std::path::Path) -> (HostProcess, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["serve", "--stdio"])
        .env("BATON_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start host");
    let stdin = child.stdin.take().expect("host stdin");
    let stderr = child.stderr.take().expect("host stderr");
    let mut reader = BufReader::new(stderr);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).expect("read host stderr");
        assert_ne!(read, 0, "host exited before readiness");
        if line.contains("host ready") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "host did not become ready: {line}"
        );
    }
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    let installation = tokens
        .iter()
        .position(|token| *token == "installation")
        .expect("installation token");
    let host_id = tokens[installation - 1].to_string();
    uuid::Uuid::parse_str(&host_id).expect("ready line names a host UUID");
    (
        HostProcess {
            child,
            _stdin: stdin,
        },
        host_id,
    )
}

#[cfg(unix)]
fn fake_codex(home: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(
        &codex,
        "#!/bin/sh\nprintf started > \"$BATON_HOME/provider-started\"\nif [ -n \"${BATON_HOST_ID:-}\" ]; then printf leaked > \"$BATON_HOME/host-id-leaked\"; fi\n/bin/sleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[cfg(unix)]
fn hosted_child(home: &std::path::Path, bin: &std::path::Path, host_id: Option<&str>) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_baton"));
    if let Some(host_id) = host_id {
        command.arg("--host-id").arg(host_id);
    }
    command
        .args(["-p", "codex", "-m", "test", "exec", "hold"])
        .env("BATON_HOME", home)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start child")
}

#[cfg(unix)]
fn run_with_stdin(home: &std::path::Path, args: &[&str], input: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(args)
        .env("BATON_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start command");
    child
        .stdin
        .take()
        .expect("command stdin")
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[cfg(unix)]
fn wait_for_file(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "missing {}", path.display());
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
#[test]
fn a_host_child_writes_under_the_host_identity() {
    let home = tempfile::tempdir().unwrap();
    let (host, host_id) = start_host(home.path());
    let bin = fake_codex(home.path());
    let mut child = hosted_child(home.path(), &bin, Some(&host_id));
    wait_for_file(&home.path().join("provider-started"));
    assert!(
        !home.path().join("host-id-leaked").exists(),
        "host authority must not enter the provider environment"
    );
    let store = BatonStore::open_read_only(&home.path().join("baton.sqlite")).unwrap();
    assert_eq!(store.list_threads(None).unwrap().len(), 1);
    child.kill().unwrap();
    let _ = child.wait();
    drop(host);
}

#[cfg(unix)]
#[test]
fn a_child_without_the_flag_is_refused_while_the_host_is_live() {
    let home = tempfile::tempdir().unwrap();
    let (host, _host_id) = start_host(home.path());
    let bin = fake_codex(home.path());
    let mut child = hosted_child(home.path(), &bin, None);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "plain child did not refuse");
        std::thread::sleep(Duration::from_millis(25));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("child stderr")
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(status.code(), Some(4), "{stderr}");
    assert!(stderr.contains("owns this store"), "{stderr}");
    assert!(!home.path().join("provider-started").exists());
    drop(host);
}

#[cfg(unix)]
#[test]
fn a_plain_client_exits_four_and_names_the_owner() {
    let home = tempfile::tempdir().unwrap();
    let (host, _host_id) = start_host(home.path());
    for args in [["threads"].as_slice(), ["mcp"].as_slice()] {
        let output = Command::new(env!("CARGO_BIN_EXE_baton"))
            .args(args)
            .env("BATON_HOME", home.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(4), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("baton: pid {} started ", host.child.id())),
            "{stderr}"
        );
        assert!(stderr.contains("owns this store"), "{stderr}");
        assert!(
            stderr.contains(&format!("baton host takeover --pid {}", host.child.id())),
            "{stderr}"
        );
    }
    drop(host);
}

#[cfg(unix)]
#[test]
fn host_takeover_replaces_the_live_claim_and_allows_a_new_host() {
    let home = tempfile::tempdir().unwrap();
    let (host, _host_id) = start_host(home.path());
    let pid = host.child.id().to_string();
    let output = run_with_stdin(home.path(), &["host", "takeover", "--pid", &pid], "y\n");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("took over from pid {pid}")),
        "{stdout}"
    );
    assert!(stdout.contains("settled 0 turn(s)"), "{stdout}");
    let store = BatonStore::open_read_only(&home.path().join("baton.sqlite")).unwrap();
    assert!(store.get_host_claim().unwrap().is_none());

    let threads = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("threads")
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(threads.status.success(), "{threads:?}");

    let (replacement, _host_id) = start_host(home.path());
    drop(replacement);
    drop(host);
}

#[cfg(unix)]
#[test]
fn host_takeover_stops_the_old_provider_generation() {
    let home = tempfile::tempdir().unwrap();
    let (host, host_id) = start_host(home.path());
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
            host.child.id(),
            baton::process::process_start(host.child.id()),
            host_id,
            chrono::Utc::now().to_rfc3339()
        ],
    )
    .unwrap();
    drop(db);

    let pid = host.child.id().to_string();
    let output = run_with_stdin(home.path(), &["host", "takeover", "--pid", &pid], "y\n");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("stopped 1 provider child(ren) from the old host"),
        "{stdout}"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if provider.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "takeover did not stop the old provider child"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    drop(host);
}

#[cfg(unix)]
#[test]
fn host_takeover_requires_confirmation_and_the_exact_pid() {
    let home = tempfile::tempdir().unwrap();
    let (host, host_id) = start_host(home.path());
    let pid = host.child.id();
    let wrong = pid.saturating_add(1).to_string();
    let refused = run_with_stdin(home.path(), &["host", "takeover", "--pid", &wrong], "y\n");
    assert!(!refused.status.success(), "{refused:?}");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains(&format!("belongs to pid {pid}")),
        "{stderr}"
    );

    let cancelled = run_with_stdin(
        home.path(),
        &["host", "takeover", "--pid", &pid.to_string()],
        "n\n",
    );
    assert!(!cancelled.status.success(), "{cancelled:?}");
    let stderr = String::from_utf8_lossy(&cancelled.stderr);
    assert!(stderr.contains("takeover cancelled"), "{stderr}");

    let store = BatonStore::open_read_only(&home.path().join("baton.sqlite")).unwrap();
    let claim = store.get_host_claim().unwrap().expect("claim survives");
    assert_eq!(claim.pid, pid);
    assert_eq!(claim.host_id, host_id);
    drop(host);
}

#[cfg(unix)]
#[test]
fn a_temp_home_child_writes_freely_without_a_claim() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("baton.sqlite");
    let store = BatonStore::open(&path).unwrap();
    store
        .create_thread(baton::store::NewThread {
            title: "temp home".to_string(),
            cwd: home.path().to_string_lossy().to_string(),
            current_provider: baton::types::ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    assert_eq!(store.list_threads(None).unwrap().len(), 1);
}
