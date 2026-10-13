use baton::{adapters, checkpoints, cli, config, paths, runtime, scanner, store, tui, types, usage};

use anyhow::Result;
use clap::Parser;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adapters::{child_halves, AcpAdapter, ClaudeAdapter, CodexAdapter, OpenCodeAdapter};
use config::{describe_auth, load_config_file, resolve_config, save_config_file};
use runtime::{run_engine, SharedAdapter};
use store::{BatonStore, ImportedMessage, NewThread};
use types::ProviderId;
use usage::{cache_hit_rate, format_usage_line, sum_usage};

fn main() {
    if let Err(error) = main_result() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn main_result() -> Result<()> {
    let cli = cli::Cli::parse();
    let cwd = cli
        .cwd
        .clone()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let home = paths::baton_home();
    let command = cli.command.clone().unwrap_or(cli::Commands::Chat);

    match command {
        cli::Commands::Chat => run_chat(cli, &home, &cwd),
        cli::Commands::Threads => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let resolved = resolve_config(&home, &cwd);
            let threads = store.list_threads(Some(cwd.to_string_lossy().as_ref()))?;
            if threads.is_empty() {
                println!("no threads in this directory yet. Run baton to start one.");
                return Ok(());
            }
            for thread in threads {
                let model = thread
                    .current_model
                    .clone()
                    .unwrap_or_else(|| resolved.for_provider(thread.current_provider).model.clone());
                println!(
                    "{}  {}/{}  {}",
                    &thread.id[..8],
                    thread.current_provider,
                    model,
                    thread.title
                );
            }
            Ok(())
        }
        cli::Commands::New { title } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let resolved = resolve_config(&home, &cwd);
            let provider = cli
                .provider
                .as_deref()
                .and_then(ProviderId::parse)
                .unwrap_or(ProviderId::Claude);
            let model = cli
                .model
                .clone()
                .or_else(|| Some(resolved.for_provider(provider).model.clone()));
            let thread_title = if title.is_empty() {
                "New thread".to_string()
            } else {
                title.join(" ")
            };
            let thread = store.create_thread(NewThread {
                title: thread_title,
                cwd: cwd.to_string_lossy().to_string(),
                current_provider: provider,
                current_model: cli.model.clone(),
            })?;
            println!(
                "created thread {} ({}/{})",
                &thread.id[..8],
                provider,
                model.unwrap_or_default()
            );
            Ok(())
        }
        cli::Commands::Import => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let resolved = resolve_config(&home, &cwd);
            let claude_dir = cli
                .claude_home
                .clone()
                .map(PathBuf::from)
                .unwrap_or_else(paths::claude_global_settings_dir)
                .join("projects");
            let codex_home = cli
                .codex_home
                .clone()
                .map(PathBuf::from)
                .unwrap_or_else(paths::codex_global_home)
                .join("sessions");
            let claude_sessions = scanner::scan_claude_sessions(&claude_dir, cli.limit);
            let codex_sessions = scanner::scan_codex_sessions(&codex_home, cli.limit);
            let mut imported = 0;
            let mut skipped = 0;
            for session in claude_sessions.into_iter().chain(codex_sessions) {
                if store
                    .find_thread_by_native_session(session.provider, &session.native_session_id)?
                    .is_some()
                {
                    skipped += 1;
                    continue;
                }
                if cli.dry_run {
                    println!(
                        "[dry-run] {} {}  {}",
                        session.provider,
                        &session.native_session_id[..8.min(session.native_session_id.len())],
                        session.title
                    );
                    continue;
                }
                let model = session.model.clone().or(Some(resolved.for_provider(session.provider).model.clone()));
                let messages: Vec<ImportedMessage> = session
                    .messages
                    .iter()
                    .map(|message| ImportedMessage {
                        role: if message.role == "user" { "user" } else { "assistant" },
                        text: message.text.clone(),
                        created_at: message.created_at.clone(),
                    })
                    .collect();
                let thread = store.import_thread(
                    NewThread {
                        title: session.title.clone(),
                        cwd: session
                            .cwd
                            .clone()
                            .unwrap_or_else(|| cwd.to_string_lossy().to_string()),
                        current_provider: session.provider,
                        current_model: model,
                    },
                    &session.native_session_id,
                    if session.provider == ProviderId::Codex {
                        Some(session.path.to_string_lossy().to_string())
                    } else {
                        None
                    },
                    &messages,
                )?;
                imported += 1;
                println!(
                    "imported {} {} as {}  {}",
                    session.provider,
                    &session.native_session_id[..8.min(session.native_session_id.len())],
                    &thread.id[..8],
                    thread.title
                );
            }
            if !cli.dry_run {
                println!("imported {imported}, skipped {skipped} already present");
            }
            Ok(())
        }
        cli::Commands::Checkpoints { thread_id } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let thread = resolve_thread(&store, &thread_id)?;
            let checkpoints = store.list_checkpoints(&thread.id)?;
            if checkpoints.is_empty() {
                println!("no checkpoints yet");
                return Ok(());
            }
            for checkpoint in checkpoints {
                println!("turn {:>3}  {}", checkpoint.turn, checkpoint.git_ref);
            }
            Ok(())
        }
        cli::Commands::Diff { thread_id, turn } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let thread = resolve_thread(&store, &thread_id)?;
            let checkpoints = store.list_checkpoints(&thread.id)?;
            if checkpoints.is_empty() {
                anyhow::bail!("no checkpoints yet");
            }
            let checkpoint = match turn {
                Some(turn) => checkpoints
                    .into_iter()
                    .find(|checkpoint| checkpoint.turn == turn)
                    .ok_or_else(|| anyhow::anyhow!("no checkpoint for turn {turn}"))?,
                None => checkpoints
                    .last()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("no checkpoints"))?,
            };
            let diff = checkpoints::diff_checkpoint_against_worktree(Path::new(&thread.cwd), &checkpoint.git_ref)?;
            if diff.is_empty() {
                println!("(no differences)");
            } else {
                println!("{diff}");
            }
            Ok(())
        }
        cli::Commands::Restore { thread_id, turn } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let thread = resolve_thread(&store, &thread_id)?;
            let checkpoint = store
                .get_checkpoint(&thread.id, turn)?
                .ok_or_else(|| anyhow::anyhow!("no checkpoint for turn {turn}"))?;
            checkpoints::restore_checkpoint(Path::new(&thread.cwd), &checkpoint.git_ref)?;
            println!("restored {} to {}", thread.cwd, checkpoint.git_ref);
            Ok(())
        }
        cli::Commands::Usage { thread_id } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let resolved = resolve_config(&home, &cwd);
            let threads = match thread_id {
                Some(thread_id) => vec![resolve_thread(&store, &thread_id)?],
                None => store.list_threads(None)?,
            };
            if threads.is_empty() {
                anyhow::bail!("no threads found");
            }
            for thread in threads {
                let rows = store.list_usage(Some(&thread.id))?;
                if rows.is_empty() {
                    continue;
                }
                let totals = sum_usage(&rows);
                let rate = cache_hit_rate(&totals.total_usage());
                let rate_label = match rate {
                    None => "n/a".to_string(),
                    Some(rate) => format!("{:.0}%", rate * 100.0),
                };
                let model = thread
                    .current_model
                    .clone()
                    .unwrap_or_else(|| resolved.for_provider(thread.current_provider).model.clone());
                println!(
                    "{}  {}/{}  {}  {}  hit {}",
                    &thread.id[..8],
                    thread.current_provider,
                    model,
                    thread.title,
                    format_usage_line(&totals),
                    rate_label
                );
            }
            Ok(())
        }
        cli::Commands::Login { provider } => {
            let provider = ProviderId::parse(&provider)
                .ok_or_else(|| anyhow::anyhow!("unknown provider {provider}, expected claude, codex, cursor, grok, opencode, or antigravity"))?;
            let command: Vec<&str> = match provider {
                ProviderId::Codex => vec!["codex", "login"],
                ProviderId::Claude => vec!["claude", "auth", "login"],
                ProviderId::Cursor => vec!["agent", "login"],
                ProviderId::Grok => vec!["grok", "login"],
                ProviderId::OpenCode => vec!["opencode", "auth", "login"],
                ProviderId::Antigravity => {
                    println!("Antigravity has no CLI login.");
                    println!("1. Install Antigravity and enable it in its settings.");
                    println!("2. Sign in with Google when prompted.");
                    println!("For API-rate billing set GEMINI_API_KEY and run: baton auth antigravity api");
                    return Ok(());
                }
            };
            println!("running: {}", command.join(" "));
            let status = std::process::Command::new(command[0])
                .args(&command[1..])
                .status()?;
            if !status.success() {
                anyhow::bail!("{} failed with {status}", command.join(" "));
            }
            if provider == ProviderId::Cursor {
                println!("cursor login complete (if the agent command was not found, install Cursor CLI and try `cursor-agent login`)");
            }
            Ok(())
        }
        cli::Commands::Auth { provider, mode } => {
            let resolved = resolve_config(&home, &cwd);
            match (provider, mode) {
                (None, _) => {
                    for provider in ProviderId::all() {
                        println!("{:<12} {}", provider, describe_auth(resolved.for_provider(*provider)));
                    }
                }
                (Some(provider), Some(mode)) => {
                    let provider = ProviderId::parse(&provider)
                        .ok_or_else(|| anyhow::anyhow!("unknown provider {provider}, expected claude or codex"))?;
                    let mode = config::AuthMode::parse(&mode)
                        .ok_or_else(|| anyhow::anyhow!("unknown auth mode {mode}, expected subscription or api"))?;
                    let provider_config = resolved.for_provider(provider);
                    if mode == config::AuthMode::Api && provider_config.api_key.is_none() {
                        anyhow::bail!(
                            "no API key configured for {provider}. Set {} in the environment, the harness config, or {}/config.json first.",
                            provider_config.api_key_env_name,
                            home.display()
                        );
                    }
                    let mut file = load_config_file(&home).unwrap_or_default();
                    file.providers
                        .entry(provider.as_str().to_string())
                        .or_default()
                        .mode = Some(mode);
                    save_config_file(&home, &file)?;
                    println!("{provider} billing set to {}", mode.as_str());
                }
                (Some(provider), None) => {
                    let parsed = ProviderId::parse(&provider)
                        .ok_or_else(|| anyhow::anyhow!("unknown provider {provider}, expected claude, codex, cursor, grok, opencode, or antigravity"))?;
                    println!("{provider}: {}", describe_auth(resolved.for_provider(parsed)));
                }
            }
            Ok(())
        }
    }
}

