use baton::handoff::{build_handoff, handoff_prompt, wrap_prompt_with_handoff, HandoffMessage};
use baton::store::{BatonStore, ImportedMessage, NewThread, PinMoveOutcome};
use baton::types::{PermissionMode, ProviderId, SessionMode, TurnPhase, TurnUsage};
use baton::usage::{cache_hit_rate, format_token_count, format_usage_line, sum_usage};
use baton::scanner::{parse_claude_transcript, parse_codex_transcript};
use std::io::Write;
use std::path::Path;

fn message(role: &str, text: &str) -> HandoffMessage {
    HandoffMessage {
        role: role.to_string(),
        provider: if role == "assistant" { Some("claude".to_string()) } else { None },
        model: if role == "assistant" { Some("m".to_string()) } else { None },
        text: text.to_string(),
    }
}

#[test]
fn draft_thread_persists_its_session_mode() {
    let store = BatonStore::memory().unwrap();
    let mut thread = store.draft_thread(NewThread {
        title: "orchestrate".to_string(),
        cwd: "/tmp/project".to_string(),
        current_provider: ProviderId::Codex,
        current_model: None,
    });
    thread.session_mode = SessionMode::Orchestrate;
    baton::runtime::ensure_persisted(&store, &mut thread).unwrap();
    assert_eq!(
        store.get_thread(&thread.id).unwrap().unwrap().session_mode,
        SessionMode::Orchestrate
    );
}

#[test]
fn default_session_mode_is_orchestrate_and_config_can_opt_out() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        baton::config::get_default_session_mode(home.path()),
        SessionMode::Orchestrate
    );
    std::fs::write(home.path().join("config.json"), r#"{"defaultMode":"single"}"#).unwrap();
    assert_eq!(
        baton::config::get_default_session_mode(home.path()),
        SessionMode::Single
    );
}

#[test]
fn roles_json_reports_overrides_and_hook_declarations() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("roles.json"),
        r#"{"roles":{"orchestrator":{"provider":"codex","model":"rits/deepseek-ai/DeepSeek-V4.1-Flash-1017"},"planner":{"provider":"codex","model":"rits/deepseek-ai/DeepSeek-V4.1-Flash-1017"},"implementor":{"provider":"codex","model":"rits/deepseek-ai/DeepSeek-V4.1-Flash-1017","effort":"high"},"reviewer":{"provider":"codex","model":"rits/deepseek-ai/DeepSeek-V4.1-Flash-1017"}},"hooks":{"before_plan":[{"argv":["true"],"timeout_secs":5}]}}"#,
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["roles", "--json"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["roles"].as_array().unwrap().len(), 4);
    assert_eq!(value["roles"][0]["provider"], "codex");
    assert_eq!(
        value["roles"][0]["model"],
        "rits/deepseek-ai/DeepSeek-V4.1-Flash-1017"
    );
    assert_eq!(value["roles"][0]["config_source"], "user");
    assert_eq!(value["roles"][0]["prompt_source"], "default");
    assert!(value["roles"][0]["prompt_path"]
        .as_str()
        .unwrap()
        .ends_with("/roles/orchestrator.md"));
    assert_eq!(value["roles"][2]["effort"], "high");
    assert_eq!(value["hooks"]["before_plan"][0]["argv"][0], "true");
    assert_eq!(value["hooks"]["before_plan"][0]["timeout_secs"], 5);
}

#[test]
fn orchestrate_plan_only_uses_the_orchestrator_role_for_greetings() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("roles.json"),
        r#"{"roles":{"orchestrator":{"provider":"codex","model":"orch-model"},"planner":{"provider":"claude","model":"plan-model"}}}"#,
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "-C",
            repo.path().to_str().unwrap(),
            "orchestrate",
            "--plan-only",
            "--json",
            "hi",
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
    assert_eq!(value["coordinator"]["provider"], "codex");
    assert_eq!(value["coordinator"]["model"], "orch-model");
    assert_eq!(value["tasks"][0]["provider"], "codex");
    assert_eq!(value["tasks"][0]["model"], "orch-model");
    assert_eq!(value["tasks"][0]["prompt"], "hi");

    let rejected = std::process::Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "-C",
            repo.path().to_str().unwrap(),
            "--provider",
            "codex",
            "orchestrate",
            "--plan-only",
            "--json",
            "hi",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("uses role settings"));

    let plan_path = repo.path().join("plan.json");
    std::fs::write(
        &plan_path,
        r#"{"tasks":[{"title":"one","provider":"codex","prompt":"do it"}]}"#,
    )
    .unwrap();
    let non_git = std::process::Command::new(env!("CARGO_BIN_EXE_baton"))
        .args([
            "-C",
            repo.path().to_str().unwrap(),
            "orchestrate",
            "--plan-file",
            plan_path.to_str().unwrap(),
            "do it",
        ])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(!non_git.status.success());
    assert!(String::from_utf8_lossy(&non_git.stderr).contains("requires a git repository"));
}

#[test]
fn shipped_worktree_branches_are_only_the_ones_with_pr_urls() {
    let store = BatonStore::memory().unwrap();
    store
        .create_orchestration_run(
            "run-1",
            "goal",
            ProviderId::Codex,
            "model",
            None,
            "{}",
            false,
        )
        .unwrap();
    for (id, branch, pr) in [
        ("task-1", "baton/run-1/one", Some("https://example.test/pr/1")),
        ("task-2", "baton/run-1/two", None),
    ] {
        store
            .add_orchestration_task(
                id,
                "run-1",
                1,
                id,
                ProviderId::Codex,
                "model",
                "prompt",
                Some("/tmp/worktree"),
                Some(branch),
                Some("commit"),
                None,
                "completed",
                None,
                None,
                "now",
                Some("now"),
                None,
                None,
                None,
            )
            .unwrap();
        if let Some(pr) = pr {
            store.mark_orchestration_task_pr(id, pr).unwrap();
        }
    }
    assert_eq!(
        store.shipped_worktree_branches().unwrap(),
        vec!["baton/run-1/one".to_string()]
    );
}

#[test]
fn worker_followup_updates_task_and_clears_merge_and_pr_markers() {
    let store = BatonStore::memory().unwrap();
    store
        .create_orchestration_run(
            "run-followup",
            "goal",
            ProviderId::Codex,
            "model",
            None,
            "{}",
            false,
        )
        .unwrap();
    store
        .add_orchestration_task(
            "task-followup",
            "run-followup",
            1,
            "worker task",
            ProviderId::Codex,
            "model",
            "prompt",
            Some("/tmp/worktree"),
            Some("baton/run-followup/worker-task"),
            Some("old-commit"),
            Some("thread-worker"),
            "completed",
            Some("old text"),
            None,
            "now",
            Some("now"),
            None,
            None,
            None,
        )
        .unwrap();
    store
        .mark_orchestration_task_merged("task-followup", "merged")
        .unwrap();
    store
        .mark_orchestration_task_pr("task-followup", "https://example.test/pr/1")
        .unwrap();
    store
        .finish_orchestration_run("run-followup", "completed")
        .unwrap();

    let task = store
        .orchestration_task_for_thread("thread-worker")
        .unwrap()
        .unwrap();
    store
        .update_orchestration_task_success(
            &task.id,
            Some("follow-up text"),
            Some("new-commit"),
            true,
        )
        .unwrap();
    store
        .refresh_orchestration_run_status("run-followup")
        .unwrap();

    let task = store
        .orchestration_task_for_thread("thread-worker")
        .unwrap()
        .unwrap();
    assert_eq!(task.status, "completed");
    assert_eq!(task.text.as_deref(), Some("follow-up text"));
    assert_eq!(task.commit_sha.as_deref(), Some("new-commit"));
    assert!(task.merged_at.is_none(), "a new commit is no longer merged");
    assert!(task.pr_url.is_none(), "a new commit must be pushed again");
    assert_eq!(
        task.review_status.as_deref(),
        Some("pending"),
        "a new commit must be reviewed again"
    );
    assert_eq!(
        store.list_orchestration_runs(10).unwrap()[0].status,
        "needs_review"
    );

    store
        .mark_orchestration_task_merged("task-followup", "merged")
        .unwrap();
    store
        .mark_orchestration_task_pr("task-followup", "https://example.test/pr/2")
        .unwrap();
    store
        .update_orchestration_task_success(
            &task.id,
            Some("no file changes"),
            None,
            false,
        )
        .unwrap();
    let task = store
        .orchestration_task_for_thread("thread-worker")
        .unwrap()
        .unwrap();
    assert_eq!(
        task.commit_sha.as_deref(),
        Some("new-commit"),
        "a no-change follow-up keeps the existing commit"
    );
    assert!(task.merged_at.is_some());
    assert!(task.pr_url.is_some());
}

