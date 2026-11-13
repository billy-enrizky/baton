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
        let text = run_coordinator(
            &prompt,
            cwd,
            provider,
            model,
            permission_mode,
            cancel,
        )?;
        if std::env::var_os("BATON_ORCHESTRATE_DEBUG").is_some() {
            eprintln!("[orchestrate] planner text (attempt {}): {text}", attempt + 1);
        }
        match parse_plan(&text, max_tasks)
            .and_then(|plan| validate_plan_providers(&plan, available).map(|()| plan))
        {
            Ok(plan) => return Ok(plan),
            Err(error) => {
                if plan_is_empty(&error) {
                    return Ok(fallback_plan(goal, provider));
                }
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("planner failed")))
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
    let repo_root = worktree::repo_root(cwd)?;
    if worktree::is_dirty(&repo_root)? {
        anyhow::bail!(
            "source checkout has uncommitted changes; commit or stash them before integrating"
        );
    }
    let tasks = store.list_orchestration_tasks(run_id)?;
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
        run_id: run_id.to_string(),
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
        tasks,
    })
}

pub fn render(report: &RunReport) -> String {
    let mut lines = vec![
        format!("run: {}  {}", &report.run_id[..8.min(report.run_id.len())], report.status),
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
                })),
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
        "tasks": tasks,
    })
}

pub fn report_from_rows(store: &BatonStore, cwd: &Path, run_id: &str) -> Result<RunReport> {
    let run = store
        .list_orchestration_runs(200)?
        .into_iter()
        .find(|run| run.id.starts_with(run_id))
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
}
