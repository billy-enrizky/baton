//! Minimal Orca-style orchestration: a coordinator plans independent tasks,
//! each worker runs in its own git worktree through the normal `baton exec`
//! path, and results are recorded durably.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::resolve_config;
use crate::store::{BatonStore, OrchestrationRunRow, OrchestrationTaskRow};
use crate::types::{PermissionMode, ProviderId};
use crate::worktree::{self, WorktreeInfo};

pub const SCHEMA_VERSION: u32 = 1;
pub const CANCELLED_ERROR: &str = "orchestration cancelled";

fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel
        .map(|cancel| cancel.load(Ordering::Relaxed))
        .unwrap_or(false)
}

struct CommandResult {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    cancelled: bool,
}

/// Runs a provider command in its own process group. The poll loop lets a
/// cancellation reach the whole group, including the provider child that
/// `baton exec` started.
fn run_command_cancellable(
    mut command: Command,
    cancel: Option<&AtomicBool>,
) -> std::io::Result<CommandResult> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pgid = child.id();
    let stdout = child.stdout.take().map(|mut stream| {
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = stream.read_to_end(&mut buffer);
            buffer
        })
    });
    let stderr = child.stderr.take().map(|mut stream| {
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = stream.read_to_end(&mut buffer);
            buffer
        })
    });
    let mut cancelled = false;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if is_cancelled(cancel) {
            cancelled = true;
            #[cfg(unix)]
            {
                if !crate::process::terminate_group(pgid) {
                    let _ = child.kill();
                }
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = child.wait()?;
    let stdout = stdout
        .map(|handle| handle.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr = stderr
        .map(|handle| handle.join().unwrap_or_default())
        .unwrap_or_default();
    Ok(CommandResult {
        status,
        stdout,
        stderr,
        cancelled,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanTask {
    pub title: String,
    pub provider: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub summary: Option<String>,
    pub tasks: Vec<PlanTask>,
}

#[derive(Debug, Clone)]
pub struct WorkerResult {
    pub id: String,
    pub title: String,
    pub provider: ProviderId,
    pub model: String,
    pub prompt: String,
    pub worktree: Option<WorktreeInfo>,
    pub commit: Option<String>,
    pub thread_id: Option<String>,
    pub status: String,
    pub text: Option<String>,
    pub error: Option<String>,
    pub pr_url: Option<String>,
    pub created_at: String,
    pub finished_at: String,
}

#[derive(Debug, Clone)]
pub enum OrchestrationProgress {
    PlanReady {
        summary: Option<String>,
        tasks: Vec<PlanTask>,
    },
    WorkerStarted {
        index: usize,
        title: String,
        provider: ProviderId,
        model: String,
        worktree: Option<PathBuf>,
    },
    WorkerFinished {
        index: usize,
        title: String,
        status: String,
        commit: Option<String>,
        error: Option<String>,
    },
}

pub type ProgressFn<'a> = dyn Fn(OrchestrationProgress) + Send + Sync + 'a;

fn emit(progress: Option<&ProgressFn<'_>>, event: OrchestrationProgress) {
    if let Some(progress) = progress {
        progress(event);
    }
}

pub fn render_progress(event: &OrchestrationProgress) -> String {
    match event {
        OrchestrationProgress::PlanReady { summary, tasks } => format!(
            "plan: {} worker(s){}",
            tasks.len(),
            summary
                .as_ref()
                .map(|summary| format!(" · {summary}"))
                .unwrap_or_default()
        ),
        OrchestrationProgress::WorkerStarted {
            index,
            title,
            provider,
            model,
            ..
        } => format!("[{index}] running {title} · {provider}/{model}"),
        OrchestrationProgress::WorkerFinished {
            index,
            title,
            status,
            commit,
            error,
        } => format!(
            "[{index}] {status} {title}{}{}",
            commit
                .as_ref()
                .map(|commit| format!(" · commit {}", &commit[..8.min(commit.len())]))
                .unwrap_or_default(),
            error
                .as_ref()
                .map(|error| format!(" · {error}"))
                .unwrap_or_default()
        ),
    }
}

#[derive(Debug, Clone)]
pub struct RunReport {
    pub run_id: String,
    pub goal: String,
    pub coordinator_provider: ProviderId,
    pub coordinator_model: String,
    pub summary: Option<String>,
    pub plan: Plan,
    pub repo_root: Option<PathBuf>,
    pub dirty_source: bool,
    pub status: String,
    pub archived_at: Option<String>,
    pub tasks: Vec<WorkerResult>,
}

fn available_providers(coordinator: ProviderId) -> Vec<ProviderId> {
    let mut available = ProviderId::all()
        .iter()
        .copied()
        .filter(|provider| {
            crate::doctor::find_executable(provider.binary_name()).is_some()
        })
        .collect::<Vec<_>>();
    if !available.contains(&coordinator) {
        available.push(coordinator);
    }
    available
}

fn planner_prompt(
    goal: &str,
    cwd: &Path,
    max_tasks: usize,
    available: &[ProviderId],
    coordinator: ProviderId,
) -> String {
    let providers = available
        .iter()
        .map(|provider| provider.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let template = r#"You are baton's coordinator. Split the goal into at most MAX independent worker tasks.
Use the smallest number of tasks that truly parallelizes. Return exactly one task for a question, a small fix, or work that touches one cohesive area. Split only when the tasks are independent, touch disjoint files, and do not need each other's output. Do not invent extra work.
Each task prompt must be self-contained. Never ask a worker to coordinate with another worker.
Each worker runs in its own git worktree, so tasks must not depend on another task's output and must avoid overlapping files.
Choose the best installed harness for each task from: PROVIDERS. Prefer COORDINATOR when the task does not benefit from another harness.
Every worker starts in its own worktree, so each prompt must refer to the current working directory, never to the source checkout path.
Return ONLY a JSON object, with no markdown fences or commentary:
{"summary":"one sentence","tasks":[{"title":"short task name","provider":"codex","prompt":"complete self-contained instructions for the worker"}]}
Goal: GOAL
Source repository: CWD"#;
    template
        .replace("MAX", &max_tasks.to_string())
        .replace("PROVIDERS", &providers)
        .replace("COORDINATOR", coordinator.as_str())
        .replace("GOAL", goal)
        .replace("CWD", &cwd.display().to_string())
}

fn validate_plan_providers(plan: &Plan, available: &[ProviderId]) -> Result<()> {
    for task in &plan.tasks {
        let provider = ProviderId::parse(task.provider.trim())
            .ok_or_else(|| anyhow::anyhow!("planner returned unknown provider {}", task.provider))?;
        if !available.contains(&provider) {
            anyhow::bail!(
                "planner selected {} but that harness is not installed; available: {}",
                provider,
                available
                    .iter()
                    .map(|provider| provider.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}

const WORKER_PREAMBLE: &str = "You are a worker in an isolated git worktree. Treat the current working directory as the repository root. Do not read or write the source checkout outside this worktree.";

fn apply_permission_args(command: &mut Command, permission_mode: PermissionMode) {
    match permission_mode {
        PermissionMode::BypassPermissions => {
            command.arg("--yolo");
        }
        PermissionMode::AcceptEdits => {
            command.arg("--full-auto");
        }
        PermissionMode::Plan => {
            command.arg("--plan");
        }
        PermissionMode::Default => {}
    }
}

fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&text[start..=end])
}

pub fn parse_plan(text: &str, max_tasks: usize) -> Result<Plan> {
    let body = extract_json(text).ok_or_else(|| anyhow::anyhow!("planner did not return a JSON object"))?;
    let mut plan: Plan = serde_json::from_str(body)?;
    if plan.tasks.is_empty() {
        anyhow::bail!("planner returned no tasks");
    }
    if plan.tasks.len() > max_tasks {
        plan.tasks.truncate(max_tasks);
    }
    for task in &plan.tasks {
        if task.title.trim().is_empty() {
            anyhow::bail!("planner returned a task without a title");
        }
        if task.prompt.trim().is_empty() {
            anyhow::bail!("planner returned a task without a prompt");
        }
        if ProviderId::parse(task.provider.trim()).is_none() {
            anyhow::bail!("planner returned unknown provider {}", task.provider);
        }
    }
    Ok(plan)
}

fn parse_exec_output(stdout: &[u8], stderr: &[u8]) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    let value: Option<serde_json::Value> = serde_json::from_slice(stdout).ok();
    if let Some(value) = value {
        return (
            value.get("thread_id").and_then(|value| value.as_str()).map(str::to_string),
            value.get("text").and_then(|value| value.as_str()).map(str::to_string),
            value.get("error").and_then(|value| value.as_str()).map(str::to_string),
            value.get("phase").and_then(|value| value.as_str()).map(str::to_string),
        );
    }
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    (None, None, (!stderr.is_empty()).then_some(stderr), None)
}

fn run_coordinator(
    prompt: &str,
    cwd: &Path,
    provider: ProviderId,
    model: &str,
    permission_mode: PermissionMode,
    cancel: Option<&AtomicBool>,
) -> Result<String> {
    let temp_home = std::env::temp_dir().join(format!("baton-planner-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_home)?;
    let binary = std::env::current_exe()?;
    let mut command = Command::new(binary);
    command
        .arg("-C")
        .arg(cwd)
        .arg("exec")
        .arg("-p")
        .arg(provider.as_str())
        .arg("-m")
        .arg(model)
        .arg("--json");
    apply_permission_args(&mut command, permission_mode);
    command.arg(prompt).env("BATON_HOME", &temp_home);
    let output = run_command_cancellable(command, cancel);
    let _ = std::fs::remove_dir_all(&temp_home);
    let output = output?;
    if output.cancelled {
        anyhow::bail!(CANCELLED_ERROR);
    }
    let (_, text, error, _) = parse_exec_output(&output.stdout, &output.stderr);
    if !output.status.success() && text.is_none() {
        anyhow::bail!(
            "coordinator failed: {}",
            error.unwrap_or_else(|| "no output from baton exec".to_string())
        );
    }
    text.ok_or_else(|| anyhow::anyhow!("coordinator returned no text"))
}

pub fn is_conversational_greeting(text: &str) -> bool {
    let normalized = text
        .trim()
        .trim_end_matches(|character: char| {
            character.is_ascii_punctuation() || character.is_whitespace()
        })
        .to_lowercase();
    matches!(
        normalized.as_str(),
        "hi"
            | "hello"
            | "hey"
            | "yo"
            | "hi there"
            | "hello there"
            | "good morning"
            | "good afternoon"
            | "good evening"
            | "thanks"
            | "thank you"
    )
}

fn run_planner(
    goal: &str,
    cwd: &Path,
    provider: ProviderId,
    model: &str,
    max_tasks: usize,
    permission_mode: PermissionMode,
    cancel: Option<&AtomicBool>,
    available: &[ProviderId],
) -> Result<Plan> {
    let base_prompt = planner_prompt(goal, cwd, max_tasks, available, provider);
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 0..2 {
        let prompt = if attempt == 0 {
            base_prompt.clone()
        } else {
            format!(
                "{base_prompt}\nYour previous plan was not usable: {}. Use only the installed harnesses listed above. Return only the JSON object.",
                last_error
                    .as_ref()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "invalid JSON".to_string())
            )
        };
        let text = match run_coordinator(
            &prompt,
            cwd,
            provider,
            model,
            permission_mode,
            cancel,
        ) {
            Ok(text) => text,
            Err(error) if error.to_string() == CANCELLED_ERROR => return Err(error),
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        if std::env::var_os("BATON_ORCHESTRATE_DEBUG").is_some() {
            eprintln!("[orchestrate] planner text (attempt {}): {text}", attempt + 1);
        }
        match parse_plan(&text, max_tasks)
            .and_then(|plan| validate_plan_providers(&plan, available).map(|()| plan))
        {
            Ok(plan) => return Ok(plan),
            Err(error) => {
                if plan_is_empty(&error) {
                    return plan_after_planner_failure(goal, provider, &error);
                }
                last_error = Some(error);
            }
        }
    }
    let error = last_error.unwrap_or_else(|| anyhow::anyhow!("planner failed"));
    if std::env::var_os("BATON_ORCHESTRATE_DEBUG").is_some() {
        eprintln!("[orchestrate] planner failed; using one direct worker: {error}");
    }
    plan_after_planner_failure(goal, provider, &error)
}

fn fallback_plan(goal: &str, provider: ProviderId) -> Plan {
    Plan {
        summary: Some("direct task".to_string()),
        tasks: vec![PlanTask {
            title: "answer".to_string(),
            provider: provider.as_str().to_string(),
            prompt: goal.to_string(),
        }],
    }
}

fn plan_is_empty(error: &anyhow::Error) -> bool {
    error.to_string() == "planner returned no tasks"
}

/// A planner that cannot produce a usable plan must not strand the user's
/// work. Cancellation is the one exception: the user asked to stop.
fn plan_after_planner_failure(
    goal: &str,
    provider: ProviderId,
    error: &anyhow::Error,
) -> Result<Plan> {
    if error.to_string() == CANCELLED_ERROR {
        anyhow::bail!(CANCELLED_ERROR);
    }
    Ok(fallback_plan(goal, provider))
}

fn load_plan(path: &Path, max_tasks: usize) -> Result<Plan> {
    let text = std::fs::read_to_string(path)?;
    parse_plan(&text, max_tasks)
}

struct WorkerFallback {
    index: usize,
    title: String,
    provider: ProviderId,
    model: String,
    prompt: String,
}

fn panicked_worker(fallback: &WorkerFallback) -> (usize, WorkerResult) {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    (
        fallback.index,
        WorkerResult {
            id: uuid::Uuid::new_v4().to_string(),
            title: fallback.title.clone(),
            provider: fallback.provider,
            model: fallback.model.clone(),
            prompt: fallback.prompt.clone(),
            worktree: None,
            commit: None,
            thread_id: None,
            status: "failed".to_string(),
            text: None,
            error: Some("worker thread panicked".to_string()),
            pr_url: None,
            created_at: now.clone(),
            finished_at: now,
        },
    )
}

fn run_worker(
    task: &PlanTask,
    index: usize,
    run_id: &str,
    home: &Path,
    cwd: &Path,
    repo_root: Option<&Path>,
    worker_bin: Option<&Path>,
    progress: Option<&ProgressFn<'_>>,
    cancel: Option<&AtomicBool>,
    permission_mode: PermissionMode,
) -> (usize, WorkerResult) {
    let (index, result) = run_worker_inner(
        task,
        index,
        run_id,
        home,
        cwd,
        repo_root,
        worker_bin,
        progress,
        cancel,
        permission_mode,
    );
    emit(
        progress,
        OrchestrationProgress::WorkerFinished {
            index: index + 1,
            title: result.title.clone(),
            status: result.status.clone(),
            commit: result.commit.clone(),
            error: result.error.clone(),
        },
    );
    (index, result)
}

fn run_worker_inner(
    task: &PlanTask,
    index: usize,
    run_id: &str,
    home: &Path,
    cwd: &Path,
    repo_root: Option<&Path>,
    worker_bin: Option<&Path>,
    progress: Option<&ProgressFn<'_>>,
    cancel: Option<&AtomicBool>,
    permission_mode: PermissionMode,
) -> (usize, WorkerResult) {
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let provider = ProviderId::parse(task.provider.trim()).unwrap_or(ProviderId::Codex);
    let resolved = resolve_config(home, cwd);
    let model = resolved.for_provider(provider).model.clone();
    if is_cancelled(cancel) {
        return (
            index,
            WorkerResult {
                id: uuid::Uuid::new_v4().to_string(),
                title: task.title.clone(),
                provider,
                model,
                prompt: task.prompt.clone(),
                worktree: None,
                commit: None,
                thread_id: None,
                status: "cancelled".to_string(),
                text: None,
                error: None,
                pr_url: None,
                created_at: created_at.clone(),
                finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            },
        );
    }
    let worktree = match repo_root {
        Some(root) => match worktree::create(home, root, run_id, &task.title) {
            Ok(worktree) => Some(worktree),
            Err(error) => {
                return (
                    index,
                    WorkerResult {
                        id: uuid::Uuid::new_v4().to_string(),
                        title: task.title.clone(),
                        provider,
                        model,
                        prompt: task.prompt.clone(),
                        worktree: None,
                        commit: None,
                        thread_id: None,
                        status: "failed".to_string(),
                        text: None,
                        error: Some(format!("cannot create worktree: {error}")),
                        pr_url: None,
                        created_at,
                        finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    },
                );
            }
        },
        None => None,
    };
    let worker_cwd = worktree
        .as_ref()
        .map(|worktree| worktree.path.clone())
        .unwrap_or_else(|| cwd.to_path_buf());
    let binary = worker_bin
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("baton"));
    let effective_prompt = format!("{WORKER_PREAMBLE}\n\n{}", task.prompt);
    emit(
        progress,
        OrchestrationProgress::WorkerStarted {
            index: index + 1,
            title: task.title.clone(),
            provider,
            model: model.clone(),
            worktree: worktree.as_ref().map(|worktree| worktree.path.clone()),
        },
    );
    let mut command = Command::new(binary);
    command
        .arg("-C")
        .arg(&worker_cwd)
        .arg("exec")
        .arg("-p")
        .arg(provider.as_str())
        .arg("-m")
        .arg(&model)
        .arg("--json");
    apply_permission_args(&mut command, permission_mode);
    command.arg(&effective_prompt).env("BATON_HOME", home);
    let output = run_command_cancellable(command, cancel);
    let (thread_id, text, error, _phase, cancelled) = match output {
        Ok(result) => {
            let (thread_id, text, error, phase) =
                parse_exec_output(&result.stdout, &result.stderr);
            let ok = result.status.success() && phase.as_deref() == Some("completed");
            let error = if result.cancelled || ok {
                None
            } else {
                error.or_else(|| Some(format!("worker exited with {}", result.status)))
            };
            (thread_id, text, error, phase, result.cancelled)
        }
        Err(error) => (None, None, Some(format!("cannot run worker: {error}")), None, false),
    };
    let mut status = if cancelled {
        "cancelled"
    } else if error.is_none() {
        "completed"
    } else {
        "failed"
    };
    let mut error = error;
    let mut commit = None;
    if status == "completed" {
        if let Some(worktree) = worktree.as_ref() {
            match worktree::commit_changes(
                &worktree.path,
                &format!("baton worker: {}", task.title),
            ) {
                Ok(sha) => commit = sha,
                Err(commit_error) => {
                    status = "failed";
                    error = Some(format!(
                        "worker completed but its changes could not be committed: {commit_error}"
                    ));
                }
            }
        }
    }
    (
        index,
        WorkerResult {
            id: uuid::Uuid::new_v4().to_string(),
            title: task.title.clone(),
            provider,
            model,
            prompt: task.prompt.clone(),
            worktree,
            commit,
            thread_id,
            status: status.to_string(),
            text,
            error,
            pr_url: None,
            created_at,
            finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        },
    )
}

#[derive(Debug, Clone)]
pub struct IntegratedTask {
    pub index: usize,
    pub title: String,
    pub branch: String,
    pub commit: String,
    pub merge_commit: String,
}

#[derive(Debug, Clone)]
pub struct SkippedTask {
    pub index: usize,
    pub title: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct IntegrateConflict {
    pub index: usize,
    pub title: String,
    pub branch: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct IntegrateReport {
    pub run_id: String,
    pub repo_root: PathBuf,
    pub merged: Vec<IntegratedTask>,
    pub skipped: Vec<SkippedTask>,
    pub conflict: Option<IntegrateConflict>,
}

#[derive(Debug, Clone)]
pub struct ShippedTask {
    pub index: usize,
    pub title: String,
    pub branch: String,
    pub commit: String,
    pub pr_url: String,
}

#[derive(Debug, Clone)]
pub struct ShipReport {
    pub run_id: String,
    pub repo_root: PathBuf,
    pub remote: String,
    pub base: String,
    pub opened: Vec<ShippedTask>,
    pub skipped: Vec<SkippedTask>,
    pub failed: Vec<SkippedTask>,
}

fn ship_status(report: &ShipReport) -> &'static str {
    if !report.failed.is_empty() {
        if report.opened.is_empty() {
            "failed"
        } else {
            "partial"
        }
    } else if report.opened.is_empty() {
        "skipped"
    } else {
        "done"
    }
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn branch_exists(root: &Path, branch: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .current_dir(root)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn conflicted_files(root: &Path) -> Vec<String> {
    Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=U"])
        .current_dir(root)
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn integrate(
    store: &BatonStore,
    cwd: &Path,
    run_id: &str,
    selection: &[usize],
) -> Result<IntegrateReport> {
    let run_id = resolve_run_id(store, run_id)?;
    let repo_root = worktree::repo_root(cwd)?;
    if worktree::is_dirty(&repo_root)? {
        anyhow::bail!(
            "source checkout has uncommitted changes; commit or stash them before integrating"
        );
    }
    let tasks = store.list_orchestration_tasks(&run_id)?;
    if tasks.is_empty() {
        anyhow::bail!("run {} has no tasks", &run_id[..8.min(run_id.len())]);
    }
    for index in selection {
        if *index == 0 || *index > tasks.len() {
            anyhow::bail!(
                "--task {index} is out of range (the run has {} tasks)",
                tasks.len()
            );
        }
    }
    let short_run = &run_id[..8.min(run_id.len())];
    let mut report = IntegrateReport {
        run_id: run_id.clone(),
        repo_root: repo_root.clone(),
        merged: Vec::new(),
        skipped: Vec::new(),
        conflict: None,
    };
    for (offset, task) in tasks.iter().enumerate() {
        let index = offset + 1;
        if !selection.is_empty() && !selection.contains(&index) {
            continue;
        }
        if task.status != "completed" {
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: format!("worker status is {}", task.status),
            });
            continue;
        }
        if task.merged_at.is_some() {
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: "already merged".to_string(),
            });
            continue;
        }
        let Some(branch) = task.branch.as_deref() else {
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: "worker has no branch".to_string(),
            });
            continue;
        };
        if task.commit_sha.is_none() {
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: "worker produced no commit".to_string(),
            });
            continue;
        }
        if !branch_exists(&repo_root, branch) {
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: format!("branch {branch} no longer exists"),
            });
            continue;
        }
        let message = format!("baton: merge {} ({short_run})", task.title);
        let output = Command::new("git")
            .args(["merge", "--no-ff", branch, "-m", &message])
            .current_dir(&repo_root)
            .output()?;
        if output.status.success() {
            let merge_commit = git_stdout(&repo_root, &["rev-parse", "HEAD"])?;
            let merged_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            store.mark_orchestration_task_merged(&task.id, &merged_at)?;
            report.merged.push(IntegratedTask {
                index,
                title: task.title.clone(),
                branch: branch.to_string(),
                commit: task.commit_sha.clone().unwrap_or_default(),
                merge_commit,
            });
            continue;
        }
        let files = conflicted_files(&repo_root);
        let _ = Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(&repo_root)
            .output();
        if files.is_empty() {
            anyhow::bail!(
                "git merge failed for task {index} ({}): {}",
                task.title,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        report.conflict = Some(IntegrateConflict {
            index,
            title: task.title.clone(),
            branch: branch.to_string(),
            files,
        });
        break;
    }
    Ok(report)
}

pub fn resolve_run_id(store: &BatonStore, run_id: &str) -> Result<String> {
    let runs = store.find_orchestration_runs(run_id)?;
    if let Some(exact) = runs.iter().find(|run| run.id == run_id) {
        return Ok(exact.id.clone());
    }
    let matches = runs
        .iter()
        .filter(|run| run.id.starts_with(run_id))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => anyhow::bail!("no orchestration run matches {run_id}"),
        [run] => Ok(run.id.clone()),
        _ => anyhow::bail!("run id {run_id} is ambiguous; use more characters"),
    }
}

pub fn archive(store: &BatonStore, run_id: &str) -> Result<String> {
    let run_id = resolve_run_id(store, run_id)?;
    let run = store
        .find_orchestration_runs(&run_id)?
        .into_iter()
        .find(|run| run.id == run_id)
        .ok_or_else(|| anyhow::anyhow!("no orchestration run matches {run_id}"))?;
    if run.status == "running" {
        anyhow::bail!("run {} is still running; archive it after it finishes", &run_id[..8.min(run_id.len())]);
    }
    store.archive_orchestration_run(&run_id)?;
    Ok(run_id)
}

fn current_branch(root: &Path) -> Result<String> {
    let branch = git_stdout(root, &["branch", "--show-current"])?;
    if branch.is_empty() {
        anyhow::bail!("source checkout is not on a branch; check out the PR base branch first");
    }
    Ok(branch)
}

fn remote_name(root: &Path) -> Result<String> {
    let remotes = git_stdout(root, &["remote"])?;
    let remotes = remotes
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if remotes.iter().any(|remote| *remote == "origin") {
        return Ok("origin".to_string());
    }
    match remotes.as_slice() {
        [] => anyhow::bail!("source checkout has no git remote; add one before opening PRs"),
        [remote] => Ok((*remote).to_string()),
        _ => anyhow::bail!(
            "source checkout has multiple remotes and no origin; add an origin remote or push the worker branch manually"
        ),
    }
}

fn gh_binary(override_path: Option<&Path>) -> PathBuf {
    override_path
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("BATON_GH_BIN").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("gh"))
}

fn extract_url(stdout: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stdout)
        .split_whitespace()
        .find(|token| token.starts_with("https://") || token.starts_with("http://"))
        .map(|token| {
            token
                .trim_matches(|character: char| matches!(character, ')' | ',' | '.' | ';'))
                .to_string()
        })
}

fn existing_pr_url(gh: &Path, root: &Path, branch: &str) -> Option<String> {
    let output = Command::new(gh)
        .args([
            "pr", "list", "--head", branch, "--state", "all", "--json", "url", "--limit", "1",
        ])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    value
        .as_array()?
        .first()?
        .get("url")?
        .as_str()
        .map(str::to_string)
}

pub fn open_prs(
    store: &BatonStore,
    cwd: &Path,
    run_id: &str,
    selection: &[usize],
) -> Result<ShipReport> {
    open_prs_with_gh(store, cwd, run_id, selection, None)
}

fn open_prs_with_gh(
    store: &BatonStore,
    cwd: &Path,
    run_id: &str,
    selection: &[usize],
    gh_bin: Option<&Path>,
) -> Result<ShipReport> {
    let run_id = resolve_run_id(store, run_id)?;
    let repo_root = worktree::repo_root(cwd)?;
    let tasks = store.list_orchestration_tasks(&run_id)?;
    if tasks.is_empty() {
        anyhow::bail!("run {} has no tasks", &run_id[..8.min(run_id.len())]);
    }
    for index in selection {
        if *index == 0 || *index > tasks.len() {
            anyhow::bail!(
                "--task {index} is out of range (the run has {} tasks)",
                tasks.len()
            );
        }
    }
    let remote = remote_name(&repo_root)?;
    let base = current_branch(&repo_root)?;
    let gh = gh_binary(gh_bin);
    let short_run = run_id.chars().take(8).collect::<String>();
    let mut report = ShipReport {
        run_id,
        repo_root: repo_root.clone(),
        remote: remote.clone(),
        base: base.clone(),
        opened: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
    };
    for (offset, task) in tasks.iter().enumerate() {
        let index = offset + 1;
        if !selection.is_empty() && !selection.contains(&index) {
            continue;
        }
        if task.status != "completed" {
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: format!("worker status is {}", task.status),
            });
            continue;
        }
        if let Some(pr_url) = &task.pr_url {
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: format!("PR already recorded: {pr_url}"),
            });
            continue;
        }
        if task.merged_at.is_some() {
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: "already merged into the source checkout".to_string(),
            });
            continue;
        }
        let Some(commit) = task.commit_sha.as_deref() else {
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: "worker produced no commit".to_string(),
            });
            continue;
        };
        let Some(branch) = task.branch.as_deref() else {
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: "worker has no branch".to_string(),
            });
            continue;
        };
        if !branch_exists(&repo_root, branch) {
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: format!("branch {branch} no longer exists"),
            });
            continue;
        }
        if let Err(error) = git_stdout(&repo_root, &["push", "-u", &remote, branch]) {
            report.failed.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: format!("cannot push {branch}: {error}"),
            });
            continue;
        }
        if let Some(pr_url) = existing_pr_url(&gh, &repo_root, branch) {
            store.mark_orchestration_task_pr(&task.id, &pr_url)?;
            report.skipped.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: format!("PR already exists: {pr_url}"),
            });
            continue;
        }
        let title = format!("baton: {} ({short_run})", task.title);
        let body = format!(
            "Run: {short_run}
Worker: [{index}] {}
Harness: {}/{}

Created by baton orchestration.",
            task.title, task.provider, task.model
        );
        let output = Command::new(&gh)
            .args([
                "pr", "create", "--base", &base, "--head", branch, "--title", &title, "--body",
                &body,
            ])
            .current_dir(&repo_root)
            .output();
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                report.failed.push(SkippedTask {
                    index,
                    title: task.title.clone(),
                    reason: format!("cannot run {}: {error}", gh.display()),
                });
                continue;
            }
        };
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
            report.failed.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: if detail.is_empty() {
                    format!("gh exited with {}", output.status)
                } else {
                    format!("gh failed: {detail}")
                },
            });
            continue;
        }
        let Some(pr_url) = extract_url(&output.stdout) else {
            report.failed.push(SkippedTask {
                index,
                title: task.title.clone(),
                reason: "gh created a PR but did not print a URL".to_string(),
            });
            continue;
        };
        store.mark_orchestration_task_pr(&task.id, &pr_url)?;
        report.opened.push(ShippedTask {
            index,
            title: task.title.clone(),
            branch: branch.to_string(),
            commit: commit.to_string(),
            pr_url,
        });
    }
    Ok(report)
}