#[test]
fn worker_failure_without_a_commit_updates_task_and_run_status() {
    let store = BatonStore::memory().unwrap();
    store
        .create_orchestration_run(
            "run-failure",
            "goal",
            ProviderId::Codex,
            "model",
            None,
            "{}",
            false,
        )
        .unwrap();
    store
        .add_orchestration_task(
            "task-failure",
            "run-failure",
            1,
            "worker task",
            ProviderId::Codex,
            "model",
            "prompt",
            Some("/tmp/worktree"),
            Some("baton/run-failure/worker-task"),
            None,
            Some("thread-failure"),
            "completed",
            None,
            None,
            "now",
            Some("now"),
            None,
            None,
            None,
        )
        .unwrap();
    store
        .finish_orchestration_run("run-failure", "completed")
        .unwrap();

    store
        .update_orchestration_task_failure(
            "task-failure",
            "failed",
            Some("last attempt"),
            "boom",
        )
        .unwrap();
    store.refresh_orchestration_run_status("run-failure").unwrap();
    let task = store
        .orchestration_task_for_thread("thread-failure")
        .unwrap()
        .unwrap();
    assert_eq!(task.status, "failed");
    assert_eq!(task.error.as_deref(), Some("boom"));
    assert_eq!(task.text.as_deref(), Some("last attempt"));
    assert!(task.commit_sha.is_none());
    assert_eq!(
        store.list_orchestration_runs(10).unwrap()[0].status,
        "failed"
    );

    store
        .update_orchestration_task_failure("task-failure", "cancelled", None, "interrupted")
        .unwrap();
    store.refresh_orchestration_run_status("run-failure").unwrap();
    assert_eq!(
        store.list_orchestration_runs(10).unwrap()[0].status,
        "cancelled"
    );
}

#[test]
fn pending_native_rewind_blocks_other_turns_and_clears_on_retry() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "rewind".to_string(),
            cwd: "/tmp/project".to_string(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    store
        .add_message(&thread.id, 1, "user", None, None, "one")
        .unwrap();
    store
        .add_message(&thread.id, 2, "user", None, None, "two")
        .unwrap();
    store
        .set_native_rewind_pending(&thread.id, ProviderId::Codex, 2)
        .unwrap();
    let error = store.rewind_thread(&thread.id, 3).unwrap_err();
    assert!(
        error.to_string().contains("pending codex native rewind to turn 2"),
        "{error}"
    );
    assert_eq!(
        store
            .native_rewind_pending(&thread.id)
            .unwrap()
            .map(|pending| pending.turn),
        Some(2)
    );

    store
        .rewind_thread_keeping_provider_sessions(&thread.id, 2)
        .unwrap();
    assert!(store.native_rewind_pending(&thread.id).unwrap().is_none());
    let messages = store.list_messages(&thread.id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "one");
}

#[test]
fn store_crud_and_thread_lifecycle() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "New thread".to_string(),
            cwd: "/tmp/project".to_string(),
            current_provider: ProviderId::Claude,
            current_model: None,
        })
        .unwrap();
    assert_eq!(store.get_thread(&thread.id).unwrap().unwrap().title, "New thread");
    assert_eq!(store.list_threads(Some("/tmp/project")).unwrap().len(), 1);
    assert_eq!(store.list_threads(Some("/tmp/other")).unwrap().len(), 0);

    assert_eq!(thread.session_mode, SessionMode::Single);
    store
        .set_thread_session_mode(&thread.id, SessionMode::Orchestrate)
        .unwrap();
    assert_eq!(
        store.get_thread(&thread.id).unwrap().unwrap().session_mode,
        SessionMode::Orchestrate
    );

    store.set_thread_provider(&thread.id, ProviderId::Codex, Some("gpt-6.1-sol".to_string())).unwrap();
    let updated = store.get_thread(&thread.id).unwrap().unwrap();
    assert_eq!(updated.current_provider, ProviderId::Codex);
    assert_eq!(updated.current_model.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(store.next_turn_number(&thread.id).unwrap(), 1);

    store.add_message(&thread.id, 1, "user", Some("codex"), Some("gpt-6.1-sol"), "hello").unwrap();
    store.add_message(&thread.id, 1, "assistant", Some("codex"), Some("gpt-6.1-sol"), "hi").unwrap();
    assert_eq!(store.next_turn_number(&thread.id).unwrap(), 2);
    assert_eq!(store.list_messages(&thread.id).unwrap().len(), 2);
}

#[test]
fn thread_pins_persist_and_sort_before_recent_threads() {
    let store = BatonStore::memory().unwrap();
    let older = store
        .create_thread(NewThread {
            title: "older".to_string(),
            cwd: "/tmp/pins".to_string(),
            current_provider: ProviderId::Claude,
            current_model: None,
        })
        .unwrap();
    let newer = store
        .create_thread(NewThread {
            title: "newer".to_string(),
            cwd: "/tmp/pins".to_string(),
            current_provider: ProviderId::Claude,
            current_model: None,
        })
        .unwrap();

    store.set_thread_pinned(&older.id, true).unwrap();
    let display = store
        .list_threads_for_path_display(std::path::Path::new("/tmp/pins"))
        .unwrap();
    assert_eq!(display[0].id, older.id);
    assert!(display[0].pinned);
    assert!(!display[1].pinned);
    let semantic = store.list_threads(Some("/tmp/pins")).unwrap();
    assert_eq!(semantic[0].id, newer.id, "default resolution stays newest");

    store.set_thread_pinned(&older.id, false).unwrap();
    let display = store
        .list_threads_for_path_display(std::path::Path::new("/tmp/pins"))
        .unwrap();
    assert_eq!(display[0].id, newer.id);
    assert!(!store.get_thread(&older.id).unwrap().unwrap().pinned);
}

#[test]
fn pin_order_keys_sort_and_can_be_swapped() {
    let store = BatonStore::memory().unwrap();
    let mut ids = Vec::new();
    for title in ["first", "second", "third"] {
        ids.push(
            store
                .create_thread(NewThread {
                    title: title.to_string(),
                    cwd: "/tmp/pin-order".to_string(),
                    current_provider: ProviderId::Claude,
                    current_model: None,
                })
                .unwrap()
                .id,
        );
    }
    for id in &ids {
        store.set_thread_pinned(id, true).unwrap();
    }
    let display = store
        .list_threads_for_path_display(std::path::Path::new("/tmp/pin-order"))
        .unwrap();
    assert_eq!(
        display
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec![ids[2].as_str(), ids[1].as_str(), ids[0].as_str()]
    );
    assert!(display.iter().all(|thread| thread.pin_order_key.is_some()));

    assert_eq!(
        store
            .move_thread_pin(&ids[1], std::path::Path::new("/tmp/pin-order"), true)
            .unwrap(),
        PinMoveOutcome::Moved
    );
    let display = store
        .list_threads_for_path_display(std::path::Path::new("/tmp/pin-order"))
        .unwrap();
    assert_eq!(
        display
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec![ids[1].as_str(), ids[2].as_str(), ids[0].as_str()]
    );

    store.set_thread_pinned(&ids[1], false).unwrap();
    assert!(store
        .get_thread(&ids[1])
        .unwrap()
        .unwrap()
        .pin_order_key
        .is_none());
}

