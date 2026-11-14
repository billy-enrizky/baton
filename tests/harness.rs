use base64::Engine as _;
use baton::checkpoints::{capture_checkpoint, checkpoint_ref, diff_checkpoint_against_worktree, restore_checkpoint};
use baton::config::{
    auth_signature, load_config_file, provider_auth_env, save_config_file, AuthMode,
};
use baton::harness_config::{
    read_claude_harness_config, read_claude_model_catalog, read_codex_harness_config,
};
use std::process::Command;

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git").args(args).current_dir(repo).output().unwrap();
    assert!(output.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn checkpoints_capture_diff_restore() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "test@test"]);
    git(repo.path(), &["config", "user.name", "test"]);
    std::fs::write(repo.path().join("a.txt"), "initial\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);

    std::fs::write(repo.path().join("a.txt"), "changed\n").unwrap();
    std::fs::write(repo.path().join("b.txt"), "new file\n").unwrap();
    let head_before = git(repo.path(), &["rev-parse", "HEAD"]);

    let reference = capture_checkpoint(repo.path(), "thread-1", 1).unwrap();
    assert_eq!(reference, checkpoint_ref("thread-1", 1));
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        git(repo.path(), &["cat-file", "-p", "refs/baton/checkpoints/thread-1/1:a.txt"]),
        "changed"
    );
    assert_eq!(
        git(repo.path(), &["cat-file", "-p", "refs/baton/checkpoints/thread-1/1:b.txt"]),
        "new file"
    );

    std::fs::write(repo.path().join("a.txt"), "version 2\n").unwrap();
    let diff = diff_checkpoint_against_worktree(repo.path(), &reference).unwrap();
    assert!(diff.contains("-changed"));
    assert!(diff.contains("+version 2"));

    restore_checkpoint(repo.path(), &reference).unwrap();
    assert_eq!(std::fs::read_to_string(repo.path().join("a.txt")).unwrap(), "changed\n");
}

#[test]
fn checkpoints_work_in_repo_without_commits() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("only.txt"), "content\n").unwrap();
    let reference = capture_checkpoint(repo.path(), "thread-empty", 1).unwrap();
    assert_eq!(reference, checkpoint_ref("thread-empty", 1));
}

#[test]
fn claude_harness_config_layers() {
    let root = tempfile::tempdir().unwrap();
    let global_dir = root.path().join("claude-global");
    let project = root.path().join("project");
    std::fs::create_dir_all(&global_dir).unwrap();
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    std::fs::write(
        global_dir.join("settings.json"),
        r#"{"model":"claude-sonnet-5-5","permissions":{"defaultMode":"default"}}"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".claude").join("settings.json"),
        r#"{"permissions":{"defaultMode":"plan"}}"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".claude").join("settings.local.json"),
        r#"{"model":"claude-opus-5-5"}"#,
    )
    .unwrap();
    let config = read_claude_harness_config(&global_dir, Some(&project));
    assert_eq!(config.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(config.permission_mode, Some(baton::types::PermissionMode::Plan));

    std::fs::write(
        global_dir.join("settings.json"),
        r#"{"env":{"ANTHROPIC_API_KEY":"sk-ant-from-settings","ANTHROPIC_MODEL":"claude-opus-5-5[1m]"},"permissions":{"defaultMode":"dontAsk"}}"#,
    )
    .unwrap();
    let config = read_claude_harness_config(&global_dir, None);
    assert_eq!(config.model.as_deref(), Some("claude-opus-5-5[1m]"));
    assert_eq!(config.auth_mode, Some(AuthMode::Api));
    assert_eq!(config.api_key.as_deref(), Some("sk-ant-from-settings"));
    assert_eq!(config.permission_mode, Some(baton::types::PermissionMode::AcceptEdits));
}

#[test]
fn claude_model_catalog_reads_newest_cached_cc_surface() {
    let root = tempfile::tempdir().unwrap();
    let claude = root.path().join("claude");
    let cache = claude.join("cache").join("model-catalog");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(
        cache.join("old-cc.json"),
        r#"{"version":2,"fetchedAt":1000,"catalog":{"surface":"cc","config":{"models":[{"id":"old-model"}]}}}"#,
    )
    .unwrap();
    std::fs::write(
        cache.join("new-cc.json"),
        r#"{"version":2,"fetchedAt":2000,"catalog":{"surface":"cc","config":{"models":[{"id":"claude-opus-5-5"},{"id":"claude-sonnet-5-5"},"claude-opus-5-5",{"id":""}]}}}"#,
    )
    .unwrap();
    std::fs::write(
        cache.join("newer-ccd.json"),
        r#"{"version":2,"fetchedAt":3000,"catalog":{"surface":"ccd","config":{"models":[{"id":"desktop-only"}]}}}"#,
    )
    .unwrap();

    assert_eq!(
        read_claude_model_catalog(&claude),
        vec!["claude-opus-5-5", "claude-sonnet-5-5"]
    );
}

#[test]
fn claude_model_catalog_falls_back_to_published_document() {
    let root = tempfile::tempdir().unwrap();
    let claude = root.path().join("claude");
    let cache = claude.join("cache").join("model-catalog");
    std::fs::create_dir_all(&cache).unwrap();
    let document = serde_json::json!({
        "surfaces": {
            "cc": {
                "model_selector_config": [{
                    "models": [
                        {"id": "claude-fable-5-1"},
                        {"id": "claude-opus-5-5"}
                    ]
                }]
            }
        }
    });
    let encoded = base64::engine::general_purpose::STANDARD.encode(document.to_string());
    std::fs::write(
        cache.join("published-test.json"),
        serde_json::json!({"version": 2, "fetchedAt": 4000, "documentBytes": encoded}).to_string(),
    )
    .unwrap();

    assert_eq!(
        read_claude_model_catalog(&claude),
        vec!["claude-fable-5-1", "claude-opus-5-5"]
    );

    std::fs::write(cache.join("broken.json"), "{").unwrap();
    assert_eq!(read_claude_model_catalog(&claude).len(), 2);
}

#[test]
fn codex_harness_config_reads_custom_providers() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("codex-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        [
            "model = \"gpt-5.6-luna\"",
            "model_provider = \"ica\"",
            "approval_policy = \"never\"",
            "",
            "[model_providers.ica]",
            "name = \"ICA\"",
            "env_key = \"CODEX_ICA_API_KEY\"",
        ]
        .join("\n"),
    )
    .unwrap();
    let config = read_codex_harness_config(&home, None);
    assert_eq!(config.model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(config.model_provider.as_deref(), Some("ica"));
    assert_eq!(config.auth_mode, Some(AuthMode::Api));
    assert_eq!(config.api_key_env_name.as_deref(), Some("CODEX_ICA_API_KEY"));
    assert_eq!(config.permission_mode, Some(baton::types::PermissionMode::BypassPermissions));
}