pub fn render_ship(report: &ShipReport) -> String {
    let short_run = &report.run_id[..8.min(report.run_id.len())];
    let mut lines = vec![
        format!("ship: {short_run}  {}", ship_status(report)),
        format!("source: {}", report.repo_root.display()),
        format!("remote: {} -> {}", report.remote, report.base),
    ];
    for task in &report.opened {
        lines.push(format!(
            "  opened [{}] {} · {} -> {}",
            task.index, task.title, task.branch, task.pr_url
        ));
    }
    for task in &report.skipped {
        lines.push(format!(
            "  skipped [{}] {} · {}",
            task.index, task.title, task.reason
        ));
    }
    for task in &report.failed {
        lines.push(format!(
            "  failed [{}] {} · {}",
            task.index, task.title, task.reason
        ));
    }
    lines.join("\n")
}

pub fn ship_to_json(report: &ShipReport) -> serde_json::Value {
    serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "run_id": report.run_id,
        "repo_root": report.repo_root.display().to_string(),
        "remote": report.remote,
        "base": report.base,
        "status": ship_status(report),
        "opened": report.opened.iter().map(|task| serde_json::json!({
            "task": task.index,
            "title": task.title,
            "branch": task.branch,
            "commit": task.commit,
            "pr_url": task.pr_url,
        })).collect::<Vec<_>>(),
        "skipped": report.skipped.iter().map(|task| serde_json::json!({
            "task": task.index,
            "title": task.title,
            "reason": task.reason,
        })).collect::<Vec<_>>(),
        "failed": report.failed.iter().map(|task| serde_json::json!({
            "task": task.index,
            "title": task.title,
            "reason": task.reason,
        })).collect::<Vec<_>>(),
    })
}

