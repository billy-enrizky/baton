//! Newline-delimited JSON protocol for `baton serve --stdio`.
//!
//! Stdout carries protocol responses only. Diagnostics stay on stderr. P2 is
//! request/response only: no events, resume cursor, or idempotency key.

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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
