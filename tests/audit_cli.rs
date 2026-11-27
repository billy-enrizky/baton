use std::process::Command;

use baton::grants::{mint, revoke, GrantRequest, Scope};
use baton::store::BatonStore;

fn baton() -> Command {
    Command::new(env!("CARGO_BIN_EXE_baton"))
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
