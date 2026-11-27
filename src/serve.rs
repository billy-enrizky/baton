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
/// The handle a socket client uses to submit work to the host engine.
#[derive(Clone)]
pub struct EngineLink {
    sender: tokio::sync::mpsc::UnboundedSender<crate::runtime::EngineCommand>,
    home: std::path::PathBuf,
    root: std::path::PathBuf,
}

impl EngineLink {
    pub fn submit(
        &self,
        thread_id: &str,
        text: &str,
        permission_mode: crate::types::PermissionMode,
    ) -> bool {
        self.sender
            .send(crate::runtime::EngineCommand::Submit {
                thread_id: thread_id.to_string(),
                text: text.to_string(),
                permission_mode,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            })
            .is_ok()
    }

    pub fn environment(&self) -> String {
        crate::claim::installation_id(&self.home).unwrap_or_else(|| "unclaimed".into())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// The host's provider engine. A socket host owns one engine for the project
/// directory it started in.
pub struct ServeEngine {
    link: EngineLink,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ServeEngine {
    pub fn start(store: Arc<Mutex<BatonStore>>, home: &Path, root: &Path) -> Result<ServeEngine> {
        let adapters = crate::runtime::build_adapters(&store, home, root);
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (engine_event_sender, engine_event_receiver) = std::sync::mpsc::channel();
        let (turn_event_sender, turn_event_receiver) = std::sync::mpsc::channel();
        // The host has no UI to consume events, and keeping the receivers alive
        // without draining would buffer every event for the life of the host.
        std::thread::spawn(move || for _ in engine_event_receiver {});
        std::thread::spawn(move || for _ in turn_event_receiver {});
        let engine_store = store.clone();
        let engine_home = home.to_path_buf();
        let thread = std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("baton: engine runtime: {error}");
                    return;
                }
            };
            let interrupts = runtime.block_on(async {
                let mut handles = Vec::new();
                for adapter in &adapters {
                    let adapter = adapter.lock().await;
                    handles.push((adapter.id(), adapter.interrupt_handle()));
                }
                handles
            });
            if let Err(error) = runtime.block_on(crate::runtime::run_engine(
                engine_store,
                engine_home,
                adapters,
                interrupts,
                Vec::new(),
                command_receiver,
                engine_event_sender,
                turn_event_sender,
            )) {
                eprintln!("baton: engine stopped: {error}");
            }
        });
        Ok(ServeEngine {
            link: EngineLink {
                sender: command_sender,
                home: home.to_path_buf(),
                root: root.to_path_buf(),
            },
            thread: Some(thread),
        })
    }

    pub fn link(&self) -> EngineLink {
        self.link.clone()
    }

    pub fn stop(&mut self) {
        let _ = self.link.sender.send(crate::runtime::EngineCommand::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ServeEngine {
    fn drop(&mut self) {
        let _ = self.link.sender.send(crate::runtime::EngineCommand::Quit);
    }
}

#[cfg(unix)]
pub async fn socket_loop(
    listener: StdUnixListener,
    store: Arc<Mutex<BatonStore>>,
    home: &Path,
    path: &Path,
    engine: Option<EngineLink>,
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
                        let engine = engine.clone();
                        clients.push(tokio::spawn(async move {
                            if let Err(error) = serve_socket_client(store, stream, engine).await {
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
    _engine: Option<EngineLink>,
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
            "baton: host commands: pair [--label <name>] [--scope <s>]... [--ceiling <mode>] [--expires-days N]; renew <id> [--days N]; help"
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
                            "baton: paired {} with {} scopes, ceiling {}, expires {}",
                            minted.summary.grant_id,
                            minted.summary.scopes.join(","),
                            minted.summary.permission_ceiling,
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
    use crate::types::PermissionMode;

    let mut scopes = vec![Scope::Read, Scope::Operate];
    let mut scopes_set = false;
    let mut label = None;
    let mut ceiling = PermissionMode::Default;
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
            "--ceiling" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--ceiling needs a mode"))?;
                if *value == "bypassPermissions" {
                    anyhow::bail!("ceiling: bypassPermissions is not grantable");
                }
                ceiling = PermissionMode::parse(value)
                    .ok_or_else(|| anyhow::anyhow!("unknown ceiling {value}"))?;
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
    request.permission_ceiling = ceiling;
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
async fn serve_socket_client(
    store: Arc<Mutex<BatonStore>>,
    stream: UnixStream,
    engine: Option<EngineLink>,
) -> Result<()> {
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
            handle_socket_line(&store, &line, engine.as_ref())
        };
        let mut text = serde_json::to_string(&response)?;
        text.push('\n');
        write.write_all(text.as_bytes()).await?;
    }
    Ok(())
}

/// Socket requests carry a grant id and secret. `ping` is the only request
/// answered without one, so a readiness probe needs no credential.
fn handle_socket_line(store: &BatonStore, line: &str, engine: Option<&EngineLink>) -> Value {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return error_response(Value::Null, "malformed_line", "line is not JSON");
    };
    if value["v"].as_u64() != Some(PROTOCOL_VERSION) {
        return error_response(Value::Null, "bad_version", "unsupported protocol version");
    }
    let id = value["id"].clone();
    let command = value["command"].as_str().unwrap_or_default();
    let mut grant = None;
    if command != "ping" {
        let (Some(grant_id), Some(secret)) = (value["grant_id"].as_str(), value["secret"].as_str())
        else {
            return error_response(id, "unauthenticated", "grant_id and secret are required");
        };
        let required = if command == "submit" {
            "operate"
        } else {
            "read"
        };
        match crate::grants::verify(store, grant_id, secret) {
            Ok(verified) => {
                if !verified.scopes.iter().any(|scope| scope == required) {
                    return error_response(
                        id,
                        "not_permitted",
                        format!("the grant lacks the {required} scope"),
                    );
                }
                grant = Some(verified);
            }
            Err(error) => {
                if let Some(refusal) = error.downcast_ref::<crate::grants::GrantRefusal>() {
                    return error_response(id, refusal.code(), refusal.to_string());
                }
                return error_response(id, "internal", error.to_string());
            }
        }
    }
    if command == "submit" {
        let grant = grant.expect("submit requires a verified grant");
        return submit_request(store, engine, &grant, id, &value);
    }
    handle_line(store, line).unwrap_or_else(|| {
        error_response(
            id,
            "not_implemented",
            format!("command {command} is not implemented"),
        )
    })
}

const SUBMIT_OPERATION: &str = "submit";

/// A replay returns the original result with `replayed: true`, so a client can
/// tell that no second turn started.
fn replay_response(id: Value, stored: &str) -> Value {
    match serde_json::from_str::<Value>(stored) {
        Ok(mut body @ Value::Object(_)) => {
            body["replayed"] = json!(true);
            response(id, body)
        }
        Ok(_) => error_response(id, "internal", "stored response is not an object"),
        Err(error) => error_response(
            id,
            "internal",
            format!("stored response is not JSON: {error}"),
        ),
    }
}

fn submit_request(
    store: &BatonStore,
    engine: Option<&EngineLink>,
    grant: &crate::store::GrantSummary,
    id: Value,
    value: &Value,
) -> Value {
    let Some(engine) = engine else {
        return error_response(
            id,
            "not_implemented",
            "this host has no engine; start baton serve without --stdio",
        );
    };
    let args = match value.get("args") {
        Some(Value::Object(args)) => args,
        _ => return error_response(id, "malformed_line", "submit args must be an object"),
    };
    let thread_id = match args.get("thread") {
        Some(Value::String(thread)) if !thread.is_empty() => thread.as_str(),
        _ => {
            return error_response(
                id,
                "malformed_line",
                "submit args.thread must be a non-empty string",
            )
        }
    };
    let text = match args.get("text") {
        Some(Value::String(text)) if !text.trim().is_empty() => text.as_str(),
        _ => {
            return error_response(
                id,
                "malformed_line",
                "submit args.text must be a non-empty string",
            )
        }
    };
    let key =
        match args.get("idempotency_key") {
            Some(Value::String(key))
                if !key.is_empty()
                    && key.chars().count() <= 128
                    && !key.chars().any(char::is_control) =>
            {
                key.as_str()
            }
            _ => return error_response(
                id,
                "malformed_line",
                "submit args.idempotency_key must be a non-empty string of at most 128 characters",
            ),
        };
    match store.replay_idempotent_request(key, SUBMIT_OPERATION) {
        Ok(Some(stored)) => return replay_response(id, &stored),
        Ok(None) => {}
        Err(error) => return error_response(id, "idempotency_conflict", error.to_string()),
    }
    let thread = match store.get_thread(thread_id) {
        Ok(Some(thread)) => thread,
        Ok(None) => {
            return error_response(id, "not_found", format!("no thread matches {thread_id}"))
        }
        Err(error) => return error_response(id, "internal", error.to_string()),
    };
    let root = std::fs::canonicalize(engine.root()).unwrap_or_else(|_| engine.root().to_path_buf());
    let cwd = std::fs::canonicalize(&thread.cwd).unwrap_or_else(|_| thread.cwd.clone().into());
    if !cwd.starts_with(&root) {
        return error_response(
            id,
            "wrong_project",
            format!(
                "thread {thread_id} is outside the served project {}",
                root.display()
            ),
        );
    }
    if thread.permission_mode == crate::types::PermissionMode::BypassPermissions {
        return error_response(
            id,
            "ceiling",
            "the thread runs with bypassPermissions; set a safe mode before submitting remotely",
        );
    }
    let ceiling = match crate::types::PermissionMode::parse(&grant.permission_ceiling) {
        Some(ceiling) => ceiling,
        None => {
            return error_response(
                id,
                "ceiling",
                "the grant carries an unknown permission ceiling",
            )
        }
    };
    if !thread.permission_mode.within_ceiling(ceiling) {
        return error_response(
            id,
            "ceiling",
            format!(
                "thread {thread_id} runs with {} above the grant ceiling {ceiling}",
                thread.permission_mode
            ),
        );
    }
    let body = json!({ "thread_id": thread_id, "accepted": true });
    match store.claim_idempotent_request(key, SUBMIT_OPERATION, thread_id, &body.to_string()) {
        Ok(Some(stored)) => return replay_response(id, &stored),
        Ok(None) => {}
        Err(error) => return error_response(id, "idempotency_conflict", error.to_string()),
    }
    let audit = match crate::audit::grant_event(
        &grant.grant_id,
        &engine.environment(),
        Some(thread_id),
        Some(thread_id),
        crate::audit::AuditAction::TurnSubmitRequest,
        &json!({
            "thread_id": thread_id,
            "text_len": text.chars().count(),
        }),
    ) {
        Ok(audit) => audit,
        Err(error) => {
            let _ = store.forget_idempotent_request(key, SUBMIT_OPERATION);
            return error_response(id, "internal", error.to_string());
        }
    };
    if let Err(error) = store.append_audit_event(&audit) {
        let _ = store.forget_idempotent_request(key, SUBMIT_OPERATION);
        return error_response(id, "internal", error.to_string());
    }
    if !engine.submit(thread_id, text, thread.permission_mode) {
        let _ = store.forget_idempotent_request(key, SUBMIT_OPERATION);
        return error_response(id, "internal", "the host engine is not running");
    }
    response(id, body)
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

fn message_json(message: &crate::store::MessageRow) -> Value {
    json!({
        "id": message.id,
        "turn": message.turn,
        "role": message.role,
        "provider": message.provider,
        "model": message.model,
        "text": message.text,
        "created_at": message.created_at,
    })
}

fn messages(store: &BatonStore, args: Option<&Value>) -> Result<Value> {
    let args = match args {
        None | Some(Value::Null) => anyhow::bail!("messages args must name a thread"),
        Some(Value::Object(args)) => args,
        Some(_) => anyhow::bail!("messages args must be an object"),
    };
    let thread = match args.get("thread") {
        Some(Value::String(thread)) if !thread.is_empty() => thread.as_str(),
        _ => anyhow::bail!("messages args.thread must be a non-empty string"),
    };
    let after = match args.get("after") {
        None | Some(Value::Null) => 0,
        Some(value) => value
            .as_i64()
            .filter(|after| *after >= 0)
            .ok_or_else(|| anyhow::anyhow!("messages args.after must be a non-negative integer"))?,
    };
    let limit = match args.get("limit") {
        None | Some(Value::Null) => 50,
        Some(value) => value
            .as_u64()
            .map(|limit| (limit as usize).clamp(1, 200))
            .ok_or_else(|| anyhow::anyhow!("messages args.limit must be an integer"))?,
    };
    if store.get_thread(thread)?.is_none() {
        return Ok(error_response(
            Value::Null,
            "not_found",
            format!("no thread matches {thread}"),
        ));
    }
    let rows = store.list_messages_after(thread, after, limit)?;
    let cursor = rows.last().map(|message| message.id).unwrap_or(after);
    Ok(response(
        Value::Null,
        json!({
            "thread_id": thread,
            "cursor": cursor,
            "running_turns": store.count_open_turns(Some(thread))?,
            "messages": rows.iter().map(message_json).collect::<Vec<_>>(),
        }),
    ))
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
        "messages" => match messages(store, object.get("args")) {
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

        let open_id = store.list_messages(&thread).unwrap()[0].id;
        let first = store
            .add_message(&thread, 1, "user", Some("codex"), None, "first")
            .unwrap();
        let second = store
            .add_message(&thread, 1, "assistant", Some("codex"), None, "second")
            .unwrap();
        let page = handle_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":14,"command":"messages","args":{{"thread":"{thread}","after":0,"limit":2}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(page["id"], 14);
        assert_eq!(page["result"]["thread_id"], thread);
        assert_eq!(page["result"]["running_turns"], 1);
        assert_eq!(page["result"]["messages"].as_array().unwrap().len(), 2);
        assert_eq!(page["result"]["messages"][0]["id"], open_id);
        assert_eq!(page["result"]["messages"][1]["id"], first);
        assert_eq!(page["result"]["cursor"], first);

        let next = handle_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":15,"command":"messages","args":{{"thread":"{thread}","after":{first},"limit":50}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(next["result"]["cursor"], second);
        assert_eq!(next["result"]["messages"].as_array().unwrap().len(), 1);
        assert_eq!(next["result"]["messages"][0]["id"], second);

        let missing = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":16,"command":"messages","args":{"thread":"missing"}}"#,
        )
        .unwrap();
        assert_eq!(missing["error"]["code"], "not_found");
        let malformed = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":17,"command":"messages","args":{}}"#,
        )
        .unwrap();
        assert_eq!(malformed["error"]["code"], "internal");
    }

    #[cfg(unix)]
    #[test]
    fn pair_parses_labels_scopes_and_expiry() {
        use crate::grants::Scope;
        use crate::types::PermissionMode;

        let request = parse_pair(Vec::new()).unwrap();
        assert_eq!(request.scopes, vec![Scope::Read, Scope::Operate]);
        assert!(request.label.is_none());
        assert_eq!(request.permission_ceiling, PermissionMode::Default);

        let request = parse_pair(vec![
            "--label",
            "laptop",
            "--scope",
            "read",
            "--scope",
            "git",
            "--ceiling",
            "acceptEdits",
            "--expires-days",
            "7",
        ])
        .unwrap();
        assert_eq!(request.scopes, vec![Scope::Read, Scope::Git]);
        assert_eq!(request.label.as_deref(), Some("laptop"));
        assert_eq!(request.permission_ceiling, PermissionMode::AcceptEdits);
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
        let error = parse_pair(vec!["--ceiling", "bypassPermissions"]).unwrap_err();
        assert!(error.to_string().contains("not grantable"), "{error}");
        let error = parse_pair(vec!["--ceiling", "nope"]).unwrap_err();
        assert!(error.to_string().contains("unknown ceiling"), "{error}");
        let error = parse_pair(vec!["--bogus"]).unwrap_err();
        assert!(error.to_string().contains("unknown pair option"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn submit_requires_operate_scope_and_a_thread_in_the_project() {
        use crate::grants::{mint, GrantRequest, Scope};

        let (_dir, store) = claimed_store();
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let read = mint(&store, home.path(), &GrantRequest::new(vec![Scope::Read])).unwrap();
        let operate = mint(
            &store,
            home.path(),
            &GrantRequest::new(vec![Scope::Read, Scope::Operate]),
        )
        .unwrap();
        let thread = store.list_threads(None).unwrap().remove(0);
        let inside = store
            .create_thread(NewThread {
                title: "inside".into(),
                cwd: root.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let outside = store
            .create_thread(NewThread {
                title: "outside".into(),
                cwd: "/elsewhere".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let engine = EngineLink {
            sender,
            home: home.path().to_path_buf(),
            root: root.path().to_path_buf(),
        };
        let request = |grant: &crate::grants::MintedGrant, thread: &str, id: u64, key: &str| {
            format!(
                r#"{{"v":1,"kind":"request","id":{id},"command":"submit","grant_id":"{}","secret":"{}","args":{{"thread":"{thread}","text":"remote hello","idempotency_key":"{key}"}}}}"#,
                grant.summary.grant_id, grant.secret
            )
        };

        let denied = handle_socket_line(
            &store,
            &request(&read, &inside.id, 1, "k-denied"),
            Some(&engine),
        );
        assert_eq!(denied["error"]["code"], "not_permitted");
        assert!(receiver.try_recv().is_err());

        let missing = handle_socket_line(
            &store,
            &request(&operate, "missing", 2, "k-missing"),
            Some(&engine),
        );
        assert_eq!(missing["error"]["code"], "not_found");

        let elsewhere = handle_socket_line(
            &store,
            &request(&operate, &outside.id, 3, "k-elsewhere"),
            Some(&engine),
        );
        assert_eq!(elsewhere["error"]["code"], "wrong_project");

        let malformed = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":4,"command":"submit","grant_id":"{}","secret":"{}","args":{{}}}}"#,
                operate.summary.grant_id, operate.secret
            ),
            Some(&engine),
        );
        assert_eq!(malformed["error"]["code"], "malformed_line");

        let missing_key = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":5,"command":"submit","grant_id":"{}","secret":"{}","args":{{"thread":"{}","text":"remote hello"}}}}"#,
                operate.summary.grant_id, operate.secret, inside.id
            ),
            Some(&engine),
        );
        assert_eq!(missing_key["error"]["code"], "malformed_line");

        let long_key = "x".repeat(129);
        let long = handle_socket_line(
            &store,
            &request(&operate, &inside.id, 6, &long_key),
            Some(&engine),
        );
        assert_eq!(long["error"]["code"], "malformed_line");

        let accepted = handle_socket_line(
            &store,
            &request(&operate, &inside.id, 7, "k-accepted"),
            Some(&engine),
        );
        assert_eq!(accepted["ok"], true, "{accepted}");
        assert_eq!(accepted["result"]["accepted"], true);
        assert_eq!(accepted["result"].get("replayed"), None);
        let command = receiver.try_recv().expect("the engine receives the submit");
        match command {
            crate::runtime::EngineCommand::Submit {
                thread_id, text, ..
            } => {
                assert_eq!(thread_id, inside.id);
                assert_eq!(text, "remote hello");
            }
            _ => panic!("expected a submit command"),
        }
        let replay = handle_socket_line(
            &store,
            &request(&operate, &inside.id, 8, "k-accepted"),
            Some(&engine),
        );
        assert_eq!(replay["ok"], true, "{replay}");
        assert_eq!(replay["result"]["accepted"], true);
        assert_eq!(replay["result"]["replayed"], true);
        assert_eq!(replay["result"]["thread_id"], inside.id);
        assert!(
            receiver.try_recv().is_err(),
            "a replay must not start a second turn"
        );
        let events = store.list_audit_events(10).unwrap();
        let event = events
            .iter()
            .find(|event| event.action == "turn.submit_request")
            .expect("the remote request writes its row");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.action == "turn.submit_request")
                .count(),
            1,
            "a replay must not write a second request row"
        );
        assert_eq!(event.actor, operate.summary.grant_id);
        assert_eq!(event.confirmation, crate::audit::SOCKET_GRANT_CONFIRMATION);
        assert_eq!(event.scope, crate::audit::OPERATE_SCOPE);
        assert_eq!(event.target.as_deref(), Some(inside.id.as_str()));
        assert_eq!(event.thread_id.as_deref(), Some(inside.id.as_str()));
        assert_ne!(thread.id, inside.id);
    }

    #[cfg(unix)]
    #[test]
    fn submit_refuses_a_bypass_thread() {
        use crate::grants::{mint, GrantRequest, Scope};

        let (_dir, store) = claimed_store();
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let operate = mint(
            &store,
            home.path(),
            &GrantRequest::new(vec![Scope::Read, Scope::Operate]),
        )
        .unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "bypass".into(),
                cwd: root.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .set_thread_permission_mode(&thread.id, crate::types::PermissionMode::BypassPermissions)
            .unwrap();
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let engine = EngineLink {
            sender,
            home: home.path().to_path_buf(),
            root: root.path().to_path_buf(),
        };
        let refused = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":1,"command":"submit","grant_id":"{}","secret":"{}","args":{{"thread":"{}","text":"run","idempotency_key":"k-bypass"}}}}"#,
                operate.summary.grant_id, operate.secret, thread.id
            ),
            Some(&engine),
        );
        assert_eq!(refused["error"]["code"], "ceiling");
        assert!(!store
            .list_audit_events(10)
            .unwrap()
            .iter()
            .any(|event| event.action == "turn.submit_request"));
    }

    #[cfg(unix)]
    #[test]
    fn submit_refuses_a_thread_above_the_grant_ceiling() {
        use crate::grants::{mint, GrantRequest, Scope};
        use crate::types::PermissionMode;

        let (_dir, store) = claimed_store();
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let accepted_thread = store
            .create_thread(NewThread {
                title: "accept edits".into(),
                cwd: root.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .set_thread_permission_mode(&accepted_thread.id, PermissionMode::AcceptEdits)
            .unwrap();
        let default_thread = store
            .create_thread(NewThread {
                title: "default".into(),
                cwd: root.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let mut default_request = GrantRequest::new(vec![Scope::Read, Scope::Operate]);
        default_request.permission_ceiling = PermissionMode::Default;
        let default_grant = mint(&store, home.path(), &default_request).unwrap();
        let mut accept_request = GrantRequest::new(vec![Scope::Read, Scope::Operate]);
        accept_request.permission_ceiling = PermissionMode::AcceptEdits;
        let accept_grant = mint(&store, home.path(), &accept_request).unwrap();
        let mut plan_request = GrantRequest::new(vec![Scope::Read, Scope::Operate]);
        plan_request.permission_ceiling = PermissionMode::Plan;
        let plan_grant = mint(&store, home.path(), &plan_request).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let engine = EngineLink {
            sender,
            home: home.path().to_path_buf(),
            root: root.path().to_path_buf(),
        };
        let request = |grant: &crate::grants::MintedGrant, thread: &str, id: u64| {
            format!(
                r#"{{"v":1,"kind":"request","id":{id},"command":"submit","grant_id":"{}","secret":"{}","args":{{"thread":"{thread}","text":"remote hello","idempotency_key":"k-{id}"}}}}"#,
                grant.summary.grant_id, grant.secret
            )
        };

        let above = handle_socket_line(
            &store,
            &request(&default_grant, &accepted_thread.id, 1),
            Some(&engine),
        );
        assert_eq!(above["error"]["code"], "ceiling");
        assert!(receiver.try_recv().is_err());

        let below = handle_socket_line(
            &store,
            &request(&plan_grant, &default_thread.id, 2),
            Some(&engine),
        );
        assert_eq!(below["error"]["code"], "ceiling");
        assert!(receiver.try_recv().is_err());

        let accepted = handle_socket_line(
            &store,
            &request(&accept_grant, &accepted_thread.id, 3),
            Some(&engine),
        );
        assert_eq!(accepted["ok"], true, "{accepted}");
        assert!(receiver.try_recv().is_ok());
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
            None,
        );
        assert_eq!(ping["result"]["pong"], true);

        let bare = handle_socket_line(
            &store,
            r#"{"v":1,"kind":"request","id":2,"command":"status"}"#,
            None,
        );
        assert_eq!(bare["error"]["code"], "unauthenticated");

        let with = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":3,"command":"status","grant_id":"{}","secret":"{}"}}"#,
                read.summary.grant_id, read.secret
            ),
            None,
        );
        assert_eq!(with["ok"], true, "{with}");
        assert_eq!(with["result"]["scope"], "host");

        let wrong = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":4,"command":"status","grant_id":"{}","secret":"wrong"}}"#,
                read.summary.grant_id
            ),
            None,
        );
        assert_eq!(wrong["error"]["code"], "bad_secret");

        let narrow = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":5,"command":"status","grant_id":"{}","secret":"{}"}}"#,
                operate.summary.grant_id, operate.secret
            ),
            None,
        );
        assert_eq!(narrow["error"]["code"], "not_permitted");

        revoke(&store, dir.path(), &read.summary.grant_id).unwrap();
        let revoked = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":6,"command":"status","grant_id":"{}","secret":"{}"}}"#,
                read.summary.grant_id, read.secret
            ),
            None,
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
