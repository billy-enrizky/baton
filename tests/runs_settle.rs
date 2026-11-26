use baton::store::BatonStore;
use baton::types::ProviderId;

#[cfg(unix)]
use baton::claim::{acquire, ClaimOutcome, SystemProbe};
#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
fn seed_run(home: &std::path::Path, run_id: &str, owner_pid: Option<u32>, task_status: &str) {
    let store = BatonStore::open(&home.join("baton.sqlite")).unwrap();
    store
        .create_orchestration_run_owned(
            run_id,
            "goal",
            ProviderId::Codex,
            "model",
            None,
            "{}",
            false,
            owner_pid,
            None,
        )
        .unwrap();
    store
        .add_orchestration_task(
            "task-1",
            run_id,
            1,
            "task",
            ProviderId::Codex,
            "model",
            "prompt",
            None,
            None,
            None,
            None,
            task_status,
            None,
            None,
            "2026-01-01T00:00:00.000Z",
            None,
            None,
            None,
            None,
        )
        .unwrap();
}

#[cfg(unix)]
#[test]
fn runs_settle_records_a_cancelled_verdict() {
    let home = tempfile::tempdir().unwrap();
    seed_run(home.path(), "run-aaaaaaaa", Some(999_999), "queued");
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "runs",
            "settle",
            "run-aaaa",
            "--status",
            "cancelled",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["run_id"], "run-aaaaaaaa");
    assert_eq!(report["status"], "cancelled");
    assert_eq!(report["tasks_settled"], 1);
    assert_eq!(report["turns_settled"], 0);
    assert_eq!(report["forced"], false);

    let store = BatonStore::open_read_only(&home.path().join("baton.sqlite")).unwrap();
    let run = store
        .find_orchestration_runs("run-aaaaaaaa")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(run.status, "cancelled");
    assert!(run.finished_at.is_some());
    let tasks = store.list_orchestration_tasks("run-aaaaaaaa").unwrap();
    assert_eq!(tasks[0].status, "cancelled");
}

#[cfg(unix)]
#[test]
fn runs_settle_refuses_completed_while_work_is_open() {
    let home = tempfile::tempdir().unwrap();
    seed_run(home.path(), "run-bbbbbbbb", Some(999_999), "queued");
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["runs", "settle", "run-bbbbbbbb", "--status", "completed"])
        .env("BATON_HOME", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("open task(s)"), "{stderr}");

    let store = BatonStore::open_read_only(&home.path().join("baton.sqlite")).unwrap();
    let run = store
        .find_orchestration_runs("run-bbbbbbbb")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(run.status, "running");
    let tasks = store.list_orchestration_tasks("run-bbbbbbbb").unwrap();
    assert_eq!(tasks[0].status, "queued");
}

#[cfg(unix)]
#[test]
fn runs_settle_is_fenced_while_a_host_holds_the_claim() {
    let home = tempfile::tempdir().unwrap();
    let host_id = uuid::Uuid::new_v4().to_string();
    let store = BatonStore::open_hosted(&home.path().join("baton.sqlite"), &host_id).unwrap();
    match acquire(&store, home.path(), &host_id, &SystemProbe).unwrap() {
        ClaimOutcome::Claimed(_) => {}
        other => panic!("expected a claim, got {other:?}"),
    }
    store
        .create_orchestration_run_owned(
            "run-cccccccc",
            "goal",
            ProviderId::Codex,
            "model",
            None,
            "{}",
            false,
            None,
            None,
        )
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["runs", "settle", "run-cccccccc", "--status", "failed"])
        .env("BATON_HOME", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(4), "{stderr}");
    assert!(stderr.contains("host"), "{stderr}");
    let run = store
        .find_orchestration_runs("run-cccccccc")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(run.status, "running");
    store.release_host_claim(&host_id).unwrap();
}