fn resolve_thread(store: &BatonStore, thread_id: &str) -> Result<store::ThreadRow> {
    let exact = store.get_thread(thread_id)?;
    if let Some(thread) = exact {
        return Ok(thread);
    }
    store
        .list_threads(None)?
        .into_iter()
        .find(|thread| thread.id.starts_with(thread_id))
        .ok_or_else(|| anyhow::anyhow!("no thread matches {thread_id}"))
}

fn run_chat(cli: cli::Cli, home: &PathBuf, cwd: &PathBuf) -> Result<()> {
    let store = Arc::new(Mutex::new(BatonStore::open(&paths::baton_db_path())?));
    let resolved = resolve_config(home, cwd);

    let thread = match &cli.thread {
        Some(thread_id) => {
            let guard = store.lock().unwrap();
            resolve_thread(&guard, thread_id)?
        }
        None => {
            let provider = cli
                .provider
                .as_deref()
                .and_then(ProviderId::parse)
                .unwrap_or(ProviderId::Claude);
            let model = cli
                .model
                .clone()
                .or_else(|| Some(resolved.for_provider(provider).model.clone()));
            guard_draft_thread(&store, provider, model, cwd)?
        }
    };

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let (adapters, interrupt_handles) = tokio_runtime.block_on(async {
        let cwd_for_codex = cwd.clone();
        let codex_spawner = Arc::new(
            move |auth_env: &[(String, String)]| -> anyhow::Result<baton::adapters::TransportHalves> {
                let mut command = tokio::process::Command::new("codex");
                command.args(["app-server"]);
                command.current_dir(&cwd_for_codex);
                command.stdin(std::process::Stdio::piped());
                command.stdout(std::process::Stdio::piped());
                command.stderr(std::process::Stdio::null());
                for (key, value) in auth_env {
                    command.env(key, value);
                }
                child_halves(command.spawn()?)
            },
        );
        let cwd_for_grok = cwd.clone();
        let grok_spawner = Arc::new(
            move |auth_env: &[(String, String)]| -> anyhow::Result<baton::adapters::TransportHalves> {
                let mut command = tokio::process::Command::new("grok");
                command.args(["agent", "stdio"]);
                command.current_dir(&cwd_for_grok);
                command.env("GROK_OAUTH2_REFERRER", "baton");
                command.stdin(std::process::Stdio::piped());
                command.stdout(std::process::Stdio::piped());
                command.stderr(std::process::Stdio::null());
                for (key, value) in auth_env {
                    command.env(key, value);
                }
                child_halves(command.spawn()?)
            },
        );
        let cwd_for_antigravity = cwd.clone();
        let antigravity_home = home.join("antigravity");
        let antigravity_spawner = Arc::new(
            move |auth_env: &[(String, String)]| -> anyhow::Result<baton::adapters::TransportHalves> {
                std::fs::create_dir_all(&antigravity_home)?;
                let mut command = tokio::process::Command::new("antigravity");
                command.current_dir(&cwd_for_antigravity);
                command.env("AGY_ACP_FORCE_FILE_STORAGE", "1");
                command.env("GEMINI_HOME", &antigravity_home);
                if !auth_env.iter().any(|(key, _)| key == "GEMINI_API_KEY" || key == "GOOGLE_API_KEY") {
                    command.env_remove("GEMINI_API_KEY");
                    command.env_remove("GOOGLE_API_KEY");
                }
                command.stdin(std::process::Stdio::piped());
                command.stdout(std::process::Stdio::piped());
                command.stderr(std::process::Stdio::null());
                for (key, value) in auth_env {
                    command.env(key, value);
                }
                child_halves(command.spawn()?)
            },
        );
        let cwd_for_cursor = cwd.clone();
        let cursor_spawner = Arc::new(
            move |auth_env: &[(String, String)]| -> anyhow::Result<baton::adapters::TransportHalves> {
                let mut command = tokio::process::Command::new("cursor-agent");
                command.current_dir(&cwd_for_cursor);
                command.stdin(std::process::Stdio::piped());
                command.stdout(std::process::Stdio::piped());
                command.stderr(std::process::Stdio::null());
                for (key, value) in auth_env {
                    command.env(key, value);
                }
                child_halves(command.spawn()?)
            },
        );
        let opencode_spawner = Arc::new(
            move |_auth_env: &[(String, String)]| -> anyhow::Result<baton::adapters::opencode::OpenCodeChild> {
                let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
                let port = listener.local_addr()?.port();
                drop(listener);
                let password = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
                let mut command = tokio::process::Command::new("opencode");
                command.args(["serve", "--hostname=127.0.0.1", "--port", &port.to_string()]);
                command.env("OPENCODE_PASSWORD", &password);
                command.env("OPENCODE_SERVER_PASSWORD", &password);
                command.env("OPENCODE_CONFIG_CONTENT", "{}");
                command.stdin(std::process::Stdio::null());
                command.stdout(std::process::Stdio::piped());
                command.stderr(std::process::Stdio::null());
                let child = command.spawn()?;
                Ok(baton::adapters::opencode::OpenCodeChild { port, password, child })
            },
        );

        let claude: SharedAdapter = Arc::new(tokio::sync::Mutex::new(Box::new(ClaudeAdapter::new())));
        let codex: SharedAdapter = Arc::new(tokio::sync::Mutex::new(Box::new(CodexAdapter::new(codex_spawner))));
        let grok: SharedAdapter = Arc::new(tokio::sync::Mutex::new(Box::new(AcpAdapter::new(
            ProviderId::Grok,
            grok_spawner,
        ))));
        let antigravity: SharedAdapter = Arc::new(tokio::sync::Mutex::new(Box::new(AcpAdapter::new(
            ProviderId::Antigravity,
            antigravity_spawner,
        ))));
        let cursor: SharedAdapter = Arc::new(tokio::sync::Mutex::new(Box::new(AcpAdapter::new(
            ProviderId::Cursor,
            cursor_spawner,
        ))));
        let opencode: SharedAdapter = Arc::new(tokio::sync::Mutex::new(Box::new(OpenCodeAdapter::new(
            opencode_spawner,
        ))));
        let adapters = vec![claude, codex, cursor, grok, opencode, antigravity];
        let mut handles = Vec::new();
        for adapter in &adapters {
            handles.push(adapter.lock().await.interrupt_handle());
        }
        (adapters, handles)
    });

    let (engine_command_sender, engine_command_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (engine_event_sender, engine_event_receiver) = std::sync::mpsc::channel();
    let (turn_event_sender, turn_event_receiver) = std::sync::mpsc::channel();

    let runtime_home = home.clone();
    let engine_store = store.clone();
    let engine_task = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let _ = rt.block_on(run_engine(
            engine_store,
            runtime_home,
            adapters,
            interrupt_handles,
            engine_command_receiver,
            engine_event_sender,
            turn_event_sender,
        ));
    });

    let result = tui::run_tui(
        store,
        home.clone(),
        thread,
        engine_command_sender,
        engine_event_receiver,
        turn_event_receiver,
    );

    let _ = engine_task.join();
    result
}

fn guard_draft_thread(
    store: &Arc<Mutex<BatonStore>>,
    provider: ProviderId,
    model: Option<String>,
    cwd: &PathBuf,
) -> Result<store::ThreadRow> {
    let guard = store.lock().unwrap();
    Ok(guard.draft_thread(NewThread {
        title: "New thread".to_string(),
        cwd: cwd.to_string_lossy().to_string(),
        current_provider: provider,
        current_model: model,
    }))
}