#[test]
fn pin_order_key_backfills_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("baton.sqlite");
    let (first_id, second_id, third_id);
    {
        let store = BatonStore::open(&path).unwrap();
        let first = store
            .create_thread(NewThread {
                title: "first".to_string(),
                cwd: "/tmp/pin-backfill".to_string(),
                current_provider: ProviderId::Claude,
                current_model: None,
            })
            .unwrap();
        let second = store
            .create_thread(NewThread {
                title: "second".to_string(),
                cwd: "/tmp/pin-backfill".to_string(),
                current_provider: ProviderId::Claude,
                current_model: None,
            })
            .unwrap();
        let third = store
            .create_thread(NewThread {
                title: "third".to_string(),
                cwd: "/tmp/pin-backfill".to_string(),
                current_provider: ProviderId::Claude,
                current_model: None,
            })
            .unwrap();
        store.set_thread_pinned(&first.id, true).unwrap();
        store.set_thread_pinned(&second.id, true).unwrap();
        store.set_thread_pinned(&third.id, true).unwrap();
        first_id = first.id;
        second_id = second.id;
        third_id = third.id;
    }
    {
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute(
            "UPDATE threads SET pin_order_key = NULL, updated_at = '2026-01-02T03:04:05.006000999Z' WHERE id = ?",
            rusqlite::params![first_id],
        )
        .unwrap();
        raw.execute(
            "UPDATE threads SET pin_order_key = NULL, updated_at = 'not-a-date' WHERE id = ?",
            rusqlite::params![second_id],
        )
        .unwrap();
        raw.execute(
            "UPDATE threads SET pin_order_key = '2026-01-02T03:04:05.006000Z', updated_at = '2000-01-02T03:04:05.006Z' WHERE id = ?",
            rusqlite::params![third_id],
        )
        .unwrap();
    }
    let store = BatonStore::open(&path).unwrap();
    assert_eq!(
        store
            .get_thread(&first_id)
            .unwrap()
            .unwrap()
            .pin_order_key
            .as_deref(),
        Some("2026-01-02T03:04:05.006001Z")
    );
    assert_eq!(
        store
            .get_thread(&second_id)
            .unwrap()
            .unwrap()
            .pin_order_key
            .as_deref(),
        Some("2026-01-02T03:04:05.006002Z")
    );
    assert_eq!(
        store
            .get_thread(&third_id)
            .unwrap()
            .unwrap()
            .pin_order_key
            .as_deref(),
        Some("2026-01-02T03:04:05.006000Z")
    );
    assert_eq!(
        store
            .list_threads_for_path_display(std::path::Path::new("/tmp/pin-backfill"))
            .unwrap()
            .into_iter()
            .map(|thread| thread.id)
            .collect::<Vec<_>>(),
        vec![second_id, first_id.clone(), third_id]
    );
    let created_at = store.get_thread(&first_id).unwrap().unwrap().created_at;
    drop(store);
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE threads SET pin_order_key = NULL, updated_at = 'not-a-date'",
        [],
    )
    .unwrap();
    drop(raw);
    let store = BatonStore::open(&path).unwrap();
    let expected = chrono::DateTime::parse_from_rfc3339(&created_at)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    assert_eq!(
        store
            .get_thread(&first_id)
            .unwrap()
            .unwrap()
            .pin_order_key
            .as_deref(),
        Some(expected.as_str())
    );
    let mut keys = store
        .list_threads_for_path_display(std::path::Path::new("/tmp/pin-backfill"))
        .unwrap()
        .into_iter()
        .filter_map(|thread| thread.pin_order_key)
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 3);
}

#[test]
fn threads_json_reports_pinned() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "pinned".to_string(),
            cwd: work.path().to_string_lossy().to_string(),
            current_provider: ProviderId::Claude,
            current_model: None,
        })
        .unwrap();
    let newer = store
        .create_thread(NewThread {
            title: "newer".to_string(),
            cwd: work.path().to_string_lossy().to_string(),
            current_provider: ProviderId::Claude,
            current_model: None,
        })
        .unwrap();
    store.set_thread_pinned(&thread.id, true).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["-C", work.path().to_str().unwrap(), "threads", "--json"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows[0]["thread_id"], thread.id);
    assert_eq!(rows[0]["pinned"], true);
    assert_eq!(
        rows[0]["pin_order_key"].as_str(),
        store
            .get_thread(&thread.id)
            .unwrap()
            .unwrap()
            .pin_order_key
            .as_deref()
    );

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["-C", work.path().to_str().unwrap(), "status", "--json"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["thread_id"], newer.id);
}

#[test]
fn agents_json_reports_subagent_rows_for_a_thread() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "agents".to_string(),
            cwd: work.path().to_string_lossy().to_string(),
            current_provider: ProviderId::Claude,
            current_model: None,
        })
        .unwrap();
    store
        .add_subagent(
            &thread.id,
            2,
            "claude",
            Some("toolu_1"),
            "Explore",
            Some("Find the parser"),
            "done",
            "2026-01-01T00:00:00Z",
            Some("2026-01-01T00:01:00Z"),
        )
        .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["-t", &thread.id, "agents", "--json"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["thread_id"], thread.id);
    assert_eq!(value["subagents"][0]["name"], "Explore");
    assert_eq!(value["subagents"][0]["status"], "done");
    assert_eq!(value["subagents"][0]["turn"], 2);
    assert_eq!(value["subagents"][0]["call_id"], "toolu_1");

    let missing = std::process::Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["agents"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("needs a thread id"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
}

#[test]
fn thread_pin_survives_store_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("baton.sqlite");
    let expected;
    {
        let store = BatonStore::open(&path).unwrap();
        let mut ids = Vec::new();
        for title in ["first", "second", "third"] {
            let thread = store
                .create_thread(NewThread {
                    title: title.to_string(),
                    cwd: "/tmp/durable-pins".to_string(),
                    current_provider: ProviderId::Claude,
                    current_model: None,
                })
                .unwrap();
            store.set_thread_pinned(&thread.id, true).unwrap();
            ids.push(thread.id);
        }
        store
            .move_thread_pin(&ids[1], std::path::Path::new("/tmp/durable-pins"), true)
            .unwrap();
        expected = store
            .list_threads_for_path_display(std::path::Path::new("/tmp/durable-pins"))
            .unwrap()
            .into_iter()
            .map(|thread| (thread.id, thread.pin_order_key))
            .collect::<Vec<_>>();
    }
    let store = BatonStore::open(&path).unwrap();
    assert_eq!(
        store
            .list_threads_for_path_display(std::path::Path::new("/tmp/durable-pins"))
            .unwrap()[0]
            .id,
        expected[0].0
    );
    assert_eq!(
        store
            .list_threads_for_path_display(std::path::Path::new("/tmp/durable-pins"))
            .unwrap()
            .into_iter()
            .map(|thread| (thread.id, thread.pin_order_key))
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn concurrent_pin_moves_serialize_without_duplicate_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("baton.sqlite");
    let store = BatonStore::open(&path).unwrap();
    let mut ids = Vec::new();
    for title in ["first", "second", "third", "fourth"] {
        let thread = store
            .create_thread(NewThread {
                title: title.to_string(),
                cwd: "/tmp/concurrent-pins".to_string(),
                current_provider: ProviderId::Claude,
                current_model: None,
            })
            .unwrap();
        store.set_thread_pinned(&thread.id, true).unwrap();
        ids.push(thread.id);
    }

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let first_store = BatonStore::open(&path).unwrap();
    let second_store = BatonStore::open(&path).unwrap();
    let first_id = ids[0].clone();
    let second_id = ids[1].clone();
    let first_barrier = barrier.clone();
    let second_barrier = barrier;
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        first_store.move_thread_pin(
            &first_id,
            std::path::Path::new("/tmp/concurrent-pins"),
            true,
        )
    });
    let second = std::thread::spawn(move || {
        second_barrier.wait();
        second_store.move_thread_pin(
            &second_id,
            std::path::Path::new("/tmp/concurrent-pins"),
            true,
        )
    });
    assert_eq!(first.join().unwrap().unwrap(), PinMoveOutcome::Moved);
    assert_eq!(second.join().unwrap().unwrap(), PinMoveOutcome::Moved);

    let display = BatonStore::open(&path)
        .unwrap()
        .list_threads_for_path_display(std::path::Path::new("/tmp/concurrent-pins"))
        .unwrap();
    let order = display
        .iter()
        .map(|thread| thread.id.as_str())
        .collect::<Vec<_>>();
    assert!(
        order
            == vec![
                ids[3].as_str(),
                ids[2].as_str(),
                ids[1].as_str(),
                ids[0].as_str()
            ]
            || order
                == vec![
                    ids[3].as_str(),
                    ids[1].as_str(),
                    ids[0].as_str(),
                    ids[2].as_str()
                ],
        "{order:?}"
    );
    let mut keys = display
        .into_iter()
        .filter_map(|thread| thread.pin_order_key)
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), ids.len());
}

#[test]
fn thread_reasoning_effort_persists_and_forks() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "effort".to_string(),
            cwd: "/tmp/project".to_string(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    store.set_thread_effort(&thread.id, Some("high")).unwrap();
    let stored = store.get_thread(&thread.id).unwrap().unwrap();
    assert_eq!(stored.reasoning_effort.as_deref(), Some("high"));

    let fork = store.fork_thread(&thread.id, "effort fork").unwrap();
    assert_eq!(fork.reasoning_effort.as_deref(), Some("high"));

    store.set_thread_effort(&thread.id, None).unwrap();
    assert_eq!(
        store
            .get_thread(&thread.id)
            .unwrap()
            .unwrap()
            .reasoning_effort,
        None
    );
}

