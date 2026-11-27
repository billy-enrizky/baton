//! Newline-delimited JSON protocol for `baton serve --stdio`.
//!
//! Stdout carries protocol responses only. Diagnostics stay on stderr. P2 is
//! request/response only: no events, resume cursor, or idempotency key.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::net::UnixListener as StdUnixListener;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener as TokioUnixListener, UnixStream};

use crate::store::{BatonStore, HostClaimRow};

pub const PROTOCOL_VERSION: u64 = 1;

/// Stops this host generation's provider children, settles work whose owner
/// stopped, and releases the claim. The server loop result is returned after
/// cleanup, so a loop error cannot leave the store claimed by an exiting
/// process.
pub fn finish_host(store: &BatonStore, host_id: &str, loop_result: Result<()>) -> Result<()> {
    match crate::process::terminate_host_children(store, host_id) {
        Ok(report) => {
            if !report.reaped.is_empty() {
                eprintln!(
                    "baton: stopped {} provider child(ren) for this host",
                    report.reaped.len()
                );
            }
            for error in &report.errors {
                eprintln!("baton: child shutdown: {error}");
            }
        }
        Err(error) => eprintln!("baton: child shutdown failed: {error}"),
    }
    // Children stop first so a provider cannot overwrite a settled turn.
    // A deliberate host stop then cancels the work this host owns with the
    // signed host reason, and reports the worktrees it leaves in place.
    match store.get_host_claim() {
        Ok(Some(claim)) if claim.host_id == host_id => {
            match store.cancel_host_work(claim.pid, Some(&claim.process_start), "host shut down") {
                Ok(settled) => {
                    if settled.turns + settled.tasks + settled.runs.len() > 0 {
                        eprintln!(
                            "baton: cancelled {} host turn(s), {} task(s), and {} run(s)",
                            settled.turns,
                            settled.tasks,
                            settled.runs.len()
                        );
                    }
                    let mut worktrees = Vec::new();
                    for run_id in &settled.runs {
                        match store.list_orchestration_tasks(run_id) {
                            Ok(tasks) => worktrees
                                .extend(tasks.into_iter().filter_map(|task| task.worktree_path)),
                            Err(error) => {
                                eprintln!("baton: worktree report failed for {run_id}: {error}")
                            }
                        }
                    }
                    if !worktrees.is_empty() {
                        eprintln!("baton: left {} worktree(s) in place", worktrees.len());
                        for path in worktrees {
                            eprintln!("baton: worktree {path}");
                        }
                    }
                }
                Err(error) => eprintln!("baton: host work cancellation failed: {error}"),
            }
        }
        Ok(_) => {}
        Err(error) => eprintln!("baton: host claim read failed: {error}"),
    }
    // Then settle work whose owner stopped while this host was running, so a
    // client cannot open the store to a turn or run that only looks live.
    match store.reconcile_running_turns(None) {
        Ok(turns) => {
            if !turns.is_empty() {
                eprintln!(
                    "baton: settled {} unfinished turn(s) on shutdown",
                    turns.len()
                );
            }
        }
        Err(error) => eprintln!("baton: turn shutdown settlement failed: {error}"),
    }
    match crate::orchestrate::reconcile_abandoned_runs(store) {
        Ok(runs) => {
            if !runs.is_empty() {
                eprintln!("baton: settled {} abandoned run(s) on shutdown", runs.len());
            }
        }
        Err(error) => eprintln!("baton: run shutdown settlement failed: {error}"),
    }
    if !store.release_host_claim(host_id)? {
        anyhow::bail!("host claim disappeared before shutdown");
    }
    loop_result
}

#[cfg(unix)]
pub fn socket_path(home: &Path) -> PathBuf {
    home.join("serve.sock")
}

