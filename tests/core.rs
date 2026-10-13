use baton::handoff::{build_handoff, wrap_prompt_with_handoff, HandoffMessage};
use baton::store::{BatonStore, ImportedMessage, NewThread};
use baton::types::{ProviderId, TurnUsage};
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

    store.rewind_thread(&thread.id, 2).unwrap();
    let messages = store.list_messages(&thread.id).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].text, "one");
    assert!(store.get_provider_session(&thread.id, ProviderId::Claude).unwrap().is_none());
    assert_eq!(store.list_usage(Some(&thread.id)).unwrap().len(), 1);
    assert_eq!(store.next_turn_number(&thread.id).unwrap(), 2);
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
    assert_eq!(store.get_thread("stale-id").unwrap().unwrap().current_model, None);
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
        &[message("user", "fix the login bug"), message("assistant", "done, tests pass")],
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
    let windowed = window_messages(&items, 6, 80, 0);
    assert_eq!(windowed.visible.len(), 2);
    assert_eq!(windowed.visible[1].id, 4);
    assert_eq!(windowed.hidden_above, 2);

    let scrolled = window_messages(&items, 6, 80, 2);
    assert_eq!(scrolled.visible[0].id, 1);
    assert_eq!(scrolled.hidden_below, 2);
}