#[test]
fn store_fork_copies_conversation_and_config_but_not_sessions() {
    let store = BatonStore::memory().unwrap();
    let source = store
        .create_thread(NewThread {
            title: "source".into(),
            cwd: "/tmp/project".into(),
            current_provider: ProviderId::Codex,
            current_model: Some("model-1".into()),
        })
        .unwrap();
    store
        .set_thread_permission_mode(&source.id, PermissionMode::Plan)
        .unwrap();
    store
        .set_thread_goal(&source.id, Some("ship the fork"), true)
        .unwrap();
    store
        .add_message(&source.id, 1, "user", Some("codex"), Some("model-1"), "hello")
        .unwrap();
    store
        .add_message(&source.id, 1, "assistant", Some("codex"), Some("model-1"), "hi")
        .unwrap();
    store
        .add_message(&source.id, 1, "tool", Some("codex"), None, "✓ shell ls")
        .unwrap();
    store
        .start_turn_record(&source.id, 1, ProviderId::Codex, Some("model-1"))
        .unwrap();
    store
        .finish_turn_record(&source.id, 1, TurnPhase::Completed, None)
        .unwrap();
    store
        .upsert_provider_session(
            &source.id,
            ProviderId::Codex,
            "native-source",
            Some("model-1".into()),
            None,
        )
        .unwrap();

    let forked = store.fork_thread(&source.id, "branched").unwrap();

    assert_ne!(forked.id, source.id);
    assert_eq!(forked.title, "branched");
    assert_eq!(forked.cwd, source.cwd);
    assert_eq!(forked.current_provider, ProviderId::Codex);
    assert_eq!(forked.current_model.as_deref(), Some("model-1"));
    assert_eq!(forked.permission_mode, PermissionMode::Plan);
    assert_eq!(forked.goal.as_deref(), Some("ship the fork"));
    assert!(forked.goal_paused);
    let source_messages = store.list_messages(&source.id).unwrap();
    let forked_messages = store.list_messages(&forked.id).unwrap();
    assert_eq!(forked_messages.len(), source_messages.len());
    assert_eq!(
        forked_messages
            .iter()
            .map(|message| (message.turn, message.role.as_str(), message.text.as_str()))
            .collect::<Vec<_>>(),
        source_messages
            .iter()
            .map(|message| (message.turn, message.role.as_str(), message.text.as_str()))
            .collect::<Vec<_>>()
    );
    assert!(store
        .get_provider_session(&forked.id, ProviderId::Codex)
        .unwrap()
        .is_none());
    let forked_turns = store.list_turn_records(&forked.id).unwrap();
    assert_eq!(forked_turns.len(), 1);
    assert_eq!(forked_turns[0].phase, TurnPhase::Completed);
    assert_eq!(store.next_turn_number(&forked.id).unwrap(), 2);
    assert_eq!(store.list_messages(&source.id).unwrap().len(), source_messages.len());
}

#[test]
fn store_fork_can_stop_at_a_completed_turn() {
    let store = BatonStore::memory().unwrap();
    let source = store
        .create_thread(NewThread {
            title: "source".into(),
            cwd: "/tmp/project".into(),
            current_provider: ProviderId::Codex,
            current_model: Some("model-1".into()),
        })
        .unwrap();
    for turn in 1..=2 {
        store
            .add_message(
                &source.id,
                turn,
                "user",
                Some("codex"),
                Some("model-1"),
                &format!("user {turn}"),
            )
            .unwrap();
        store
            .add_message(
                &source.id,
                turn,
                "assistant",
                Some("codex"),
                Some("model-1"),
                &format!("assistant {turn}"),
            )
            .unwrap();
        store
            .start_turn_record(&source.id, turn, ProviderId::Codex, Some("model-1"))
            .unwrap();
        store
            .finish_turn_record(&source.id, turn, TurnPhase::Completed, None)
            .unwrap();
    }
    store
        .set_turn_provider_id(&source.id, 1, "provider-turn-1")
        .unwrap();
    store
        .set_turn_provider_id(&source.id, 2, "provider-turn-2")
        .unwrap();
    store
        .start_turn_record(&source.id, 3, ProviderId::Codex, Some("model-1"))
        .unwrap();

    assert_eq!(
        store.fork_provider_turn_id(&source.id, Some(1)).unwrap(),
        Some("provider-turn-1".into())
    );
    assert_eq!(
        store.fork_provider_turn_id(&source.id, Some(2)).unwrap(),
        Some("provider-turn-2".into())
    );
    assert!(store.fork_provider_turn_id(&source.id, None).is_err());
    assert!(store.fork_provider_turn_id(&source.id, Some(3)).is_err());
    assert!(store.fork_provider_turn_id(&source.id, Some(99)).is_err());

    let first = store
        .fork_thread_through_turn(&source.id, "through one", Some(1))
        .unwrap();
    let first_messages = store.list_messages(&first.id).unwrap();
    assert_eq!(first_messages.len(), 2);
    assert!(first_messages.iter().all(|message| message.turn == 1));
    assert_eq!(store.list_turn_records(&first.id).unwrap().len(), 1);
    assert_eq!(store.next_turn_number(&first.id).unwrap(), 2);

    let second = store
        .fork_thread_through_turn(&source.id, "through two", Some(2))
        .unwrap();
    assert_eq!(store.list_messages(&second.id).unwrap().len(), 4);
    assert_eq!(store.list_turn_records(&second.id).unwrap().len(), 2);
    assert_eq!(store.next_turn_number(&second.id).unwrap(), 3);

    assert!(store.fork_thread(&source.id, "whole").is_err());
    assert!(store
        .fork_thread_through_turn(&source.id, "open cutoff", Some(3))
        .is_err());
    assert!(store
        .fork_thread_through_turn(&source.id, "missing cutoff", Some(99))
        .is_err());
}

#[test]
fn store_provider_sessions_per_provider() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "t".into(),
            cwd: "/tmp/x".into(),
            current_provider: ProviderId::Claude,
            current_model: None,
        })
        .unwrap();
    assert!(store.get_provider_session(&thread.id, ProviderId::Codex).unwrap().is_none());
    store
        .upsert_provider_session(&thread.id, ProviderId::Codex, "native-1", Some("gpt-6.1-sol".into()), Some("/rollout.jsonl".into()))
        .unwrap();
    store
        .upsert_provider_session(&thread.id, ProviderId::Claude, "session-a", None, None)
        .unwrap();
    assert_eq!(
        store.get_provider_session(&thread.id, ProviderId::Codex).unwrap().unwrap().native_session_id,
        "native-1"
    );
    store
        .upsert_provider_session(&thread.id, ProviderId::Codex, "native-2", None, None)
        .unwrap();
    let codex = store.get_provider_session(&thread.id, ProviderId::Codex).unwrap().unwrap();
    assert_eq!(codex.native_session_id, "native-2");
    assert_eq!(codex.rollout_path.as_deref(), Some("/rollout.jsonl"));
    assert_eq!(
        store.find_thread_by_native_session(ProviderId::Codex, "native-2").unwrap().unwrap().id,
        thread.id
    );
}

#[test]
fn store_rewind_truncates_and_clears_sessions() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "t".into(),
            cwd: "/tmp/x".into(),
            current_provider: ProviderId::Claude,
            current_model: None,
        })
        .unwrap();
    store.add_message(&thread.id, 1, "user", None, None, "one").unwrap();
    store.add_message(&thread.id, 1, "assistant", None, None, "reply one").unwrap();
    store.add_message(&thread.id, 2, "user", None, None, "two").unwrap();
    store.add_message(&thread.id, 2, "assistant", None, None, "reply two").unwrap();
    store.upsert_provider_session(&thread.id, ProviderId::Claude, "native-a", None, None).unwrap();
    store
        .add_usage(&thread.id, 1, ProviderId::Claude, None, &TurnUsage { input_tokens: 1, output_tokens: 1, cache_read_tokens: 1, cache_creation_tokens: 1, cost_usd: None })
        .unwrap();
    store
        .add_usage(&thread.id, 2, ProviderId::Claude, None, &TurnUsage { input_tokens: 2, output_tokens: 2, cache_read_tokens: 2, cache_creation_tokens: 2, cost_usd: None })
        .unwrap();

    store.start_turn_record(&thread.id, 1, ProviderId::Claude, None).unwrap();
    store.finish_turn_record(&thread.id, 1, TurnPhase::Completed, None).unwrap();
    store.start_turn_record(&thread.id, 2, ProviderId::Claude, None).unwrap();

    store.rewind_thread(&thread.id, 2).unwrap();
    let messages = store.list_messages(&thread.id).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].text, "one");
    assert!(store.get_provider_session(&thread.id, ProviderId::Claude).unwrap().is_none());
    assert_eq!(store.list_usage(Some(&thread.id)).unwrap().len(), 1);
    assert_eq!(store.next_turn_number(&thread.id).unwrap(), 2);
    assert_eq!(store.list_turn_records(&thread.id).unwrap().len(), 1, "rewind removes turn records at or after the cutoff");
}

