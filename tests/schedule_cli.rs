use std::process::Command;

use baton::store::{BatonStore, NewThread};
use baton::types::ProviderId;

fn baton() -> Command {
    Command::new(env!("CARGO_BIN_EXE_baton"))
}

#[test]
fn schedule_add_list_remove_and_audit() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let project_arg = project.path().to_string_lossy().to_string();
    let store = BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "scheduled".into(),
            cwd: project_arg.clone(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    drop(store);
    let run = |args: &[&str]| {
        baton()
            .args(args)
            .env("BATON_HOME", home.path())
            .output()
            .unwrap()
    };

    let add = run(&[
        "-C",
        &project_arg,
        "schedule",
        "add",
        "--thread",
        &thread.id,
        "--cron",
        "0 * * * *",
        "--prompt",
        "nightly",
        "--json",
    ]);
    assert!(add.status.success(), "{add:?}");
    let value: serde_json::Value = serde_json::from_slice(&add.stdout).unwrap();
    let id = value["id"].as_str().unwrap().to_string();
    assert_eq!(value["thread_id"], thread.id);
    assert_eq!(value["expression"], "0 * * * *");
    assert!(value["next_run_at"].as_str().unwrap().ends_with('Z'));

    let list = run(&["schedule", "list", "--json"]);
    assert!(list.status.success(), "{list:?}");
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let schedules = value["schedules"].as_array().unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0]["id"], id);
    assert_eq!(schedules[0]["prompt"], "nightly");
    assert_eq!(schedules[0]["enabled"], true);

    let audit = run(&["audit", "list", "--json"]);
    assert!(audit.status.success(), "{audit:?}");
    let value: serde_json::Value = serde_json::from_slice(&audit.stdout).unwrap();
    assert!(value["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["action"] == "schedule.add" && event["target"] == id));

    let remove = run(&["schedule", "remove", &id[..8], "--json"]);
    assert!(remove.status.success(), "{remove:?}");
    let value: serde_json::Value = serde_json::from_slice(&remove.stdout).unwrap();
    assert_eq!(value["removed"], true);
    let list = run(&["schedule", "list", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert!(value["schedules"].as_array().unwrap().is_empty());

    let audit = run(&["audit", "list", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&audit.stdout).unwrap();
    assert!(value["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["action"] == "schedule.remove" && event["target"] == id));
}

#[test]
fn schedule_add_refuses_an_invalid_expression() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let project_arg = project.path().to_string_lossy().to_string();
    let store = BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "scheduled".into(),
            cwd: project_arg.clone(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    drop(store);
    let add = baton()
        .args([
            "-C",
            &project_arg,
            "schedule",
            "add",
            "--thread",
            &thread.id,
            "--cron",
            "not a cron",
            "--prompt",
            "nightly",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(!add.status.success(), "{add:?}");
    let stderr = String::from_utf8_lossy(&add.stderr);
    assert!(stderr.contains("cron"), "{stderr}");
}