/// Binds the host socket. A stale socket is replaced; any other kind of file
/// at the path is refused, so the host never deletes something it did not
/// create.
#[cfg(unix)]
pub fn bind_socket(path: &Path) -> Result<StdUnixListener> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        use std::os::unix::fs::FileTypeExt;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
            anyhow::bail!(
                "refusing to replace {}: the path is not a socket",
                path.display()
            );
        }
        std::fs::remove_file(path)
            .with_context(|| format!("remove stale socket {}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        crate::claim::create_private_dir(parent)?;
    }
    let listener =
        StdUnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    listener.set_nonblocking(true)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("set mode on {}", path.display()))?;
    }
    Ok(listener)
}

#[cfg(not(unix))]
pub fn socket_path(home: &Path) -> PathBuf {
    home.join("serve.sock")
}

#[cfg(not(unix))]
pub fn bind_socket(_path: &Path) -> Result<()> {
    anyhow::bail!("the host socket is Unix-only")
}

/// Accepts same-user clients until a signal arrives. Every client task is
/// aborted before the socket file is removed, so the store lock can be
/// released and shutdown cannot race a request.
#[cfg(unix)]
pub async fn socket_loop(
    listener: StdUnixListener,
    store: Arc<Mutex<BatonStore>>,
    home: &Path,
    path: &Path,
) -> Result<()> {
    let listener = TokioUnixListener::from_std(listener).context("register the host socket")?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    // The host terminal is the pairing surface. A daemonized host sees EOF
    // immediately and keeps serving with no host commands.
    let (host_sender, mut host_receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if host_sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut stdin_open = true;
    let mut clients = Vec::new();
    let result = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break Ok(()),
            _ = terminate.recv() => break Ok(()),
            line = host_receiver.recv(), if stdin_open => {
                match line {
                    Some(line) => handle_host_command(&store, home, &line),
                    None => stdin_open = false,
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _address)) => {
                        let store = store.clone();
                        clients.push(tokio::spawn(async move {
                            if let Err(error) = serve_socket_client(store, stream).await {
                                eprintln!("baton: socket client ended: {error}");
                            }
                        }));
                    }
                    Err(error) => break Err(error).context("accept on the host socket"),
                }
            }
        }
    };
    for client in &clients {
        client.abort();
    }
    for client in clients {
        let _ = client.await;
    }
    drop(listener);
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!("baton: socket cleanup: {error}"),
    }
    result
}

#[cfg(not(unix))]
pub async fn socket_loop(
    _listener: (),
    _store: Arc<Mutex<BatonStore>>,
    _home: &Path,
    _path: &Path,
) -> Result<()> {
    anyhow::bail!("the host socket is Unix-only")
}

#[cfg(unix)]
fn handle_host_command(store: &Arc<Mutex<BatonStore>>, home: &Path, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut tokens = trimmed.split_whitespace();
    let command = tokens.next().unwrap_or_default();
    match command {
        "help" => eprintln!(
            "baton: host commands: pair [--label <name>] [--scope <s>]... [--expires-days N]; renew <id> [--days N]; help"
        ),
        "pair" => match parse_pair(tokens.collect()) {
            Ok(request) => {
                let minted = {
                    let store = store.lock().unwrap();
                    crate::grants::mint(&store, home, &request)
                };
                match minted {
                    Ok(minted) => {
                        println!("{}", pair_url(&minted));
                        eprintln!(
                            "baton: paired {} with {} scopes, expires {}",
                            minted.summary.grant_id,
                            minted.summary.scopes.join(","),
                            minted.summary.expires_at
                        );
                    }
                    Err(error) => eprintln!("baton: pair failed: {error}"),
                }
            }
            Err(error) => eprintln!("baton: {error}"),
        },
        "renew" => match parse_renew(tokens.collect()) {
            Ok((grant_id, days)) => {
                let renewed = {
                    let store = store.lock().unwrap();
                    crate::grants::renew(&store, &grant_id, days)
                };
                match renewed {
                    Ok(crate::grants::RenewOutcome::Renewed(payload)) => eprintln!(
                        "baton: renewed {} until {} (was {})",
                        payload.grant.grant_id, payload.grant.expires_at, payload.previous_expires_at
                    ),
                    Ok(crate::grants::RenewOutcome::Missing) => {
                        eprintln!("baton: no grant matches {grant_id}")
                    }
                    Ok(crate::grants::RenewOutcome::Revoked) => {
                        eprintln!("baton: {grant_id} is revoked; pair the client again")
                    }
                    Err(error) => eprintln!("baton: renew failed: {error}"),
                }
            }
            Err(error) => eprintln!("baton: {error}"),
        },
        other => eprintln!("baton: unknown host command {other}; type help"),
    }
}