#[test]
fn rewind_can_keep_a_provider_session_for_native_rollback() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "native rewind".into(),
            cwd: "/tmp/x".into(),
            current_provider: ProviderId::Codex,
            current_model: Some("m".into()),
        })
        .unwrap();
    store
        .add_message(&thread.id, 1, "user", Some("codex"), Some("m"), "one")
        .unwrap();
    store
        .upsert_provider_session(&thread.id, ProviderId::Codex, "native-a", Some("m".into()), None)
        .unwrap();
    store
        .rewind_thread_keeping_provider_sessions(&thread.id, 1)
        .unwrap();
    assert!(store.list_messages(&thread.id).unwrap().is_empty());
    assert_eq!(
        store
            .get_provider_session(&thread.id, ProviderId::Codex)
            .unwrap()
            .unwrap()
            .native_session_id,
        "native-a"
    );
}

#[test]
fn store_turn_records_and_reconcile() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "t".into(),
            cwd: "/tmp/project".into(),
            current_provider: ProviderId::Codex,
            current_model: Some("m".into()),
        })
        .unwrap();
    store.start_turn_record(&thread.id, 1, ProviderId::Codex, Some("m")).unwrap();
    store.finish_turn_record(&thread.id, 1, TurnPhase::Completed, None).unwrap();
    store.start_turn_record(&thread.id, 2, ProviderId::Codex, Some("m")).unwrap();

    let running = store.get_turn_record(&thread.id, 2).unwrap().unwrap();
    assert_eq!(running.phase, TurnPhase::Running);
    assert!(running.ended_at.is_none());

    let reconciled = store.reconcile_running_turns(Some("/tmp/project")).unwrap();
    assert_eq!(reconciled.len(), 1);
    assert_eq!(reconciled[0].turn, 2);

    let turns = store.list_turn_records(&thread.id).unwrap();
    assert_eq!(turns[0].phase, TurnPhase::Completed);
    assert_eq!(turns[1].phase, TurnPhase::Unverified);
    assert!(turns[1].ended_at.is_some());
    let phases = store
        .latest_turn_phases(&[thread.id.clone()])
        .unwrap();
    assert_eq!(phases.get(&thread.id), Some(&TurnPhase::Unverified));
    assert_eq!(
        store.reconcile_running_turns(Some("/tmp/project")).unwrap().len(),
        0,
        "reconcile is idempotent"
    );
}

#[cfg(unix)]
#[test]
fn store_thread_lookup_accepts_symlinked_project_paths() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "linked".into(),
            cwd: real.to_string_lossy().to_string(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    let linked = store
        .create_thread(NewThread {
            title: "stored through link".into(),
            cwd: link.to_string_lossy().to_string(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    for path in [&real, &link] {
        let found = store.list_threads_for_path(path).unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|row| row.id == thread.id));
        assert!(found.iter().any(|row| row.id == linked.id));
    }
}

#[cfg(unix)]
#[test]
fn store_thread_lookup_canonicalizes_the_stored_path() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "stored through link".into(),
            cwd: link.to_string_lossy().to_string(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    let found = store.list_threads_for_path(&real).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, thread.id);
}

#[test]
fn store_search_is_project_scoped_and_matches_titles_or_text() {
    let store = BatonStore::memory().unwrap();
    let first = store
        .create_thread(NewThread {
            title: "authentication refactor".into(),
            cwd: "/tmp/one".into(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    let second = store
        .create_thread(NewThread {
            title: "other project".into(),
            cwd: "/tmp/two".into(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    store
        .add_message(&first.id, 1, "user", None, None, "Fix the LOGIN redirect")
        .unwrap();
    store
        .add_message(&first.id, 2, "assistant", None, None, "coverage is 100% now")
        .unwrap();
    store
        .add_message(&second.id, 1, "assistant", None, None, "login also appears here")
        .unwrap();
    let scoped = store.search_messages(Some("/tmp/one"), "login", 20).unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].thread_id, first.id);
    let by_title = store
        .search_messages(Some("/tmp/one"), "AUTHENTICATION", 20)
        .unwrap();
    assert_eq!(by_title.len(), 2, "a title match returns each matching message");
    assert!(by_title.iter().all(|result| result.title == "authentication refactor"));
    let all = store.search_messages(None, "login", 20).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(store.search_messages(None, "", 20).unwrap().len(), 0);
    let literal_percent = store.search_messages(Some("/tmp/one"), "%", 20).unwrap();
    assert_eq!(literal_percent.len(), 1, "LIKE wildcards are escaped");
    assert!(literal_percent[0].text.contains("100%"));
}

#[test]
fn store_review_notes_track_sent_state_and_rewind() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "review".into(),
            cwd: "/tmp/project".into(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();

    let first = store
        .add_review_note(&thread.id, 1, "src/lib.rs", 2, true, "fix this", "diff-a")
        .unwrap();
    store
        .add_review_note(&thread.id, 2, "src/main.rs", 7, false, "remove this", "diff-b")
        .unwrap();
    assert_eq!(
        store
            .list_unsent_review_notes(&thread.id, 1, "diff-a")
            .unwrap()
            .len(),
        1,
        "unsent notes are restored for their exact diff"
    );
    assert!(
        store
            .list_unsent_review_notes(&thread.id, 1, "other-diff")
            .unwrap()
            .is_empty(),
        "a fingerprint mismatch must not attach stale notes"
    );

    store.mark_review_notes_sent(&thread.id, &[first]).unwrap();
    assert!(
        store
            .list_unsent_review_notes(&thread.id, 1, "diff-a")
            .unwrap()
            .is_empty(),
        "sent notes do not reappear on reopen"
    );

    let second = store
        .add_review_note(&thread.id, 2, "src/main.rs", 8, true, "delete me", "diff-b")
        .unwrap();
    store.delete_review_notes(&thread.id, &[second]).unwrap();
    assert_eq!(
        store
            .list_unsent_review_notes(&thread.id, 2, "diff-b")
            .unwrap()
            .len(),
        1,
        "deleting one note leaves the other note on the same turn"
    );

    let turn_one = store
        .add_review_note(&thread.id, 1, "src/lib.rs", 3, true, "keep", "diff-a")
        .unwrap();
    store
        .add_review_note(&thread.id, 2, "src/main.rs", 9, true, "drop", "diff-b")
        .unwrap();
    store.rewind_thread(&thread.id, 2).unwrap();
    let remaining = store
        .list_unsent_review_notes(&thread.id, 1, "diff-a")
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, turn_one);
    assert!(
        store
            .list_unsent_review_notes(&thread.id, 2, "diff-b")
            .unwrap()
            .is_empty(),
        "rewind removes review notes at the cutoff"
    );
}

#[test]
fn store_queue_is_fifo_and_cleared_on_rewind() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "t".into(),
            cwd: "/tmp/project".into(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    store.enqueue_message(&thread.id, "first").unwrap();
    store.enqueue_message(&thread.id, "second").unwrap();
    assert_eq!(
        store.list_queued_messages(&thread.id).unwrap(),
        vec!["first", "second"]
    );
    assert_eq!(store.pop_queued_message(&thread.id).unwrap().as_deref(), Some("first"));
    assert_eq!(
        store.list_queued_messages(&thread.id).unwrap(),
        vec!["second"]
    );

    store.rewind_thread(&thread.id, 1).unwrap();
    assert!(store.list_queued_messages(&thread.id).unwrap().is_empty());
}

#[test]
fn store_thread_mode_and_goal_persist() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "t".into(),
            cwd: "/tmp/project".into(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    assert_eq!(thread.permission_mode, PermissionMode::Default);
    assert_eq!(thread.goal, None);
    assert!(!thread.goal_paused);

    store.set_thread_permission_mode(&thread.id, PermissionMode::Plan).unwrap();
    store
        .set_thread_goal(&thread.id, Some("ship the fix"), true)
        .unwrap();
    let updated = store.get_thread(&thread.id).unwrap().unwrap();
    assert_eq!(updated.permission_mode, PermissionMode::Plan);
    assert_eq!(updated.goal.as_deref(), Some("ship the fix"));
    assert!(updated.goal_paused);
}

#[test]
fn store_migration_resets_stale_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    {
        let legacy = rusqlite::Connection::open(&path).unwrap();
        legacy
            .execute_batch(
                r#"
                CREATE TABLE threads (
                  id TEXT PRIMARY KEY,
                  title TEXT NOT NULL,
                  cwd TEXT NOT NULL,
                  current_provider TEXT NOT NULL,
                  current_model TEXT NOT NULL,
                  created_at TEXT NOT NULL,
                  updated_at TEXT NOT NULL
                );
                "#,
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO threads VALUES ('stale-id', 'stale', '/x', 'codex', 'gpt-6.1-sol', '2026-01-01', '2026-01-01')",
                [],
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO threads VALUES ('fresh-id', 'fresh', '/x', 'claude', 'claude-opus-5-5', '2026-01-01', '2026-01-01')",
                [],
            )
            .unwrap();
    }
    let store = BatonStore::open(&path).unwrap();
    let stale = store.get_thread("stale-id").unwrap().unwrap();
    assert_eq!(stale.current_model, None);
    assert_eq!(stale.permission_mode, PermissionMode::Default);
    assert_eq!(stale.goal, None);
    assert!(!stale.goal_paused);
    assert!(!stale.pinned);
    assert_eq!(
        store.get_thread("fresh-id").unwrap().unwrap().current_model.as_deref(),
        Some("claude-opus-5-5")
    );
}

#[test]
fn store_import_thread() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .import_thread(
            NewThread {
                title: "imported".into(),
                cwd: "/tmp/repo".into(),
                current_provider: ProviderId::Claude,
                current_model: Some("claude-opus-5-5".into()),
            },
            "native-import",
            None,
            &[
                ImportedMessage { role: "user", text: "fix the bug".into(), created_at: "2026-01-01".into() },
                ImportedMessage { role: "assistant", text: "done".into(), created_at: "2026-01-01".into() },
                ImportedMessage { role: "user", text: "thanks".into(), created_at: "2026-01-01".into() },
            ],
        )
        .unwrap();
    assert_eq!(
        store.get_provider_session(&thread.id, ProviderId::Claude).unwrap().unwrap().native_session_id,
        "native-import"
    );
    assert_eq!(store.list_messages(&thread.id).unwrap().len(), 3);
    assert_eq!(store.next_turn_number(&thread.id).unwrap(), 3);
}

#[test]
fn handoff_builds_sections_and_respects_budget() {
    let handoff = build_handoff(
        "/work/repo",
        "fix login",
        &[
            message("user", "fix the login bug"),
            message("assistant", "done, tests pass"),
        ],
        Some("M src/auth.ts"),
        None,
    );
    assert!(handoff.contains("[baton context handoff]"));
    assert!(handoff.contains("Working directory: /work/repo"));
    assert!(handoff.contains("[user]\nfix the login bug"));
    assert!(handoff.contains("[assistant (claude/m)]\ndone, tests pass"));
    assert!(handoff.contains("M src/auth.ts"));
    assert!(handoff.contains("[end handoff]"));

    let long_messages: Vec<HandoffMessage> = (0..30)
        .map(|index| {
            let role = if index % 2 == 0 { "user" } else { "assistant" };
            message(role, &format!("message number {index} {}", "x".repeat(200)))
        })
        .collect();
    let capped = build_handoff("/r", "t", &long_messages, None, Some(2000));
    assert!(capped.contains("earlier messages omitted]"));
    assert!(!capped.contains("message number 0 "));
    assert!(capped.contains("message number 29 "));

    let wrapped = wrap_prompt_with_handoff("HANDOFF", "do the thing");
    assert!(wrapped.starts_with("HANDOFF"));
    assert!(wrapped.ends_with("do the thing"));
}

#[test]
fn an_unknown_provider_session_key_round_trips_without_becoming_claude() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "custom".into(),
            cwd: "/tmp/custom".into(),
            current_provider: ProviderId::Claude,
            current_model: None,
        })
        .unwrap();
    let key = baton::types::ProviderKey::from_stored("custom-agent");
    store
        .upsert_provider_session(&thread.id, key.clone(), "native-1", None, None)
        .unwrap();
    let sessions = store.list_provider_sessions(&thread.id).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].provider.as_str(), "custom-agent");
    assert_eq!(
        sessions[0].provider.builtin(),
        None,
        "an unknown stored provider must not be coerced to a built-in"
    );
    assert!(store.get_provider_session(&thread.id, key).unwrap().is_some());
    assert!(
        store
            .get_provider_session(&thread.id, ProviderId::Claude)
            .unwrap()
            .is_none(),
        "the built-in name must not match a different stored key"
    );
}

