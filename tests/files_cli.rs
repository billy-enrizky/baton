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
    std::fs::write(dir.path().join("src/parser.rs"), "pub fn parse() {}\n").unwrap();
    std::fs::write(dir.path().join("README.md"), "# project\n").unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname='p'\n").unwrap();
    std::fs::write(dir.path().join("ignored.log"), "ignored\n").unwrap();
    std::fs::write(dir.path().join(".gitignore"), "ignored.log\n").unwrap();
    dir
}

#[test]
fn files_json_reports_paths_limit_query_and_ignored() {
    let project = init_project();
    let home = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_baton");
    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "files",
            "--json",
            "--limit",
            "2",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
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
    assert_eq!(value["query"], "");
    assert_eq!(value["ignored"], false);
    assert_eq!(value["limit"], 2);
    assert_eq!(value["capped"], false);
    assert_eq!(value["count"], 2);
    assert_eq!(value["truncated"], true);
    let paths = value["files"].as_array().unwrap();
    assert!(paths
        .iter()
        .all(|path| path.as_str().unwrap() != "ignored.log"));

    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "files",
            "parser",
            "--ignored",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["query"], "parser");
    assert_eq!(value["ignored"], true);
    assert_eq!(value["limit"], 50);
    assert_eq!(value["files"], serde_json::json!(["src/parser.rs"]));

    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "files",
            "--ignored",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path == "ignored.log"));
}

#[cfg(unix)]
#[test]
fn open_print_uses_the_editor_and_rejects_escape_paths() {
    let project = init_project();
    let home = tempfile::tempdir().unwrap();
    let file = project.path().join("src/parser.rs");
    let expected = file.canonicalize().unwrap();
    let binary = env!("CARGO_BIN_EXE_baton");
    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "open",
            "src/parser.rs",
            "--print",
        ])
        .env("BATON_HOME", home.path())
        .env("VISUAL", "code --wait")
        .env_remove("EDITOR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed = String::from_utf8(output.stdout).unwrap();
    assert!(printed.starts_with("code --wait "), "{printed}");
    assert!(printed.contains(expected.to_str().unwrap()), "{printed}");

    let outside = project.path().parent().unwrap().join("outside.rs");
    std::fs::write(&outside, "outside\n").unwrap();
    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "open",
            outside.to_str().unwrap(),
            "--print",
        ])
        .env("BATON_HOME", home.path())
        .env("EDITOR", "vim")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("outside the -C directory"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn open_prefers_visual_over_editor_in_a_real_process() {
    let project = init_project();
    let home = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_baton");
    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "open",
            "README.md",
            "--print",
        ])
        .env("BATON_HOME", home.path())
        .env("VISUAL", "nvim -f")
        .env("EDITOR", "vim")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed = String::from_utf8(output.stdout).unwrap();
    let target = project.path().join("README.md").canonicalize().unwrap();
    assert_eq!(printed, format!("nvim -f {}\n", target.display()));
}

