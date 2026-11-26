use baton::{activity, adapters, checkpoints, claim, cli, config, doctor, exec_events, files, grep, inbox, mcp, orchestrate, paths, process, provider_runtime, resume, roles, runtime, sanitize, scanner, status, store, tasks, tui, types, usage, worktree};

use anyhow::Result;
use clap::{CommandFactory, Parser};
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::AsyncReadExt;

use adapters::{AcpAdapter, BatonAdapter, ClaudeAdapter};
use config::{describe_auth, load_config_file, resolve_config, save_config_file};
use runtime::{run_engine, EngineCommand, EngineEvent, SharedAdapter, SharedStore};
use store::{BatonStore, ImportedMessage, NewThread};
use types::{
    is_effort_clear, normalize_reasoning_effort, PermissionMode, ProviderId, SessionMode,
    TurnPhase,
};
use adapters::TurnEvent;
use usage::{cache_hit_rate, format_usage_line, group_usage, sum_usage, UsageGroup};

fn main() {
    let started = Instant::now();
    if let Err(error) = main_result(started) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn main_result(started: Instant) -> Result<()> {
    let cli = cli::Cli::parse();
    let cwd = cli
        .cwd
        .clone()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let home = paths::baton_home();
    config::validate_provider_overrides(&home)?;
    let requested_effort = match cli.effort.as_deref() {
        Some(value) if is_effort_clear(value) => None,
        Some(value) => Some(normalize_reasoning_effort(value).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid --effort \"{value}\"; use one token such as low, medium, high, xhigh, or max"
            )
        })?),
        None => None,
    };
    let command = cli.command.clone().unwrap_or(cli::Commands::Chat);
    let cli_prompt = cli.prompt.clone();
    if cli.host_id.is_some() && !matches!(&command, cli::Commands::Exec { .. }) {
        anyhow::bail!("--host-id is reserved for internal child processes");
    }

    let serving = matches!(&command, cli::Commands::Serve { .. });
    let hosted_child = cli.host_id.is_some();
    if !serving && !hosted_child {
        reap_startup_processes();
    }
    if !hosted_child && !matches!(&command, cli::Commands::Chat | cli::Commands::Serve { .. }) {
        reconcile_startup_orchestrations();
    }

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
        cli::Commands::Roles { json } => {
            let registry = roles::load(&home)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "config_path": registry.config_path.display().to_string(),
                        "roles": registry.roles.iter().map(|role| serde_json::json!({
                            "name": role.id.as_str(),
                            "provider": role.provider.as_str(),
                            "model": role.model,
                            "effort": role.effort,
                            "hook_events": role.hook_events,
                            "config_source": role.config_source.as_str(),
                            "prompt_source": role.prompt_source.as_str(),
                            "prompt_path": role.prompt_path.display().to_string(),
                        })).collect::<Vec<_>>(),
                        "hooks": registry.hooks,
                    }))?
                );
            } else {
                println!("role          provider/model                       config   prompt");
                for role in &registry.roles {
                    let model = format!("{}/{}", role.provider, role.model);
                    let model = sanitize::sanitize_terminal_text(&model);
                    println!(
                        "{:<13} {:<38} {:<8} {}",
                        role.id.as_str(),
                        model,
                        role.config_source.as_str(),
                        role.prompt_source.as_str()
                    );
                }
                let config_path = registry.config_path.display().to_string();
                let config_path = sanitize::sanitize_terminal_text(&config_path);
                println!("config: {config_path}");
                let hook_events = registry.hooks.keys().cloned().collect::<Vec<_>>();
                println!(
                    "hooks: {}",
                    if hook_events.is_empty() {
                        "none".to_string()
                    } else {
                        hook_events.join(", ")
                    }
                );
            }
            return Ok(());
        }
        cli::Commands::Exec { prompt, json, stream_json, no_review } => {
            if cli.mode.is_some() {
                anyhow::bail!(
                    "--mode applies to the interactive TUI; use `baton orchestrate` for a non-interactive run"
                );
            }
            let prompt = prompt.join(" ");
            if prompt.is_empty() {
                eprintln!("usage: baton exec <prompt>");
                std::process::exit(2);
            }
            let store = Arc::new(Mutex::new(open_store(cli.host_id.as_deref())?));
            let resolved = resolve_config(&home, &cwd);
            let requested_provider = cli.provider.as_deref().and_then(ProviderId::parse);
            if let Some(provider) = requested_provider {
                if let Some(hint) = doctor::missing_provider_hint(provider) {
                    anyhow::bail!(hint);
                }
            }
            let is_new = cli.thread.is_none();
            let mut thread = if let Some(thread_id) = cli.thread.as_deref() {
                let guard = store.lock().unwrap();
                resolve_thread(&guard, thread_id)?
            } else {
                let provider = requested_provider
                    .unwrap_or_else(|| default_provider_from_configs(&home, &cwd));
                let model = cli
                    .model
                    .clone()
                    .unwrap_or_else(|| resolved.for_provider(provider).model.clone());
                let mut thread = store.lock().unwrap().draft_thread(crate::store::NewThread {
                    title: baton::handoff::derive_title(&prompt),
                    cwd: cwd.to_string_lossy().to_string(),
                    current_provider: provider,
                    current_model: Some(model),
                });
                thread.reasoning_effort = requested_effort.clone();
                thread
            };
            if !is_new {
                let guard = store.lock().unwrap();
                if let Some(provider) = requested_provider {
                    if provider != thread.current_provider {
                        let model = cli
                            .model
                            .clone()
                            .or_else(|| thread.current_model.clone());
                        guard.set_thread_provider(&thread.id, provider, model.clone())?;
                        thread.current_provider = provider;
                        thread.current_model = model;
                    } else if let Some(model) = cli.model.clone() {
                        guard.set_thread_model(&thread.id, &model)?;
                        thread.current_model = Some(model);
                    }
                } else if let Some(model) = cli.model.clone() {
                    guard.set_thread_model(&thread.id, &model)?;
                    thread.current_model = Some(model);
                }
                if cli.effort.is_some() {
                    guard.set_thread_effort(&thread.id, requested_effort.as_deref())?;
                    thread.reasoning_effort = requested_effort.clone();
                }
                if permission_override.is_some() {
                    guard.set_thread_permission_mode(&thread.id, permission_mode)?;
                }
            }
            let provider = thread.current_provider;
            if let Some(hint) = doctor::missing_provider_hint(provider) {
                anyhow::bail!(hint);
            }
            if requested_effort.is_some() && !provider.supports_effort() {
                anyhow::bail!("{provider} does not support --effort; use codex or claude");
            }
            let permission_mode = permission_override
                .unwrap_or_else(|| resolved.for_provider(provider).permission_mode);
            if is_new {
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
                if let Some(effort) = requested_effort.as_deref() {
                    store
                        .lock()
                        .unwrap()
                        .set_thread_effort(&thread.id, Some(effort))?;
                }
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
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            });

            let mut stream = exec_events::TurnStreamState::default();
            let mut result_phase: Option<TurnPhase> = None;
            let mut result_error: Option<String> = None;
            let mut result_ok: Option<bool> = None;
            let mut timed_out = false;
            let machine = json || stream_json;
            let show_progress = std::io::stderr().is_terminal() && !machine;
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
                    exec_events::apply_turn_event(&mut stream, &event);
                    if stream_json {
                        println!(
                            "{}",
                            serde_json::to_string(&exec_events::turn_event_json(
                                &event_thread,
                                &event
                            ))?
                        );
                        let _ = std::io::stdout().flush();
                    }
                    match &event {
                        TurnEvent::TextDelta { text } if !machine => {
                            if !text.is_empty() {
                                print!("{}", sanitize::sanitize_terminal_text(text));
                                let _ = std::io::stdout().flush();
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
                        if !ok && !stream_json {
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
                    Ok(EngineEvent::Turn(event)) => {
                        exec_events::apply_turn_event(&mut stream, &event);
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
            let _ = engine_command_sender.send(EngineCommand::Quit);
            let _ = engine_task.join();

            // Drain events that raced the terminal engine event, so the final
            // text is present and stream-json rows precede the result row.
            while let Ok((event_thread, event)) = turn_event_receiver.try_recv() {
                if event_thread != thread.id {
                    continue;
                }
                exec_events::apply_turn_event(&mut stream, &event);
                if stream_json {
                    println!(
                        "{}",
                        serde_json::to_string(&exec_events::turn_event_json(
                            &event_thread,
                            &event
                        ))?
                    );
                }
            }
            if stream_json {
                let _ = std::io::stdout().flush();
            }

            // A `-t` turn against an orchestration worker thread answers its
            // question (or records a failed follow-up) through the same path
            // the TUI uses, so `baton exec` can unblock a run.
            let worker_followup = match result_phase {
                Some(phase) => {
                    let guard = store.lock().unwrap();
                    match orchestrate::record_worker_followup(
                        &guard,
                        &thread.id,
                        result_ok.unwrap_or(false),
                        result_error.as_deref(),
                        phase,
                    ) {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            if !machine {
                                eprintln!("worker follow-up update failed: {error}");
                            }
                            None
                        }
                    }
                }
                None => None,
            };

            let followup_review = if no_review {
                None
            } else {
                match &worker_followup {
                    Some(outcome)
                        if outcome.status == "completed"
                            && outcome.commit.is_some()
                            && outcome.error.is_none()
                            && outcome.question.is_none() =>
                    {
                        match open_store(cli.host_id.as_deref()).and_then(|review_store| {
                            orchestrate::auto_review_followup(
                                &review_store,
                                &home,
                                &cwd,
                                outcome,
                                permission_override,
                            )
                        }) {
                            Ok(review) => review,
                            Err(error) => {
                                if !machine {
                                    eprintln!("auto-review failed: {error}");
                                }
                                None
                            }
                        }
                    }
                    _ => None,
                }
            };

            // Providers that do not emit deltas only deliver final text here.
            // If deltas were streamed, do not print the same answer twice.
            if stream.streamed_text.is_empty() && !stream.final_text.is_empty() {
                if !machine {
                    println!("{}", sanitize::sanitize_terminal_text(&stream.final_text));
                }
                stream.saw_output = true;
            } else if !stream.streamed_text.is_empty() && !machine {
                println!();
            }
            if timed_out {
                let mut value = exec_events::result_json(&exec_events::ExecResult {
                    thread_id: &thread.id,
                    effort: requested_effort.as_deref(),
                    ok: false,
                    phase: "unverified",
                    error: Some("exec timed out after 300s"),
                    text: "",
                    worker_followup: None,
                    review: None,
                    plan: stream.plan.as_ref(),
                });
                if stream_json {
                    value["type"] = serde_json::json!("result");
                    println!("{}", serde_json::to_string(&value)?);
                } else if json {
                    println!("{}", serde_json::to_string_pretty(&value)?);
                } else {
                    eprintln!("exec timed out after 300s");
                }
                std::process::exit(1);
            }
            let result_ok = result_ok.unwrap_or(false);
            let machine_ok = if stream_json {
                result_ok && stream.saw_output
            } else {
                result_ok
            };
            let machine_error = if stream_json && result_ok && !stream.saw_output {
                Some("no output from provider".to_string())
            } else {
                result_error.clone()
            };
            if machine {
                let text = if stream.final_text.is_empty() {
                    stream.streamed_text.as_str()
                } else {
                    stream.final_text.as_str()
                };
                let mut value = exec_events::result_json(&exec_events::ExecResult {
                    thread_id: &thread.id,
                    effort: requested_effort.as_deref(),
                    ok: machine_ok,
                    phase: result_phase.map(|phase| phase.as_str()).unwrap_or("unverified"),
                    error: machine_error.as_deref(),
                    text,
                    worker_followup: worker_followup.as_ref(),
                    review: followup_review.as_ref(),
                    plan: stream.plan.as_ref(),
                });
                if stream_json {
                    value["type"] = serde_json::json!("result");
                    println!("{}", serde_json::to_string(&value)?);
                } else {
                    println!("{}", serde_json::to_string_pretty(&value)?);
                }
            } else if result_ok && !stream.saw_output {
                eprintln!("no output from provider");
            }
            if !machine {
                if let Some(review) = &followup_review {
                    eprintln!(
                        "auto-review: {} · {}",
                        review.status,
                        review.text.lines().next().unwrap_or("")
                    );
                }
                if let Some(outcome) = &worker_followup {
                    let run = &outcome.run_id[..8.min(outcome.run_id.len())];
                    let label = if outcome.question.is_some() {
                        "worker question recorded"
                    } else {
                        "worker follow-up recorded"
                    };
                    match (&outcome.error, &outcome.commit) {
                        // A follow-up that fails after a committed turn leaves
                        // the task untouched, so there is nothing to report.
                        (Some(_), Some(_)) => {}
                        (Some(error), None) => eprintln!("worker failure recorded in run {run} · {error}"),
                        (None, Some(commit)) => eprintln!(
                            "{label} in run {run} · commit {}",
                            &commit[..8.min(commit.len())]
                        ),
                        (None, None) => eprintln!("{label} in run {run} · no file changes"),
                    }
                }
            }
            if !result_ok || !stream.saw_output {
                std::process::exit(1);
            }
            return Ok(());
        }
        cli::Commands::Task { number, json } => {
            if cli.mode.is_some() {
                anyhow::bail!("--mode applies to the interactive TUI; omit it for task");
            }
            if cli.thread.is_some() {
                anyhow::bail!("task creates a new worktree; omit --thread");
            }
            let plan = tasks::plan_task(&cwd, number)?;
            let worktree = tasks::create_worktree(&home, &plan)?;
            if json {
                let value = serde_json::json!({
                    "number": plan.issue.number,
                    "title": plan.issue.title,
                    "url": plan.issue.url,
                    "repo": plan.repo,
                    "path": worktree.path.to_string_lossy(),
                    "branch": worktree.branch,
                    "base": worktree.base_ref,
                });
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                let base = worktree.base_ref.chars().take(8).collect::<String>();
                println!(
                    "task #{} at {} on branch {} from {}",
                    plan.issue.number,
                    worktree.path.display(),
                    worktree.branch,
                    base
                );
                println!("{}", plan.issue.url);
            }
            return Ok(());
        }
        cli::Commands::Spawn { prompt, json } => {
            if cli.mode.is_some() {
                anyhow::bail!("--mode applies to the interactive TUI; omit it for spawn");
            }
            if cli.thread.is_some() {
                anyhow::bail!("spawn creates a new worker thread; omit --thread");
            }
            let prompt = prompt.join(" ");
            if prompt.trim().is_empty() {
                anyhow::bail!("usage: baton spawn [options] <task>");
            }
            let provider = cli
                .provider
                .as_deref()
                .and_then(ProviderId::parse)
                .unwrap_or_else(|| default_provider_from_configs(&home, &cwd));
            if let Some(hint) = doctor::missing_provider_hint(provider) {
                anyhow::bail!(hint);
            }
            worktree::repo_root(&cwd).map_err(|_| {
                anyhow::anyhow!("spawn needs a git repository; run it inside one or use `baton exec`")
            })?;
            let resolved = resolve_config(&home, &cwd);
            let model = cli
                .model
                .clone()
                .unwrap_or_else(|| resolved.for_provider(provider).model.clone());
            if requested_effort.is_some() && !provider.supports_effort() {
                anyhow::bail!("{provider} does not support --effort; use codex or claude");
            }
            let permission_mode = permission_override
                .unwrap_or_else(|| resolved.for_provider(provider).permission_mode);
            let plan = orchestrate::single_task_plan(
                &prompt,
                provider,
                model.clone(),
                requested_effort.clone(),
            );
            std::fs::create_dir_all(&home)?;
            let plan_path = home.join(format!("spawn-{}.json", uuid::Uuid::new_v4()));
            std::fs::write(&plan_path, serde_json::to_string(&plan)?)?;
            let store = BatonStore::open(&paths::baton_db_path())?;
            let result = orchestrate::run(
                &prompt,
                &cwd,
                &home,
                provider,
                &model,
                1,
                Some(&plan_path),
                &store,
                permission_mode,
            );
            let _ = std::fs::remove_file(&plan_path);
            let report = result?;
            let Some(task) = report.tasks.into_iter().next() else {
                anyhow::bail!("spawn produced no task");
            };
            let question = task.text.as_deref().and_then(orchestrate::worker_question);
            let ok = matches!(task.status.as_str(), "completed" | "needs_input");
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "run_id": report.run_id,
                        "task_id": task.id,
                        "thread_id": task.thread_id,
                        "status": task.status,
                        "provider": task.provider.as_str(),
                        "model": task.model,
                        "worktree": task.worktree.as_ref().map(|worktree| worktree.path.display().to_string()),
                        "branch": task.worktree.as_ref().map(|worktree| worktree.branch.clone()),
                        "commit": task.commit,
                        "text": task.text,
                        "question": question,
                        "error": task.error,
                    }))?
                );
            } else {
                println!(
                    "spawned {}/{} · run {} · {}",
                    task.provider,
                    task.model,
                    &report.run_id[..8.min(report.run_id.len())],
                    task.status
                );
                if let Some(worktree) = &task.worktree {
                    println!("worktree: {}", worktree.path.display());
                }
                if let Some(commit) = &task.commit {
                    println!("commit: {}", &commit[..8.min(commit.len())]);
                }
                if let Some(text) = &task.text {
                    if !text.is_empty() {
                        println!("{text}");
                    }
                }
                if let Some(question) = &question {
                    println!("question: {question}");
                }
                if let Some(error) = &task.error {
                    println!("error: {error}");
                }
            }
            if !ok {
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
        cli::Commands::Bench {
            target,
            json,
            repeat,
        } => return run_bench(&home, started, &target, json, repeat),
        cli::Commands::Chat => run_chat(
            cli,
            &home,
            &cwd,
            permission_override,
            cli_prompt,
            requested_effort,
        ),
        cli::Commands::Serve { stdio } => run_serve(&home, stdio),
        cli::Commands::Mcp => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            mcp::run_stdio(mcp::McpServer::new(store, home.clone(), cwd.clone()))
        }
        cli::Commands::Threads { json, all } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let resolved = resolve_config(&home, &cwd);
            let threads = if all {
                store.list_threads_for_path_display_all(&cwd)?
            } else {
                store.list_threads_for_path_display(&cwd)?
            };
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
                        "reasoning_effort": thread.reasoning_effort,
                        "session_mode": thread.session_mode.as_str(),
                        "permission_mode": thread.permission_mode.as_str(),
                        "pinned": thread.pinned,
                        "pin_order_key": thread.pin_order_key,
                        "snoozed_until": thread.snoozed_until,
                        "archived_at": thread.archived_at,
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
                    "{}{}  {}/{}{}  session:{}  {}  turns:{}  queue:{}  {}{}",
                    if thread.pinned { "* " } else { "" },
                    &thread.id[..8],
                    thread.current_provider,
                    model,
                    thread
                        .reasoning_effort
                        .as_deref()
                        .map(|effort| format!("@{effort}"))
                        .unwrap_or_default(),
                    thread.session_mode,
                    phase,
                    turn_count,
                    queued,
                    thread.title,
                    if thread.archived_at.is_some() {
                        "  [archived]"
                    } else {
                        ""
                    }
                );
            }
            if has_open {
                println!("note: one or more turns are open; use `baton status <id>` to inspect them");
            }
            Ok(())
        }
        cli::Commands::Inbox { json } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let items = inbox::items(&store)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(());
            }
            if items.is_empty() {
                println!("inbox is empty");
                return Ok(());
            }
            for item in items {
                let id: String = item.thread_id.chars().take(8).collect();
                println!(
                    "{id}  {}  [{}]  {}  {}",
                    item.title,
                    item.status,
                    item.reasons.join(" · "),
                    item.updated_at
                );
            }
            Ok(())
        }
        cli::Commands::Completions { shell } => {
            let mut command = cli::Cli::command();
            let mut output = Vec::new();
            clap_complete::generate(shell, &mut command, "baton", &mut output);
            if let Err(error) = std::io::stdout().write_all(&output) {
                if error.kind() == std::io::ErrorKind::BrokenPipe {
                    return Ok(());
                }
                return Err(error.into());
            }
            return Ok(());
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
            if requested_effort.is_some() && !provider.supports_effort() {
                anyhow::bail!("{provider} does not support --effort; use codex or claude");
            }
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
            let initial_permission = permission_override
                .unwrap_or_else(|| resolved.for_provider(provider).permission_mode);
            store.set_thread_permission_mode(&thread.id, initial_permission)?;
            thread.permission_mode = initial_permission;
            if let Some(effort) = requested_effort.as_deref() {
                store.set_thread_effort(&thread.id, Some(effort))?;
                thread.reasoning_effort = Some(effort.to_string());
            }
            println!(
                "created thread {} ({}/{}{}) session {} mode {}",
                &thread.id[..8],
                provider,
                model.unwrap_or_default(),
                thread
                    .reasoning_effort
                    .as_deref()
                    .map(|effort| format!("@{effort}"))
                    .unwrap_or_default(),
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
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?;
                let recorder_store = Arc::new(Mutex::new(BatonStore::open(&paths::baton_db_path())?));
                let recorder = process::ProcessRecorder::new(recorder_store, source.cwd.clone());
                native_session = runtime.block_on(provider_runtime::fork_native_session(
                    &store,
                    &home,
                    &source,
                    turn,
                    recorder,
                ))?;
                if native_session.is_none() {
                    anyhow::bail!(
                        "{} did not return a native fork id",
                        source.current_provider
                    );
                }
            }
            let forked = store.fork_thread_through_turn(&source.id, &title, turn)?;
            if let Some((native_provider, native_id, model)) = native_session.as_ref() {
                store.upsert_provider_session(
                    &forked.id,
                    *native_provider,
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
                        "native_session_provider": native_session
                            .as_ref()
                            .map(|(provider, _, _)| provider.as_str()),
                        "native_session_id": native_session.as_ref().map(|(_, id, _)| id),
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
            plan_only,
            list,
            all,
            run,
            archive,
            review,
            integrate,
            open_prs,
            tasks,
            progress,
            stream_json,
            json,
        } => {
            if stream_json
                && (list
                    || run.is_some()
                    || archive.is_some()
                    || review.is_some()
                    || integrate.is_some()
                    || open_prs.is_some()
                    || plan_only)
            {
                anyhow::bail!(
                    "--stream-json applies to starting a run; use --json for inspection commands"
                );
            }
            if cli.provider.is_some() || cli.model.is_some() || cli.effort.is_some() {
                anyhow::bail!(
                    "orchestrate uses role settings; set provider, model, and effort in roles.json instead of --provider, --model, or --effort"
                );
            }
            let store = BatonStore::open(&paths::baton_db_path())?;
            if list {
                let runs = orchestrate::run_rows(&store, 20)?
                    .into_iter()
                    .filter(|run| all || run.archived_at.is_none())
                    .collect::<Vec<_>>();
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
                                "worker_baseline": if run.dirty_source { "source snapshot" } else { "HEAD" },
                                "archived_at": run.archived_at,
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
                            "{}  {:<9}  {}/{}  {}{}",
                            &run.id[..8.min(run.id.len())],
                            run.status,
                            run.coordinator_provider,
                            run.coordinator_model,
                            run.summary.unwrap_or_else(|| run.goal.clone()),
                            if run.archived_at.is_some() { " (archived)" } else { "" }
                        );
                    }
                }
                return Ok(());
            }
            if let Some(run_id) = run {
                let run_id = orchestrate::resolve_run_id(&store, &run_id)?;
                let report = orchestrate::report_from_rows(&store, &cwd, &run_id)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&orchestrate::to_json(&report))?
                    );
                } else {
                    println!(
                        "run {}  {}",
                        &run_id[..8.min(run_id.len())],
                        report.status
                    );
                    println!("goal: {}", report.goal);
                    if let Some(summary) = report.summary.as_deref() {
                        println!("summary: {summary}");
                    }
                    if report.tasks.is_empty() {
                        println!("no tasks for run {run_id}");
                    } else {
                        for task in &report.tasks {
                            println!(
                                "{}  {:<9}  {}/{}  {}",
                                task.id,
                                task.status,
                                task.provider,
                                task.model,
                                task.title
                            );
                            if let Some(worktree) = &task.worktree {
                                println!("  worktree: {}", worktree.path.display());
                            }
                            if let Some(commit) = &task.commit {
                                println!("  commit: {}", &commit[..8.min(commit.len())]);
                            }
                            if let Some(merged_at) = &task.merged_at {
                                println!("  merged: {merged_at}");
                            }
                            if let Some(pr_url) = &task.pr_url {
                                println!("  pr: {pr_url}");
                            }
                            if let Some(error) = &task.error {
                                println!("  error: {error}");
                            }
                            if let Some(question) =
                                task.text.as_deref().and_then(orchestrate::worker_question)
                            {
                                println!("  question: {question}");
                            }
                        }
                    }
                }
                return Ok(());
            }
            if let Some(run_id) = archive {
                let run_id = orchestrate::archive(&store, &run_id)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema_version": orchestrate::SCHEMA_VERSION,
                            "run_id": run_id,
                            "archived": true,
                            "worktrees_remain": true,
                        }))?
                    );
                } else {
                    println!(
                        "archived {}; worktrees remain, use `baton worktrees --clean --run {}`",
                        &run_id[..8.min(run_id.len())],
                        &run_id[..8.min(run_id.len())]
                    );
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
            if let Some(run_id) = review {
                let report = orchestrate::review_run(
                    &store,
                    &home,
                    &cwd,
                    &run_id,
                    &tasks,
                    permission_override,
                )?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&orchestrate::to_json(&report))?
                    );
                } else {
                    println!("{}", orchestrate::render(&report));
                }
                if report
                    .tasks
                    .iter()
                    .any(|task| task.review_status.as_deref() == Some(orchestrate::REVIEW_FAILED))
                {
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
                    "usage: baton orchestrate [--plan-only] <goal> | --list [--all] | --run <id> | --archive <run-id> | --review <run-id> | --integrate <run-id> | --open-prs <run-id>"
                );
            }
            let resolved = resolve_config(&home, &cwd);
            let plan_path = plan_file.map(PathBuf::from);
            let coordinator_role = orchestrate::coordinator_role_for(&goal, plan_path.as_deref());
            let planner = roles::load(&home)?.get(coordinator_role).clone();
            let provider = planner.provider;
            let model = planner.model.clone();
            if plan_only {
                let plan = orchestrate::plan(
                    &goal,
                    &cwd,
                    &home,
                    provider,
                    &model,
                    max_tasks,
                    plan_path.as_deref(),
                    permission_mode,
                    None,
                )?;
                let dirty_source = worktree::repo_root(&cwd)
                    .ok()
                    .and_then(|root| worktree::is_dirty(&root).ok())
                    .unwrap_or(false);
                if json {
                    let tasks = plan
                        .tasks
                        .iter()
                        .map(|task| {
                            let task_provider = ProviderId::parse(task.provider.trim())
                                .unwrap_or(ProviderId::Codex);
                            let effective_model = task.model.clone().unwrap_or_else(|| {
                                resolved.for_provider(task_provider).model.clone()
                            });
                            serde_json::json!({
                                "title": task.title,
                                "provider": task_provider.as_str(),
                                "model": effective_model,
                                "model_override": task.model.is_some(),
                                "effort": task.effort,
                                "effort_override": task.effort.is_some(),
                                "prompt": task.prompt,
                            })
                        })
                        .collect::<Vec<_>>();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema_version": orchestrate::SCHEMA_VERSION,
                            "plan_only": true,
                            "goal": goal,
                            "coordinator": {
                                "provider": provider.as_str(),
                                "model": model,
                            },
                            "dirty_source": dirty_source,
                            "worker_baseline": if dirty_source { "source snapshot" } else { "HEAD" },
                            "summary": plan.summary,
                            "tasks": tasks,
                        }))?
                    );
                } else {
                    println!("plan · {} worker(s)", plan.tasks.len());
                    println!("goal: {goal}");
                    println!("coordinator: {provider}/{model}");
                    println!(
                        "source: {}{}",
                        cwd.display(),
                        if dirty_source {
                            " (dirty; workers will receive a snapshot)"
                        } else {
                            ""
                        }
                    );
                    if let Some(summary) = &plan.summary {
                        println!("summary: {summary}");
                    }
                    for (index, task) in plan.tasks.iter().enumerate() {
                        let task_provider = ProviderId::parse(task.provider.trim())
                            .unwrap_or(ProviderId::Codex);
                        let effective_model = task.model.clone().unwrap_or_else(|| {
                            resolved.for_provider(task_provider).model.clone()
                        });
                        println!(
                            "  [{}] {} · {}/{}{}",
                            index + 1,
                            task.title,
                            task_provider,
                            effective_model,
                            task.effort
                                .as_deref()
                                .map(|effort| format!("@{effort}"))
                                .unwrap_or_default()
                        );
                        if let Some(first) = task.prompt.lines().next() {
                            println!("      {first}");
                        }
                    }
                }
                return Ok(());
            }
            let report = if stream_json {
                let sink = |event: orchestrate::OrchestrationProgress| {
                    if let Ok(line) =
                        serde_json::to_string(&orchestrate::progress_json(&event))
                    {
                        println!("{line}");
                        let _ = std::io::stdout().flush();
                    }
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
            } else if progress {
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
            if stream_json {
                let mut value = orchestrate::to_json(&report);
                value["type"] = serde_json::json!("result");
                println!("{}", serde_json::to_string(&value)?);
            } else if json {
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
            let mut provider_retry_skipped = false;
            let native_reverted = if native {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?;
                let recorder_store =
                    Arc::new(Mutex::new(BatonStore::open(&paths::baton_db_path())?));
                let recorder = process::ProcessRecorder::new(recorder_store, source.cwd.clone());
                let outcome = runtime.block_on(provider_runtime::rewind_to_turn(
                    &store,
                    &home,
                    &source,
                    turn,
                    recorder,
                ))?;
                provider_retry_skipped = outcome.provider_retry_skipped;
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
                        "provider_retry_skipped": provider_retry_skipped,
                    }))?
                );
            } else {
                println!(
                    "rewound {} from turn {}{}{}",
                    &source.id[..8.min(source.id.len())],
                    turn,
                    if native_reverted {
                        " (native, provider session preserved)"
                    } else {
                        ""
                    },
                    if provider_retry_skipped {
                        " (completed pending store rewind)"
                    } else {
                        ""
                    }
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
        cli::Commands::Restore {
            thread_id,
            turn,
            force,
        } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let thread = resolve_thread(&store, &thread_id)?;
            let checkpoint = store
                .get_checkpoint(&thread.id, turn)?
                .ok_or_else(|| anyhow::anyhow!("no checkpoint for turn {turn}"))?;
            let consent = if force {
                checkpoints::RestoreConsent::Force
            } else {
                checkpoints::RestoreConsent::Safe
            };
            checkpoints::restore_thread_checkpoint(&store, &thread, &checkpoint.git_ref, consent)?;
            println!("restored {} to {}", thread.cwd, checkpoint.git_ref);
            Ok(())
        }
        cli::Commands::Usage {
            thread_id,
            by,
            json,
        } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            if let Some(group) = by {
                let group = match group {
                    cli::UsageGroupBy::Day => UsageGroup::Day,
                    cli::UsageGroupBy::Model => UsageGroup::Model,
                    cli::UsageGroupBy::Provider => UsageGroup::Provider,
                };
                let filter = match &thread_id {
                    Some(thread_id) => Some(resolve_thread(&store, thread_id)?.id),
                    None => None,
                };
                let rows = store.list_usage(filter.as_deref())?;
                let grouped = group_usage(&rows, group);
                if json {
                    let groups = grouped
                        .iter()
                        .map(|(key, totals)| {
                            serde_json::json!({
                                "key": key,
                                "turns": totals.turns,
                                "input_tokens": totals.input_tokens,
                                "output_tokens": totals.output_tokens,
                                "cache_read_tokens": totals.cache_read_tokens,
                                "cache_creation_tokens": totals.cache_creation_tokens,
                                "cost_usd": totals.cost_usd,
                            })
                        })
                        .collect::<Vec<_>>();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema_version": 1,
                            "by": group.as_str(),
                            "groups": groups,
                        }))?
                    );
                } else if grouped.is_empty() {
                    println!("no usage yet");
                } else {
                    for (key, totals) in grouped {
                        println!(
                            "{:<24} {:>3} turns  {}",
                            key,
                            totals.turns,
                            format_usage_line(&totals)
                        );
                    }
                }
                return Ok(());
            }
            let resolved = resolve_config(&home, &cwd);
            let threads = match thread_id {
                Some(thread_id) => vec![resolve_thread(&store, &thread_id)?],
                None => store.list_threads(None)?,
            };
            if threads.is_empty() {
                anyhow::bail!("no threads found");
            }
            let mut entries = Vec::new();
            for thread in threads {
                let rows = store.list_usage(Some(&thread.id))?;
                if rows.is_empty() {
                    continue;
                }
                let totals = sum_usage(&rows);
                let model = thread
                    .current_model
                    .clone()
                    .unwrap_or_else(|| resolved.for_provider(thread.current_provider).model.clone());
                if json {
                    entries.push(serde_json::json!({
                        "thread_id": thread.id,
                        "title": thread.title,
                        "provider": thread.current_provider.as_str(),
                        "model": model,
                        "turns": totals.turns,
                        "input_tokens": totals.input_tokens,
                        "output_tokens": totals.output_tokens,
                        "cache_read_tokens": totals.cache_read_tokens,
                        "cache_creation_tokens": totals.cache_creation_tokens,
                        "cost_usd": totals.cost_usd,
                    }));
                    continue;
                }
                let rate = cache_hit_rate(&totals.total_usage());
                let rate_label = match rate {
                    None => "n/a".to_string(),
                    Some(rate) => format!("{:.0}%", rate * 100.0),
                };
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
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": 1,
                        "threads": entries,
                    }))?
                );
            }
            Ok(())
        }
        cli::Commands::Memories { action, json } => {
            if cli.thread.is_some() {
                anyhow::bail!("memories uses -C/--cwd for a project; -t/--thread is only for threads");
            }
            let store = BatonStore::open(&paths::baton_db_path())?;
            let project = baton::memory::project_key(&cwd.to_string_lossy());
            match action {
                None => {
                    let rows = store.list_memories(&project)?;
                    if json {
                        let memories: Vec<_> = rows
                            .iter()
                            .map(|memory| serde_json::json!({
                                "id": memory.id,
                                "scope": memory.scope,
                                "cwd": memory.cwd,
                                "text": memory.text,
                                "created_at": memory.created_at,
                                "updated_at": memory.updated_at,
                            }))
                            .collect();
                        println!(
                            "{}",
                            serde_json::json!({
                                "schema_version": 1,
                                "count": rows.len(),
                                "memories": memories,
                            })
                        );
                    } else if rows.is_empty() {
                        println!("memories: none");
                    } else {
                        println!("memories: {}", rows.len());
                        for memory in rows {
                            println!("  {}. {} · {}", memory.id, memory.scope, memory.text);
                        }
                    }
                }
                Some(cli::MemoriesCommand::Add { global, text }) => {
                    let text = text.join(" ");
                    if text.trim().is_empty() {
                        anyhow::bail!("usage: baton memories add [--global] <text>");
                    }
                    let scope = if global { "global" } else { "project" };
                    let id = store.add_memory(scope, &project, &text)?;
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "schema_version": 1,
                                "action": "add",
                                "id": id,
                                "scope": scope,
                            })
                        );
                    } else {
                        println!("memory {id} saved ({scope})");
                    }
                }
                Some(cli::MemoriesCommand::Remove { id }) => {
                    if !store.remove_memory(&project, id)? {
                        anyhow::bail!("memory {id} not found for this project");
                    }
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "schema_version": 1,
                                "action": "remove",
                                "id": id,
                                "removed": true,
                            })
                        );
                    } else {
                        println!("memory {id} removed");
                    }
                }
                Some(cli::MemoriesCommand::Clear { global, yes }) => {
                    if global && !yes {
                        anyhow::bail!("refusing to clear every global memory without --yes");
                    }
                    let cleared = store.clear_memories(&project, global)?;
                    let scope = if global { "global" } else { "project" };
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "schema_version": 1,
                                "action": "clear",
                                "scope": scope,
                                "cleared": cleared,
                            })
                        );
                    } else {
                        println!("cleared {cleared} {scope} memory(s)");
                    }
                }
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
            let Some(command) = cli::login_command(provider) else {
                println!("Antigravity has no CLI login.");
                println!("1. Install Antigravity and enable it in its settings.");
                println!("2. Sign in with Google when prompted.");
                println!("For API-rate billing set GEMINI_API_KEY and run: baton auth antigravity api");
                return Ok(());
            };
            println!("running: {}", command.join(" "));
            let status = std::process::Command::new(command[0])
                .args(&command[1..])
                .status()?;
            if !status.success() {
                anyhow::bail!("{} failed with {status}", command.join(" "));
            }
            if provider == ProviderId::Cursor {
                println!("cursor login complete");
            }
            Ok(())
        }
        cli::Commands::Secret { command } => match command {
            cli::SecretCommand::Check { json } => {
                let rows = ProviderId::all()
                    .iter()
                    .map(|provider| config::provider_secret(*provider, &home, &cwd))
                    .collect::<Vec<_>>();
                if json {
                    let providers = rows
                        .iter()
                        .map(|row| {
                            serde_json::json!({
                                "provider": row.provider.as_str(),
                                "mode": row.mode.as_str(),
                                "key_source": row.key_source.as_str(),
                                "key_variable": row.key_variable,
                                "key_present": row.key_present,
                                "base_url_present": row.base_url_present,
                            })
                        })
                        .collect::<Vec<_>>();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema_version": 1,
                            "providers": providers,
                        }))?
                    );
                } else {
                    for row in &rows {
                        if row.mode == config::AuthMode::Subscription {
                            let extra = if row.key_present {
                                format!("; key also present from {}", row.key_source.as_str())
                            } else {
                                String::new()
                            };
                            println!(
                                "{:<12} subscription · harness login{}",
                                row.provider.as_str(),
                                extra
                            );
                        } else {
                            println!(
                                "{:<12} api · {:<14} · {} · {}",
                                row.provider.as_str(),
                                row.key_source.as_str(),
                                row.key_variable,
                                if row.key_present {
                                    "key set"
                                } else {
                                    "key missing"
                                }
                            );
                        }
                    }
                }
                Ok(())
            }
        },
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
        cli::Commands::Files {
            query,
            ignored,
            json,
        } => {
            let query = query.join(" ");
            let root = std::fs::canonicalize(&cwd).map_err(|error| {
                anyhow::anyhow!("cannot resolve -C directory {}: {error}", cwd.display())
            })?;
            let project = files::recent_project_key(&root.to_string_lossy());
            let recent = BatonStore::open(&paths::baton_db_path())
                .and_then(|store| {
                    let _ = store.prune_missing_recent_projects();
                    store.list_recent_files(&project, crate::store::RECENT_FILE_LIMIT)
                })
                .unwrap_or_default();
            let listing = files::list_files(
                &root,
                files::FileOptions {
                    query: (!query.trim().is_empty()).then_some(query.as_str()),
                    limit: cli.limit,
                    include_ignored: ignored,
                    recent: &recent,
                },
            )?;
            if json {
                print_json(&serde_json::json!({
                    "schema_version": files::SCHEMA_VERSION,
                    "cwd": root.to_string_lossy(),
                    "query": query,
                    "ignored": ignored,
                    "limit": cli.limit,
                    "truncated": listing.truncated,
                    "capped": listing.capped,
                    "count": listing.files.len(),
                    "files": listing.files,
                }))?;
            } else {
                print_file_paths(&listing.files)?;
                if listing.capped {
                    eprintln!("results truncated at the 20,001-match cap; refine the query");
                } else if listing.truncated {
                    eprintln!("showing {} files; raise --limit", listing.files.len());
                }
            }
            Ok(())
        }
        cli::Commands::Grep {
            query,
            case_sensitive,
            word,
            regex,
            json,
        } => {
            let query = query.join(" ");
            let root = std::fs::canonicalize(&cwd).map_err(|error| {
                anyhow::anyhow!("cannot resolve -C directory {}: {error}", cwd.display())
            })?;
            let listing = grep::search(
                &root,
                grep::ContentOptions {
                    query: query.trim(),
                    limit: cli.limit,
                    case_sensitive,
                    word,
                    regex,
                },
            )?;
            if json {
                let matches = listing
                    .matches
                    .iter()
                    .map(|matched| {
                        serde_json::json!({
                            "path": matched.path,
                            "line": matched.line,
                            "text": matched.text,
                        })
                    })
                    .collect::<Vec<_>>();
                print_json(&serde_json::json!({
                    "schema_version": grep::SCHEMA_VERSION,
                    "cwd": root.to_string_lossy(),
                    "query": query.trim(),
                    "case_sensitive": case_sensitive,
                    "word": word,
                    "regex": regex,
                    "limit": cli.limit,
                    "truncated": listing.truncated,
                    "count": listing.matches.len(),
                    "matches": matches,
                }))?;
            } else {
                let lines = listing
                    .matches
                    .iter()
                    .map(|matched| {
                        if matched.text.is_empty() {
                            format!("{}:{}", matched.path, matched.line)
                        } else {
                            format!("{}:{}: {}", matched.path, matched.line, matched.text)
                        }
                    })
                    .collect::<Vec<_>>();
                print_file_paths(&lines)?;
                if listing.truncated {
                    eprintln!("results truncated; refine the query or raise --limit");
                }
            }
            Ok(())
        }
        cli::Commands::Open { path, print } => {
            let (path, line) = files::split_line_target(&path);
            let target = files::resolve_project_file(&cwd, Path::new(&path))?;
            let root = std::fs::canonicalize(&cwd).unwrap_or_else(|_| cwd.clone());
            if let Ok(relative) = target.strip_prefix(&root) {
                let project = files::recent_project_key(&root.to_string_lossy());
                if let Ok(store) = BatonStore::open(&paths::baton_db_path()) {
                    let relative = relative.to_string_lossy().replace('\\', "/");
                    let _ = store.record_recent_file(&project, &relative);
                }
            }
            let editor = files::editor_from_env().ok_or_else(|| {
                anyhow::anyhow!(
                    "no editor configured; set VISUAL or EDITOR, for example `EDITOR=vim baton open {path}`"
                )
            })?;
            let command = files::build_open_command_at_line(&editor, &target, line)?;
            if print {
                print_file_paths(&[command.shell_line()])?;
            } else {
                command.run(&cwd)?;
            }
            Ok(())
        }
        cli::Commands::Activity { json } => {
            let store = BatonStore::open(&paths::baton_db_path())?;
            let thread = match cli.thread.as_deref() {
                Some(thread_id) => resolve_thread(&store, thread_id)?,
                None => store
                    .list_threads_for_path(&cwd)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("no threads in this directory yet"))?,
            };
            let turns = store.list_turn_records(&thread.id)?;
            let messages = store.list_messages(&thread.id)?;
            let usage = store.list_usage(Some(&thread.id))?;
            let rows = activity::build(&thread, &turns, &messages, &usage);
            if json {
                println!("{}", serde_json::to_string_pretty(&activity::to_json(&thread.id, &rows))?);
            } else if rows.is_empty() {
                println!("activity: no turns");
            } else {
                println!("thread: {} {}", &thread.id[..8.min(thread.id.len())], thread.title);
                for row in &rows {
                    println!("{}", activity::summary(row));
                }
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
        cli::Commands::Agents { json } => {
            let Some(thread_id) = cli.thread.as_deref() else {
                anyhow::bail!("agents needs a thread id: baton agents -t <thread-id> [--json]");
            };
            let store = BatonStore::open(&paths::baton_db_path())?;
            let thread = resolve_thread(&store, thread_id)?;
            let rows = store.list_subagents(&thread.id)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "thread_id": thread.id,
                        "subagents": rows.iter().map(|row| serde_json::json!({
                            "id": row.id,
                            "turn": row.turn,
                            "provider": row.provider,
                            "call_id": row.call_id,
                            "name": row.name,
                            "detail": row.detail,
                            "status": row.status,
                            "started_at": row.started_at,
                            "finished_at": row.finished_at,
                        })).collect::<Vec<_>>(),
                    }))?
                );
            } else if rows.is_empty() {
                println!(
                    "no subagents recorded for {}",
                    &thread.id[..8.min(thread.id.len())]
                );
            } else {
                println!(
                    "{} subagent(s) for {}",
                    rows.len(),
                    &thread.id[..8.min(thread.id.len())]
                );
                for row in &rows {
                    println!(
                        "  turn {:<3} {:<9} {} · {}{}",
                        row.turn,
                        row.status,
                        sanitize::sanitize_terminal_text(&row.name),
                        row.detail
                            .as_deref()
                            .map(|detail| sanitize::sanitize_terminal_text(detail))
                            .unwrap_or_default(),
                        row.call_id
                            .as_deref()
                            .map(|id| format!(" · {id}"))
                            .unwrap_or_default()
                    );
                }
            }
            return Ok(());
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
                } else if state.command_matches && state.start_matches {
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
                None => match store.list_threads_for_path(&cwd)?.into_iter().next() {
                    Some(thread) => thread,
                    None => {
                        if json {
                            println!("{}", serde_json::to_string_pretty(&status::empty_json())?);
                        } else {
                            println!("no thread in this directory yet. Run baton to start one.");
                        }
                        return Ok(());
                    }
                },
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
                reasoning_effort: thread.reasoning_effort.clone(),
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
                    let expected = ProviderId::all()
                        .iter()
                        .map(|provider| provider.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let provider = ProviderId::parse(&provider).ok_or_else(|| {
                        anyhow::anyhow!("unknown provider {provider}, expected one of {expected}")
                    })?;
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
                    let mut file = config::load_config_file_strict(&home)?.unwrap_or_default();
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

fn print_json(value: &serde_json::Value) -> Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    let mut output = std::io::BufWriter::new(std::io::stdout().lock());
    if let Err(error) = writeln!(output, "{text}").and_then(|()| output.flush()) {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(error.into());
    }
    Ok(())
}

fn print_file_paths(paths: &[String]) -> Result<()> {
    let mut output = std::io::BufWriter::new(std::io::stdout().lock());
    for path in paths {
        if let Err(error) = writeln!(output, "{path}") {
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(error.into());
        }
    }
    if let Err(error) = output.flush() {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(error.into());
    }
    Ok(())
}

fn run_bench(home: &Path, started: Instant, target: &str, json: bool, repeat: u32) -> Result<()> {
    if target != "startup" {
        anyhow::bail!("unknown bench target \"{target}\"; only startup is available");
    }
    let repeat = repeat.max(1);
    let mut samples = Vec::new();
    for _ in 0..repeat {
        let iteration = Instant::now();
        let phase = Instant::now();
        config::validate_provider_overrides(home)?;
        let config_ms = phase.elapsed().as_secs_f64() * 1000.0;

        let phase = Instant::now();
        let store = BatonStore::open(&paths::baton_db_path())?;
        let store_ms = phase.elapsed().as_secs_f64() * 1000.0;

        let phase = Instant::now();
        let _ = process::reap_orphans(&store)?;
        let reap_ms = phase.elapsed().as_secs_f64() * 1000.0;

        let phase = Instant::now();
        let threads = store.list_threads(None)?;
        if let Some(thread) = threads.first() {
            let _ = store.list_messages(&thread.id)?;
        }
        let hydrate_ms = phase.elapsed().as_secs_f64() * 1000.0;

        samples.push(serde_json::json!({
            "config_ms": config_ms,
            "store_ms": store_ms,
            "reap_ms": reap_ms,
            "hydrate_ms": hydrate_ms,
            "total_ms": iteration.elapsed().as_secs_f64() * 1000.0,
            "threads": threads.len(),
        }));
    }
    let median = |key: &str| -> f64 {
        let mut values: Vec<f64> = samples
            .iter()
            .filter_map(|sample| sample.get(key).and_then(serde_json::Value::as_f64))
            .collect();
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        match values.len() {
            0 => 0.0,
            len if len % 2 == 1 => values[len / 2],
            len => (values[len / 2 - 1] + values[len / 2]) / 2.0,
        }
    };
    let process_ms = started.elapsed().as_secs_f64() * 1000.0;
    if json {
        let summary = serde_json::json!({
            "target": "startup",
            "repeat": repeat,
            "process_ms": process_ms,
            "samples": samples,
            "median_ms": {
                "config": median("config_ms"),
                "store": median("store_ms"),
                "reap": median("reap_ms"),
                "hydrate": median("hydrate_ms"),
                "total": median("total_ms"),
            },
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "startup over {repeat} run(s): config {:.1} ms, store {:.1} ms, reap {:.1} ms, hydrate {:.1} ms, per-run total {:.1} ms, process {:.1} ms",
            median("config_ms"),
            median("store_ms"),
            median("reap_ms"),
            median("hydrate_ms"),
            median("total_ms"),
            process_ms,
        );
    }
    Ok(())
}

fn open_store(host_id: Option<&str>) -> Result<BatonStore> {
    let path = paths::baton_db_path();
    match host_id {
        Some(host_id) => BatonStore::open_hosted(&path, host_id),
        None => BatonStore::open(&path),
    }
}

fn reconcile_startup_orchestrations() {
    let Ok(store) = BatonStore::open(&paths::baton_db_path()) else {
        return;
    };
    match orchestrate::reconcile_abandoned_runs(&store) {
        Ok(runs) => {
            for run in runs {
                eprintln!(
                    "recovered abandoned orchestration {} ({}, {} completed, {} failed, {} skipped, {} cancelled, {} needs input)",
                    &run.run_id[..8.min(run.run_id.len())],
                    run.status,
                    run.completed_tasks,
                    run.failed_tasks,
                    run.skipped_tasks,
                    run.cancelled_tasks,
                    run.needs_input_tasks
                );
            }
        }
        Err(error) => {
            if std::env::var_os("BATON_DEBUG_PROCESS").is_some() {
                eprintln!("baton orchestration recovery failed: {error}");
            }
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
            report.identity_mismatches.len(),
            report.unverifiable,
            report.errors.len()
        );
        for (row, actual) in &report.identity_mismatches {
            eprintln!(
                "baton process mismatch: pid={} provider={} expected={:?} actual={:?}",
                row.pid, row.provider, row.command, actual
            );
        }
    }
}

fn run_serve(home: &Path, stdio: bool) -> Result<()> {
    let host_id = uuid::Uuid::new_v4().to_string();
    let store = BatonStore::open_hosted(&paths::baton_db_path(), &host_id)?;
    let claimed = match claim::acquire(&store, home, &host_id, &claim::SystemProbe)? {
        claim::ClaimOutcome::Claimed(claim) => claim,
        claim::ClaimOutcome::Refused(refusal) => {
            eprintln!("baton: {refusal}");
            std::process::exit(refusal.exit_code());
        }
    };
    let reaped = process::reap_orphans(&store)?;
    if let Some(summary) = reaped.summary() {
        eprintln!("baton: {summary}");
    }
    for error in &reaped.errors {
        eprintln!("baton: reaper: {error}");
    }
    let turns = store.reconcile_running_turns(None)?;
    let runs = orchestrate::reconcile_abandoned_runs(&store)?;
    eprintln!(
        "baton: host ready pid {} host {} installation {}",
        claimed.pid, claimed.host_id, claimed.installation_id
    );
    if !turns.is_empty() {
        eprintln!("baton: settled {} unfinished turn(s)", turns.len());
    }
    if !runs.is_empty() {
        eprintln!(
            "baton: settled {} abandoned orchestration run(s)",
            runs.len()
        );
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(wait_for_host_stop(stdio))?;
    if !store.release_host_claim(&host_id)? {
        anyhow::bail!("host claim disappeared before shutdown");
    }
    eprintln!("baton: host stopped");
    Ok(())
}

async fn wait_for_host_stop(stdio: bool) -> Result<()> {
    let stdin_wait = async move {
        if stdio {
            wait_for_stdin_eof().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
            _ = stdin_wait => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = stdin_wait => {}
        }
    }
    Ok(())
}

async fn wait_for_stdin_eof() {
    let mut stdin = tokio::io::stdin();
    let mut buffer = [0u8; 4096];
    loop {
        match stdin.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
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
    let antigravity_gemini = antigravity_home.join("gemini");
    let antigravity_spawner = Arc::new(
        move |auth_env: &[(String, String)]| -> anyhow::Result<baton::adapters::TransportHalves> {
            std::fs::create_dir_all(&antigravity_gemini)?;
            // The agy CLI serves ACP itself. The explicit gemini dir and app
            // data dir keep baton's profile isolated from the user's IDE.
            let mut command = tokio::process::Command::new("agy");
            command.arg("acp");
            command.arg("--gemini-dir").arg(&antigravity_gemini);
            command.arg("--app-data-dir").arg("antigravity-acp");
            command.current_dir(&cwd_for_antigravity);
            command.env("AGY_ACP_FORCE_FILE_STORAGE", "1");
            command.env("GEMINI_HOME", &antigravity_gemini);
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
            // Cursor's ACP authenticate opens a browser login. Fail fast with
            // a clear hint instead of hanging a headless turn.
            if auth_env.is_empty() {
                let status = std::process::Command::new("cursor-agent")
                    .arg("status")
                    .output()?;
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&status.stdout),
                    String::from_utf8_lossy(&status.stderr)
                );
                if !status.status.success()
                    || text.to_ascii_lowercase().contains("not logged in")
                {
                    anyhow::bail!(
                        "cursor-agent is not logged in; run `cursor-agent login`, then retry"
                    );
                }
            }
            let mut command = tokio::process::Command::new("cursor-agent");
            command.arg("acp");
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
    vec![
        Arc::new(tokio::sync::Mutex::new(Box::new(ClaudeAdapter::new(ClaudeAdapter::real_spawner_with_recorder(claude_recorder))) as Box<dyn BatonAdapter>)),
        provider_runtime::build_codex_adapter(store, cwd),
        Arc::new(tokio::sync::Mutex::new(Box::new(AcpAdapter::new(ProviderId::Cursor, cursor_spawner)) as Box<dyn BatonAdapter>)),
        Arc::new(tokio::sync::Mutex::new(Box::new(AcpAdapter::new(ProviderId::Grok, grok_spawner)) as Box<dyn BatonAdapter>)),
        provider_runtime::build_opencode_adapter(store, cwd),
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
    let configured = config::get_default_provider(home);
    let resolved = resolve_config(home, cwd);
    let installed = ProviderId::all()
        .iter()
        .copied()
        .filter(|provider| doctor::find_executable(provider.binary_name()).is_some())
        .collect::<Vec<_>>();
    let api = ProviderId::all()
        .iter()
        .copied()
        .filter(|provider| resolved.for_provider(*provider).mode == crate::config::AuthMode::Api)
        .collect::<Vec<_>>();
    config::pick_default_provider(configured, &installed, &api)
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

fn run_chat(
    cli: cli::Cli,
    home: &PathBuf,
    cwd: &PathBuf,
    permission_override: Option<PermissionMode>,
    initial_prompt: Option<String>,
    requested_effort: Option<String>,
) -> Result<()> {
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
            thread.permission_mode = resolved.for_provider(provider).permission_mode;
            thread
        }
    };
    if let Some(mode) = permission_override {
        thread.permission_mode = mode;
    }
    if let Some(effort) = requested_effort.as_deref() {
        if !thread.current_provider.supports_effort() {
            anyhow::bail!(
                "{} does not support --effort; use codex or claude",
                thread.current_provider
            );
        }
        thread.reasoning_effort = Some(effort.to_string());
        let guard = store.lock().unwrap();
        if guard.get_thread(&thread.id)?.is_some() {
            guard.set_thread_effort(&thread.id, Some(effort))?;
        }
    } else if cli.effort.is_some() {
        thread.reasoning_effort = None;
        let guard = store.lock().unwrap();
        if guard.get_thread(&thread.id)?.is_some() {
            guard.set_thread_effort(&thread.id, None)?;
        }
    }
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
