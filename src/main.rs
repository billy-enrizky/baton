use baton::{adapters, checkpoints, cli, config, doctor, mcp, orchestrate, paths, process, provider_runtime, resume, runtime, scanner, status, store, tui, types, usage, worktree};

use anyhow::Result;
use clap::Parser;
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adapters::{AcpAdapter, BatonAdapter, ClaudeAdapter, OpenCodeAdapter};
use config::{describe_auth, load_config_file, resolve_config, save_config_file};
use runtime::{run_engine, EngineCommand, EngineEvent, SharedAdapter, SharedStore};
use store::{BatonStore, ImportedMessage, NewThread};
use types::{PermissionMode, ProviderId, SessionMode, TurnPhase};
use adapters::TurnEvent;
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
    let cli_prompt = cli.prompt.clone();

    reap_startup_processes();

    let permission_override = if cli.yolo {
        Some(PermissionMode::BypassPermissions)
    } else if cli.full_auto {
        Some(PermissionMode::AcceptEdits)
    } else if cli.plan {
        Some(PermissionMode::Plan)
    } else {
        None
    };
    let permission_mode = permission_override.unwrap_or(PermissionMode::Default);

    match command {
        cli::Commands::Exec { prompt, json } => {
            if cli.mode.is_some() {
                anyhow::bail!(
                    "--mode applies to the interactive TUI; use `baton orchestrate` for a non-interactive run"
                );
            }
            let prompt = prompt.join(" ");
            if prompt.is_empty() {
                anyhow::bail!("usage: baton exec <prompt>");
            }
            let store = Arc::new(Mutex::new(BatonStore::open(&paths::baton_db_path())?));
            let resolved = resolve_config(&home, &cwd);
            let provider = cli
                .provider
                .as_deref()
                .and_then(ProviderId::parse)
                .unwrap_or_else(|| default_provider_from_configs(&home, &cwd));
            let model = cli
                .model
                .clone()
                .unwrap_or_else(|| resolved.for_provider(provider).model.clone());
            let thread = store.lock().unwrap().draft_thread(crate::store::NewThread {
                title: baton::handoff::derive_title(&prompt),
                cwd: cwd.to_string_lossy().to_string(),
                current_provider: provider,
                current_model: Some(model),
            });
            {
                let guard = store.lock().unwrap();
                let persisted = guard.create_thread_with_id(
                    crate::store::NewThread {
                        title: thread.title.clone(),
                        cwd: thread.cwd.clone(),
                        current_provider: thread.current_provider,
                        current_model: thread.current_model.clone(),
                    },
                    thread.id.clone(),
                )?;
                drop(guard);
                let _ = persisted;
                store
                    .lock()
                    .unwrap()
                    .set_thread_permission_mode(&thread.id, permission_mode)?;
            }

            let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let adapters = build_all_adapters(&store, &home, &cwd);
            let interrupt_handles: Vec<(ProviderId, Arc<dyn Fn() + Send + Sync>)> =
                tokio_runtime.block_on(async {
                    let mut handles = Vec::new();
                    for adapter in &adapters {
                        let adapter = adapter.lock().await;
                        handles.push((adapter.id(), adapter.interrupt_handle()));
                    }
                    handles
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
                    Vec::new(),
                    engine_command_receiver,
                    engine_event_sender,
                    turn_event_sender,
                ));
            });

            let _ = engine_command_sender.send(EngineCommand::Submit {
                thread_id: thread.id.clone(),
                text: prompt,
                permission_mode,
            });

            let mut result_text = String::new();
            let mut streamed_text = String::new();
            let mut result_phase: Option<TurnPhase> = None;
            let mut result_error: Option<String> = None;
            let mut result_ok: Option<bool> = None;
            let mut timed_out = false;
            let mut saw_output = false;
            let show_progress = std::io::stderr().is_terminal() && !json;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                if std::time::Instant::now() > deadline {
                    timed_out = true;
                    break;
                }
                while let Ok((event_thread, event)) = turn_event_receiver.try_recv() {
                    if event_thread != thread.id {
                        continue;
                    }
                    match event {
                        TurnEvent::TextDelta { text } => {
                            if !text.is_empty() {
                                if !json {
                                    print!("{text}");
                                    let _ = std::io::stdout().flush();
                                }
                                streamed_text.push_str(&text);
                                saw_output = true;
                            }
                        }
                        TurnEvent::ToolStarted { tool, detail, .. } if show_progress => {
                            match detail {
                                Some(detail) => eprintln!("· {tool} {detail}"),
                                None => eprintln!("· {tool}"),
                            }
                        }
                        TurnEvent::ToolCompleted { tool, detail, exit_code, .. } if show_progress => {
                            let marker = if exit_code.unwrap_or(0) == 0 { "✓" } else { "✗" };
                            match detail {
                                Some(detail) => eprintln!("{marker} {tool} {detail}"),
                                None => eprintln!("{marker} {tool}"),
                            }
                        }
                        _ => {}
                    }
                }
                match engine_event_receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(EngineEvent::TurnFinished { ok, error, phase, .. }) => {
                        result_ok = Some(ok);
                        result_phase = Some(phase);
                        result_error = error.clone();
                        if !ok {
                            if let Some(error) = error {
                                let label = match phase {
                                    TurnPhase::Interrupted => "turn interrupted",
                                    TurnPhase::Unverified => "turn unverified",
                                    _ => "turn failed",
                                };
                                eprintln!("{label}: {error}");
                            }
                        }
                        break;
                    }
                    Ok(EngineEvent::Turn(TurnEvent::TurnCompleted { final_text, .. })) => {
                        result_text = final_text;
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
            let _ = engine_command_sender.send(EngineCommand::Quit);
            let _ = engine_task.join();

            // Providers that do not emit deltas only deliver final text here.
            // If deltas were streamed, do not print the same answer twice.
            if streamed_text.is_empty() && !result_text.is_empty() {
                if !json {
                    println!("{result_text}");
                }
                saw_output = true;
            } else if !streamed_text.is_empty() && !json {
                println!();
            }
            if timed_out {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema_version": status::SCHEMA_VERSION,
                            "thread_id": thread.id,
                            "ok": false,
                            "phase": "unverified",
                            "error": "exec timed out after 300s",
                            "text": "",
                        }))?
                    );
                } else {
                    eprintln!("exec timed out after 300s");
                }
                std::process::exit(1);
            }
            let result_ok = result_ok.unwrap_or(false);
            if json {
                let text = if result_text.is_empty() {
                    streamed_text
                } else {
                    result_text
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "thread_id": thread.id,
                        "ok": result_ok,
                        "phase": result_phase.map(|phase| phase.as_str()).unwrap_or("unverified"),
                        "error": result_error,
                        "text": text,
                    }))?
                );
            } else if result_ok && !saw_output {
                eprintln!("no output from provider");
            }
            if !result_ok || !saw_output {
                std::process::exit(1);
            }
            return Ok(());
        }
        cli::Commands::Update => {
            println!("updating baton...");
            let status = std::process::Command::new("cargo")
                .args(["install", "--path", "--force"])
                .current_dir(&cwd)
                .status();
            match status {
                Ok(status) if status.success() => println!("baton updated"),
                _ => {
                    println!("cargo install not available in this context.");
                    println!("to update: cargo install --git https://github.com/billy-enrizky/baton --force");
                    println!("or from source: cargo install --path /path/to/baton --force");
                }
            }
            return Ok(());
        }
        cli::Commands::Chat => run_chat(cli, &home, &cwd, permission_override, cli_prompt),
        cli::Commands::Mcp => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            mcp::run_stdio(mcp::McpServer::new(store, home.clone(), cwd.clone()))
        }
        cli::Commands::Threads { json } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let resolved = resolve_config(&home, &cwd);
            let threads = store.list_threads_for_path(&cwd)?;
            if threads.is_empty() {
                println!("no threads in this directory yet. Run baton to start one.");
                return Ok(());
            }
            if json {
                let mut values = Vec::new();
                for thread in &threads {
                    let model = thread
                        .current_model
                        .clone()
                        .unwrap_or_else(|| resolved.for_provider(thread.current_provider).model.clone());
                    let turn_records = store.list_turn_records(&thread.id)?;
                    let turn_count = turn_records.len();
                    let latest = turn_records.last();
                    let latest_phase = latest.map(|turn| turn.phase);
                    let open_turn = latest_phase
                        .map(|phase| !phase.is_terminal())
                        .unwrap_or(false);
                    let phase = latest_phase
                        .map(|phase| phase.as_str().to_string())
                        .unwrap_or_else(|| "idle".to_string());
                    let last_error = latest.and_then(|turn| turn.error.clone());
                    let provider_turn_id = latest.and_then(|turn| turn.provider_turn_id.clone());
                    let started_at = latest.map(|turn| turn.started_at.clone());
                    let ended_at = latest.and_then(|turn| turn.ended_at.clone());
                    let queued = store.list_queued_messages(&thread.id)?;
                    values.push(serde_json::json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "thread_id": thread.id,
                        "title": thread.title,
                        "provider": thread.current_provider.as_str(),
                        "model": model,
                        "session_mode": thread.session_mode.as_str(),
                        "permission_mode": thread.permission_mode.as_str(),
                        "goal": thread.goal,
                        "goal_paused": thread.goal_paused,
                        "phase": phase,
                        "turn_count": turn_count,
                        "provider_turn_id": provider_turn_id,
                        "started_at": started_at,
                        "ended_at": ended_at,
                        "last_error": last_error,
                        "open_turn": open_turn,
                        "stale_hint": if open_turn { Some(status::STALE_HINT) } else { None },
                        "queue_count": queued.len(),
                        "queue": queued,
                    }));
                }
                println!("{}", serde_json::to_string_pretty(&values)?);
                return Ok(());
            }
            let mut has_open = false;
            for thread in threads {
                let model = thread
                    .current_model
                    .clone()
                    .unwrap_or_else(|| resolved.for_provider(thread.current_provider).model.clone());
                let turns = store.list_turn_records(&thread.id)?;
                let turn_count = turns.len();
                let latest_phase = turns.into_iter().last().map(|turn| turn.phase);
                if latest_phase
                    .map(|phase| {
                        !phase.is_terminal()
                    })
                    .unwrap_or(false)
                {
                    has_open = true;
                }
                let phase = latest_phase
                    .map(|phase| phase.as_str().to_string())
                    .unwrap_or_else(|| "idle".to_string());
                let queued = store.list_queued_messages(&thread.id)?.len();
                println!(
                    "{}  {}/{}  session:{}  {}  turns:{}  queue:{}  {}",
                    &thread.id[..8],
                    thread.current_provider,
                    model,
                    thread.session_mode,
                    phase,
                    turn_count,
                    queued,
                    thread.title
                );
            }
            if has_open {
                println!("note: one or more turns are open; use `baton status <id>` to inspect them");
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
                .unwrap_or_else(|| default_provider_from_configs(&home, &cwd));
            let model = cli
                .model
                .clone()
                .or_else(|| Some(resolved.for_provider(provider).model.clone()));
            let thread_title = if title.is_empty() {
                "New thread".to_string()
            } else {
                title.join(" ")
            };
            let session_mode = match cli.mode.as_deref() {
                Some(value) => SessionMode::parse(value).ok_or_else(|| {
                    anyhow::anyhow!("unknown mode \"{value}\"; use single or orchestrate")
                })?,
                None => config::get_default_session_mode(home.as_path()),
            };
            let mut thread = store.create_thread_with_mode(
                NewThread {
                    title: thread_title,
                    cwd: cwd.to_string_lossy().to_string(),
                    current_provider: provider,
                    current_model: cli.model.clone(),
                },
                session_mode,
            )?;
            if let Some(mode) = permission_override {
                store.set_thread_permission_mode(&thread.id, mode)?;
                thread.permission_mode = mode;
            }
            println!(
                "created thread {} ({}/{}) session {} mode {}",
                &thread.id[..8],
                provider,
                model.unwrap_or_default(),
                thread.session_mode,
                thread.permission_mode
            );
            Ok(())
        }
        cli::Commands::Fork { title, turn, native, json } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let source = match cli.thread.as_deref() {
                Some(thread_id) => resolve_thread(&store, thread_id)?,
                None => store
                    .list_threads_for_path(&cwd)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("no threads in this directory yet"))?,
            };
            let title = if title.is_empty() {
                match turn {
                    Some(turn) => format!("{} (fork at turn {turn})", source.title),
                    None => format!("{} (fork)", source.title),
                }
            } else {
                title.join(" ")
            };
            let mut native_session = None;
            if native {
                if source.current_provider != ProviderId::Codex {
                    anyhow::bail!(
                        "native fork is implemented for codex only; omit --native to fork this transcript"
                    );
                }
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?;
                let recorder_store = Arc::new(Mutex::new(BatonStore::open(&paths::baton_db_path())?));
                let recorder = process::ProcessRecorder::new(recorder_store, source.cwd.clone());
                native_session = runtime.block_on(provider_runtime::fork_codex_session(
                    &store,
                    &home,
                    &source,
                    turn,
                    recorder,
                ))?;
                if native_session.is_none() {
                    anyhow::bail!("codex did not return a native fork id");
                }
            }
            let forked = store.fork_thread_through_turn(&source.id, &title, turn)?;
            if let Some((native_id, model)) = native_session.as_ref() {
                store.upsert_provider_session(
                    &forked.id,
                    ProviderId::Codex,
                    native_id,
                    Some(model.clone()),
                    None,
                )?;
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "source_thread_id": source.id,
                        "through_turn": turn,
                        "thread_id": forked.id,
                        "title": forked.title,
                        "cwd": forked.cwd,
                        "provider": forked.current_provider.as_str(),
                        "model": forked.current_model,
                        "native": native,
                        "native_session_id": native_session.as_ref().map(|(id, _)| id),
                    }))?
                );
            } else {
                println!(
                    "forked {} -> {}{} ({}/{}){}",
                    &source.id[..8.min(source.id.len())],
                    &forked.id[..8.min(forked.id.len())],
                    turn.map(|turn| format!(" through turn {turn}"))
                        .unwrap_or_default(),
                    forked.current_provider,
                    forked.current_model.as_deref().unwrap_or("default"),
                    if native_session.is_some() { " native" } else { "" },
                );
            }
            Ok(())
        }
        cli::Commands::Orchestrate {
            goal,
            max_tasks,
            plan_file,
            list,
            run,
            integrate,
            open_prs,
            tasks,
            progress,
            json,
        } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            if list {
                let runs = orchestrate::run_rows(&store, 20)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema_version": orchestrate::SCHEMA_VERSION,
                            "runs": runs.iter().map(|run| serde_json::json!({
                                "run_id": run.id,
                                "goal": run.goal,
                                "status": run.status,
                                "coordinator_provider": run.coordinator_provider.as_str(),
                                "coordinator_model": run.coordinator_model,
                                "summary": run.summary,
                                "dirty_source": run.dirty_source,
                                "created_at": run.created_at,
                                "finished_at": run.finished_at,
                            })).collect::<Vec<_>>(),
                        }))?
                    );
                } else if runs.is_empty() {
                    println!("no orchestration runs yet");
                } else {
                    for run in runs {
                        println!(
                            "{}  {:<9}  {}/{}  {}",
                            &run.id[..8.min(run.id.len())],
                            run.status,
                            run.coordinator_provider,
                            run.coordinator_model,
                            run.summary.unwrap_or_else(|| run.goal.clone())
                        );
                    }
                }
                return Ok(());
            }
            if let Some(run_id) = run {
                let run_id = orchestrate::resolve_run_id(&store, &run_id)?;
                let tasks = orchestrate::task_rows(&store, &run_id)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema_version": orchestrate::SCHEMA_VERSION,
                            "run_id": run_id,
                            "tasks": tasks.iter().map(|task| serde_json::json!({
                                "id": task.id,
                                "task": task.position,
                                "title": task.title,
                                "provider": task.provider.as_str(),
                                "model": task.model,
                                "status": task.status,
                                "thread_id": task.thread_id,
                                "text": task.text,
                                "error": task.error,
                                "worktree_path": task.worktree_path,
                                "branch": task.branch,
                                "commit": task.commit_sha,
                                "merged_at": task.merged_at,
                                "pr_url": task.pr_url,
                                "created_at": task.created_at,
                                "finished_at": task.finished_at,
                            })).collect::<Vec<_>>(),
                        }))?
                    );
                } else if tasks.is_empty() {
                    println!("no tasks for run {run_id}");
                } else {
                    for task in tasks {
                        println!(
                            "{}  {:<9}  {}/{}  {}",
                            task.id,
                            task.status,
                            task.provider,
                            task.model,
                            task.title
                        );
                        if let Some(worktree) = task.worktree_path {
                            println!("  worktree: {worktree}");
                        }
                        if let Some(commit) = task.commit_sha {
                            println!("  commit: {}", &commit[..8.min(commit.len())]);
                        }
                        if let Some(merged_at) = task.merged_at {
                            println!("  merged: {merged_at}");
                        }
                        if let Some(pr_url) = task.pr_url {
                            println!("  pr: {pr_url}");
                        }
                        if let Some(error) = task.error {
                            println!("  error: {error}");
                        }
                    }
                }
                return Ok(());
            }
            if let Some(run_id) = open_prs {
                let report = orchestrate::open_prs(&store, &cwd, &run_id, &tasks)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&orchestrate::ship_to_json(&report))?
                    );
                } else {
                    println!("{}", orchestrate::render_ship(&report));
                }
                if !report.failed.is_empty() {
                    std::process::exit(1);
                }
                return Ok(());
            }
            if let Some(run_id) = integrate {
                let report = orchestrate::integrate(&store, &cwd, &run_id, &tasks)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&orchestrate::integrate_to_json(&report))?
                    );
                } else {
                    println!("{}", orchestrate::render_integrate(&report));
                }
                if report.conflict.is_some() {
                    std::process::exit(1);
                }
                return Ok(());
            }
            let goal = goal.join(" ");
            if goal.trim().is_empty() {
                anyhow::bail!(
                    "usage: baton orchestrate <goal> | --list | --run <id> | --integrate <run-id> | --open-prs <run-id>"
                );
            }
            let provider = cli
                .provider
                .as_deref()
                .and_then(ProviderId::parse)
                .unwrap_or_else(|| default_provider_from_configs(&home, &cwd));
            let resolved = resolve_config(&home, &cwd);
            let model = cli
                .model
                .clone()
                .unwrap_or_else(|| resolved.for_provider(provider).model.clone());
            let plan_path = plan_file.map(PathBuf::from);
            let report = if progress {
                let sink = |event: orchestrate::OrchestrationProgress| {
                    eprintln!("{}", orchestrate::render_progress(&event));
                };
                orchestrate::run_with_progress(
                    &goal,
                    &cwd,
                    &home,
                    provider,
                    &model,
                    max_tasks,
                    plan_path.as_deref(),
                    &store,
                    permission_mode,
                    Some(&sink as &orchestrate::ProgressFn<'_>),
                    None,
                )?
            } else {
                orchestrate::run(
                    &goal,
                    &cwd,
                    &home,
                    provider,
                    &model,
                    max_tasks,
                    plan_path.as_deref(),
                    &store,
                    permission_mode,
                )?
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&orchestrate::to_json(&report))?);
            } else {
                println!("{}", orchestrate::render(&report));
            }
            if report.status == "cancelled" {
                std::process::exit(130);
            }
            if report.status == "failed" {
                std::process::exit(1);
            }
            Ok(())
        }
        cli::Commands::Worktrees { clean, force, run, json } => {
            let (shipped_branches, store_warning) =
                match BatonStore::open(&paths::baton_db_path())
                    .and_then(|store| store.shipped_worktree_branches())
                {
                    Ok(branches) => (branches, None),
                    Err(error) => (
                        Vec::new(),
                        Some(format!("could not read shipped PR markers: {error}")),
                    ),
                };
            let is_shipped = |entry: &worktree::ManagedWorktree| {
                entry
                    .branch
                    .as_ref()
                    .map(|branch| shipped_branches.iter().any(|shipped| shipped == branch))
                    .unwrap_or(false)
            };
            let entries_json = |entries: &[worktree::ManagedWorktree]| {
                entries
                    .iter()
                    .map(|entry| {
                        serde_json::json!({
                            "path": entry.path.display().to_string(),
                            "run": entry.run,
                            "task": entry.task,
                            "branch": entry.branch,
                            "repo_root": entry.repo_root.as_ref().map(|root| root.display().to_string()),
                            "head": entry.head,
                            "dirty": entry.dirty,
                            "merged": entry.merged,
                            "shipped": is_shipped(entry),
                            "error": entry.error,
                        })
                    })
                    .collect::<Vec<_>>()
            };
            if clean {
                let mut cleanup = worktree::clean_managed(
                    &home,
                    run.as_deref(),
                    force,
                    &shipped_branches,
                )?;
                if let Some(warning) = store_warning {
                    cleanup.warnings.push(warning);
                }
                let remaining: Vec<_> = worktree::list_managed(&home)?
                    .into_iter()
                    .filter(|entry| {
                        run.as_deref()
                            .map(|filter| worktree::run_matches(entry, filter))
                            .unwrap_or(true)
                    })
                    .collect();
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema_version": 1,
                            "worktrees": entries_json(&remaining),
                            "removed": entries_json(&cleanup.removed),
                            "skipped": cleanup.skipped.iter().map(|skip| serde_json::json!({
                                "path": skip.path.display().to_string(),
                                "branch": skip.branch,
                                "reason": skip.reason,
                            })).collect::<Vec<_>>(),
                            "warnings": cleanup.warnings,
                        }))?
                    );
                } else {
                    if cleanup.removed.is_empty() {
                        println!("removed no worktrees");
                    } else {
                        println!("removed {} worktree(s)", cleanup.removed.len());
                        for entry in &cleanup.removed {
                            println!(
                                "  {} · {}",
                                entry.branch.as_deref().unwrap_or("-"),
                                entry.path.display()
                            );
                        }
                    }
                    for skip in &cleanup.skipped {
                        println!(
                            "  skipped {} · {}",
                            skip.branch.as_deref().unwrap_or("-"),
                            skip.reason
                        );
                    }
                    for warning in &cleanup.warnings {
                        println!("  warning: {warning}");
                    }
                    if !remaining.is_empty() {
                        println!("{} worktree(s) remain", remaining.len());
                    }
                }
            } else {
                let entries: Vec<_> = worktree::list_managed(&home)?
                    .into_iter()
                    .filter(|entry| {
                        run.as_deref()
                            .map(|filter| worktree::run_matches(entry, filter))
                            .unwrap_or(true)
                    })
                    .collect();
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema_version": 1,
                            "worktrees": entries_json(&entries),
                        }))?
                    );
                } else if entries.is_empty() {
                    println!(
                        "no managed worktrees under {}",
                        home.join("worktrees").display()
                    );
                } else {
                    for entry in &entries {
                        let state = match (&entry.error, entry.dirty, entry.merged) {
                            (Some(error), _, _) => format!("invalid: {error}"),
                            (None, true, _) => "dirty".to_string(),
                            (None, false, true) => "clean · merged".to_string(),
                            (None, false, false) if is_shipped(entry) => {
                                "clean · shipped".to_string()
                            }
                            (None, false, false) => "clean · unmerged".to_string(),
                        };
                        println!(
                            "{}  {}/{}  {}  {}",
                            state,
                            entry.run,
                            entry.task,
                            entry.branch.as_deref().unwrap_or("-"),
                            entry.path.display()
                        );
                    }
                }
            }
            Ok(())
        }
        cli::Commands::Rewind { thread_id, turn, native, json } => {
            if turn < 1 {
                anyhow::bail!("--turn must be at least 1");
            }
            let store = BatonStore::open(&paths::baton_db_path())?;
            let source = match thread_id.as_deref().or(cli.thread.as_deref()) {
                Some(thread_id) => resolve_thread(&store, thread_id)?,
                None => store
                    .list_threads_for_path(&cwd)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("no threads in this directory yet"))?,
            };
            let native_reverted = if native {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?;
                let recorder_store = Arc::new(Mutex::new(BatonStore::open(&paths::baton_db_path())?));
                let recorder = process::ProcessRecorder::new(recorder_store, source.cwd.clone());
                let reverted = runtime.block_on(provider_runtime::revert_codex_session(
                    &store,
                    &home,
                    &source,
                    turn,
                    recorder,
                ))?;
                if !reverted {
                    anyhow::bail!("codex did not confirm native rewind");
                }
                store.rewind_thread_keeping_provider_sessions(&source.id, turn)?;
                true
            } else {
                store.rewind_thread(&source.id, turn)?;
                false
            };
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "thread_id": source.id,
                        "from_turn": turn,
                        "native": native_reverted,
                        "provider_session_preserved": native_reverted,
                    }))?
                );
            } else {
                println!(
                    "rewound {} from turn {}{}",
                    &source.id[..8.min(source.id.len())],
                    turn,
                    if native_reverted { " (native, provider session preserved)" } else { "" }
                );
            }
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
        cli::Commands::Default { provider } => {
            let parsed = ProviderId::parse(&provider)
                .ok_or_else(|| anyhow::anyhow!("unknown provider {provider}, expected claude, codex, cursor, grok, opencode, or antigravity"))?;
            let mut file = load_config_file(&home).unwrap_or_default();
            file.default_provider = Some(parsed.as_str().to_string());
            save_config_file(&home, &file)?;
            println!("default provider set to {provider}");
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
        cli::Commands::Search { query, all, json } => {
            let query = query.join(" ");
            if query.trim().is_empty() {
                anyhow::bail!("usage: baton search <query> [--all] [--json]");
            }
            let store = BatonStore::open(&paths::baton_db_path())?;
            let cwd_filter = (!all).then(|| cwd.to_string_lossy().to_string());
            let results = store.search_messages(cwd_filter.as_deref(), &query, cli.limit)?;
            if json {
                let values: Vec<serde_json::Value> = results
                    .iter()
                    .map(|result| {
                        serde_json::json!({
                            "schema_version": status::SCHEMA_VERSION,
                            "thread_id": result.thread_id,
                            "title": result.title,
                            "message_id": result.message_id,
                            "turn": result.turn,
                            "role": result.role,
                            "provider": result.provider,
                            "model": result.model,
                            "text": result.text,
                            "created_at": result.created_at,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "query": query,
                        "all": all,
                        "count": values.len(),
                        "results": values,
                    }))?
                );
                return Ok(());
            }
            println!("query: {query}");
            println!("results: {}", results.len());
            for result in results {
                println!(
                    "{}  turn {}  {}  {}",
                    &result.thread_id[..8.min(result.thread_id.len())],
                    result.turn,
                    result.role,
                    search_snippet(&result.text, &query)
                );
            }
            Ok(())
        }
        cli::Commands::Turns { thread_id, json } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let thread = match thread_id.as_deref().or(cli.thread.as_deref()) {
                Some(thread_id) => resolve_thread(&store, thread_id)?,
                None => store
                    .list_threads_for_path(&cwd)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("no threads in this directory yet"))?,
            };
            let turns = store.list_turn_records(&thread.id)?;
            if json {
                let values: Vec<serde_json::Value> = turns
                    .iter()
                    .map(|turn| {
                        let open = !turn.phase.is_terminal();
                        serde_json::json!({
                            "schema_version": status::SCHEMA_VERSION,
                            "turn": turn.turn,
                            "phase": turn.phase.as_str(),
                            "open": open,
                            "stale_hint": if open { Some(status::STALE_HINT) } else { None },
                            "provider": turn.provider.as_str(),
                            "model": turn.model,
                            "started_at": turn.started_at,
                            "ended_at": turn.ended_at,
                            "provider_turn_id": turn.provider_turn_id,
                            "error": turn.error,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&values)?);
                return Ok(());
            }
            if turns.is_empty() {
                println!("no turn records");
            } else {
                println!("thread: {} {}", &thread.id[..8.min(thread.id.len())], thread.title);
                println!("turns: {}", turns.len());
                let mut has_open = false;
                for turn in turns {
                    if !turn.phase.is_terminal() {
                        has_open = true;
                    }
                    let ended = turn.ended_at.as_deref().unwrap_or("open");
                    let provider_turn = turn
                        .provider_turn_id
                        .as_deref()
                        .map(|id| format!(" provider-turn={id}"))
                        .unwrap_or_default();
                    let error = turn
                        .error
                        .as_deref()
                        .map(|error| {
                            let preview: String = error.chars().take(160).collect();
                            format!(" error={preview}")
                        })
                        .unwrap_or_default();
                    println!(
                        "turn {} {} {}/{} started={} ended={}{}{}",
                        turn.turn,
                        turn.phase.as_str(),
                        turn.provider,
                        turn.model.as_deref().unwrap_or("-"),
                        turn.started_at,
                        ended,
                        provider_turn,
                        error
                    );
                }
                if has_open {
                    println!("note: an open turn may be stale; use `baton status` for details");
                }
            }
            Ok(())
        }
        cli::Commands::Queue { thread_id, clear, json } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let thread = match thread_id.as_deref().or(cli.thread.as_deref()) {
                Some(thread_id) => resolve_thread(&store, thread_id)?,
                None => store
                    .list_threads_for_path(&cwd)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("no threads in this directory yet"))?,
            };
            if clear {
                store.clear_queued_messages(&thread.id)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema_version": status::SCHEMA_VERSION,
                            "thread_id": thread.id,
                            "cleared": true,
                        }))?
                    );
                } else {
                    println!("thread: {} {}", &thread.id[..8.min(thread.id.len())], thread.title);
                    println!("queue cleared");
                }
                return Ok(());
            }
            let queued = store.list_queued_messages(&thread.id)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "thread_id": thread.id,
                        "count": queued.len(),
                        "messages": queued,
                    }))?
                );
                return Ok(());
            }
            println!("thread: {} {}", &thread.id[..8.min(thread.id.len())], thread.title);
            if queued.is_empty() {
                println!("queue: empty");
            } else {
                println!("queue: {} waiting", queued.len());
                for (index, message) in queued.iter().enumerate() {
                    let preview: String = message
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(100)
                        .collect();
                    println!("{:>3}. {preview}", index + 1);
                }
            }
            Ok(())
        }
        cli::Commands::Doctor { json } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let report = doctor::build(&home, &cwd, &store)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&doctor::to_json(&report))?);
            } else {
                println!("{}", doctor::render(&report));
            }
            Ok(())
        }
        cli::Commands::Ps { json } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let rows = store.list_provider_processes()?;
            if json {
                let processes = rows
                    .iter()
                    .map(|row| {
                        let state = process::probe(row);
                        serde_json::json!({
                            "pid": row.pid,
                            "provider": row.provider.as_str(),
                            "port": row.port,
                            "cwd": row.cwd,
                            "owner_pid": row.owner_pid,
                            "owner_running": state.owner_running,
                            "process_alive": state.alive,
                            "command_matches": state.command_matches,
                            "process_start": state.process_start,
                            "start_matches": state.start_matches,
                            "command": state.command.unwrap_or_else(|| row.command.clone()),
                            "started_at": row.started_at,
                        })
                    })
                    .collect::<Vec<_>>();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "processes": processes,
                    }))?
                );
                return Ok(());
            }
            if rows.is_empty() {
                println!("no tracked provider processes");
                return Ok(());
            }
            println!("{:<7} {:<11} {:<6} {:<8} {}", "PID", "PROVIDER", "PORT", "OWNER", "COMMAND");
            for row in &rows {
                let state = process::probe(row);
                let status = if !state.alive {
                    "gone"
                } else if !state.owner_running {
                    "orphan"
                } else if state.command_matches {
                    "live"
                } else {
                    "mismatch"
                };
                let port = row.port.map(|port| port.to_string()).unwrap_or_else(|| "-".into());
                println!(
                    "{:<7} {:<11} {:<6} {:<8} {:<8} {}  {}",
                    row.pid,
                    row.provider,
                    port,
                    row.owner_pid,
                    status,
                    row.cwd,
                    state.command.as_deref().unwrap_or(&row.command)
                );
            }
            Ok(())
        }
        cli::Commands::Status { thread_id, json } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let thread = match thread_id.as_deref().or(cli.thread.as_deref()) {
                Some(thread_id) => resolve_thread(&store, thread_id)?,
                None => store
                    .list_threads_for_path(&cwd)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("no threads in this directory yet"))?,
            };
            let resolved = resolve_config(&home, Path::new(&thread.cwd));
            let model = thread
                .current_model
                .clone()
                .unwrap_or_else(|| resolved.for_provider(thread.current_provider).model.clone());
            let turn_records = store.list_turn_records(&thread.id)?;
            let turn_count = turn_records.len();
            let latest_turn = turn_records
                .into_iter()
                .last()
                .map(|turn| status::TurnStatus {
                    turn: turn.turn,
                    phase: turn.phase,
                    started_at: turn.started_at,
                    ended_at: turn.ended_at,
                    error: turn.error,
                    provider_turn_id: turn.provider_turn_id,
                });
            let usage_rows = store.list_usage(Some(&thread.id))?;
            let usage = if usage_rows.is_empty() {
                None
            } else {
                Some(format_usage_line(&sum_usage(&usage_rows)))
            };
            let report = status::ThreadStatus {
                thread_id: thread.id.clone(),
                title: thread.title.clone(),
                provider: thread.current_provider,
                model,
                permission_mode: thread.permission_mode,
                session_mode: thread.session_mode,
                goal: thread.goal.clone(),
                goal_paused: thread.goal_paused,
                latest_turn,
                turn_count,
                queued: store.list_queued_messages(&thread.id)?,
                usage,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&status::to_json(&report))?);
            } else {
                println!("{}", status::render(&report));
            }
            Ok(())
        }
        cli::Commands::Resume { fork, print, force } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let thread = match &cli.thread {
                Some(thread_id) => resolve_thread(&store, thread_id)?,
                None => store
                    .list_threads_for_path(&cwd)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("no threads in this directory yet"))?,
            };
            let latest_phase = store
                .list_turn_records(&thread.id)?
                .into_iter()
                .last()
                .map(|turn| turn.phase);
            resume::ensure_no_open_turn(latest_phase, force)?;
            let provider = match cli.provider.as_deref() {
                Some(value) => ProviderId::parse(value)
                    .ok_or_else(|| anyhow::anyhow!("unknown provider {value}"))?,
                None => thread.current_provider,
            };
            let session = match store.get_provider_session(&thread.id, provider)? {
                Some(session) => session,
                None => {
                    let available = store
                        .list_provider_sessions(&thread.id)?
                        .into_iter()
                        .map(|session| session.provider.to_string())
                        .collect::<Vec<_>>();
                    let suffix = if available.is_empty() {
                        "it has no native sessions yet".to_string()
                    } else {
                        format!("available sessions: {}", available.join(", "))
                    };
                    anyhow::bail!(
                        "thread {} has no {} session; {suffix}",
                        &thread.id[..8.min(thread.id.len())],
                        provider
                    );
                }
            };
            let command = resume::build(provider, &session.native_session_id, fork)?;
            if print {
                println!("{}", command.shell_line());
                return Ok(());
            }
            let resolved = resolve_config(&home, Path::new(&thread.cwd));
            let mut process = std::process::Command::new(&command.program);
            process.args(&command.args).current_dir(&thread.cwd);
            for (key, value) in config::provider_auth_env(resolved.for_provider(provider)) {
                process.env(key, value);
            }
            let status = process
                .status()
                .map_err(|error| anyhow::anyhow!("cannot run {}: {error}", command.program))?;
            if !status.success() {
                anyhow::bail!("{} exited with {status}", command.program);
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

fn reap_startup_processes() {
    let Ok(store) = BatonStore::open(&paths::baton_db_path()) else {
        return;
    };
    let Ok(report) = process::reap_orphans(&store) else {
        return;
    };
    if let Some(summary) = report.summary() {
        eprintln!("{summary}");
    }
    if std::env::var_os("BATON_DEBUG_PROCESS").is_some() {
        eprintln!(
            "baton process check: reaped={} gone={} live_owners={} mismatches={} unverifiable={} errors={}",
            report.reaped.len(),
            report.already_gone,
            report.live_owners,
            report.command_mismatches.len(),
            report.unverifiable,
            report.errors.len()
        );
        for (row, actual) in &report.command_mismatches {
            eprintln!(
                "baton process mismatch: pid={} provider={} expected={:?} actual={:?}",
                row.pid, row.provider, row.command, actual
            );
        }
    }
}

fn build_all_adapters(
    store: &SharedStore,
    home: &PathBuf,
    cwd: &PathBuf,
) -> Vec<SharedAdapter> {
    let process_recorder = process::ProcessRecorder::new(
        store.clone(),
        cwd.to_string_lossy().to_string(),
    );
    let claude_recorder = process_recorder.clone();
    let grok_recorder = process_recorder.clone();
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
            provider_runtime::spawn_tracked_provider(command, ProviderId::Grok, &grok_recorder)
        },
    );
    let antigravity_recorder = process_recorder.clone();
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
            provider_runtime::spawn_tracked_provider(command, ProviderId::Antigravity, &antigravity_recorder)
        },
    );
    let cursor_recorder = process_recorder.clone();
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
            provider_runtime::spawn_tracked_provider(command, ProviderId::Cursor, &cursor_recorder)
        },
    );
    let opencode_recorder = process_recorder.clone();
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
            // The server never sees stdin EOF (it is null), so it must die with baton.
            command.kill_on_drop(true);
            let mut child = command.spawn()?;
            let pid = child
                .id()
                .ok_or_else(|| anyhow::anyhow!("opencode did not expose a process id"))?;
            let command_line = process::command_line(pid)
                .unwrap_or_else(|| format!("opencode serve --hostname=127.0.0.1 --port {port}"));
            let process_start = process::process_start(pid);
            if let Err(error) = opencode_recorder.record(
                pid,
                ProviderId::OpenCode,
                Some(port),
                &command_line,
                process_start.as_deref(),
            ) {
                let _ = child.start_kill();
                return Err(error);
            }
            Ok(baton::adapters::opencode::OpenCodeChild { port, password, child })
        },
    );

    vec![
        Arc::new(tokio::sync::Mutex::new(Box::new(ClaudeAdapter::new(ClaudeAdapter::real_spawner_with_recorder(claude_recorder))) as Box<dyn BatonAdapter>)),
        provider_runtime::build_codex_adapter(store, cwd),
        Arc::new(tokio::sync::Mutex::new(Box::new(AcpAdapter::new(ProviderId::Cursor, cursor_spawner)) as Box<dyn BatonAdapter>)),
        Arc::new(tokio::sync::Mutex::new(Box::new(AcpAdapter::new(ProviderId::Grok, grok_spawner)) as Box<dyn BatonAdapter>)),
        Arc::new(tokio::sync::Mutex::new(Box::new(OpenCodeAdapter::with_recorder(opencode_spawner, process_recorder)) as Box<dyn BatonAdapter>)),
        Arc::new(tokio::sync::Mutex::new(Box::new(AcpAdapter::new(ProviderId::Antigravity, antigravity_spawner)) as Box<dyn BatonAdapter>)),
    ]
}