#[test]
fn codex_project_config_overrides_global() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("codex-home");
    let project = root.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(project.join(".codex")).unwrap();
    std::fs::write(
        home.join("config.toml"),
        ["model = \"gpt-5.6-luna\"", "model_provider = \"ica\"", "approval_policy = \"on-request\"", "", "[model_providers.ica]", "env_key = \"KEY_A\""]
            .join("\n"),
    )
    .unwrap();
    std::fs::write(project.join(".codex").join("config.toml"), "model = \"gpt-6.1-sol\"").unwrap();
    let config = read_codex_harness_config(&home, Some(&project));
    assert_eq!(config.model.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(config.permission_mode, Some(baton::types::PermissionMode::Default));
    assert_eq!(config.api_key_env_name.as_deref(), Some("KEY_A"));
}

#[test]
fn resolve_config_precedence() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let claude_dir = root.path().join("claude-home");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"model":"claude-opus-5-5","permissions":{"defaultMode":"acceptEdits"}}"#,
    )
    .unwrap();
    let codex_home = root.path().join("codex-home");
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::write(codex_home.join("config.toml"), "model = \"gpt-5.6-luna\"").unwrap();

    let mut providers = std::collections::BTreeMap::new();
    providers.insert(
        "claude".to_string(),
        baton::config::ProviderAuthFile { mode: Some(AuthMode::Api), api_key: Some("sk-ant-file".into()), api_key_env: None, api_base_url: None },
    );
    save_config_file(
        home.path(),
        &baton::config::BatonConfigFile { providers, default_models: None, default_provider: None, default_mode: None },
    )
    .unwrap();
    assert!(load_config_file(home.path()).is_some());

    let resolved = baton::config::resolve_config_in(
        home.path(),
        std::path::Path::new("/nowhere"),
        Some(&claude_dir),
        Some(&codex_home),
        None,
    );
    assert_eq!(resolved.for_provider(baton::types::ProviderId::Claude).model, "claude-opus-5-5");
    assert_eq!(
        resolved.for_provider(baton::types::ProviderId::Claude).permission_mode,
        baton::types::PermissionMode::AcceptEdits
    );
    assert_eq!(resolved.for_provider(baton::types::ProviderId::Claude).mode, AuthMode::Api);
    assert_eq!(
        resolved.for_provider(baton::types::ProviderId::Claude).api_key.as_deref(),
        Some("sk-ant-file")
    );
    assert_eq!(resolved.for_provider(baton::types::ProviderId::Codex).model, "gpt-5.6-luna");
}