#[cfg(unix)]
fn parse_renew(args: Vec<&str>) -> Result<(String, i64)> {
    let mut grant_id = None;
    let mut days = crate::grants::DEFAULT_GRANT_DAYS;
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            "--days" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--days needs a number"))?;
                days = value
                    .parse::<i64>()
                    .map_err(|_| anyhow::anyhow!("--days needs a number"))?;
            }
            other if other.starts_with("--") => {
                anyhow::bail!("unknown renew option {other}")
            }
            other => {
                if grant_id.replace(other.to_string()).is_some() {
                    anyhow::bail!("renew takes exactly one grant id");
                }
            }
        }
        index += 1;
    }
    let grant_id = grant_id.ok_or_else(|| anyhow::anyhow!("renew needs a grant id"))?;
    Ok((grant_id, days))
}

#[cfg(unix)]
fn parse_pair(args: Vec<&str>) -> Result<crate::grants::GrantRequest> {
    use crate::grants::{expiry_after_days, GrantRequest, Scope, DEFAULT_GRANT_DAYS};

    let mut scopes = vec![Scope::Read, Scope::Operate];
    let mut scopes_set = false;
    let mut label = None;
    let mut expires_days = DEFAULT_GRANT_DAYS;
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            "--label" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--label needs a name"))?;
                label = Some((*value).to_string());
            }
            "--scope" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--scope needs a scope"))?;
                let scope =
                    Scope::parse(value).ok_or_else(|| anyhow::anyhow!("unknown scope {value}"))?;
                if !scopes_set {
                    scopes.clear();
                    scopes_set = true;
                }
                if !scopes.contains(&scope) {
                    scopes.push(scope);
                }
            }
            "--expires-days" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--expires-days needs a number"))?;
                expires_days = value
                    .parse::<i64>()
                    .map_err(|_| anyhow::anyhow!("--expires-days needs a number"))?;
                if expires_days <= 0 {
                    anyhow::bail!("--expires-days must be positive");
                }
            }
            other => anyhow::bail!("unknown pair option {other}"),
        }
        index += 1;
    }
    let mut request = GrantRequest::new(scopes);
    request.label = label;
    request.expires_at = expiry_after_days(expires_days);
    Ok(request)
}

#[cfg(unix)]
fn pair_url(minted: &crate::grants::MintedGrant) -> String {
    format!(
        "baton://local#grant={}&secret={}",
        minted.summary.grant_id, minted.secret
    )
}

#[cfg(unix)]
async fn serve_socket_client(store: Arc<Mutex<BatonStore>>, stream: UnixStream) -> Result<()> {
    let peer = socket_peer_uid(&stream)?;
    let host = unsafe { libc::getuid() };
    if !same_user(peer, host) {
        let response = error_response(
            Value::Null,
            "peer_uid",
            "the client does not run as the host user",
        );
        let mut stream = stream;
        let mut text = serde_json::to_string(&response)?;
        text.push('\n');
        let _ = stream.write_all(text.as_bytes()).await;
        return Ok(());
    }
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let response = {
            let store = store.lock().unwrap();
            handle_socket_line(&store, &line)
        };
        let mut text = serde_json::to_string(&response)?;
        text.push('\n');
        write.write_all(text.as_bytes()).await?;
    }
    Ok(())
}