#[test]
fn provider_session_covered_turn_defaults_to_zero_and_only_moves_forward() {
    let store = BatonStore::memory().unwrap();
    let thread = store
        .create_thread(NewThread {
            title: "covered".into(),
            cwd: "/tmp/covered".into(),
            current_provider: ProviderId::Codex,
            current_model: Some("m".into()),
        })
        .unwrap();
    store
        .upsert_provider_session(&thread.id, ProviderId::Codex, "s1", Some("m".into()), None)
        .unwrap();
    assert_eq!(
        store
            .get_provider_session(&thread.id, ProviderId::Codex)
            .unwrap()
            .unwrap()
            .covered_turn,
        0,
        "a brand new session has seen nothing"
    );
    store
        .mark_provider_session_covered(&thread.id, ProviderId::Codex, 3)
        .unwrap();
    store
        .mark_provider_session_covered(&thread.id, ProviderId::Codex, 2)
        .unwrap();
    assert_eq!(
        store
            .get_provider_session(&thread.id, ProviderId::Codex)
            .unwrap()
            .unwrap()
            .covered_turn,
        3,
        "the marker never regresses"
    );
}

#[test]
fn handoff_prompt_includes_the_user_prompt_once() {
    let prompt = handoff_prompt(
        "/work/repo",
        "fix login",
        &[message("user", "fix the login bug"), message("assistant", "done, tests pass")],
        Some("M src/auth.rs"),
        "now add a test",
        None,
    );
    assert_eq!(prompt.matches("now add a test").count(), 1);
    assert!(prompt.contains("[user]\nfix the login bug"));
}

#[test]
fn handoff_truncation_keeps_utf8_intact() {
    let inside = format!("{}é{}", "a".repeat(3998), "z".repeat(10));
    let inside_handoff = build_handoff("/r", "t", &[message("user", &inside)], None, Some(20000));
    assert!(
        inside_handoff.contains("é"),
        "a character ending exactly at the limit must survive"
    );
    assert!(inside_handoff.contains("[...truncated]"));

    let straddling = format!("{}é{}", "a".repeat(3999), "z".repeat(10));
    let straddling_handoff = build_handoff(
        "/r",
        "t",
        &[message("user", &straddling)],
        None,
        Some(20000),
    );
    assert!(straddling_handoff.contains("[...truncated]"));
    assert!(
        straddling_handoff.contains(&"a".repeat(3999)),
        "truncation must back off to a character boundary"
    );
}

#[test]
fn usage_math_and_formatting() {
    let rate = cache_hit_rate(&TurnUsage { input_tokens: 100, cache_read_tokens: 900, cache_creation_tokens: 0, ..Default::default() });
    assert!((rate.unwrap() - 0.9).abs() < 1e-9);
    assert!(cache_hit_rate(&TurnUsage::default()).is_none());

    assert_eq!(format_token_count(999), "999");
    assert_eq!(format_token_count(1500), "1.5k");
    assert_eq!(format_token_count(2_500_000), "2.5M");

    let rows = vec![baton::store::UsageRow {
        thread_id: "t".into(),
        turn: 1,
        provider: ProviderId::Claude,
        model: None,
        usage: TurnUsage { input_tokens: 100, output_tokens: 50, cache_read_tokens: 900, cache_creation_tokens: 0, cost_usd: Some(0.05) },
        created_at: String::new(),
    }];
    let totals = sum_usage(&rows);
    let line = format_usage_line(&totals);
    assert!(line.contains("1.0k"));
    assert!(line.contains("cache 90%"));
    assert!(line.contains("$0.05"));
}

