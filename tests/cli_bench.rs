use std::process::Command;

#[test]
fn bench_startup_reports_phases_as_json() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["bench", "startup", "--json", "--repeat", "2"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "bench exited {:?}", output.status);

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["target"], "startup");
    assert_eq!(value["repeat"], 2);
    assert_eq!(value["samples"].as_array().unwrap().len(), 2);
    for key in ["config", "store", "reap", "hydrate", "total"] {
        let median = value["median_ms"][key].as_f64().unwrap();
        assert!(median >= 0.0, "{key} median was {median}");
    }
    assert!(value["process_ms"].as_f64().unwrap() > 0.0);
}

#[test]
fn bench_rejects_an_unknown_target() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["bench", "everything"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("only startup is available"),
        "stderr: {stderr}"
    );
}