/// Socket requests carry a grant id and secret. `ping` is the only request
/// answered without one, so a readiness probe needs no credential.
fn handle_socket_line(store: &BatonStore, line: &str) -> Value {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return error_response(Value::Null, "malformed_line", "line is not JSON");
    };
    if value["v"].as_u64() != Some(PROTOCOL_VERSION) {
        return error_response(Value::Null, "bad_version", "unsupported protocol version");
    }
    let id = value["id"].clone();
    let command = value["command"].as_str().unwrap_or_default();
    if command != "ping" {
        let (Some(grant_id), Some(secret)) = (value["grant_id"].as_str(), value["secret"].as_str())
        else {
            return error_response(id, "unauthenticated", "grant_id and secret are required");
        };
        match crate::grants::verify(store, grant_id, secret) {
            Ok(grant) => {
                if !grant.scopes.iter().any(|scope| scope == "read") {
                    return error_response(id, "not_permitted", "the grant lacks the read scope");
                }
            }
            Err(error) => {
                if let Some(refusal) = error.downcast_ref::<crate::grants::GrantRefusal>() {
                    return error_response(id, refusal.code(), refusal.to_string());
                }
                return error_response(id, "internal", error.to_string());
            }
        }
    }
    handle_line(store, line).unwrap_or_else(|| {
        error_response(
            id,
            "not_implemented",
            format!("command {command} is not implemented"),
        )
    })
}

#[cfg(unix)]
fn same_user(peer_uid: u32, host_uid: u32) -> bool {
    peer_uid == host_uid
}

#[cfg(target_os = "linux")]
fn socket_peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    use std::os::unix::io::AsRawFd;
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut libc::ucred as *mut libc::c_void,
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

#[cfg(target_os = "macos")]
fn socket_peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    use std::os::unix::io::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(uid)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn socket_peer_uid(_stream: &UnixStream) -> std::io::Result<u32> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "peer credentials are unavailable on this platform",
    ))
}

fn response(id: Value, result: Value) -> Value {
    json!({
        "v": PROTOCOL_VERSION,
        "kind": "response",
        "id": id,
        "ok": true,
        "result": result,
    })
}

fn error_response(id: Value, code: &str, message: impl Into<String>) -> Value {
    json!({
        "v": PROTOCOL_VERSION,
        "kind": "response",
        "id": id,
        "ok": false,
        "error": {
            "code": code,
            "message": message.into(),
        },
    })
}

fn claim_json(claim: &HostClaimRow) -> Value {
    json!({
        "host_id": claim.host_id,
        "kind": claim.kind,
        "installation_id": claim.installation_id,
        "pid": claim.pid,
        "process_start": claim.process_start,
        "claimed_at": claim.claimed_at,
        "heartbeat_at": claim.heartbeat_at,
    })
}

fn status(store: &BatonStore, args: Option<&Value>) -> Result<Value> {
    let thread = match args {
        None | Some(Value::Null) => None,
        Some(Value::Object(args)) => match args.get("thread") {
            None | Some(Value::Null) => None,
            Some(Value::String(thread)) => Some(thread.as_str()),
            Some(_) => anyhow::bail!("status args.thread must be a string or null"),
        },
        Some(_) => anyhow::bail!("status args must be an object"),
    };
    match thread {
        Some(thread) => {
            if store.get_thread(thread)?.is_none() {
                return Ok(error_response(
                    Value::Null,
                    "not_found",
                    format!("no thread matches {thread}"),
                ));
            }
            Ok(response(
                Value::Null,
                json!({
                    "scope": "thread",
                    "threads": 1,
                    "running_turns": store.count_open_turns(Some(thread))?,
                }),
            ))
        }
        None => Ok(response(
            Value::Null,
            json!({
                "scope": "host",
                "threads": store.list_threads(None)?.len(),
                "running_turns": store.count_open_turns(None)?,
            }),
        )),
    }
}