#[test]
fn scanner_parses_claude_transcripts() {
    let contents = [
        r#"{"type":"user","sessionId":"session-123","cwd":"/work/project","timestamp":"2026-10-11T10:00:00.000Z","message":{"role":"user","content":"fix the login bug"}}"#,
        r#"{"type":"assistant","sessionId":"session-123","timestamp":"2026-10-11T10:00:05.000Z","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"fixed it"},{"type":"tool_use","id":"t1","name":"Bash"}]}}"#,
        r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"subagent chatter"}}"#,
        r#"{"type":"assistant","message":{"model":"<synthetic>","content":"local error"}}"#,
    ]
    .join("\n");
    let path = Path::new("/x/session-123.jsonl");
    let session = parse_claude_transcript(&contents, path, 1000, "session-123").unwrap();
    assert_eq!(session.native_session_id, "session-123");
    assert_eq!(session.cwd.as_deref(), Some("/work/project"));
    assert_eq!(session.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(session.messages.len(), 3);
    assert_eq!(session.messages[0].text, "fix the login bug");
}

#[test]
fn scanner_parses_codex_transcripts_with_dedupe() {
    let contents = [
        r#"{"type":"session_meta","payload":{"id":"native-thread-1","cwd":"/work/repo"}}"#,
        r#"{"type":"turn_context","payload":{"model":"gpt-5.6-luna","cwd":"/work/repo"}}"#,
        r#"{"type":"event_msg","payload":{"type":"user_message","message":"add the endpoint"},"timestamp":"2026-10-11T10:59:00.000Z"}"#,
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add the endpoint"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-9"}},"timestamp":"2026-10-11T10:59:00.000Z"}"#,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"endpoint added"}]},"timestamp":"2026-10-11T11:00:00.000Z"}"#,
    ]
    .join("\n");
    let path = Path::new("/home/.codex/sessions/rollout-x.jsonl");
    let session = parse_codex_transcript(&contents, path, 1000).unwrap();
    assert_eq!(session.native_session_id, "native-thread-1");
    assert_eq!(session.model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(session.cwd.as_deref(), Some("/work/repo"));
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].text, "add the endpoint");
    assert_eq!(session.messages[1].text, "endpoint added");
}

#[test]
fn scanner_file_discovery() {
    let root = tempfile::tempdir().unwrap();
    let claude_project = root.path().join("projects").join("-work-repo");
    std::fs::create_dir_all(&claude_project).unwrap();
    let transcript = [
        r#"{"type":"user","sessionId":"sess-1","cwd":"/work/repo","message":{"role":"user","content":"hi there"}}"#,
        r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":"hello!"}}"#,
    ]
    .join("\n");
    let mut file = std::fs::File::create(claude_project.join("sess-1.jsonl")).unwrap();
    file.write_all(transcript.as_bytes()).unwrap();

    let sessions = baton::scanner::scan_claude_sessions(&root.path().join("projects"), 10);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].native_session_id, "sess-1");
    assert_eq!(sessions[0].messages.len(), 2);
}

#[test]
fn viewport_windowing() {
    use baton::tui::viewport::{wrapped_rows, window_messages, ViewportItem};
    assert_eq!(wrapped_rows("hello", 80), 1);
    assert_eq!(wrapped_rows(&"x".repeat(79), 80), 2);
    assert_eq!(wrapped_rows("a\nb\nc", 80), 3);

    let items: Vec<ViewportItem> = (1..=4)
        .map(|id| ViewportItem {
            id,
            role: "assistant".into(),
            text: format!("message {id}"),
            has_label: true,
            has_usage: false,
        })
        .collect();
    // Each message is 3 rows (margin + label + text) = 12 total rows
    // Budget 6 rows, offset 0 (bottom): shows m3, m4
    let windowed = window_messages(&items, 6, 80, 0);
    assert_eq!(windowed.visible.len(), 2);
    assert_eq!(windowed.visible[1].id, 4);
    assert_eq!(windowed.hidden_above, 2);
    assert_eq!(windowed.hidden_below, 0);

    // Offset 6 lines from bottom = top 6 rows: shows m1, m2
    let scrolled = window_messages(&items, 6, 80, 6);
    assert_eq!(scrolled.visible[0].id, 1);
    assert_eq!(scrolled.visible.last().unwrap().id, 2);
    assert_eq!(scrolled.hidden_below, 2);

    // Offset 3 lines from bottom: viewport covers rows 3-8, showing m2, m3
    let partial = window_messages(&items, 6, 80, 3);
    assert!(partial.visible.len() >= 2);
    assert_eq!(partial.visible.first().unwrap().id, 2);
    assert_eq!(partial.visible.last().unwrap().id, 3);
}

#[test]
fn prompt_history_is_project_scoped_capped_and_deduped() {
    let store = BatonStore::memory().unwrap();
    store.record_prompt("/proj-a", "first").unwrap();
    store.record_prompt("/proj-a", "first").unwrap(); // consecutive repeat skipped
    store.record_prompt("/proj-b", "other project").unwrap();
    store.record_prompt("/proj-a", "second").unwrap();
    assert_eq!(store.list_prompt_history("/proj-a").unwrap(), vec!["first", "second"]);
    assert_eq!(store.list_prompt_history("/proj-b").unwrap(), vec!["other project"]);
    assert!(store.list_prompt_history("/proj-c").unwrap().is_empty());

    for index in 0..150 {
        store.record_prompt("/proj-a", &format!("prompt {index}")).unwrap();
    }
    let history = store.list_prompt_history("/proj-a").unwrap();
    assert_eq!(history.len(), 100, "history caps at 100 entries");
    assert_eq!(history.first().unwrap(), "prompt 50", "oldest entries fall off");
    assert_eq!(history.last().unwrap(), "prompt 149");
}

#[test]
fn memories_are_scoped_deduped_and_removable() {
    let store = BatonStore::memory().unwrap();
    let project = store.add_memory("project", "/proj-a", "Use tabs").unwrap();
    assert_eq!(
        store.add_memory("project", "/proj-a", "Use tabs").unwrap(),
        project,
        "an identical project memory is returned as-is"
    );
    let global = store.add_memory("global", "/proj-a", "Answer briefly").unwrap();
    let other = store.add_memory("project", "/proj-b", "Other project").unwrap();

    let a = store.list_memories("/proj-a").unwrap();
    assert_eq!(a.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), vec!["Use tabs", "Answer briefly"]);
    assert_eq!(a[0].scope, "project");
    let b = store.list_memories("/proj-b").unwrap();
    assert_eq!(b.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), vec!["Other project", "Answer briefly"]);

    assert!(!store.remove_memory("/proj-a", other).unwrap(), "another project's memory is protected");
    assert!(store.remove_memory("/proj-a", project).unwrap());
    assert_eq!(store.list_memories("/proj-a").unwrap().len(), 1);
    assert!(store.remove_memory("/proj-a", global).unwrap());
    assert!(store.list_memories("/proj-a").unwrap().is_empty());

    assert!(store.add_memory("project", "/proj-a", &"x".repeat(2001)).is_err());
    assert!(store.add_memory("nonsense", "/proj-a", "x").is_err());
}