pub fn render_integrate(report: &IntegrateReport) -> String {
    let short_run = &report.run_id[..8.min(report.run_id.len())];
    let mut lines = vec![
        format!(
            "integrate: {short_run}  {}",
            if report.conflict.is_some() {
                "conflict"
            } else {
                "done"
            }
        ),
        format!("source: {}", report.repo_root.display()),
    ];
    for task in &report.merged {
        lines.push(format!(
            "  merged [{}] {} · {} -> {}",
            task.index,
            task.title,
            task.branch,
            &task.merge_commit[..8.min(task.merge_commit.len())]
        ));
    }
    for task in &report.skipped {
        lines.push(format!(
            "  skipped [{}] {} · {}",
            task.index, task.title, task.reason
        ));
    }
    if let Some(conflict) = &report.conflict {
        lines.push(format!(
            "  conflict [{}] {} · {}",
            conflict.index, conflict.title, conflict.branch
        ));
        if !conflict.files.is_empty() {
            lines.push(format!("      files: {}", conflict.files.join(", ")));
        }
    }
    lines.join("\n")
}

pub fn integrate_to_json(report: &IntegrateReport) -> serde_json::Value {
    serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "run_id": report.run_id,
        "repo_root": report.repo_root.display().to_string(),
        "status": if report.conflict.is_some() { "conflict" } else { "done" },
        "merged": report.merged.iter().map(|task| serde_json::json!({
            "task": task.index,
            "title": task.title,
            "branch": task.branch,
            "commit": task.commit,
            "merge_commit": task.merge_commit,
        })).collect::<Vec<_>>(),
        "skipped": report.skipped.iter().map(|task| serde_json::json!({
            "task": task.index,
            "title": task.title,
            "reason": task.reason,
        })).collect::<Vec<_>>(),
        "conflict": report.conflict.as_ref().map(|conflict| serde_json::json!({
            "task": conflict.index,
            "title": conflict.title,
            "branch": conflict.branch,
            "files": conflict.files,
        })),
    })
}