fn search_snippet(text: &str, query: &str) -> String {
    let query = query.to_lowercase();
    let line = text
        .lines()
        .find(|line| line.to_lowercase().contains(&query))
        .or_else(|| text.lines().next())
        .unwrap_or("");
    let mut snippet: String = line.trim().chars().take(160).collect();
    if line.trim().chars().count() > 160 {
        snippet.push('…');
    }
    snippet
}

fn default_provider_from_configs(home: &PathBuf, cwd: &PathBuf) -> ProviderId {
    if let Some(provider) = config::get_default_provider(home) {
        return provider;
    }
    let resolved = resolve_config(home, cwd);
    for provider in ProviderId::all() {
        let config = resolved.for_provider(*provider);
        if config.mode == crate::config::AuthMode::Api {
            return *provider;
        }
    }
    ProviderId::Claude
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

fn run_chat(cli: cli::Cli, home: &PathBuf, cwd: &PathBuf, permission_override: Option<PermissionMode>, initial_prompt: Option<String>) -> Result<()> {
    let store = Arc::new(Mutex::new(BatonStore::open(&paths::baton_db_path())?));
    let resolved = resolve_config(home, cwd);
    let requested_mode = match cli.mode.as_deref() {
        Some(value) => Some(SessionMode::parse(value).ok_or_else(|| {
            anyhow::anyhow!("unknown mode \"{value}\"; use single or orchestrate")
        })?),
        None => None,
    };

    let mut thread = match &cli.thread {
        Some(thread_id) => {
            let guard = store.lock().unwrap();
            resolve_thread(&guard, thread_id)?
        }
        None => {
            let provider = cli
                .provider
                .as_deref()
                .and_then(ProviderId::parse)
                .unwrap_or_else(|| default_provider_from_configs(&home, &cwd));
            let model = cli
                .model
                .clone()
                .or_else(|| Some(resolved.for_provider(provider).model.clone()));
            let mut thread = guard_draft_thread(&store, provider, model, cwd)?;
            thread.session_mode = config::get_default_session_mode(home);
            thread
        }
    };
    if let Some(mode) = requested_mode {
        thread.session_mode = mode;
        let guard = store.lock().unwrap();
        if guard.get_thread(&thread.id)?.is_some() {
            guard.set_thread_session_mode(&thread.id, mode)?;
        }
    }

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let adapters = build_all_adapters(&store, &home, &cwd);
    let (interrupt_handles, steer_handles, adapter_lifecycles) = tokio_runtime.block_on(async {
        let mut handles = Vec::new();
        let mut steer_handles = Vec::new();
        let mut lifecycles = HashMap::new();
        for adapter in &adapters {
            let adapter = adapter.lock().await;
            handles.push((adapter.id(), adapter.interrupt_handle()));
            if let Some(handle) = adapter.steer_handle() {
                steer_handles.push((adapter.id(), handle));
            }
            if let Some(lifecycle) = adapter.lifecycle() {
                lifecycles.insert(adapter.id(), lifecycle);
            }
        }
        (handles, steer_handles, lifecycles)
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
            steer_handles,
            engine_command_receiver,
            engine_event_sender,
            turn_event_sender,
        ));
    });

    let result = tui::run_tui(
        store,
        home.clone(),
        thread,
        adapter_lifecycles,
        engine_command_sender,
        engine_event_receiver,
        turn_event_receiver,
        initial_prompt,
        permission_override,
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