#[test]
fn abandoned_orchestration_is_reconciled_without_losing_completed_tasks() {
    let store = BatonStore::memory().unwrap();
    store
        .create_orchestration_run_owned(
            "run-abandoned",
            "finish the work",
            ProviderId::Codex,
            "model",
            None,
            "{}",
            false,
            Some(0),
            None,
        )
        .unwrap();
    store
        .add_orchestration_task(
            "task-done",
            "run-abandoned",
            1,
            "completed worker",
            ProviderId::Codex,
            "model",
            "done",
            Some("/tmp/done"),
            Some("baton/run/done"),
            Some("abc123"),
            Some("thread-done"),
            "completed",
            Some("finished"),
            None,
            "now",
            Some("now"),
            None,
            None,
            None,
        )
        .unwrap();
    store
        .add_orchestration_task(
            "task-running",
            "run-abandoned",
            2,
            "running worker",
            ProviderId::Codex,
            "model",
            "running",
            Some("/tmp/running"),
            Some("baton/run/running"),
            None,
            Some("thread-running"),
            "running",
            None,
            None,
            "now",
            None,
            None,
            None,
            None,
        )
        .unwrap();
    store
        .add_orchestration_task(
            "task-waiting",
            "run-abandoned",
            3,
            "waiting worker",
            ProviderId::Codex,
            "model",
            "waiting",
            Some("/tmp/waiting"),
            Some("baton/run/waiting"),
            Some("def456"),
            Some("thread-waiting"),
            "needs_input",
            Some("BATON_QUESTION: choose a path"),
            None,
            "now",
            Some("now"),
            None,
            None,
            None,
        )
        .unwrap();
    store
        .start_turn_record("thread-done", 1, ProviderId::Codex, Some("model"))
        .unwrap();
    store
        .finish_turn_record("thread-done", 1, TurnPhase::Completed, None)
        .unwrap();
    store
        .start_turn_record("thread-running", 1, ProviderId::Codex, Some("model"))
        .unwrap();

    let reconciled = baton::orchestrate::reconcile_abandoned_runs(&store).unwrap();
    assert_eq!(reconciled.len(), 1);
    assert_eq!(reconciled[0].run_id, "run-abandoned");
    assert_eq!(reconciled[0].status, "needs_input");
    assert_eq!(reconciled[0].completed_tasks, 1);
    assert_eq!(reconciled[0].failed_tasks, 1);
    assert_eq!(reconciled[0].needs_input_tasks, 1);

    let run = &store.list_orchestration_runs(10).unwrap()[0];
    assert_eq!(run.status, "needs_input");
    assert!(run.finished_at.is_some());
    let tasks = store.list_orchestration_tasks("run-abandoned").unwrap();
    assert_eq!(tasks[0].status, "completed");
    assert_eq!(tasks[0].commit_sha.as_deref(), Some("abc123"));
    assert_eq!(tasks[1].status, "failed");
    assert_eq!(tasks[2].status, "needs_input");
    assert_eq!(tasks[2].commit_sha.as_deref(), Some("def456"));
    assert!(
        tasks[1]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("exited before the run finished"),
        "task error: {:?}",
        tasks[1].error
    );
    assert_eq!(
        store
            .get_turn_record("thread-done", 1)
            .unwrap()
            .unwrap()
            .phase,
        TurnPhase::Completed
    );
    assert_eq!(
        store
            .get_turn_record("thread-running", 1)
            .unwrap()
            .unwrap()
            .phase,
        TurnPhase::Unverified
    );
}

#[test]
fn live_orchestration_owner_is_not_reconciled() {
    let store = BatonStore::memory().unwrap();
    let owner_pid = std::process::id();
    let owner_start = baton::process::process_start(owner_pid);
    store
        .create_orchestration_run_owned(
            "run-live",
            "keep working",
            ProviderId::Codex,
            "model",
            None,
            "{}",
            false,
            Some(owner_pid),
            owner_start.as_deref(),
        )
        .unwrap();
    assert!(baton::orchestrate::reconcile_abandoned_runs(&store)
        .unwrap()
        .is_empty());
    assert_eq!(
        store.list_orchestration_runs(10).unwrap()[0].status,
        "running"
    );

    store
        .create_orchestration_run_owned(
            "run-reused-pid",
            "reused pid",
            ProviderId::Codex,
            "model",
            None,
            "{}",
            false,
            Some(owner_pid),
            Some("different process start"),
        )
        .unwrap();
    let reconciled = baton::orchestrate::reconcile_abandoned_runs(&store).unwrap();
    assert_eq!(reconciled.len(), 1);
    assert_eq!(reconciled[0].run_id, "run-reused-pid");

    store
        .create_orchestration_run(
            "run-legacy",
            "ownerless legacy row",
            ProviderId::Codex,
            "model",
            None,
            "{}",
            false,
        )
        .unwrap();
    assert!(
        baton::orchestrate::reconcile_abandoned_runs(&store)
            .unwrap()
            .is_empty(),
        "an ownerless run cannot be proven abandoned"
    );
}

#[test]
fn secret_check_reports_sources_without_printing_values() {
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join("claude");
    let codex = home.path().join("codex");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::create_dir_all(&codex).unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"codex":{"mode":"api","api_key":"super-secret-value","api_key_env":"BATON_TEST_SECRET_CLI_KEY"}}"#,
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["secret", "check", "--json"])
        .env("BATON_HOME", home.path())
        .env("CLAUDE_CONFIG_DIR", &claude)
        .env("CODEX_HOME", &codex)
        .env_remove("BATON_TEST_SECRET_CLI_KEY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("super-secret-value"),
        "the report must never print the key: {stdout}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    let providers = value["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 6);
    let codex = providers
        .iter()
        .find(|provider| provider["provider"] == "codex")
        .expect("codex row");
    assert_eq!(codex["mode"], "api");
    assert_eq!(codex["key_source"], "config-file");
    assert_eq!(codex["key_present"], true);
    assert_eq!(codex["key_variable"], "BATON_TEST_SECRET_CLI_KEY");
}

fn usage_row(
    thread_id: &str,
    turn: i64,
    provider: ProviderId,
    model: Option<&str>,
    created_at: &str,
    input: i64,
    output: i64,
    cache_read: i64,
    cost: Option<f64>,
) -> baton::store::UsageRow {
    baton::store::UsageRow {
        thread_id: thread_id.to_string(),
        turn,
        provider,
        model: model.map(str::to_string),
        usage: TurnUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_creation_tokens: 0,
            cost_usd: cost,
        },
        created_at: created_at.to_string(),
    }
}

#[test]
fn group_usage_sums_days_models_and_providers() {
    let rows = vec![
        usage_row(
            "t1",
            1,
            ProviderId::Codex,
            Some("gpt-6.1-sol"),
            "2026-11-24T10:00:00Z",
            10,
            2,
            5,
            Some(0.01),
        ),
        usage_row(
            "t1",
            2,
            ProviderId::Codex,
            Some("gpt-6.1-sol"),
            "2026-11-25T10:00:00Z",
            20,
            4,
            10,
            Some(0.02),
        ),
        usage_row(
            "t2",
            1,
            ProviderId::Claude,
            None,
            "2026-11-25T11:00:00Z",
            30,
            6,
            15,
            None,
        ),
    ];

    let days = baton::usage::group_usage(&rows, baton::usage::UsageGroup::Day);
    assert_eq!(days.len(), 2);
    assert_eq!(days[0].0, "2026-11-25", "days are newest first");
    assert_eq!(days[0].1.turns, 2);
    assert_eq!(days[0].1.input_tokens, 50);
    assert_eq!(days[0].1.cache_read_tokens, 25);
    assert_eq!(days[0].1.cost_usd, Some(0.02));
    assert_eq!(days[1].0, "2026-11-24");
    assert_eq!(days[1].1.turns, 1);

    let models = baton::usage::group_usage(&rows, baton::usage::UsageGroup::Model);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].0, "gpt-6.1-sol");
    assert_eq!(models[0].1.turns, 2);
    assert_eq!(models[0].1.cost_usd, Some(0.03));
    assert_eq!(models[1].0, "unknown");

    let providers = baton::usage::group_usage(&rows, baton::usage::UsageGroup::Provider);
    assert_eq!(
        providers
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        vec!["claude", "codex"]
    );
}

#[test]
fn usage_by_model_reads_recorded_history() {
    let home = tempfile::tempdir().unwrap();
    {
        let store = BatonStore::open(&home.path().join("baton.sqlite")).unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "usage thread".to_string(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .add_usage(
                &thread.id,
                1,
                ProviderId::Codex,
                Some("gpt-6.1-sol"),
                &TurnUsage {
                    input_tokens: 12,
                    output_tokens: 3,
                    cache_read_tokens: 6,
                    cache_creation_tokens: 0,
                    cost_usd: Some(0.01),
                },
            )
            .unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["usage", "--by", "model"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("gpt-6.1-sol"), "{stdout}");
    assert!(stdout.contains("1 turns"), "{stdout}");
    assert!(stdout.contains("in 18"), "{stdout}");

    let rejected = std::process::Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["usage", "--by", "fortnight"])
        .env("BATON_HOME", home.path())
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("invalid value"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

#[test]
fn side_chat_marks_the_source_and_leaves_it_unchanged() {
    let store = BatonStore::memory().unwrap();
    let source = store
        .create_thread(NewThread {
            title: "source".to_string(),
            cwd: "/tmp/side-chat".to_string(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
    store
        .add_message(&source.id, 1, "user", None, None, "hello")
        .unwrap();
    let side = store.fork_thread(&source.id, "source (side chat)").unwrap();
    store.mark_side_chat(&side.id, &source.id).unwrap();

    let marked = store.side_chat_source(&side.id).unwrap().expect("source");
    assert_eq!(marked.id, source.id);
    assert!(store.side_chat_source(&source.id).unwrap().is_none());
    assert_eq!(store.list_messages(&source.id).unwrap().len(), 1);
    assert_eq!(store.list_messages(&side.id).unwrap().len(), 1);
    assert_eq!(side.title, "source (side chat)");
}