#[test]
fn files_scope_to_the_current_directory() {
    let project = init_project();
    std::fs::create_dir_all(project.path().join("docs")).unwrap();
    std::fs::write(project.path().join("docs/guide.md"), "# guide\n").unwrap();
    let home = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_baton");
    let output = Command::new(binary)
        .args([
            "-C",
            project.path().join("docs").to_str().unwrap(),
            "files",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["files"], serde_json::json!(["guide.md"]));

    let output = Command::new(binary)
        .args([
            "-C",
            project.path().join("docs").to_str().unwrap(),
            "open",
            "../README.md",
            "--print",
        ])
        .env("BATON_HOME", home.path())
        .env("VISUAL", "vim")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("outside the -C directory"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn files_fall_back_to_git_when_ripgrep_is_missing() {
    let project = init_project();
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
        .args(["-C", project.path().to_str().unwrap(), "files", "--json"])
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
    let files = value["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(files.contains(&"src/parser.rs"), "{files:?}");
    assert!(files.contains(&"README.md"), "{files:?}");

    let empty = empty_project();
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["-C", empty.path().to_str().unwrap(), "files", "--json"])
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
    assert_eq!(value["files"], serde_json::json!([]));
}

#[test]
fn file_listings_survive_a_closed_pipe() {
    let project = init_project();
    let home = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_baton");
    for json in [false, true] {
        let mut command = Command::new(binary);
        command.args(["-C", project.path().to_str().unwrap(), "files"]);
        if json {
            command.arg("--json");
        }
        let mut child = command
            .env("BATON_HOME", home.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take());
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "json={json} {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn empty_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    dir
}

#[test]
fn empty_pruned_and_ignored_only_projects_list_cleanly() {
    let project = empty_project();
    let home = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_baton");
    let run = |extra: &[&str]| {
        let mut args = vec!["-C", project.path().to_str().unwrap(), "files", "--json"];
        args.extend_from_slice(extra);
        let output = Command::new(binary)
            .args(&args)
            .env("BATON_HOME", home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };

    let value = run(&[]);
    assert_eq!(value["files"], serde_json::json!([]));
    assert_eq!(value["count"], 0);
    assert_eq!(value["truncated"], false);

    std::fs::create_dir_all(project.path().join("target/debug")).unwrap();
    std::fs::write(project.path().join("target/debug/baton"), "bin\n").unwrap();
    let exclude = project.path().join(".git/info/exclude");
    std::fs::write(&exclude, "target/\n").unwrap();
    let value = run(&[]);
    assert_eq!(value["files"], serde_json::json!([]));
    let value = run(&["--ignored"]);
    assert_eq!(value["files"], serde_json::json!([]));

    std::fs::write(project.path().join(".env"), "TOKEN=1\n").unwrap();
    std::fs::write(&exclude, "target/\n.env\n").unwrap();
    let value = run(&[]);
    assert_eq!(value["files"], serde_json::json!([]));
    let value = run(&["--ignored"]);
    assert_eq!(value["files"], serde_json::json!([".env"]));

    std::fs::write(project.path().join("notes.txt"), "notes\n").unwrap();
    let output = Command::new(binary)
        .args([
            "-C",
            project.path().join("notes.txt").to_str().unwrap(),
            "files",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not a directory"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ignored_files_reach_the_default_limit() {
    let project = init_project();
    for index in 0..60 {
        std::fs::write(
            project.path().join(format!("src/gen{index}.rs")),
            "// generated\n",
        )
        .unwrap();
    }
    std::fs::write(project.path().join(".env"), "TOKEN=1\n").unwrap();
    std::fs::write(project.path().join(".gitignore"), "ignored.log\n.env\n").unwrap();
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "files",
            "--ignored",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let files = value["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(files.contains(&".env"), "{files:?}");
    assert!(files.contains(&"src/gen0.rs"), "{files:?}");
    assert_eq!(value["truncated"], true);
}

#[cfg(unix)]
#[test]
fn open_runs_the_editor_and_propagates_failure() {
    use std::os::unix::fs::PermissionsExt;

    let project = init_project();
    let home = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    let log = tools.path().join("argv.txt");
    let editor = tools.path().join("editor.sh");
    std::fs::write(
        &editor,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let binary = env!("CARGO_BIN_EXE_baton");

    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "open",
            "src/parser.rs",
        ])
        .env("BATON_HOME", home.path())
        .env("VISUAL", editor.to_str().unwrap())
        .env_remove("EDITOR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let recorded = std::fs::read_to_string(&log).unwrap();
    let expected = project.path().join("src/parser.rs").canonicalize().unwrap();
    assert_eq!(recorded.trim(), expected.to_str().unwrap());

    let failing = tools.path().join("fail.sh");
    std::fs::write(&failing, "#!/bin/sh\nexit 7\n").unwrap();
    std::fs::set_permissions(&failing, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "open",
            "src/parser.rs",
        ])
        .env("BATON_HOME", home.path())
        .env("VISUAL", failing.to_str().unwrap())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("exited with"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "open",
            "src/parser.rs",
        ])
        .env("BATON_HOME", home.path())
        .env_remove("VISUAL")
        .env_remove("EDITOR")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no editor configured"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn json_cwd_renders_a_non_utf8_directory_lossily() {
    use std::os::unix::ffi::OsStrExt;

    let parent = tempfile::tempdir().unwrap();
    let dir = parent
        .path()
        .join(std::ffi::OsStr::from_bytes(b"baton-\xFF-dir"));
    std::fs::create_dir(&dir).unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(status.success());
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["files", "--json"])
        .current_dir(&dir)
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["cwd"].as_str().unwrap().contains("baton-"));
}

#[test]
fn files_match_subsequence_tokens() {
    let project = init_project();
    std::fs::create_dir_all(project.path().join("src/parser")).unwrap();
    std::fs::write(
        project.path().join("src/parser/mod.rs"),
        "pub fn parse() {}\n",
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_baton");
    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "files",
            "pmod",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["files"], serde_json::json!(["src/parser/mod.rs"]));

    for query in [["pmod", "rs"], ["rs", "pmod"]] {
        let output = Command::new(binary)
            .args([
                "-C",
                project.path().to_str().unwrap(),
                "files",
                query[0],
                query[1],
                "--json",
            ])
            .env("BATON_HOME", home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["files"], serde_json::json!(["src/parser/mod.rs"]));
    }

    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "files",
            "pmod",
            "rz",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["files"], serde_json::json!([]));

    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "files",
            "dopm",
            "--json",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["files"], serde_json::json!([]));
}

#[cfg(unix)]
#[test]
fn open_records_a_recent_file_for_files_ranking() {
    let project = init_project();
    let home = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_baton");
    let output = Command::new(binary)
        .args([
            "-C",
            project.path().to_str().unwrap(),
            "open",
            "README.md",
            "--print",
        ])
        .env("BATON_HOME", home.path())
        .env("EDITOR", "vim")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = Command::new(binary)
        .args(["-C", project.path().to_str().unwrap(), "files", "--json"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["files"][0], "README.md");
}
