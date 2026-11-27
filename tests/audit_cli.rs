use std::process::Command;

use baton::grants::{mint, revoke, GrantRequest, Scope};
use baton::store::BatonStore;

fn baton() -> Command {
    Command::new(env!("CARGO_BIN_EXE_baton"))
}

#[test]
fn memory_writes_are_recorded_with_their_ids() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let project_arg = project.path().to_string_lossy().to_string();
    let run = |args: &[&str]| {
        baton()
            .args(args)
            .env("BATON_HOME", home.path())
            .output()
            .unwrap()
    };

    let add = run(&["-C", &project_arg, "memories", "add", "--json", "Use tabs"]);
    assert!(add.status.success(), "{add:?}");
    let value: serde_json::Value = serde_json::from_slice(&add.stdout).unwrap();
    let id = value["id"].as_i64().unwrap().to_string();
    let remove = run(&["-C", &project_arg, "memories", "remove", &id, "--json"]);
    assert!(remove.status.success(), "{remove:?}");
    let global = run(&[
        "-C",
        &project_arg,
        "memories",
        "add",
        "--global",
        "Answer briefly",
    ]);
    assert!(global.status.success(), "{global:?}");
    let clear = run(&["-C", &project_arg, "memories", "clear", "--global", "--yes"]);
    assert!(clear.status.success(), "{clear:?}");

    let audit = run(&["audit", "list", "--json"]);
    assert!(audit.status.success(), "{audit:?}");
    let value: serde_json::Value = serde_json::from_slice(&audit.stdout).unwrap();
    let events = value["events"].as_array().unwrap();
    assert_eq!(events.len(), 4, "{events:?}");
    assert_eq!(events[0]["action"], "memory.clear");
    assert_eq!(events[0]["target"], "global");
    assert_eq!(events[1]["action"], "memory.add");
    assert_eq!(events[2]["action"], "memory.remove");
    assert_eq!(events[2]["target"], id);
    assert_eq!(events[3]["action"], "memory.add");
    assert_eq!(events[3]["target"], id);
    assert!(events.iter().all(|event| event["actor"] == "local_cli"));
    assert!(events.iter().all(|event| event["scope"] == "settings"));
    assert!(events
        .iter()
        .all(|event| event["environment"].as_str().unwrap().len() == 36));
}

#[test]
fn audit_list_reports_grant_events_and_respects_the_limit() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("baton.sqlite");
    let store = BatonStore::open(&path).unwrap();
    let minted = mint(&store, home.path(), &GrantRequest::new(vec![Scope::Read])).unwrap();
    drop(store);

    let output = baton()
        .args(["audit", "list", "--json"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    let events = value["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["action"], "grant.mint");
    assert_eq!(events[0]["target"], minted.summary.grant_id);
    assert_eq!(events[0]["actor"], "host_terminal");
    assert_eq!(events[0]["confirmation"], "host_terminal");
    assert_eq!(events[0]["result"], "ok");
    assert_eq!(events[0]["retention"], "durable");
    assert_eq!(events[0]["payload_digest"].as_str().unwrap().len(), 64);
    assert!(!events[0]["environment"].as_str().unwrap().is_empty());
    assert!(!stdout.contains(&minted.secret));

    let output = baton()
        .args(["audit", "list"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("grant.mint"), "{stdout}");
    assert!(stdout.contains("target="), "{stdout}");
    assert!(stdout.contains("actor=host_terminal"), "{stdout}");
    assert!(!stdout.contains(&minted.secret));

    let store = BatonStore::open(&path).unwrap();
    assert!(revoke(&store, home.path(), &minted.summary.grant_id).unwrap());
    drop(store);
    let output = baton()
        .args(["audit", "list", "--limit", "1", "--json"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["events"].as_array().unwrap().len(), 1);
    assert_eq!(value["events"][0]["action"], "grant.revoke");
    assert_eq!(value["events"][0]["target"], minted.summary.grant_id);
    assert_eq!(value["events"][0]["actor"], "local_cli");

    let empty = tempfile::tempdir().unwrap();
    let output = baton()
        .args(["audit", "list"])
        .env("BATON_HOME", empty.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("no audit events yet"));

    let output = baton()
        .args(["audit", "list", "--limit", "0"])
        .env("BATON_HOME", empty.path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("between 1 and 1000"));
}