/// Handles one protocol line. `None` means the line produced no response.
pub fn handle_line(store: &BatonStore, line: &str) -> Option<Value> {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => {
            return Some(error_response(
                Value::Null,
                "malformed_line",
                format!("invalid JSON: {error}"),
            ))
        }
    };
    let version = value.get("v").and_then(Value::as_u64);
    if version != Some(PROTOCOL_VERSION) {
        return Some(error_response(
            Value::Null,
            "bad_version",
            format!("expected protocol version {PROTOCOL_VERSION}"),
        ));
    }
    let object = match value.as_object() {
        Some(object) => object,
        None => {
            return Some(error_response(
                Value::Null,
                "malformed_line",
                "request must be a JSON object",
            ))
        }
    };
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    if object.get("kind").and_then(Value::as_str) != Some("request") {
        return Some(error_response(
            id,
            "not_implemented",
            "only request messages are supported",
        ));
    }
    let Some(command) = object.get("command").and_then(Value::as_str) else {
        return Some(error_response(
            id,
            "malformed_line",
            "request.command is required",
        ));
    };
    let result = match command {
        "ping" => Some(response(id.clone(), json!({ "pong": true }))),
        "claim-info" => match store.get_host_claim() {
            Ok(claim) => Some(response(
                id.clone(),
                claim.map(|claim| claim_json(&claim)).unwrap_or(Value::Null),
            )),
            Err(error) => Some(error_response(id.clone(), "internal", error.to_string())),
        },
        "status" => match status(store, object.get("args")) {
            Ok(mut value) => {
                value["id"] = id.clone();
                Some(value)
            }
            Err(error) => Some(error_response(id.clone(), "internal", error.to_string())),
        },
        _ => Some(error_response(
            id.clone(),
            "not_implemented",
            format!("command {command} is not implemented"),
        )),
    };
    result
}