pub fn run(
    goal: &str,
    cwd: &Path,
    home: &Path,
    coordinator_provider: ProviderId,
    coordinator_model: &str,
    max_tasks: usize,
    plan_path: Option<&Path>,
    store: &BatonStore,
    permission_mode: PermissionMode,
) -> Result<RunReport> {
    run_with_progress(
        goal,
        cwd,
        home,
        coordinator_provider,
        coordinator_model,
        max_tasks,
        plan_path,
        store,
        permission_mode,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_with_progress(
    goal: &str,
    cwd: &Path,
    home: &Path,
    coordinator_provider: ProviderId,
    coordinator_model: &str,
    max_tasks: usize,
    plan_path: Option<&Path>,
    store: &BatonStore,
    permission_mode: PermissionMode,
    progress: Option<&ProgressFn<'_>>,
    cancel: Option<&AtomicBool>,
) -> Result<RunReport> {
    run_with_worker_bin_and_progress(
        goal,
        cwd,
        home,
        coordinator_provider,
        coordinator_model,
        max_tasks,
        plan_path,
        store,
        None,
        progress,
        cancel,
        permission_mode,
    )
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn run_with_worker_bin(
    goal: &str,
    cwd: &Path,
    home: &Path,
    coordinator_provider: ProviderId,
    coordinator_model: &str,
    max_tasks: usize,
    plan_path: Option<&Path>,
    store: &BatonStore,
    worker_bin: Option<&Path>,
    permission_mode: PermissionMode,
) -> Result<RunReport> {
    run_with_worker_bin_and_progress(
        goal,
        cwd,
        home,
        coordinator_provider,
        coordinator_model,
        max_tasks,
        plan_path,
        store,
        worker_bin,
        None,
        None,
        permission_mode,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_with_worker_bin_and_progress(
    goal: &str,
    cwd: &Path,
    home: &Path,
    coordinator_provider: ProviderId,
    coordinator_model: &str,
    max_tasks: usize,
    plan_path: Option<&Path>,
    store: &BatonStore,
    worker_bin: Option<&Path>,
    progress: Option<&ProgressFn<'_>>,
    cancel: Option<&AtomicBool>,
    permission_mode: PermissionMode,
) -> Result<RunReport> {
    if max_tasks == 0 {
        anyhow::bail!("max tasks must be at least 1");
    }
    if is_cancelled(cancel) {
        anyhow::bail!(CANCELLED_ERROR);
    }
    let repo_root = worktree::repo_root(cwd).ok();
    let dirty_source = repo_root
        .as_ref()
        .and_then(|root| worktree::is_dirty(root).ok())
        .unwrap_or(false);
    let plan = match plan_path {
        Some(path) => load_plan(path, max_tasks)?,
        None if is_conversational_greeting(goal) => fallback_plan(goal, coordinator_provider),
        None => {
            let available = available_providers(coordinator_provider);
            run_planner(
                goal,
                cwd,
                coordinator_provider,
                coordinator_model,
                max_tasks,
                permission_mode,
                cancel,
                &available,
            )?
        }
    };
    let run_id = uuid::Uuid::new_v4().to_string();
    let summary = plan.summary.clone();
    store.create_orchestration_run(
        &run_id,
        goal,
        coordinator_provider,
        coordinator_model,
        summary.as_deref(),
        &serde_json::to_string(&plan)?,
        dirty_source,
    )?;
    emit(
        progress,
        OrchestrationProgress::PlanReady {
            summary: summary.clone(),
            tasks: plan.tasks.clone(),
        },
    );
    let mut results = std::thread::scope(|scope| {
        let handles = plan
            .tasks
            .iter()
            .enumerate()
            .map(|(index, task)| {
                let fallback_provider = ProviderId::parse(task.provider.trim())
                    .unwrap_or(ProviderId::Codex);
                let fallback = WorkerFallback {
                    index,
                    title: task.title.clone(),
                    provider: fallback_provider,
                    model: resolve_config(home, cwd)
                        .for_provider(fallback_provider)
                        .model
                        .clone(),
                    prompt: task.prompt.clone(),
                };
                let run_id = run_id.clone();
                let home = home.to_path_buf();
                let cwd = cwd.to_path_buf();
                let repo_root = repo_root.clone();
                let worker_bin = worker_bin.map(Path::to_path_buf);
                let handle = scope.spawn(move || {
                    run_worker(
                        task,
                        index,
                        &run_id,
                        &home,
                        &cwd,
                        repo_root.as_deref(),
                        worker_bin.as_deref(),
                        progress,
                        cancel,
                        permission_mode,
                    )
                });
                (handle, fallback)
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|(handle, fallback)| match handle.join() {
                Ok(result) => result,
                Err(_) => {
                    let (index, result) = panicked_worker(&fallback);
                    emit(
                        progress,
                        OrchestrationProgress::WorkerFinished {
                            index: index + 1,
                            title: result.title.clone(),
                            status: result.status.clone(),
                            commit: None,
                            error: result.error.clone(),
                        },
                    );
                    (index, result)
                }
            })
            .collect::<Vec<_>>()
    });
    results.sort_by_key(|(index, _)| *index);
    let mut tasks = Vec::new();
    for (position, (_, task)) in results.into_iter().enumerate() {
        store.add_orchestration_task(
            &task.id,
            &run_id,
            (position + 1) as i64,
            &task.title,
            task.provider,
            &task.model,
            &task.prompt,
            task.worktree.as_ref().map(|worktree| worktree.path.to_string_lossy().to_string()).as_deref(),
            task.worktree.as_ref().map(|worktree| worktree.branch.as_str()),
            task.commit.as_deref(),
            task.thread_id.as_deref(),
            &task.status,
            task.text.as_deref(),
            task.error.as_deref(),
            &task.created_at,
            Some(&task.finished_at),
        )?;
        tasks.push(task);
    }
    let completed = tasks.iter().filter(|task| task.status == "completed").count();
    let cancelled = tasks.iter().any(|task| task.status == "cancelled");
    let status = if cancelled {
        "cancelled"
    } else if completed == tasks.len() {
        "completed"
    } else if completed == 0 {
        "failed"
    } else {
        "partial"
    };
    store.finish_orchestration_run(&run_id, status)?;
    Ok(RunReport {
        run_id,
        goal: goal.to_string(),
        coordinator_provider,
        coordinator_model: coordinator_model.to_string(),
        summary,
        plan,
        repo_root,
        dirty_source,
        status: status.to_string(),
        archived_at: None,
        tasks,
    })
}

pub fn render(report: &RunReport) -> String {
    let mut lines = vec![
        format!(
            "run: {}  {}{}",
            &report.run_id[..8.min(report.run_id.len())],
            report.status,
            if report.archived_at.is_some() { " (archived)" } else { "" }
        ),
        format!("goal: {}", report.goal),
        format!("coordinator: {}/{}", report.coordinator_provider, report.coordinator_model),
        format!(
            "source: {}{}",
            report
                .repo_root
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "not a git repository".to_string()),
            if report.dirty_source { " (dirty; worktrees branch from HEAD)" } else { "" }
        ),
    ];
    if let Some(summary) = &report.summary {
        lines.push(format!("plan: {summary}"));
    }
    lines.push(format!("workers: {}", report.tasks.len()));
    for (index, task) in report.tasks.iter().enumerate() {
        lines.push(format!(
            "  [{}] {} · {}/{} · {}{}",
            index + 1,
            task.title,
            task.provider,
            task.model,
            task.status,
            task.thread_id
                .as_ref()
                .map(|thread| format!(" · thread {}", &thread[..8.min(thread.len())]))
                .unwrap_or_default()
        ));
        if let Some(worktree) = &task.worktree {
            lines.push(format!(
                "      worktree: {} ({})",
                worktree.path.display(),
                worktree.branch
            ));
        }
        if let Some(commit) = &task.commit {
            lines.push(format!(
                "      commit: {}",
                &commit[..8.min(commit.len())]
            ));
        }
        if let Some(text) = &task.text {
            let preview: Vec<&str> = text.lines().take(3).collect();
            if !preview.is_empty() {
                lines.push(format!("      {}", preview.join(" ")));
            }
        }
        if let Some(error) = &task.error {
            lines.push(format!("      error: {error}"));
        }
    }
    lines.join("\n")
}

pub fn to_json(report: &RunReport) -> serde_json::Value {
    let tasks = report
        .tasks
        .iter()
        .map(|task| {
            serde_json::json!({
                "id": task.id,
                "title": task.title,
                "provider": task.provider.as_str(),
                "model": task.model,
                "prompt": task.prompt,
                "status": task.status,
                "thread_id": task.thread_id,
                "text": task.text,
                "error": task.error,
                "commit": task.commit,
                "worktree": task.worktree.as_ref().map(|worktree| serde_json::json!({
                    "path": worktree.path.display().to_string(),
                    "branch": worktree.branch,
                    "base_ref": worktree.base_ref,
                    "shared_paths": worktree.shared_paths,
                })),
                "pr_url": task.pr_url,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "run_id": report.run_id,
        "goal": report.goal,
        "status": report.status,
        "coordinator": {
            "provider": report.coordinator_provider.as_str(),
            "model": report.coordinator_model,
        },
        "summary": report.summary,
        "plan": report.plan,
        "repo_root": report.repo_root.as_ref().map(|path| path.display().to_string()),
        "dirty_source": report.dirty_source,
        "archived_at": report.archived_at,
        "tasks": tasks,
    })
}

pub fn report_from_rows(store: &BatonStore, cwd: &Path, run_id: &str) -> Result<RunReport> {
    let run_id = resolve_run_id(store, run_id)?;
    let run = store
        .find_orchestration_runs(&run_id)?
        .into_iter()
        .find(|run| run.id == run_id)
        .ok_or_else(|| anyhow::anyhow!("no orchestration run matches {run_id}"))?;
    let plan: Plan = serde_json::from_str(&run.plan_json).unwrap_or_else(|_| Plan {
        summary: run.summary.clone(),
        tasks: Vec::new(),
    });
    let repo_root = worktree::repo_root(cwd).ok();
    let tasks = store
        .list_orchestration_tasks(&run.id)?
        .into_iter()
        .map(|task| {
            let worktree = match (task.worktree_path.as_deref(), task.branch.as_deref()) {
                (Some(path), Some(branch)) => Some(WorktreeInfo {
                    repo_root: repo_root.clone().unwrap_or_else(|| cwd.to_path_buf()),
                    path: PathBuf::from(path),
                    branch: branch.to_string(),
                    base_ref: "HEAD".to_string(),
                    shared_paths: Vec::new(),
                }),
                _ => None,
            };
            WorkerResult {
                id: task.id,
                title: task.title,
                provider: task.provider,
                model: task.model,
                prompt: task.prompt,
                worktree,
                commit: task.commit_sha,
                thread_id: task.thread_id,
                status: task.status,
                text: task.text,
                error: task.error,
                pr_url: task.pr_url,
                created_at: task.created_at,
                finished_at: task.finished_at.unwrap_or_default(),
            }
        })
        .collect();
    Ok(RunReport {
        run_id: run.id,
        goal: run.goal,
        coordinator_provider: run.coordinator_provider,
        coordinator_model: run.coordinator_model,
        summary: run.summary,
        plan,
        repo_root,
        dirty_source: run.dirty_source,
        status: run.status,
        archived_at: run.archived_at,
        tasks,
    })
}

pub fn run_rows(store: &BatonStore, limit: usize) -> Result<Vec<OrchestrationRunRow>> {
    store.list_orchestration_runs(limit)
}

pub fn task_rows(store: &BatonStore, run_id: &str) -> Result<Vec<OrchestrationTaskRow>> {
    store.list_orchestration_tasks(run_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::BatonStore;

    #[test]
    fn empty_planner_plan_falls_back_to_one_worker() {
        let plan = fallback_plan("hi", ProviderId::Claude);
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].provider, "claude");
        assert_eq!(plan.tasks[0].prompt, "hi");
        assert!(plan_is_empty(&anyhow::anyhow!("planner returned no tasks")));
        assert!(!plan_is_empty(&anyhow::anyhow!("some other failure")));
    }

    #[test]
    fn planner_failures_fall_back_unless_the_user_cancelled() {
        let plan = plan_after_planner_failure(
            "fix the test",
            ProviderId::Codex,
            &anyhow::anyhow!("planner returned no tasks"),
        )
        .unwrap();
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].provider, "codex");
        assert_eq!(plan.tasks[0].prompt, "fix the test");

        let cancelled = plan_after_planner_failure(
            "fix the test",
            ProviderId::Codex,
            &anyhow::anyhow!(CANCELLED_ERROR),
        )
        .unwrap_err();
        assert_eq!(cancelled.to_string(), CANCELLED_ERROR);
    }

    #[test]
    fn greetings_use_a_direct_plan() {
        assert!(is_conversational_greeting("hi"));
        assert!(is_conversational_greeting(" hello! "));
        assert!(is_conversational_greeting("thanks."));
        assert!(!is_conversational_greeting("hi, fix the tests"));
        assert!(!is_conversational_greeting("fix the bug"));
    }

    #[test]
    fn planner_prompt_asks_for_the_smallest_plan_and_lists_available_harnesses() {
        let prompt = planner_prompt(
            "fix the bug",
            Path::new("/repo"),
            4,
            &[ProviderId::Codex, ProviderId::Claude],
            ProviderId::Codex,
        );
        assert!(prompt.contains("smallest number of tasks"), "{prompt}");
        assert!(prompt.contains("exactly one task"), "{prompt}");
        assert!(prompt.contains("codex, claude"), "{prompt}");
        assert!(prompt.contains("Prefer codex"), "{prompt}");
        assert!(prompt.contains("Never ask a worker to coordinate"), "{prompt}");
    }

    #[test]
    fn plan_validation_rejects_uninstalled_harnesses() {
        let plan = parse_plan(
            r#"{"tasks":[{"title":"audit","provider":"grok","prompt":"do it"}]}"#,
            4,
        )
        .unwrap();
        let error = validate_plan_providers(&plan, &[ProviderId::Codex, ProviderId::Claude])
            .unwrap_err();
        assert!(
            error.to_string().contains("not installed"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parses_fenced_plan_and_caps_tasks() {
        let text = "```json\n{\"summary\":\"split\",\"tasks\":[{\"title\":\"a\",\"provider\":\"codex\",\"prompt\":\"do a\"},{\"title\":\"b\",\"provider\":\"claude\",\"prompt\":\"do b\"}]}\n```";
        let plan = parse_plan(text, 1).unwrap();
        assert_eq!(plan.summary.as_deref(), Some("split"));
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].provider, "codex");
    }

    #[test]
    fn rejects_unknown_provider() {
        let text = r#"{"tasks":[{"title":"a","provider":"not-a-provider","prompt":"do a"}]}"#;
        assert!(parse_plan(text, 4).is_err());
    }

    fn fake_gh(root: &Path) -> PathBuf {
        let path = root.join("fake-gh");
        let log = root.join("gh.log");
        let script = format!(
            r#"#!/bin/sh
set -eu
LOG='{log}'
printf '%s\n' "$*" >> "$LOG"
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
  head=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "--head" ]; then head="$arg"; fi
    prev="$arg"
  done
  slug=$(printf '%s' "$head" | tr '/' '-')
  printf 'https://github.com/example/repo/pull/%s\n' "$slug"
  exit 0
fi
printf 'unexpected gh args: %s\n' "$*" >&2
exit 1
"#,
            log = log.display()
        );
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn fake_gh_with_existing_pr(root: &Path, url: &str) -> PathBuf {
        let path = root.join("fake-gh-existing");
        let script = format!(
            r#"#!/bin/sh
set -eu
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  printf '%s\n' '[{{"url":"{url}"}}]'
  exit 0
fi
printf 'gh should not create a PR when one already exists\n' >&2
exit 1
"#
        );
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn git_at(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn fake_worker(root: &Path) -> PathBuf {
        let path = root.join("fake-baton");
        std::fs::write(
            &path,
            "#!/bin/sh\ncd \"$2\" || exit 1\nprintf 'edited by worker\\n' > worker-output.txt\nprintf '%s\\n' '{\"schema_version\":1,\"thread_id\":\"fake-thread\",\"ok\":true,\"phase\":\"completed\",\"error\":null,\"text\":\"FAKE_OK\"}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn worker_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let json = "printf '%s\\n' '{\"schema_version\":1,\"thread_id\":\"fake-thread\",\"ok\":true,\"phase\":\"completed\",\"error\":null,\"text\":\"FAKE_OK\"}'\n";
        let script = format!("#!/bin/sh\ncd \"$2\" || exit 1\n{body}\n{json}");
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn run_fixture(plan: &str, worker_body: &str) -> (tempfile::TempDir, tempfile::TempDir, BatonStore, RunReport) {
        run_fixture_with_progress(plan, worker_body, None)
    }

    fn run_fixture_with_progress(
        plan: &str,
        worker_body: &str,
        progress: Option<&ProgressFn<'_>>,
    ) -> (tempfile::TempDir, tempfile::TempDir, BatonStore, RunReport) {
        run_fixture_with_control(plan, worker_body, progress, None)
    }

    fn run_fixture_with_control(
        plan: &str,
        worker_body: &str,
        progress: Option<&ProgressFn<'_>>,
        cancel: Option<std::sync::Arc<AtomicBool>>,
    ) -> (tempfile::TempDir, tempfile::TempDir, BatonStore, RunReport) {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).current_dir(repo.path()).output().unwrap();
            assert!(output.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&output.stderr));
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(repo.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let home = tempfile::tempdir().unwrap();
        let worker = worker_script(home.path(), "fake-worker", worker_body);
        let plan_path = home.path().join("plan.json");
        std::fs::write(&plan_path, plan).unwrap();
        let store = BatonStore::memory().unwrap();
        let cancel_watcher = cancel.clone().map(|cancel| {
            let home = home.path().to_path_buf();
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now()
                    + std::time::Duration::from_secs(20);
                while std::time::Instant::now() < deadline {
                    if let Ok(runs) = std::fs::read_dir(home.join("worktrees")) {
                        for run in runs.flatten() {
                            if let Ok(tasks) = std::fs::read_dir(run.path()) {
                                for task in tasks.flatten() {
                                    if task.path().join("worker.pid").is_file() {
                                        cancel.store(true, Ordering::Relaxed);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                cancel.store(true, Ordering::Relaxed);
            })
        });
        let report = run_with_worker_bin_and_progress(
            "test goal",
            repo.path(),
            home.path(),
            ProviderId::Codex,
            "test-model",
            4,
            Some(&plan_path),
            &store,
            Some(&worker),
            progress,
            cancel.as_deref(),
            PermissionMode::Default,
        )
        .unwrap();
        if let Some(watcher) = cancel_watcher {
            watcher.join().unwrap();
        }
        (repo, home, store, report)
    }

    #[test]
    fn panic_fallback_keeps_task_identity_and_position() {
        let fallback = WorkerFallback {
            index: 2,
            title: "second task".to_string(),
            provider: ProviderId::Claude,
            model: "claude-test".to_string(),
            prompt: "do two".to_string(),
        };
        let (index, result) = panicked_worker(&fallback);
        assert_eq!(index, 2, "position survives a panic");
        assert_eq!(result.title, "second task");
        assert_eq!(result.provider, ProviderId::Claude);
        assert_eq!(result.model, "claude-test");
        assert_eq!(result.prompt, "do two");
        assert_eq!(result.status, "failed");
        assert!(result.error.as_deref().unwrap().contains("panicked"));
        assert!(result.worktree.is_none());
        assert!(result.commit.is_none());
        assert!(result.thread_id.is_none());
    }

    #[test]
    fn progress_lines_are_readable() {
        let plan = render_progress(&OrchestrationProgress::PlanReady {
            summary: Some("two workers".to_string()),
            tasks: vec![
                PlanTask {
                    title: "a".to_string(),
                    provider: "codex".to_string(),
                    prompt: "a".to_string(),
                },
                PlanTask {
                    title: "b".to_string(),
                    provider: "claude".to_string(),
                    prompt: "b".to_string(),
                },
            ],
        });
        assert_eq!(plan, "plan: 2 worker(s) · two workers");
        let started = render_progress(&OrchestrationProgress::WorkerStarted {
            index: 1,
            title: "first".to_string(),
            provider: ProviderId::Codex,
            model: "gpt-test".to_string(),
            worktree: None,
        });
        assert_eq!(started, "[1] running first · codex/gpt-test");
        let finished = render_progress(&OrchestrationProgress::WorkerFinished {
            index: 1,
            title: "first".to_string(),
            status: "completed".to_string(),
            commit: Some("0123456789abcdef".to_string()),
            error: None,
        });
        assert_eq!(finished, "[1] completed first · commit 01234567");
    }

    #[test]
    fn archives_a_finished_run_idempotently_and_refuses_a_running_run() {
        let store = BatonStore::memory().unwrap();
        store
            .create_orchestration_run(
                "abcdef1234567890",
                "goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
            )
            .unwrap();
        store
            .finish_orchestration_run("abcdef1234567890", "completed")
            .unwrap();
        let archived = archive(&store, "abcdef12").unwrap();
        assert_eq!(archived, "abcdef1234567890");
        let runs = store.list_orchestration_runs(10).unwrap();
        assert!(runs[0].archived_at.is_some());
        let first = runs[0].archived_at.clone();
        archive(&store, "abcdef12").unwrap();
        let runs = store.list_orchestration_runs(10).unwrap();
        assert_eq!(runs[0].archived_at, first, "a second archive keeps the first timestamp");

        store
            .create_orchestration_run(
                "run-running",
                "goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
            )
            .unwrap();
        let error = archive(&store, "run-running").unwrap_err();
        assert!(error.to_string().contains("still running"), "{error}");
    }

    #[test]
    fn stored_run_reconstructs_a_report() {
        let plan = r#"{"summary":"one worker","tasks":[{"title":"first task","provider":"codex","prompt":"do one"}]}"#;
        let (repo, _home, store, report) =
            run_fixture(plan, "printf 'edited by worker\n' > worker-output.txt");
        let stored = report_from_rows(&store, repo.path(), &report.run_id).unwrap();
        assert_eq!(stored.run_id, report.run_id);
        assert_eq!(stored.goal, report.goal);
        assert_eq!(stored.status, "completed");
        assert_eq!(stored.tasks.len(), 1);
        assert_eq!(stored.tasks[0].title, "first task");
        assert_eq!(stored.tasks[0].thread_id.as_deref(), Some("fake-thread"));
        assert!(stored.tasks[0].worktree.is_some());
        assert_eq!(stored.dirty_source, report.dirty_source);
    }

    #[test]
    fn cancellation_stops_the_worker_group_and_marks_the_run_cancelled() {
        let plan = r#"{"summary":"one worker","tasks":[{"title":"long task","provider":"codex","prompt":"wait"}]}"#;
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let (_repo, _home, store, report) = run_fixture_with_control(
            plan,
            "echo $$ > worker.pid; sleep 30",
            None,
            Some(cancel),
        );
        assert_eq!(report.status, "cancelled");
        assert_eq!(report.tasks[0].status, "cancelled");
        let worktree = report.tasks[0]
            .worktree
            .as_ref()
            .expect("the worker had time to create its worktree");
        let pid: u32 = std::fs::read_to_string(worktree.path.join("worker.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(!alive, "the worker process group is gone");
        let tasks = store.list_orchestration_tasks(&report.run_id).unwrap();
        assert_eq!(tasks[0].status, "cancelled");
        let runs = store.list_orchestration_runs(10).unwrap();
        assert_eq!(runs[0].status, "cancelled");
    }

    #[test]
    fn emits_plan_started_and_finished_progress() {
        let plan = r#"{"summary":"two workers","tasks":[{"title":"first task","provider":"codex","prompt":"do one"},{"title":"second task","provider":"codex","prompt":"do two"}]}"#;
        let events = std::sync::Mutex::new(Vec::new());
        let progress = |event: OrchestrationProgress| {
            events.lock().unwrap().push(event);
        };
        let (_repo, _home, _store, report) = run_fixture_with_progress(
            plan,
            "printf 'edited by worker\n' > worker-output.txt",
            Some(&progress as &ProgressFn<'_>),
        );
        assert_eq!(report.tasks.len(), 2);
        let events = events.into_inner().unwrap();
        match &events[0] {
            OrchestrationProgress::PlanReady { tasks, .. } => assert_eq!(tasks.len(), 2),
            other => panic!("first progress event was {other:?}"),
        }
        let started: Vec<usize> = events
            .iter()
            .filter_map(|event| match event {
                OrchestrationProgress::WorkerStarted { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        let finished: Vec<usize> = events
            .iter()
            .filter_map(|event| match event {
                OrchestrationProgress::WorkerFinished { index, status, .. }
                    if status == "completed" =>
                {
                    Some(*index)
                }
                _ => None,
            })
            .collect();
        assert_eq!(started.len(), 2);
        assert!(started.contains(&1) && started.contains(&2));
        assert_eq!(finished.len(), 2);
        assert!(finished.contains(&1) && finished.contains(&2));
    }

    #[test]
    fn integrates_worker_branches_with_selection_and_idempotency() {
        let plan = r#"{"summary":"two workers","tasks":[{"title":"first task","provider":"codex","prompt":"do one"},{"title":"second task","provider":"codex","prompt":"do two"}]}"#;
        let (repo, _home, store, report) =
            run_fixture(plan, "printf 'edited by worker\\n' > worker-output.txt");
        assert_eq!(report.tasks.len(), 2);

        let selected = integrate(&store, repo.path(), &report.run_id, &[2]).unwrap();
        assert!(selected.conflict.is_none());
        assert_eq!(selected.merged.len(), 1);
        assert_eq!(selected.merged[0].index, 2);

        let rest = integrate(&store, repo.path(), &report.run_id, &[]).unwrap();
        assert!(rest.conflict.is_none());
        assert_eq!(rest.merged.len(), 1);
        assert_eq!(rest.merged[0].index, 1);
        assert_eq!(rest.skipped.len(), 1);
        assert_eq!(rest.skipped[0].reason, "already merged");
        assert_eq!(
            std::fs::read_to_string(repo.path().join("worker-output.txt")).unwrap(),
            "edited by worker\n"
        );

        let tasks = store.list_orchestration_tasks(&report.run_id).unwrap();
        assert!(tasks.iter().all(|task| task.merged_at.is_some()));

        let again = integrate(&store, repo.path(), &report.run_id, &[]).unwrap();
        assert!(again.merged.is_empty());
        assert_eq!(again.skipped.len(), 2);
    }

    #[test]
    fn aborts_on_conflict_and_records_which_task_stopped() {
        let plan = r#"{"summary":"two workers","tasks":[{"title":"first task","provider":"codex","prompt":"do one"},{"title":"second task","provider":"codex","prompt":"do two"}]}"#;
        let body = "if [ \"$(basename \"$PWD\")\" = \"second-task\" ]; then printf 'two\\n' > worker-output.txt; else printf 'one\\n' > worker-output.txt; fi";
        let (repo, _home, store, report) = run_fixture(plan, body);

        let integrated = integrate(&store, repo.path(), &report.run_id, &[]).unwrap();
        assert_eq!(integrated.merged.len(), 1);
        let conflict = integrated.conflict.expect("second merge conflicts");
        assert_eq!(conflict.index, 2);
        assert!(conflict.files.contains(&"worker-output.txt".to_string()));
        assert_eq!(
            std::fs::read_to_string(repo.path().join("worker-output.txt")).unwrap(),
            "one\n",
            "the successful merge stays and the conflicted merge is aborted"
        );
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            "merge --abort leaves a clean source checkout"
        );
        let tasks = store.list_orchestration_tasks(&report.run_id).unwrap();
        assert!(tasks[0].merged_at.is_some());
        assert!(tasks[1].merged_at.is_none());
    }

    #[test]
    fn refuses_to_integrate_into_a_dirty_source() {
        let plan = r#"{"summary":"one worker","tasks":[{"title":"first task","provider":"codex","prompt":"do one"}]}"#;
        let (repo, _home, store, report) =
            run_fixture(plan, "printf 'edited by worker\\n' > worker-output.txt");
        std::fs::write(repo.path().join("dirty.txt"), "uncommitted\n").unwrap();
        let error = integrate(&store, repo.path(), &report.run_id, &[]).unwrap_err();
        assert!(
            error.to_string().contains("uncommitted changes"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn runs_plan_in_isolated_worktrees_and_records_rows() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).current_dir(repo.path()).output().unwrap();
            assert!(output.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&output.stderr));
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(repo.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let home = tempfile::tempdir().unwrap();
        let plan_path = repo.path().join("plan.json");
        std::fs::write(
            &plan_path,
            r#"{"summary":"one worker","tasks":[{"title":"worker one","provider":"codex","prompt":"do it"}]}"#,
        )
        .unwrap();
        let store = BatonStore::memory().unwrap();
        let worker = fake_worker(repo.path());
        let report = run_with_worker_bin(
            "test goal",
            repo.path(),
            home.path(),
            ProviderId::Codex,
            "test-model",
            4,
            Some(&plan_path),
            &store,
            Some(&worker),
            PermissionMode::Default,
        )
        .unwrap();
        assert_eq!(report.status, "completed");
        assert_eq!(report.tasks.len(), 1);
        assert_eq!(report.tasks[0].text.as_deref(), Some("FAKE_OK"));
        assert!(report.tasks[0].worktree.as_ref().unwrap().path.is_dir());
        assert_eq!(store.list_orchestration_runs(10).unwrap().len(), 1);
        let tasks = store.list_orchestration_tasks(&report.run_id).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, "completed");
        assert_eq!(tasks[0].thread_id.as_deref(), Some("fake-thread"));
        let commit = report.tasks[0]
            .commit
            .as_ref()
            .expect("worker changes are committed");
        assert_eq!(commit.len(), 40);
        assert_eq!(tasks[0].commit_sha.as_deref(), Some(commit.as_str()));
        let worktree = report.tasks[0].worktree.as_ref().unwrap();
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&worktree.path)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            "worker changes are committed and the worktree is clean"
        );
        let log = Command::new("git")
            .args(["log", "-1", "--pretty=%s"])
            .current_dir(&worktree.path)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&log.stdout).trim(),
            "baton worker: worker one"
        );
        assert!(worktree.path.join("worker-output.txt").is_file());
    }
    #[test]
    fn ships_completed_worker_branches_idempotently() {
        let plan = r#"{"summary":"two workers","tasks":[{"title":"first task","provider":"codex","prompt":"do one"},{"title":"second task","provider":"codex","prompt":"do two"}]}"#;
        let body = "if [ \"$(basename \"$PWD\")\" = \"first-task\" ]; then printf 'one\\n' > one.txt; else printf 'two\\n' > two.txt; fi";
        let (repo, home, store, report) = run_fixture(plan, body);
        assert_eq!(report.tasks.len(), 2);

        let remote = home.path().join("remote.git");
        let init = Command::new("git")
            .args(["init", "--bare", "-q"])
            .arg(&remote)
            .status()
            .unwrap();
        assert!(init.success());
        git_at(
            repo.path(),
            &["remote", "add", "origin", &remote.to_string_lossy()],
        );
        let gh = fake_gh(home.path());
        let short_run = &report.run_id[..8];

        let shipped = open_prs_with_gh(&store, repo.path(), short_run, &[], Some(&gh)).unwrap();
        assert_eq!(ship_status(&shipped), "done");
        assert_eq!(shipped.opened.len(), 2, "both worker branches open a PR");
        assert_eq!(shipped.remote, "origin");
        let tasks = store.list_orchestration_tasks(&report.run_id).unwrap();
        assert!(tasks.iter().all(|task| task.pr_url.is_some()));
        for task in &tasks {
            let branch = task.branch.as_deref().unwrap();
            let remote_head = git_at(
                repo.path(),
                &[
                    "ls-remote",
                    "--heads",
                    remote.to_string_lossy().as_ref(),
                    branch,
                ],
            );
            assert!(
                !remote_head.is_empty(),
                "branch {branch} reached the remote"
            );
        }
        let log = std::fs::read_to_string(home.path().join("gh.log")).unwrap();
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("pr create"))
                .count(),
            2,
            "one PR create per eligible worker: {log}"
        );
        assert!(
            log.contains("--base"),
            "the current branch is the PR base: {log}"
        );

        let again = open_prs_with_gh(&store, repo.path(), short_run, &[], Some(&gh)).unwrap();
        assert!(again.opened.is_empty());
        assert_eq!(again.skipped.len(), 2);
        assert!(
            again
                .skipped
                .iter()
                .all(|task| task.reason.contains("PR already recorded")),
            "retry is idempotent: {:?}",
            again.skipped
        );
        let log = std::fs::read_to_string(home.path().join("gh.log")).unwrap();
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("pr create"))
                .count(),
            2,
            "a retry must not create duplicate PRs"
        );
    }

    #[test]
    fn ship_skips_failed_and_changeless_workers() {
        let plan = r#"{"summary":"three workers","tasks":[{"title":"first task","provider":"codex","prompt":"do one"},{"title":"second task","provider":"codex","prompt":"do two"},{"title":"third task","provider":"codex","prompt":"do three"}]}"#;
        let body = "name=$(basename \"$PWD\"); if [ \"$name\" = \"third-task\" ]; then exit 1; fi; if [ \"$name\" = \"first-task\" ]; then printf 'one\\n' > one.txt; fi";
        let (repo, home, store, report) = run_fixture(plan, body);
        let remote = home.path().join("remote.git");
        assert!(Command::new("git")
            .args(["init", "--bare", "-q"])
            .arg(&remote)
            .status()
            .unwrap()
            .success());
        git_at(
            repo.path(),
            &["remote", "add", "origin", &remote.to_string_lossy()],
        );
        let gh = fake_gh(home.path());
        let shipped =
            open_prs_with_gh(&store, repo.path(), &report.run_id, &[], Some(&gh)).unwrap();
        assert_eq!(shipped.opened.len(), 1);
        assert_eq!(shipped.skipped.len(), 2);
        assert!(
            shipped
                .skipped
                .iter()
                .any(|task| task.reason == "worker produced no commit"),
            "a completed worker with no changes is not shippable: {:?}",
            shipped.skipped
        );
        assert!(
            shipped
                .skipped
                .iter()
                .any(|task| task.reason == "worker status is failed"),
            "a failed worker is not shippable: {:?}",
            shipped.skipped
        );
    }

    #[test]
    fn ship_reports_a_missing_branch_and_rejects_a_bad_selection() {
        let plan = r#"{"summary":"one worker","tasks":[{"title":"first task","provider":"codex","prompt":"do one"}]}"#;
        let (repo, home, store, report) = run_fixture(plan, "printf 'one\\n' > one.txt");
        let remote = home.path().join("remote.git");
        assert!(Command::new("git")
            .args(["init", "--bare", "-q"])
            .arg(&remote)
            .status()
            .unwrap()
            .success());
        git_at(
            repo.path(),
            &["remote", "add", "origin", &remote.to_string_lossy()],
        );
        let task = store
            .list_orchestration_tasks(&report.run_id)
            .unwrap()
            .remove(0);
        let branch = task.branch.clone().unwrap();
        let worktree = task.worktree_path.clone().unwrap();
        assert!(Command::new("git")
            .args(["worktree", "remove", "--force", &worktree])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["branch", "-D", &branch])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        let gh = fake_gh(home.path());
        let shipped =
            open_prs_with_gh(&store, repo.path(), &report.run_id, &[], Some(&gh)).unwrap();
        assert!(shipped.opened.is_empty());
        assert_eq!(shipped.skipped.len(), 1);
        assert!(shipped.skipped[0].reason.contains("no longer exists"));
        let error =
            open_prs_with_gh(&store, repo.path(), &report.run_id, &[2], Some(&gh)).unwrap_err();
        assert!(error.to_string().contains("out of range"), "{error}");
    }

    #[test]
    fn ship_links_an_existing_pr_instead_of_creating_a_duplicate() {
        let plan = r#"{"summary":"one worker","tasks":[{"title":"first task","provider":"codex","prompt":"do one"}]}"#;
        let (repo, home, store, report) =
            run_fixture(plan, "printf 'one\n' > one.txt");
        let remote = home.path().join("remote.git");
        assert!(
            Command::new("git")
                .args(["init", "--bare", "-q"])
                .arg(&remote)
                .status()
                .unwrap()
                .success()
        );
        git_at(
            repo.path(),
            &["remote", "add", "origin", &remote.to_string_lossy()],
        );
        let existing = "https://github.com/example/repo/pull/9";
        let gh = fake_gh_with_existing_pr(home.path(), existing);
        let shipped =
            open_prs_with_gh(&store, repo.path(), &report.run_id, &[], Some(&gh)).unwrap();
        assert!(shipped.opened.is_empty());
        assert_eq!(shipped.skipped.len(), 1);
        assert!(
            shipped.skipped[0].reason.contains(existing),
            "linked PR is reported: {:?}",
            shipped.skipped
        );
        let tasks = store.list_orchestration_tasks(&report.run_id).unwrap();
        assert_eq!(tasks[0].pr_url.as_deref(), Some(existing));
    }

    #[test]
    fn ship_requires_a_remote() {
        let plan = r#"{"summary":"one worker","tasks":[{"title":"first task","provider":"codex","prompt":"do one"}]}"#;
        let (repo, home, store, report) = run_fixture(plan, "printf 'one\\n' > one.txt");
        let gh = fake_gh(home.path());
        let error =
            open_prs_with_gh(&store, repo.path(), &report.run_id, &[], Some(&gh)).unwrap_err();
        assert!(error.to_string().contains("no git remote"), "{error}");
    }
}