#[test]
fn provider_config_accepts_camel_case_key_env_and_base_url() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"antigravity":{"mode":"api","apiKeyEnv":"CODEX_ICA_API_KEY","apiBaseUrl":"https://api.servicesessentials.ibm.com"}}"#,
    )
    .unwrap();
    let resolved = baton::config::resolve_config_in(
        home.path(),
        std::path::Path::new("/nowhere"),
        None,
        None,
        None,
    );
    let config = resolved.for_provider(baton::types::ProviderId::Antigravity);
    assert_eq!(config.mode, AuthMode::Api);
    assert_eq!(config.api_key_env_name, "CODEX_ICA_API_KEY");
    assert_eq!(
        config.api_base_url.as_deref(),
        Some("https://api.servicesessentials.ibm.com")
    );
}

#[test]
fn provider_auth_env_maps_a_neutral_source_to_the_harness_native_names() {
    let config = baton::config::EffectiveProviderConfig {
        provider: baton::types::ProviderId::Antigravity,
        mode: AuthMode::Api,
        api_key: Some("ica-key".into()),
        api_key_env_name: "CODEX_ICA_API_KEY".into(),
        api_base_url: Some("https://api.servicesessentials.ibm.com".into()),
        model: "gemini-3.7-flash".into(),
        permission_mode: baton::types::PermissionMode::Default,
    };
    assert_eq!(
        provider_auth_env(&config),
        vec![
            ("GEMINI_API_KEY".to_string(), "ica-key".to_string()),
            (
                "GOOGLE_GEMINI_BASE_URL".to_string(),
                "https://api.servicesessentials.ibm.com".to_string()
            )
        ]
    );
    let subscription = baton::config::EffectiveProviderConfig { mode: AuthMode::Subscription, ..config };
    assert!(provider_auth_env(&subscription).is_empty());
}

#[test]
fn auth_signature_is_order_independent() {
    assert_eq!(
        auth_signature(&[("A".into(), "1".into()), ("B".into(), "2".into())]),
        auth_signature(&[("B".into(), "2".into()), ("A".into(), "1".into())])
    );
    assert_eq!(auth_signature(&[]), "");
}

#[test]
fn opencode_harness_config_reads_model_slug() {
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("opencode-config");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::write(
        global.join("opencode.json"),
        r#"{"$schema":"https://opencode.ai/config.json","provider":{"myprovider":{"npm":"@ai-sdk/openai-compatible","models":{"rits/zai-org/glm-5-3":{"name":"glm"}}}},"model":"myprovider/rits/zai-org/glm-5-3"}"#,
    )
    .unwrap();

    let config = baton::harness_config::read_opencode_harness_config(&global, None);
    assert_eq!(config.model.as_deref(), Some("myprovider/rits/zai-org/glm-5-3"));
    assert_eq!(config.auth_mode, None, "opencode manages its own auth");

    let resolved = baton::config::resolve_config_in(
        root.path(),
        std::path::Path::new("/nowhere"),
        None,
        None,
        Some(&global),
    );
    assert_eq!(
        resolved.for_provider(baton::types::ProviderId::OpenCode).model,
        "myprovider/rits/zai-org/glm-5-3"
    );
}

#[test]
fn opencode_harness_config_project_jsonc_overrides_global() {
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("opencode-config");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::write(global.join("opencode.json"), r#"{"model":"global/model"}"#).unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("opencode.jsonc"),
        "{\n  // project-level provider\n  \"model\": \"myprovider/project-model\", // trailing comment\n}\n",
    )
    .unwrap();

    let config = baton::harness_config::read_opencode_harness_config(&global, Some(project.path()));
    assert_eq!(config.model.as_deref(), Some("myprovider/project-model"));

    let global_only = baton::harness_config::read_opencode_harness_config(&global, None);
    assert_eq!(global_only.model.as_deref(), Some("global/model"));
}

#[test]
fn opencode_harness_config_missing_files_stay_default() {
    let root = tempfile::tempdir().unwrap();
    let config = baton::harness_config::read_opencode_harness_config(root.path(), None);
    assert_eq!(config.model, None);
}

#[test]
fn turn_diff_between_start_and_end_checkpoints() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "test@test"]);
    git(repo.path(), &["config", "user.name", "test"]);
    std::fs::write(repo.path().join("a.txt"), "initial\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);

    let start = baton::checkpoints::checkpoint_start_ref("thread-diff", 1);
    baton::checkpoints::capture_checkpoint_at(repo.path(), &start, "start").unwrap();
    std::fs::write(repo.path().join("a.txt"), "changed\n").unwrap();
    std::fs::write(repo.path().join("b.txt"), "new\n").unwrap();
    let end = baton::checkpoints::capture_checkpoint(repo.path(), "thread-diff", 1).unwrap();

    let diff = baton::checkpoints::diff_checkpoints(repo.path(), &start, &end).unwrap();
    assert!(diff.contains("-initial"), "removed line shows: {}", diff);
    assert!(diff.contains("+changed"), "added line shows: {}", diff);
    assert!(diff.contains("+new"), "new file shows: {}", diff);
    assert!(diff.contains("b.txt"));
}