pub async fn stdio_loop(store: &BatonStore) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        let next = async { lines.next_line().await };
        #[cfg(unix)]
        let line = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
            line = next => line,
        };
        #[cfg(not(unix))]
        let line = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            line = next => line,
        };
        let line = match line {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                eprintln!("baton: protocol read failed: {error}");
                break;
            }
        };
        if let Some(value) = handle_line(store, &line) {
            let bytes = format!("{}\n", serde_json::to_string(&value)?);
            if stdout.write_all(bytes.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim::{acquire, ClaimOutcome, SystemProbe};
    use crate::store::NewThread;
    use crate::types::ProviderId;

    #[cfg(unix)]
    fn claimed_store() -> (tempfile::TempDir, BatonStore) {
        let dir = tempfile::tempdir().unwrap();
        let host_id = uuid::Uuid::new_v4().to_string();
        let store = BatonStore::open_hosted(&dir.path().join("baton.sqlite"), &host_id).unwrap();
        assert!(matches!(
            acquire(&store, dir.path(), &host_id, &SystemProbe).unwrap(),
            ClaimOutcome::Claimed(_)
        ));
        let thread = store
            .create_thread(NewThread {
                title: "protocol".into(),
                cwd: dir.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn(&thread.id, ProviderId::Codex, None, "open", false)
            .unwrap();
        (dir, store)
    }

    #[cfg(unix)]
    #[test]
    fn protocol_answers_ping_status_claim_info_and_errors() {
        let (_dir, store) = claimed_store();

        let ping = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":7,"command":"ping"}"#,
        )
        .unwrap();
        assert_eq!(ping["id"], 7);
        assert_eq!(ping["ok"], true);
        assert_eq!(ping["result"]["pong"], true);

        let status = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":8,"command":"status","args":{"thread":null}}"#,
        )
        .unwrap();
        assert_eq!(status["result"]["scope"], "host");
        assert_eq!(status["result"]["threads"], 1);
        assert_eq!(status["result"]["running_turns"], 1);

        let thread = store.list_threads(None).unwrap().remove(0).id;
        let thread_status = handle_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":12,"command":"status","args":{{"thread":"{thread}"}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(thread_status["result"]["scope"], "thread");
        assert_eq!(thread_status["result"]["threads"], 1);
        assert_eq!(thread_status["result"]["running_turns"], 1);

        let missing = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":13,"command":"status","args":{"thread":"missing"}}"#,
        )
        .unwrap();
        assert_eq!(missing["id"], 13);
        assert_eq!(missing["error"]["code"], "not_found");

        let claim = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":9,"command":"claim-info"}"#,
        )
        .unwrap();
        assert_eq!(claim["result"]["pid"], std::process::id());

        let malformed = handle_line(&store, "{").unwrap();
        assert_eq!(malformed["id"], Value::Null);
        assert_eq!(malformed["error"]["code"], "malformed_line");

        let version = handle_line(
            &store,
            r#"{"v":2,"kind":"request","id":10,"command":"ping"}"#,
        )
        .unwrap();
        assert_eq!(version["id"], Value::Null);
        assert_eq!(version["error"]["code"], "bad_version");

        let unknown = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":11,"command":"exec"}"#,
        )
        .unwrap();
        assert_eq!(unknown["id"], 11);
        assert_eq!(unknown["error"]["code"], "not_implemented");
    }

    #[cfg(unix)]
    #[test]
    fn pair_parses_labels_scopes_and_expiry() {
        use crate::grants::Scope;

        let request = parse_pair(Vec::new()).unwrap();
        assert_eq!(request.scopes, vec![Scope::Read, Scope::Operate]);
        assert!(request.label.is_none());

        let request = parse_pair(vec![
            "--label",
            "laptop",
            "--scope",
            "read",
            "--scope",
            "git",
            "--expires-days",
            "7",
        ])
        .unwrap();
        assert_eq!(request.scopes, vec![Scope::Read, Scope::Git]);
        assert_eq!(request.label.as_deref(), Some("laptop"));
        let expiry = request
            .expires_at
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let expected = chrono::Utc::now() + chrono::Duration::days(7);
        assert!((expiry - expected).num_seconds().abs() < 60);

        let error = parse_pair(vec!["--scope", "nope"]).unwrap_err();
        assert!(error.to_string().contains("unknown scope"), "{error}");
        let error = parse_pair(vec!["--expires-days", "0"]).unwrap_err();
        assert!(error.to_string().contains("positive"), "{error}");
        let error = parse_pair(vec!["--bogus"]).unwrap_err();
        assert!(error.to_string().contains("unknown pair option"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn renew_parses_a_grant_id_and_days() {
        let (grant_id, days) = parse_renew(vec!["00000000-0000-4000-8000-000000000000"]).unwrap();
        assert_eq!(grant_id, "00000000-0000-4000-8000-000000000000");
        assert_eq!(days, crate::grants::DEFAULT_GRANT_DAYS);

        let (grant_id, days) = parse_renew(vec!["grant", "--days", "7"]).unwrap();
        assert_eq!(grant_id, "grant");
        assert_eq!(days, 7);

        let error = parse_renew(Vec::new()).unwrap_err();
        assert!(error.to_string().contains("needs a grant id"), "{error}");
        let error = parse_renew(vec!["a", "b"]).unwrap_err();
        assert!(
            error.to_string().contains("exactly one grant id"),
            "{error}"
        );
        let error = parse_renew(vec!["a", "--days"]).unwrap_err();
        assert!(
            error.to_string().contains("--days needs a number"),
            "{error}"
        );
        let error = parse_renew(vec!["a", "--bogus"]).unwrap_err();
        assert!(
            error.to_string().contains("unknown renew option"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn socket_lines_require_a_live_read_grant() {
        use crate::grants::{mint, revoke, GrantRequest, Scope};

        let dir = tempfile::tempdir().unwrap();
        let store = BatonStore::memory().unwrap();
        let read = mint(&store, dir.path(), &GrantRequest::new(vec![Scope::Read])).unwrap();
        let operate = mint(&store, dir.path(), &GrantRequest::new(vec![Scope::Operate])).unwrap();

        let ping = handle_socket_line(
            &store,
            r#"{"v":1,"kind":"request","id":1,"command":"ping"}"#,
        );
        assert_eq!(ping["result"]["pong"], true);

        let bare = handle_socket_line(
            &store,
            r#"{"v":1,"kind":"request","id":2,"command":"status"}"#,
        );
        assert_eq!(bare["error"]["code"], "unauthenticated");

        let with = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":3,"command":"status","grant_id":"{}","secret":"{}"}}"#,
                read.summary.grant_id, read.secret
            ),
        );
        assert_eq!(with["ok"], true, "{with}");
        assert_eq!(with["result"]["scope"], "host");

        let wrong = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":4,"command":"status","grant_id":"{}","secret":"wrong"}}"#,
                read.summary.grant_id
            ),
        );
        assert_eq!(wrong["error"]["code"], "bad_secret");

        let narrow = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":5,"command":"status","grant_id":"{}","secret":"{}"}}"#,
                operate.summary.grant_id, operate.secret
            ),
        );
        assert_eq!(narrow["error"]["code"], "not_permitted");

        revoke(&store, dir.path(), &read.summary.grant_id).unwrap();
        let revoked = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":6,"command":"status","grant_id":"{}","secret":"{}"}}"#,
                read.summary.grant_id, read.secret
            ),
        );
        assert_eq!(revoked["error"]["code"], "revoked");
    }

    #[cfg(unix)]
    #[test]
    fn peer_credentials_accept_only_the_host_user() {
        let host = unsafe { libc::getuid() };
        assert!(same_user(host, host));
        assert!(!same_user(host.wrapping_add(1), host));
    }
    #[cfg(unix)]
    #[test]
    fn finish_host_settles_dead_owner_work_before_release() {
        let (dir, store) = claimed_store();
        let host_id = store.get_host_claim().unwrap().unwrap().host_id;
        let thread_id = store.list_threads(None).unwrap().remove(0).id;
        {
            let db = rusqlite::Connection::open(dir.path().join("baton.sqlite")).unwrap();
            db.execute(
                "UPDATE turns SET owner_pid = ?1, owner_start = NULL",
                rusqlite::params![u32::MAX],
            )
            .unwrap();
        }
        finish_host(&store, &host_id, Ok(())).unwrap();
        let turn = store.get_turn_record(&thread_id, 1).unwrap().unwrap();
        assert_eq!(turn.phase, crate::types::TurnPhase::Unverified);
        assert!(store.get_host_claim().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn finish_host_releases_the_claim_when_the_loop_failed() {
        let (_dir, store) = claimed_store();
        let host_id = store.get_host_claim().unwrap().unwrap().host_id;
        let thread_id = store.list_threads(None).unwrap().remove(0).id;
        let error =
            finish_host(&store, &host_id, Err(anyhow::anyhow!("stdout closed"))).unwrap_err();
        assert_eq!(error.to_string(), "stdout closed");
        let turn = store.get_turn_record(&thread_id, 1).unwrap().unwrap();
        assert_eq!(turn.phase, crate::types::TurnPhase::Unverified);
        assert_eq!(turn.error.as_deref(), Some("host shut down"));
        assert!(store.get_host_claim().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn finish_host_cancels_its_own_work_before_release() {
        let (_dir, store) = claimed_store();
        let claim = store.get_host_claim().unwrap().unwrap();
        let thread_id = store.list_threads(None).unwrap().remove(0).id;
        store
            .create_orchestration_run_owned(
                "run-host",
                "host goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
                Some(claim.pid),
                Some(&claim.process_start),
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
                "do host",
                Some("/tmp/host-worktree"),
                None,
                None,
                Some(&thread_id),
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

        finish_host(&store, &claim.host_id, Ok(())).unwrap();
        let turn = store.get_turn_record(&thread_id, 1).unwrap().unwrap();
        assert_eq!(turn.phase, crate::types::TurnPhase::Unverified);
        assert_eq!(turn.error.as_deref(), Some("host shut down"));
        let tasks = store.list_orchestration_tasks("run-host").unwrap();
        assert_eq!(tasks[0].status, "cancelled");
        assert_eq!(tasks[0].error.as_deref(), Some("host shut down"));
        let run = store
            .find_orchestration_runs("run-host")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(run.status, "cancelled");
        assert!(store.get_host_claim().unwrap().is_none());
    }
}
