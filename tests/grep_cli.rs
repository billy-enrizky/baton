use std::process::{Command, Stdio};

fn init_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "pub fn helper() {}\n").unwrap();
    std::fs::write(dir.path().join("src/words.txt"), "Alpha\nalpha\nalphabet\n").unwrap();
    std::fs::write(dir.path().join("README.md"), "# hello\n").unwrap();
    dir
}

fn run(project: &tempfile::TempDir, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["-C", project.path().to_str().unwrap(), "grep"])
        .args(args)
        .env("BATON_HOME", tempfile::tempdir().unwrap().path())
        .output()
        .unwrap()
}

#[test]
fn grep_json_reports_matches_and_flags() {
    let project = init_project();
    let output = run(&project, &["hello", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(
        value["cwd"],
        project.path().canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(value["query"], "hello");
    assert_eq!(value["case_sensitive"], false);
    assert_eq!(value["word"], false);
    assert_eq!(value["regex"], false);
    assert_eq!(value["limit"], 50);
    assert_eq!(value["truncated"], false);
    assert_eq!(value["count"], 2);
    assert_eq!(
        value["matches"],
        serde_json::json!([
            {"path": "README.md", "line": 1, "text": "# hello"},
            {"path": "src/main.rs", "line": 2, "text": "    println!(\"hello\");"}
        ])
    );
}

#[test]
fn grep_plain_honors_case_word_and_regex() {
    let project = init_project();
    let output = run(&project, &["alpha", "--case-sensitive"]);
    assert!(output.status.success());
    let plain = String::from_utf8(output.stdout).unwrap();
    assert!(plain.contains("src/words.txt:2: alpha"), "{plain}");
    assert!(plain.contains("src/words.txt:3: alphabet"), "{plain}");
    assert!(!plain.contains("src/words.txt:1:"), "{plain}");

    let output = run(&project, &["alpha", "--word", "--json"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["count"], 2);
    let texts = value["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|matched| matched["text"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(texts, vec!["Alpha", "alpha"]);

    let output = run(&project, &["al.ha", "--regex", "--json"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["count"], 3);
}

#[test]
fn grep_no_matches_succeed_and_an_empty_query_fails() {
    let project = init_project();
    let output = run(&project, &["absent", "--json"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["count"], 0);
    assert_eq!(value["matches"], serde_json::json!([]));

    let output = run(&project, &[]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("non-empty query"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn grep_survives_a_closed_pipe() {
    let project = init_project();
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["-C", project.path().to_str().unwrap(), "grep", "hello"])
        .env("BATON_HOME", home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn grep_falls_back_to_git_when_ripgrep_is_missing() {
    let project = init_project();
    std::fs::write(
        project.path().join("src/extra.txt"),
        "needle one\nneedle two\n",
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    let located = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(located.status.success());
    let git = String::from_utf8(located.stdout)
        .unwrap()
        .trim()
        .to_string();
    std::os::unix::fs::symlink(&git, tools.path().join("git")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "grep",
            "needle",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .env("PATH", tools.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["count"], 2);
    assert_eq!(
        value["matches"],
        serde_json::json!([
            {"path": "src/extra.txt", "line": 1, "text": "needle one"},
            {"path": "src/extra.txt", "line": 2, "text": "needle two"}
        ])
    );
}
