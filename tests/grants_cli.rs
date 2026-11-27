use baton::grants::{mint, GrantRefusal, GrantRequest, Scope};
use baton::store::BatonStore;
use std::process::Command;

fn run(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(args)
        .env("BATON_HOME", home)
        .output()
        .unwrap()
}

#[test]
fn grant_list_reports_grants_without_secrets() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("baton.sqlite");
    {
        let store = BatonStore::open(&db).unwrap();
        mint(&store, home.path(), &GrantRequest::new(vec![Scope::Read])).unwrap();
        let mut request = GrantRequest::new(vec![Scope::Operate]);
        request.label = Some("evil\u{1b}[31m\nrow".into());
        mint(&store, home.path(), &request).unwrap();
    }
    let output = run(home.path(), &["grant", "list", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let grants = value["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 2);
    assert_eq!(grants[0]["scopes"], serde_json::json!(["read"]));
    assert_eq!(grants[0]["state"], "live");
    assert_eq!(grants[1]["label"], "evil\u{1b}[31m\nrow");
    let raw = String::from_utf8_lossy(&output.stdout);
    assert!(!raw.contains("\"secret\""), "{raw}");
    assert!(!raw.contains("secret_hash"), "{raw}");

    let output = run(home.path(), &["grant", "list"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(grants[0]["grant_id"].as_str().unwrap()),
        "{text}"
    );
    assert!(text.contains("default"), "{text}");
    assert!(text.contains("live"), "{text}");
    assert!(text.contains("evil"), "{text}");
    assert!(
        !text.contains('\u{1b}'),
        "the label cannot drive the terminal"
    );
    assert!(!text.contains("\nrow"), "the label stays on one line");
    assert!(!text.contains("secret"), "{text}");
}

#[test]
fn grant_renew_extends_the_window_and_reports_refusals() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("baton.sqlite");
    let minted = {
        let store = BatonStore::open(&db).unwrap();
        let mut request = GrantRequest::new(vec![Scope::Read]);
        request.expires_at = baton::grants::expiry_after_days(2);
        baton::grants::mint(&store, home.path(), &request).unwrap()
    };
    let original = minted.summary.expires_at.clone();

    let output = run(
        home.path(),
        &["grant", "renew", &minted.summary.grant_id, "--days", "45"],
    );
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("renewed"), "{text}");
    assert!(!text.contains(&minted.secret), "{text}");
    assert!(
        text.contains(&original),
        "the text form shows both dates: {text}"
    );

    let store = BatonStore::open_read_only(&db).unwrap();
    let row = store
        .find_grant_row(&minted.summary.grant_id)
        .unwrap()
        .unwrap();
    let extended = row
        .expires_at
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    assert!(
        extended > chrono::Utc::now() + chrono::Duration::days(44),
        "{extended}"
    );
    assert!(
        baton::grants::verify(&store, &minted.summary.grant_id, &minted.secret).is_ok(),
        "the client secret still works"
    );
    drop(store);

    let output = run(
        home.path(),
        &[
            "grant",
            "renew",
            &minted.summary.grant_id,
            "--days",
            "10",
            "--json",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["grant_id"], minted.summary.grant_id);
    assert_eq!(value["previous_expires_at"], row.expires_at);
    let unchanged = value["expires_at"].as_str().unwrap();
    assert_eq!(
        unchanged, row.expires_at,
        "a shorter request keeps the window"
    );

    let output = run(
        home.path(),
        &["grant", "renew", "00000000-0000-4000-8000-000000000000"],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no grant matches"),
        "{output:?}"
    );

    let output = run(home.path(), &["grant", "renew", "not-a-uuid"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("lowercase UUID"),
        "{output:?}"
    );

    {
        let store = BatonStore::open(&db).unwrap();
        assert!(baton::grants::revoke(&store, home.path(), &minted.summary.grant_id).unwrap());
    }
    let output = run(home.path(), &["grant", "renew", &minted.summary.grant_id]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("revoked"),
        "{output:?}"
    );

    let output = run(home.path(), &["audit", "list", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let actions: Vec<&str> = value["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        vec!["grant.revoke", "grant.renew", "grant.renew", "grant.mint"]
    );
}

#[test]
fn grant_revoke_removes_the_secret_and_refuses_unknown_ids() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("baton.sqlite");
    let minted = {
        let store = BatonStore::open(&db).unwrap();
        mint(&store, home.path(), &GrantRequest::new(vec![Scope::Read])).unwrap()
    };
    let secret = home.path().join("grants").join(&minted.summary.grant_id);
    assert!(secret.exists());

    let output = run(home.path(), &["grant", "revoke", &minted.summary.grant_id]);
    assert!(output.status.success(), "{output:?}");
    assert!(!secret.exists(), "revoke deletes the client secret");
    let store = BatonStore::open_read_only(&db).unwrap();
    let refused =
        baton::grants::verify(&store, &minted.summary.grant_id, &minted.secret).unwrap_err();
    assert_eq!(
        refused.downcast_ref::<GrantRefusal>(),
        Some(&GrantRefusal::Revoked)
    );

    let output = run(
        home.path(),
        &["grant", "revoke", "00000000-0000-4000-8000-000000000000"],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no grant matches"), "{stderr}");
}
