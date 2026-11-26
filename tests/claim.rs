use baton::claim::{acquire, ClaimOutcome, SystemProbe};
use baton::store::BatonStore;

#[cfg(unix)]
use std::io::{BufRead, BufReader, Read};
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
    assert!(!status.success());
    assert!(stderr.contains("not_the_host"), "{stderr}");
    assert!(!home.path().join("provider-started").exists());
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
