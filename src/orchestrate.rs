//! Minimal Orca-style orchestration: a coordinator plans independent tasks,
//! each worker runs in its own git worktree through the normal `baton exec`
//! path, and results are recorded durably.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::resolve_config;
use crate::roles::{RoleId, RoleRegistry, RoleSpec};
use crate::store::{BatonStore, OrchestrationRunRow, OrchestrationTaskRow};
use crate::types::{PermissionMode, ProviderId, TurnPhase};
use crate::worktree::{self, WorktreeInfo};

pub const SCHEMA_VERSION: u32 = 1;
pub const CANCELLED_ERROR: &str = "orchestration cancelled";
pub const MAX_TASKS: usize = 16;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub summary: Option<String>,
    pub tasks: Vec<PlanTask>,
    /// Provider, model, and effort defaults were already resolved into these
    /// tasks. A saved plan keeps this marker so a preview and its run use the
    /// same settings instead of applying another role after reload.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub role_defaults_applied: bool,
}

#[derive(Debug, Clone)]
pub struct WorkerSnapshot {
    pub base_ref: String,
    pub tree: String,
}

#[derive(Debug, Clone)]
pub struct WorkerResult {
    pub id: String,
    pub title: String,
    pub provider: ProviderId,
    pub model: String,
    pub effort: Option<String>,
    pub prompt: String,
    pub worktree: Option<WorktreeInfo>,
    pub snapshot: Option<WorkerSnapshot>,
    pub commit: Option<String>,
    pub thread_id: Option<String>,
    pub status: String,
    pub text: Option<String>,
    pub error: Option<String>,
    /// "approve", "request_changes", "failed", or "skipped". None means the
    /// run predates the reviewer phase.
    pub review_status: Option<String>,
    pub review_text: Option<String>,
    pub reviewed_at: Option<String>,
    pub pr_url: Option<String>,
    pub merged_at: Option<String>,
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
            "[{index}] {} {title}{}{}",
            if status == "needs_input" {
                "waiting for input"
            } else {
                status.as_str()
            },
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

/// One JSONL row for `baton orchestrate --stream-json`.
pub fn progress_json(event: &OrchestrationProgress) -> serde_json::Value {
    let mut value = match event {
        OrchestrationProgress::PlanReady { summary, tasks } => serde_json::json!({
            "type": "plan",
            "summary": summary,
            "tasks": tasks.iter().map(|task| serde_json::json!({
                "title": task.title,
                "provider": task.provider,
                "model": task.model,
                "effort": task.effort,
                "prompt": task.prompt,
            })).collect::<Vec<_>>(),
        }),
        OrchestrationProgress::WorkerStarted {
            index,
            title,
            provider,
            model,
            worktree,
        } => serde_json::json!({
            "type": "worker_started",
            "index": index,
            "title": title,
            "provider": provider.as_str(),
            "model": model,
            "worktree": worktree.as_ref().map(|path| path.display().to_string()),
        }),
        OrchestrationProgress::WorkerFinished {
            index,
            title,
            status,
            commit,
            error,
        } => serde_json::json!({
            "type": "worker_finished",
            "index": index,
            "title": title,
            "status": status,
            "commit": commit,
            "error": error,
        }),
    };
    value["schema_version"] = serde_json::json!(SCHEMA_VERSION);
    value
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

fn default_worker_provider(
    available: &[ProviderId],
    coordinator: ProviderId,
    implementor: ProviderId,
) -> ProviderId {
    if available.contains(&implementor) {
        implementor
    } else {
        coordinator
    }
}

fn planner_prompt(
    goal: &str,
    cwd: &Path,
    max_tasks: usize,
    available: &[ProviderId],
    roles: &RoleRegistry,
    default_worker: ProviderId,
) -> String {
    let orchestrator = roles.get(RoleId::Orchestrator);
    let planner = roles.get(RoleId::Planner);
    let providers = available
        .iter()
        .map(|provider| provider.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let role_context = format!(
        "{}\n\n{}",
        orchestrator.prompt.trim(),
        planner.prompt.trim()
    );
    let template = r#"You are baton's coordinator. Split the goal into at most MAX independent worker tasks.
Use the smallest number of tasks that truly parallelizes. Return exactly one task for a question, a small fix, or work that touches one cohesive area. Split only when the tasks are independent, touch disjoint files, and do not need each other's output. Do not invent extra work.
Each task prompt must be self-contained. Never ask a worker to coordinate with another worker.
Each worker runs in its own git worktree, so tasks must not depend on another task's output and must avoid overlapping files.
Choose the best installed harness for each task from: PROVIDERS. Default every task to DEFAULT_WORKER. Choose another harness only when the goal explicitly names one.
When the goal explicitly names a model for a task, set that task's "model" field. Otherwise omit "model" so Baton applies the role default for the default worker provider; a task on another harness uses its config default.
When the goal explicitly names a reasoning effort, set that task's "effort" field. Use a lowercase value such as low, medium, high, xhigh, or max. Only codex and claude support effort. Otherwise omit "effort" so Baton applies the role default for the default worker provider; another harness uses its default.
Baton runs a reviewer after each completed worker, outside this planning request. Do not wait for a reviewer verdict; return only the plan.
Every worker starts in its own worktree, so each prompt must refer to the current working directory, never to the source checkout path.
Return ONLY a JSON object, with no markdown fences or commentary:
{"summary":"one sentence","tasks":[{"title":"short task name","provider":"DEFAULT_WORKER","prompt":"complete self-contained instructions for the worker"}]}"#;
    let prompt = template
        .replace("MAX", &max_tasks.to_string())
        .replace("PROVIDERS", &providers)
        .replace("DEFAULT_WORKER", default_worker.as_str());
    format!(
        "{role_context}\n\nApply the role context above within the coordinator contract below. The output format and safety rules below take precedence if they conflict.\n\n{prompt}\nGoal: {goal}\nSource repository: {}",
        cwd.display()
    )
}

fn normalize_plan_for_role(plan: &mut Plan, role: &RoleSpec) {
    for task in &mut plan.tasks {
        if task.provider.trim() != role.provider.as_str() {
            continue;
        }
        if task.model.is_none() {
            task.model = Some(role.model.clone());
        }
        if task.effort.is_none() {
            task.effort = role.effort.clone();
        }
    }
}

fn fallback_plan_from_role(goal: &str, role: &RoleSpec) -> Plan {
    fallback_plan_with_values(
        goal,
        role.provider,
        Some(role.model.as_str()),
        role.effort.as_deref(),
    )
}

fn fallback_plan_with_values(
    goal: &str,
    provider: ProviderId,
    model: Option<&str>,
    effort: Option<&str>,
) -> Plan {
    let mut plan = fallback_plan(goal, provider);
    if let Some(task) = plan.tasks.first_mut() {
        task.model = model.map(str::to_string);
        task.effort = effort.map(str::to_string);
    }
    plan.role_defaults_applied = true;
    plan
}

fn planner_failure_plan(goal: &str, available: &[ProviderId], roles: &RoleRegistry) -> Plan {
    let implementor = roles.get(RoleId::Implementor);
    if available.contains(&implementor.provider) {
        fallback_plan_from_role(goal, implementor)
    } else {
        fallback_plan_from_role(goal, roles.get(RoleId::Planner))
    }
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

const WORKER_PREAMBLE: &str = "You are a worker in an isolated git worktree. Treat the current working directory as the repository root. Do not read or write the source checkout outside this worktree. If you cannot finish without a user decision or information only the user can provide, stop and end your reply with exactly one line formatted as `BATON_QUESTION: <question>`. Do not guess. Otherwise finish the task.";

const MAX_TASK_PROMPT_BYTES: usize = 12 * 1024;
const MAX_ORCHESTRATION_GOAL_BYTES: usize = 6 * 1024;

pub const QUESTION_PREFIX: &str = "BATON_QUESTION:";

/// Extracts a worker's decision request from the last matching line. The
/// sentinel is a protocol marker, not ordinary prose.
pub fn worker_question(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        line.trim()
            .strip_prefix(QUESTION_PREFIX)
            .map(str::trim)
            .filter(|question| !question.is_empty())
            .map(str::to_string)
    })
}

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
    let limit = max_tasks.min(MAX_TASKS);
    if limit == 0 {
        anyhow::bail!("max tasks must be at least 1");
    }
    if plan.tasks.len() > limit {
        plan.tasks.truncate(limit);
    }
    for task in &mut plan.tasks {
        if task.title.trim().is_empty() {
            anyhow::bail!("planner returned a task without a title");
        }
        let model = task.model.take();
        task.model = match model {
            Some(model) if model.trim().is_empty() => None,
            Some(model) => {
                let model = model.trim().to_string();
                if !crate::roles::safe_model_token(&model) {
                    anyhow::bail!("planner returned an invalid model");
                }
                Some(model)
            }
            None => None,
        };
        let effort = task.effort.take();
        task.effort = match effort {
            Some(effort)
                if effort.trim().is_empty() || crate::types::is_effort_clear(&effort) =>
            {
                None
            }
            Some(effort) => Some(
                crate::types::normalize_reasoning_effort(&effort)
                    .ok_or_else(|| anyhow::anyhow!("planner returned an invalid reasoning effort"))?,
            ),
            None => None,
        };
        if task.prompt.trim().is_empty() {
            anyhow::bail!("planner returned a task without a prompt");
        }
        if task.prompt.len() > MAX_TASK_PROMPT_BYTES {
            anyhow::bail!(
                "planner returned a task prompt larger than {MAX_TASK_PROMPT_BYTES} bytes"
            );
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

fn coordinator_command(
    binary: &Path,
    cwd: &Path,
    provider: ProviderId,
    model: &str,
    effort: Option<&str>,
    permission_mode: PermissionMode,
) -> Command {
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
    if let Some(effort) = effort {
        command.arg("--effort").arg(effort);
    }
    apply_permission_args(&mut command, permission_mode);
    command.env("BATON_DISABLE_SKILL_DISPATCH", "1");
    command
}

fn run_coordinator(
    prompt: &str,
    cwd: &Path,
    provider: ProviderId,
    model: &str,
    effort: Option<&str>,
    permission_mode: PermissionMode,
    cancel: Option<&AtomicBool>,
) -> Result<String> {
    let temp_home = std::env::temp_dir().join(format!("baton-planner-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_home)?;
    let binary = std::env::current_exe()?;
    let mut command = coordinator_command(&binary, cwd, provider, model, effort, permission_mode);
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

pub fn coordinator_role_for(goal: &str, plan_path: Option<&Path>) -> RoleId {
    if plan_path.is_none() && is_conversational_greeting(goal) {
        RoleId::Orchestrator
    } else {
        RoleId::Planner
    }
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
    roles: &RoleRegistry,
    effort: Option<&str>,
    default_worker: ProviderId,
) -> Result<Plan> {
    crate::roles::run_hooks_from(roles, "before_plan", cwd)?;
    let base_prompt = planner_prompt(goal, cwd, max_tasks, available, roles, default_worker);
    let fallback = planner_failure_plan(goal, available, roles);
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
            effort,
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
            Ok(plan) => return finish_plan(plan, roles, cwd),
            Err(error) => {
                if plan_is_empty(&error) {
                    return finish_plan(plan_after_planner_failure(&fallback, &error)?, roles, cwd);
                }
                last_error = Some(error);
            }
        }
    }
    let error = last_error.unwrap_or_else(|| anyhow::anyhow!("planner failed"));
    if std::env::var_os("BATON_ORCHESTRATE_DEBUG").is_some() {
        eprintln!("[orchestrate] planner failed; using one direct worker: {error}");
    }
    finish_plan(plan_after_planner_failure(&fallback, &error)?, roles, cwd)
}

/// Run the `after_plan` hooks before a plan can launch workers, including a
/// fallback plan, so a failing project check stops the run.
fn finish_plan(plan: Plan, roles: &RoleRegistry, cwd: &Path) -> Result<Plan> {
    crate::roles::run_hooks_from(roles, "after_plan", cwd)?;
    Ok(plan)
}

fn fallback_plan(goal: &str, provider: ProviderId) -> Plan {
    Plan {
        summary: Some("direct task".to_string()),
        role_defaults_applied: false,
        tasks: vec![PlanTask {
            title: "answer".to_string(),
            provider: provider.as_str().to_string(),
            model: None,
            effort: None,
            prompt: goal.to_string(),
        }],
    }
}

/// One deterministic task for `baton spawn`, with no coordinator call.
pub fn single_task_plan(
    prompt: &str,
    provider: ProviderId,
    model: String,
    effort: Option<String>,
) -> Plan {
    Plan {
        summary: Some("single subagent".to_string()),
        role_defaults_applied: true,
        tasks: vec![PlanTask {
            title: crate::handoff::derive_title(prompt),
            provider: provider.as_str().to_string(),
            model: Some(model),
            effort,
            prompt: prompt.to_string(),
        }],
    }
}

fn plan_is_empty(error: &anyhow::Error) -> bool {
    error.to_string() == "planner returned no tasks"
}

/// A planner that cannot produce a usable plan must not strand the user's
/// work. Cancellation is the one exception: the user asked to stop.
fn plan_after_planner_failure(fallback: &Plan, error: &anyhow::Error) -> Result<Plan> {
    if error.to_string() == CANCELLED_ERROR {
        anyhow::bail!(CANCELLED_ERROR);
    }
    Ok(fallback.clone())
}

fn worker_prompt(implementor_prompt: &str, task_prompt: &str) -> String {
    format!(
        "{WORKER_PREAMBLE}\n\n{}\n\n{}",
        implementor_prompt.trim(),
        task_prompt
    )
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
    effort: Option<String>,
    prompt: String,
}

fn panicked_worker(fallback: &WorkerFallback) -> (usize, WorkerResult) {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    (
        fallback.index,
        WorkerResult {
            review_status: None,
            review_text: None,
            reviewed_at: None,
            id: uuid::Uuid::new_v4().to_string(),
            title: fallback.title.clone(),
            provider: fallback.provider,
            model: fallback.model.clone(),
            effort: fallback.effort.clone(),
            prompt: fallback.prompt.clone(),
            worktree: None,
            commit: None,
            thread_id: None,
            status: "failed".to_string(),
            text: None,
            error: Some("worker thread panicked".to_string()),
            pr_url: None,
            merged_at: None,
            created_at: now.clone(),
            finished_at: now,
            snapshot: None,
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
    repo_snapshot: Option<&worktree::RepoSnapshot>,
    worker_bin: Option<&Path>,
    implementor_prompt: &str,
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
        repo_snapshot,
        worker_bin,
        implementor_prompt,
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

fn worker_model(task: &PlanTask, home: &Path, cwd: &Path, provider: ProviderId) -> String {
    task.model
        .clone()
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| resolve_config(home, cwd).for_provider(provider).model.clone())
}

fn run_worker_inner(
    task: &PlanTask,
    index: usize,
    run_id: &str,
    home: &Path,
    cwd: &Path,
    repo_root: Option<&Path>,
    repo_snapshot: Option<&worktree::RepoSnapshot>,
    worker_bin: Option<&Path>,
    implementor_prompt: &str,
    progress: Option<&ProgressFn<'_>>,
    cancel: Option<&AtomicBool>,
    permission_mode: PermissionMode,
) -> (usize, WorkerResult) {
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let provider = ProviderId::parse(task.provider.trim()).unwrap_or(ProviderId::Codex);
    let model = worker_model(task, home, cwd, provider);
    if is_cancelled(cancel) {
        return (
            index,
            WorkerResult {
                review_status: None,
                review_text: None,
                reviewed_at: None,
                id: uuid::Uuid::new_v4().to_string(),
                title: task.title.clone(),
                provider,
                model,
                effort: task.effort.clone(),
                prompt: task.prompt.clone(),
                worktree: None,
                commit: None,
                thread_id: None,
                status: "cancelled".to_string(),
                text: None,
                error: None,
                pr_url: None,
                merged_at: None,
                created_at: created_at.clone(),
                finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                snapshot: None,
            },
        );
    }
    let effort = task.effort.clone();
    if let Some(effort) = effort.as_deref() {
        if !provider.supports_effort() {
            return (
                index,
                WorkerResult {
                    review_status: None,
                    review_text: None,
                    reviewed_at: None,
                    id: uuid::Uuid::new_v4().to_string(),
                    title: task.title.clone(),
                    provider,
                    model,
                    effort: Some(effort.to_string()),
                    prompt: task.prompt.clone(),
                    worktree: None,
                    commit: None,
                    thread_id: None,
                    status: "failed".to_string(),
                    text: None,
                    error: Some(format!(
                        "{provider} does not support reasoning effort; use codex or claude"
                    )),
                    pr_url: None,
                    merged_at: None,
                    created_at,
                    finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    snapshot: None,
                },
            );
        }
    }
    let worktree = match repo_root {
        Some(root) => {
            let created = match repo_snapshot {
                Some(snapshot) => worktree::create_at(
                    home,
                    root,
                    run_id,
                    &task.title,
                    &snapshot.base_ref,
                ),
                None => worktree::create(home, root, run_id, &task.title),
            };
            match created {
                Ok(worktree) => Some(worktree),
                Err(error) => {
                    return (
                        index,
                        WorkerResult {
                            review_status: None,
                            review_text: None,
                            reviewed_at: None,
                            id: uuid::Uuid::new_v4().to_string(),
                            title: task.title.clone(),
                            provider,
                            model,
                            effort: effort.clone(),
                            prompt: task.prompt.clone(),
                            worktree: None,
                            commit: None,
                            thread_id: None,
                            status: "failed".to_string(),
                            text: None,
                            error: Some(format!("cannot create worktree: {error}")),
                            pr_url: None,
                            merged_at: None,
                            created_at,
                            finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                            snapshot: None,
                        },
                    );
                }
            }
        }
        None => None,
    };
    let worker_snapshot = match (worktree.as_ref(), repo_snapshot) {
        (Some(worktree), Some(snapshot)) => match worktree::apply_snapshot(&worktree.path, snapshot) {
            Ok(()) => Some(WorkerSnapshot {
                base_ref: snapshot.base_ref.clone(),
                tree: snapshot.tree.clone(),
            }),
            Err(error) => {
                return (
                    index,
                    WorkerResult {
                        review_status: None,
                        review_text: None,
                        reviewed_at: None,
                        id: uuid::Uuid::new_v4().to_string(),
                        title: task.title.clone(),
                        provider,
                        model,
                        effort: effort.clone(),
                        prompt: task.prompt.clone(),
                        worktree: Some(worktree.clone()),
                        commit: None,
                        thread_id: None,
                        status: "failed".to_string(),
                        text: None,
                        error: Some(format!("cannot apply source snapshot: {error}")),
                        pr_url: None,
                        merged_at: None,
                        created_at,
                        finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        snapshot: None,
                    },
                );
            }
        },
        _ => None,
    };
    let worker_cwd = worktree
        .as_ref()
        .map(|worktree| worktree.path.clone())
        .unwrap_or_else(|| cwd.to_path_buf());
    if let Err(error) = crate::roles::run_hooks(home, "before_implement", &worker_cwd) {
        return (
            index,
            WorkerResult {
                review_status: None,
                review_text: None,
                reviewed_at: None,
                id: uuid::Uuid::new_v4().to_string(),
                title: task.title.clone(),
                provider,
                model,
                effort,
                prompt: task.prompt.clone(),
                worktree,
                commit: None,
                thread_id: None,
                status: "failed".to_string(),
                text: None,
                error: Some(format!("before_implement hook failed: {error}")),
                pr_url: None,
                merged_at: None,
                created_at,
                finished_at: chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                snapshot: worker_snapshot,
            },
        );
    }
    let binary = worker_bin
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("baton"));
    let effective_prompt = worker_prompt(implementor_prompt, &task.prompt);
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
    if let Some(effort) = effort.as_deref() {
        command.arg("--effort").arg(effort);
    }
    apply_permission_args(&mut command, permission_mode);
    command
        .arg(&effective_prompt)
        .env("BATON_HOME", home)
        .env("BATON_DISABLE_SKILL_DISPATCH", "1");
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
    let question = text.as_deref().and_then(worker_question);
    let mut status = if cancelled {
        "cancelled"
    } else if error.is_none() {
        if question.is_some() {
            "needs_input"
        } else {
            "completed"
        }
    } else {
        "failed"
    };
    let mut error = error;
    let mut commit = None;
    // A completed worker runs its post hook before the commit, so formatting
    // and validation changes belong to the same commit.
    if status == "completed" {
        if let Err(hook_error) = crate::roles::run_hooks(home, "after_implement", &worker_cwd) {
            status = "failed";
            error = Some(format!("after_implement hook failed: {hook_error}"));
        }
    }
    if matches!(status, "completed" | "needs_input") {
        if let Some(worktree) = worktree.as_ref() {
            let message = format!("baton worker: {}", task.title);
            let committed = match worker_snapshot.as_ref() {
                Some(snapshot) => worktree::commit_changes_after_snapshot(
                    &worktree.path,
                    &snapshot.tree,
                    &message,
                ),
                None => worktree::commit_changes(&worktree.path, &message),
            };
            match committed {
                Ok(sha) => commit = sha,
                Err(commit_error) => {
                    status = "failed";
                    error = Some(format!(
                        "worker finished but its changes could not be committed: {commit_error}"
                    ));
                }
            }
        }
    }
    (
        index,
        WorkerResult {
            review_status: None,
            review_text: None,
            reviewed_at: None,
            id: uuid::Uuid::new_v4().to_string(),
            title: task.title.clone(),
            provider,
            model,
            effort,
            prompt: task.prompt.clone(),
            worktree,
            snapshot: worker_snapshot,
            commit,
            thread_id,
            status: status.to_string(),
            text,
            error,
            pr_url: None,
            merged_at: None,
            created_at,
            finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        },
    )
}

pub const REVIEW_APPROVE: &str = "approve";
pub const REVIEW_REQUEST_CHANGES: &str = "request_changes";
pub const REVIEW_FAILED: &str = "failed";
pub const REVIEW_SKIPPED: &str = "skipped";
pub const REVIEW_PENDING: &str = "pending";
const MAX_REVIEW_DIFF_CHARS: usize = 12_000;
const MAX_REVIEW_EVIDENCE_CHARS: usize = 4_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewOutcome {
    pub status: &'static str,
    pub text: String,
}

/// The reviewer is asked for a line that is exactly `APPROVE` or
/// `REQUEST_CHANGES`. A missing verdict fails closed, because an unreadable
/// review must not read as approval.
pub fn parse_review_verdict(text: &str) -> &'static str {
    fn verdict(line: &str) -> Option<&'static str> {
        let line = line
            .trim()
            .trim_start_matches(|character: char| !character.is_ascii_alphanumeric());
        let rest = line.to_ascii_uppercase();
        let word_ends = |prefix: &str| {
            !rest[prefix.len()..]
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphabetic())
        };
        if rest.starts_with("APPROVE") && word_ends("APPROVE") {
            return Some(REVIEW_APPROVE);
        }
        for prefix in ["REQUEST_CHANGES", "REQUEST CHANGES"] {
            if rest.starts_with(prefix) && word_ends(prefix) {
                return Some(REVIEW_REQUEST_CHANGES);
            }
        }
        None
    }
    for line in text.lines() {
        let line = line.trim();
        if let Some(found) = verdict(line) {
            return found;
        }
        // A label such as `Verdict: APPROVE` keeps the verdict after the
        // colon. Only the tail is read when the label itself has no verdict.
        if let Some((label, tail)) = line.rsplit_once(':') {
            if !label.to_ascii_uppercase().contains("APPROVE")
                && !label.to_ascii_uppercase().contains("REQUEST")
            {
                if let Some(found) = verdict(tail) {
                    return found;
                }
            }
        }
    }
    REVIEW_REQUEST_CHANGES
}

fn bounded_review_text(text: &str, max: usize, marker: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut capped = trimmed.chars().take(max).collect::<String>();
    capped.push_str(marker);
    capped
}

fn review_diff(cwd: &Path, commit: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["show", "--format=", "--patch", commit])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("cannot read commit {commit} in {}", cwd.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "git show {commit} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(bounded_review_text(
        &String::from_utf8_lossy(&output.stdout),
        MAX_REVIEW_DIFF_CHARS,
        "\n[...diff truncated]",
    ))
}

fn reviewer_prompt(reviewer: &RoleSpec, task: &WorkerResult, diff: &str) -> String {
    let reported = match task.text.as_deref() {
        Some(text) => text,
        None => "(the worker reported no text)",
    };
    let evidence = bounded_review_text(
        reported,
        MAX_REVIEW_EVIDENCE_CHARS,
        "\n[...worker report truncated]",
    );
    format!(
        "{prompt}\n\nTask: {title}\nWorker: {provider}/{model}\nCommit: {commit}\n\nWorker report:\n{evidence}\n\nDiff:\n{diff}\n\nCheck the diff against the task and the worker's claims. End with a line that is exactly APPROVE or REQUEST_CHANGES.",
        prompt = reviewer.prompt,
        title = task.title,
        provider = task.provider,
        model = task.model,
        commit = task.commit.as_deref().unwrap_or("(none)"),
    )
}

/// One reviewer turn over a completed worker's commit. The reviewer runs in the
/// worker worktree with a role marker, so a harness or fixture can tell the two
/// turns apart.
fn review_task(
    task: &WorkerResult,
    home: &Path,
    repo_root: &Path,
    reviewer: &RoleSpec,
    binary: &Path,
    permission_mode: PermissionMode,
    cancel: Option<&AtomicBool>,
) -> ReviewOutcome {
    let Some(commit) = task.commit.as_deref() else {
        return ReviewOutcome {
            status: REVIEW_SKIPPED,
            text: "no commit to review".to_string(),
        };
    };
    if is_cancelled(cancel) {
        return ReviewOutcome {
            status: REVIEW_FAILED,
            text: "review cancelled".to_string(),
        };
    }
    let cwd = task
        .worktree
        .as_ref()
        .map(|worktree| worktree.path.clone())
        .unwrap_or_else(|| repo_root.to_path_buf());
    if let Err(error) = crate::roles::run_hooks(home, "before_review", &cwd) {
        return ReviewOutcome {
            status: REVIEW_FAILED,
            text: format!("before_review hook failed: {error}"),
        };
    }
    let diff = match review_diff(&cwd, commit) {
        Ok(diff) => diff,
        Err(error) => {
            return ReviewOutcome {
                status: REVIEW_FAILED,
                text: error.to_string(),
            };
        }
    };
    let mut command = Command::new(binary);
    command
        .arg("-C")
        .arg(&cwd)
        .arg("exec")
        .arg("-p")
        .arg(reviewer.provider.as_str())
        .arg("-m")
        .arg(&reviewer.model)
        .arg("--json");
    if let Some(effort) = reviewer.effort.as_deref() {
        command.arg("--effort").arg(effort);
    }
    apply_permission_args(&mut command, permission_mode);
    command
        .arg(reviewer_prompt(reviewer, task, &diff))
        .env("BATON_HOME", home)
        .env("BATON_DISABLE_SKILL_DISPATCH", "1")
        .env("BATON_ROLE", "reviewer");
    let outcome = match run_command_cancellable(command, cancel) {
        Ok(result) => {
            let (_, text, error, phase) = parse_exec_output(&result.stdout, &result.stderr);
            if !result.status.success() || phase.as_deref() != Some("completed") {
                ReviewOutcome {
                    status: REVIEW_FAILED,
                    text: error
                        .unwrap_or_else(|| format!("reviewer turn exited with {}", result.status)),
                }
            } else {
                let text = text.unwrap_or_default();
                ReviewOutcome {
                    status: parse_review_verdict(&text),
                    text,
                }
            }
        }
        Err(error) => ReviewOutcome {
            status: REVIEW_FAILED,
            text: format!("cannot run reviewer: {error}"),
        },
    };
    if let Err(error) = crate::roles::run_hooks(home, "after_review", &cwd) {
        return ReviewOutcome {
            status: REVIEW_FAILED,
            text: format!(
                "after_review hook failed: {error} (verdict: {})",
                outcome.text
            ),
        };
    }
    outcome
}

pub fn review_blocked(tasks: &[WorkerResult]) -> Vec<String> {
    tasks
        .iter()
        .filter(|task| {
            matches!(
                task.review_status.as_deref(),
                Some(REVIEW_PENDING) | Some(REVIEW_REQUEST_CHANGES) | Some(REVIEW_FAILED)
            )
        })
        .map(|task| task.title.clone())
        .collect()
}

/// Re-run the reviewer over the completed worker commits of a stored run.
/// Follow-up turns that create a commit mark the task `pending`, so integration
/// stays blocked until a reviewer reads the new commit.
pub fn review_run(
    store: &BatonStore,
    home: &Path,
    cwd: &Path,
    run_id: &str,
    selection: &[usize],
    permission_override: Option<PermissionMode>,
) -> Result<RunReport> {
    let binary = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("baton"));
    review_run_with_bin(
        store,
        home,
        cwd,
        run_id,
        selection,
        permission_override,
        Some(&binary),
    )
}

fn review_run_with_bin(
    store: &BatonStore,
    home: &Path,
    cwd: &Path,
    run_id: &str,
    selection: &[usize],
    permission_override: Option<PermissionMode>,
    binary: Option<&Path>,
) -> Result<RunReport> {
    let run_id = resolve_run_id(store, run_id)?;
    let repo_root = worktree::repo_root(cwd)?;
    let registry = crate::roles::load(home)?;
    if !registry.review {
        anyhow::bail!(
            "review is disabled in roles.json; remove \"review\": false to run the reviewer"
        );
    }
    let reviewer = registry.get(RoleId::Reviewer);
    let permission_mode = permission_override.unwrap_or_else(|| {
        resolve_config(home, cwd)
            .for_provider(reviewer.provider)
            .permission_mode
    });
    let report = report_from_rows(store, cwd, &run_id)?;
    if report.tasks.is_empty() {
        anyhow::bail!("run {} has no tasks", &run_id[..8.min(run_id.len())]);
    }
    for index in selection {
        if *index == 0 || *index > report.tasks.len() {
            anyhow::bail!(
                "--task {index} is out of range (the run has {} tasks)",
                report.tasks.len()
            );
        }
    }
    let binary = binary
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("baton"));
    let mut reviewed = 0usize;
    for (offset, task) in report.tasks.iter().enumerate() {
        let index = offset + 1;
        if !selection.is_empty() && !selection.contains(&index) {
            continue;
        }
        if task.status != "completed" || task.commit.is_none() {
            continue;
        }
        let outcome = review_task(
            task,
            home,
            &repo_root,
            reviewer,
            &binary,
            permission_mode,
            None,
        );
        let reviewed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        store.set_orchestration_task_review(
            &run_id,
            &task.id,
            outcome.status,
            Some(&outcome.text),
            &reviewed_at,
        )?;
        reviewed += 1;
    }
    if reviewed == 0 {
        anyhow::bail!("no completed worker commits to review");
    }
    store.refresh_orchestration_run_status(&run_id)?;
    report_from_rows(store, cwd, &run_id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowupReview {
    pub run_id: String,
    pub task_id: String,
    pub status: String,
    pub text: String,
}

/// Runs the reviewer for one worker follow-up commit. Returns None when the
/// reviewer role is disabled or the task is not part of the run.
pub fn auto_review_followup(
    store: &BatonStore,
    home: &Path,
    cwd: &Path,
    outcome: &WorkerFollowupOutcome,
    permission_override: Option<PermissionMode>,
) -> Result<Option<FollowupReview>> {
    let binary = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("baton"));
    auto_review_followup_with_bin(
        store,
        home,
        cwd,
        outcome,
        permission_override,
        Some(&binary),
    )
}

fn auto_review_followup_with_bin(
    store: &BatonStore,
    home: &Path,
    cwd: &Path,
    outcome: &WorkerFollowupOutcome,
    permission_override: Option<PermissionMode>,
    binary: Option<&Path>,
) -> Result<Option<FollowupReview>> {
    let registry = crate::roles::load(home)?;
    if !registry.review {
        return Ok(None);
    }
    let tasks = store.list_orchestration_tasks(&outcome.run_id)?;
    let Some(position) = tasks.iter().position(|task| task.id == outcome.task_id) else {
        return Ok(None);
    };
    let report = review_run_with_bin(
        store,
        home,
        cwd,
        &outcome.run_id,
        &[position + 1],
        permission_override,
        binary,
    )?;
    Ok(report.tasks.get(position).map(|task| FollowupReview {
        run_id: outcome.run_id.clone(),
        task_id: outcome.task_id.clone(),
        status: task
            .review_status
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        text: task.review_text.clone().unwrap_or_default(),
    }))
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

fn ensure_no_worker_questions(tasks: &[OrchestrationTaskRow]) -> Result<()> {
    let waiting = tasks
        .iter()
        .filter(|task| task.status == "needs_input")
        .map(|task| task.title.as_str())
        .collect::<Vec<_>>();
    if !waiting.is_empty() {
        anyhow::bail!(
            "worker(s) waiting for input: {}; answer them before integrating or opening PRs",
            waiting.join(", ")
        );
    }
    Ok(())
}

fn ensure_reviews_clear(tasks: &[OrchestrationTaskRow]) -> Result<()> {
    let blocked = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.review_status.as_deref(),
                Some(REVIEW_PENDING) | Some(REVIEW_REQUEST_CHANGES) | Some(REVIEW_FAILED)
            )
        })
        .map(|task| {
            format!(
                "{} ({})",
                task.title,
                task.review_status.as_deref().unwrap_or("unknown")
            )
        })
        .collect::<Vec<_>>();
    if !blocked.is_empty() {
        anyhow::bail!(
            "review did not approve: {}; fix the review findings before integrating or opening PRs",
            blocked.join(", ")
        );
    }
    Ok(())
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

enum BranchMergeOutcome {
    Merged(String),
    Conflict(Vec<String>),
}

fn merge_snapshot_branch(
    root: &Path,
    branch: &str,
    snapshot: &WorkerSnapshot,
    result_tree: &str,
    message: &str,
) -> Result<BranchMergeOutcome> {
    let head = git_stdout(root, &["rev-parse", "HEAD"])?;
    let merge_base = format!("--merge-base={}", snapshot.tree);
    let output = Command::new("git")
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            &merge_base,
            "HEAD",
            result_tree,
        ])
        .current_dir(root)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut lines = stdout.lines();
    let tree = lines.next().unwrap_or("").trim().to_string();
    if tree.is_empty() {
        anyhow::bail!(
            "snapshot-aware merge failed before producing a tree: {}",
            stderr.trim()
        );
    }
    if output.status.success() {
        let current = git_stdout(root, &["rev-parse", "HEAD"])?;
        if current != head {
            anyhow::bail!("source HEAD moved during snapshot integration; retry");
        }
        let merge_commit = git_stdout(
            root,
            &["commit-tree", &tree, "-p", &head, "-p", branch, "-m", message],
        )?;
        let fast_forward = Command::new("git")
            .args(["merge", "--ff-only", &merge_commit])
            .current_dir(root)
            .output()?;
        if !fast_forward.status.success() {
            anyhow::bail!(
                "snapshot merge commit {merge_commit} could not fast-forward the source: {}",
                String::from_utf8_lossy(&fast_forward.stderr).trim()
            );
        }
        return Ok(BranchMergeOutcome::Merged(merge_commit));
    }
    let files = lines
        .take_while(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if files.is_empty() {
        anyhow::bail!("snapshot-aware merge failed: {}", stderr.trim());
    }
    Ok(BranchMergeOutcome::Conflict(files))
}

fn merge_branch(
    root: &Path,
    branch: &str,
    snapshot: Option<&WorkerSnapshot>,
    message: &str,
) -> Result<BranchMergeOutcome> {
    if let Some(snapshot) = snapshot {
        let result_tree = worktree::snapshot_result_tree(
            root,
            &snapshot.base_ref,
            &snapshot.tree,
            branch,
        )?;
        return merge_snapshot_branch(root, branch, snapshot, &result_tree, message);
    }
    let output = Command::new("git")
        .args(["merge", "--no-ff", branch, "-m", message])
        .current_dir(root)
        .output()?;
    if output.status.success() {
        return Ok(BranchMergeOutcome::Merged(git_stdout(
            root,
            &["rev-parse", "HEAD"],
        )?));
    }
    let files = conflicted_files(root);
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let _ = Command::new("git")
        .args(["merge", "--abort"])
        .current_dir(root)
        .output();
    if files.is_empty() {
        anyhow::bail!("git merge failed: {stderr}");
    }
    Ok(BranchMergeOutcome::Conflict(files))
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
    ensure_no_worker_questions(&tasks)?;
    ensure_reviews_clear(&tasks)?;
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
        let snapshot = match (task.snapshot_base.as_deref(), task.snapshot_tree.as_deref()) {
            (Some(base_ref), Some(tree)) => Some(WorkerSnapshot {
                base_ref: base_ref.to_string(),
                tree: tree.to_string(),
            }),
            _ => None,
        };
        match merge_branch(&repo_root, branch, snapshot.as_ref(), &message)? {
            BranchMergeOutcome::Merged(merge_commit) => {
                let merged_at =
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                store.mark_orchestration_task_merged(&task.id, &merged_at)?;
                report.merged.push(IntegratedTask {
                    index,
                    title: task.title.clone(),
                    branch: branch.to_string(),
                    commit: task.commit_sha.clone().unwrap_or_default(),
                    merge_commit,
                });
            }
            BranchMergeOutcome::Conflict(files) => {
                report.conflict = Some(IntegrateConflict {
                    index,
                    title: task.title.clone(),
                    branch: branch.to_string(),
                    files,
                });
                break;
            }
        }
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
    ensure_no_worker_questions(&tasks)?;
    ensure_reviews_clear(&tasks)?;
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

#[allow(clippy::too_many_arguments)]
pub fn plan(
    goal: &str,
    cwd: &Path,
    home: &Path,
    coordinator_provider: ProviderId,
    coordinator_model: &str,
    max_tasks: usize,
    plan_path: Option<&Path>,
    permission_mode: PermissionMode,
    cancel: Option<&AtomicBool>,
) -> Result<Plan> {
    if max_tasks == 0 {
        anyhow::bail!("max tasks must be at least 1");
    }
    if max_tasks > MAX_TASKS {
        anyhow::bail!("max tasks must be at most {MAX_TASKS}");
    }
    if goal.len() > MAX_ORCHESTRATION_GOAL_BYTES {
        anyhow::bail!(
            "orchestration goal exceeds {MAX_ORCHESTRATION_GOAL_BYTES} bytes"
        );
    }
    if is_cancelled(cancel) {
        anyhow::bail!(CANCELLED_ERROR);
    }
    let registry = crate::roles::load(home)?;
    let orchestrator = registry.get(RoleId::Orchestrator);
    let planner = registry.get(RoleId::Planner);
    let implementor = registry.get(RoleId::Implementor);
    let available = available_providers(coordinator_provider);
    let default_worker =
        default_worker_provider(&available, coordinator_provider, implementor.provider);
    let normalization_role = if available.contains(&implementor.provider) {
        implementor
    } else {
        planner
    };
    let greeting = coordinator_role_for(goal, plan_path) == RoleId::Orchestrator;
    let mut plan = match plan_path {
        Some(path) => load_plan(path, max_tasks)?,
        None if greeting => fallback_plan_from_role(goal, orchestrator),
        None => run_planner(
            goal,
            cwd,
            coordinator_provider,
            coordinator_model,
            max_tasks,
            permission_mode,
            cancel,
            &available,
            &registry,
            planner.effort.as_deref(),
            default_worker,
        )?,
    };
    if plan_path.is_none() && !greeting {
        // Planner output is untrusted metadata. Baton always applies the role
        // defaults itself, even when the model copied this marker into JSON.
        plan.role_defaults_applied = false;
    }
    if !plan.role_defaults_applied {
        if !greeting {
            normalize_plan_for_role(&mut plan, normalization_role);
        }
        plan.role_defaults_applied = true;
    }
    Ok(plan)
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
    let registry = crate::roles::load(home)?;
    let implementor = registry.get(RoleId::Implementor);
    let implementor_prompt = implementor.prompt.clone();
    let repo_root = worktree::repo_root(cwd).context("orchestrate requires a git repository")?;
    let plan = plan(
        goal,
        cwd,
        home,
        coordinator_provider,
        coordinator_model,
        max_tasks,
        plan_path,
        permission_mode,
        cancel,
    )?;
    // Capture the source after planning so workers see the latest dirty state.
    let dirty_source = worktree::is_dirty(&repo_root).unwrap_or(false);
    let repo_snapshot = if dirty_source {
        worktree::snapshot(&repo_root)?
    } else {
        None
    };
    let run_id = uuid::Uuid::new_v4().to_string();
    let summary = plan.summary.clone();
    let owner_pid = std::process::id();
    let owner_start = crate::process::process_start(owner_pid);
    store.create_orchestration_run_owned(
        &run_id,
        goal,
        coordinator_provider,
        coordinator_model,
        summary.as_deref(),
        &serde_json::to_string(&plan)?,
        dirty_source,
        Some(owner_pid),
        owner_start.as_deref(),
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
                    model: worker_model(task, home, cwd, fallback_provider),
                    effort: task.effort.clone(),
                    prompt: task.prompt.clone(),
                };
                let run_id = run_id.clone();
                let home = home.to_path_buf();
                let cwd = cwd.to_path_buf();
                let repo_root = repo_root.clone();
                let worker_bin = worker_bin.map(Path::to_path_buf);
                let implementor_prompt = implementor_prompt.clone();
                let worker_snapshot = repo_snapshot.as_ref();
                let handle = scope.spawn(move || {
                    run_worker(
                        task,
                        index,
                        &run_id,
                        &home,
                        &cwd,
                        Some(repo_root.as_path()),
                        worker_snapshot,
                        worker_bin.as_deref(),
                        &implementor_prompt,
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
    let reviewer = registry.get(RoleId::Reviewer);
    let review_enabled = registry.review;
    let reviewer_bin = worker_bin
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("baton"));
    let mut tasks = Vec::new();
    for (position, (_, mut task)) in results.into_iter().enumerate() {
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
            task.snapshot.as_ref().map(|snapshot| snapshot.base_ref.as_str()),
            task.snapshot.as_ref().map(|snapshot| snapshot.tree.as_str()),
            task.effort.as_deref(),
        )?;
        if task.status == "completed" && task.commit.is_some() {
            let outcome = if review_enabled {
                emit(
                    progress,
                    OrchestrationProgress::WorkerFinished {
                        index: position + 1,
                        title: format!("review {}", task.title),
                        status: "running".to_string(),
                        commit: task.commit.clone(),
                        error: None,
                    },
                );
                review_task(
                    &task,
                    home,
                    &repo_root,
                    reviewer,
                    &reviewer_bin,
                    permission_mode,
                    None,
                )
            } else {
                ReviewOutcome {
                    status: REVIEW_SKIPPED,
                    text: "review disabled by roles.json".to_string(),
                }
            };
            let reviewed_at =
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            store.set_orchestration_task_review(
                &run_id,
                &task.id,
                outcome.status,
                Some(&outcome.text),
                &reviewed_at,
            )?;
            task.review_status = Some(outcome.status.to_string());
            task.review_text = Some(outcome.text);
            task.reviewed_at = Some(reviewed_at);
        }
        tasks.push(task);
    }
    let completed = tasks.iter().filter(|task| task.status == "completed").count();
    let cancelled = tasks.iter().any(|task| task.status == "cancelled");
    let needs_input = tasks.iter().any(|task| task.status == "needs_input");
    let blocked_reviews = review_blocked(&tasks);
    let status = if cancelled {
        "cancelled"
    } else if needs_input {
        "needs_input"
    } else if !blocked_reviews.is_empty() {
        "needs_review"
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
        repo_root: Some(repo_root),
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
            if report.dirty_source { " (dirty; workers received a source snapshot)" } else { "" }
        ),
    ];
    if let Some(summary) = &report.summary {
        lines.push(format!("plan: {summary}"));
    }
    lines.push(format!("workers: {}", report.tasks.len()));
    for (index, task) in report.tasks.iter().enumerate() {
        lines.push(format!(
            "  [{}] {} · {}/{}{} · {}{}",
            index + 1,
            task.title,
            task.provider,
            task.model,
            task.effort
                .as_deref()
                .map(|effort| format!("@{effort}"))
                .unwrap_or_default(),
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
        if let Some(review) = &task.review_status {
            lines.push(format!("      review: {review}"));
            if let Some(text) = &task.review_text {
                let preview: Vec<&str> = text.lines().take(3).collect();
                if !preview.is_empty() {
                    lines.push(format!("      {}", preview.join(" ")));
                }
            }
        }
        if let Some(question) = task.text.as_deref().and_then(worker_question) {
            lines.push(format!("      question: {question}"));
        } else if let Some(text) = &task.text {
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
                "effort": task.effort,
                "question": task.text.as_deref().and_then(worker_question),
                "prompt": task.prompt,
                "status": task.status,
                "review_status": task.review_status,
                "review_text": task.review_text,
                "reviewed_at": task.reviewed_at,
                "thread_id": task.thread_id,
                "text": task.text,
                "error": task.error,
                "commit": task.commit,
                "snapshot": task.snapshot.as_ref().map(|snapshot| serde_json::json!({
                    "base_ref": snapshot.base_ref,
                    "tree": snapshot.tree,
                })),
                "worktree": task.worktree.as_ref().map(|worktree| serde_json::json!({
                    "path": worktree.path.display().to_string(),
                    "branch": worktree.branch,
                    "base_ref": worktree.base_ref,
                    "shared_paths": worktree.shared_paths,
                })),
                "pr_url": task.pr_url,
                "merged_at": task.merged_at,
                "created_at": task.created_at,
                "finished_at": task.finished_at,
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
        "worker_baseline": if report.dirty_source { "source snapshot" } else { "HEAD" },
        "archived_at": report.archived_at,
        "tasks": tasks,
    })
}

#[derive(Debug, Clone)]
pub struct WorkerFollowupOutcome {
    pub run_id: String,
    pub task_id: String,
    pub title: String,
    pub status: String,
    pub commit: Option<String>,
    pub question: Option<String>,
    pub error: Option<String>,
}

/// Records a normal thread turn as a worker follow-up. Returns None when the
/// thread is not attached to an orchestration task.
pub fn record_worker_followup(
    store: &BatonStore,
    thread_id: &str,
    ok: bool,
    error: Option<&str>,
    phase: TurnPhase,
) -> Result<Option<WorkerFollowupOutcome>> {
    let Some(task) = store.orchestration_task_for_thread(thread_id)? else {
        return Ok(None);
    };
    let text = store
        .list_messages(thread_id)?
        .into_iter()
        .rev()
        .find(|message| message.role == "assistant")
        .map(|message| message.text);
    if !ok {
        if task.commit_sha.is_some() {
            return Ok(Some(WorkerFollowupOutcome {
                run_id: task.run_id,
                task_id: task.id,
                title: task.title,
                status: task.status,
                commit: task.commit_sha,
                question: None,
                error: error.map(str::to_string),
            }));
        }
        let status = if phase == TurnPhase::Interrupted {
            "cancelled"
        } else {
            "failed"
        };
        let detail = error.unwrap_or("worker turn failed");
        store
            .update_orchestration_task_failure(&task.id, status, text.as_deref(), detail)
            .and_then(|()| store.refresh_orchestration_run_status(&task.run_id))
            .map_err(|error| anyhow::anyhow!("worker failure recorded but run update failed: {error}"))?;
        return Ok(Some(WorkerFollowupOutcome {
            run_id: task.run_id,
            task_id: task.id,
            title: task.title,
            status: status.to_string(),
            commit: None,
            question: None,
            error: Some(detail.to_string()),
        }));
    }
    let Some(worktree_path) = task.worktree_path.as_deref() else {
        return Ok(None);
    };
    let path = Path::new(worktree_path);
    if !path.is_dir() {
        return Ok(None);
    }
    let commit = worktree::commit_changes(path, &format!("baton worker: {}", task.title))
        .map_err(|error| anyhow::anyhow!("worker follow-up commit failed: {error}"))?;
    let clear_markers = commit.is_some();
    let question = text.as_deref().and_then(worker_question);
    if question.is_some() {
        store
            .update_orchestration_task_needs_input(
                &task.id,
                text.as_deref(),
                commit.as_deref(),
                clear_markers,
            )
            .map_err(|error| anyhow::anyhow!("worker question recorded but run update failed: {error}"))?;
    } else {
        store
            .update_orchestration_task_success(
                &task.id,
                text.as_deref(),
                commit.as_deref(),
                clear_markers,
            )
            .map_err(|error| anyhow::anyhow!("worker follow-up recorded but run update failed: {error}"))?;
    }
    store.refresh_orchestration_run_status(&task.run_id)?;
    Ok(Some(WorkerFollowupOutcome {
        run_id: task.run_id,
        task_id: task.id,
        title: task.title,
        status: if question.is_some() {
            "needs_input".to_string()
        } else {
            "completed".to_string()
        },
        commit,
        question,
        error: None,
    }))
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
        role_defaults_applied: false,
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
                review_status: task.review_status,
                review_text: task.review_text,
                reviewed_at: task.reviewed_at,
                id: task.id,
                title: task.title,
                provider: task.provider,
                model: task.model,
                effort: task.reasoning_effort,
                prompt: task.prompt,
                worktree,
                snapshot: match (task.snapshot_base, task.snapshot_tree) {
                    (Some(base_ref), Some(tree)) => Some(WorkerSnapshot { base_ref, tree }),
                    _ => None,
                },
                commit: task.commit_sha,
                thread_id: task.thread_id,
                status: task.status,
                text: task.text,
                error: task.error,
                pr_url: task.pr_url,
                merged_at: task.merged_at,
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

#[derive(Debug, Clone)]
pub struct ReconciledRun {
    pub run_id: String,
    pub goal: String,
    pub status: String,
    pub completed_tasks: usize,
    pub failed_tasks: usize,
    pub cancelled_tasks: usize,
    pub needs_input_tasks: usize,
}

/// A run whose orchestrator process is gone can never finish on its own.
/// Mark queued/running tasks failed, keep completed tasks, and settle the run
/// verdict. Runs owned by a live, identity-matching Baton process are left
/// alone. Ownerless rows from older builds are also left alone because their
/// liveness cannot be proven.
pub fn reconcile_abandoned_runs(store: &BatonStore) -> Result<Vec<ReconciledRun>> {
    let mut reconciled = Vec::new();
    for run in store.list_running_orchestration_runs()? {
        let Some(owner_pid) = run.owner_pid else {
            continue;
        };
        if crate::process::process_identity_alive(owner_pid, run.owner_start.as_deref()) {
            continue;
        }
        let error = format!(
            "orchestrator process {owner_pid} exited before the run finished"
        );
        store.abandon_orchestration_run(&run.id, &error)?;
        let tasks = store.list_orchestration_tasks(&run.id)?;
        let completed_tasks = tasks.iter().filter(|task| task.status == "completed").count();
        let failed_tasks = tasks.iter().filter(|task| task.status == "failed").count();
        let cancelled_tasks = tasks.iter().filter(|task| task.status == "cancelled").count();
        let needs_input_tasks = tasks.iter().filter(|task| task.status == "needs_input").count();
        let status = if cancelled_tasks > 0 {
            "cancelled"
        } else if needs_input_tasks > 0 {
            "needs_input"
        } else if !tasks.is_empty() && completed_tasks == tasks.len() {
            "completed"
        } else if completed_tasks > 0 {
            "partial"
        } else {
            "failed"
        };
        reconciled.push(ReconciledRun {
            run_id: run.id,
            goal: run.goal,
            status: status.to_string(),
            completed_tasks,
            failed_tasks,
            cancelled_tasks,
            needs_input_tasks,
        });
    }
    Ok(reconciled)
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

    #[cfg(unix)]
    #[test]
    fn a_failing_before_plan_hook_aborts_the_plan() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("roles.json"),
            r#"{"hooks":{"before_plan":[{"argv":["sh","-c","echo no plan >&2; exit 9"]}]}}"#,
        )
        .unwrap();
        let roles = crate::roles::load(home.path()).unwrap();
        let error = run_planner(
            "ship it",
            home.path(),
            ProviderId::Codex,
            "model",
            4,
            PermissionMode::Default,
            None,
            &[ProviderId::Codex],
            &roles,
            None,
            ProviderId::Codex,
        )
        .unwrap_err();
        assert!(error.to_string().contains("before_plan"), "{error}");
        assert!(error.to_string().contains("no plan"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_after_plan_hook_rejects_the_plan() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("roles.json"),
            r#"{"hooks":{"after_plan":[{"argv":["sh","-c","exit 2"]}]}}"#,
        )
        .unwrap();
        let roles = crate::roles::load(home.path()).unwrap();
        let error = finish_plan(
            fallback_plan("goal", ProviderId::Codex),
            &roles,
            home.path(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("after_plan"), "{error}");

        let clean = tempfile::tempdir().unwrap();
        let roles = crate::roles::load(clean.path()).unwrap();
        let plan = finish_plan(
            fallback_plan("goal", ProviderId::Codex),
            &roles,
            clean.path(),
        )
        .expect("no hooks means the plan passes");
        assert_eq!(plan.tasks.len(), 1);
    }

    #[cfg(unix)]
    fn hook_fixture(
        roles: &str,
        worker_body: &str,
    ) -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("roles.json"), roles).unwrap();
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "hook@example.com"]);
        git(&["config", "user.name", "hook"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        let worker = home.path().join("fake-worker");
        std::fs::write(
            &worker,
            format!("#!/bin/sh\nset -eu\ncd \"$2\"\n{worker_body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
        (home, repo, worker)
    }

    #[cfg(unix)]
    fn hooked_task() -> PlanTask {
        PlanTask {
            title: "hooked task".to_string(),
            provider: "codex".to_string(),
            model: None,
            effort: None,
            prompt: "do the work".to_string(),
        }
    }

    const WORKER_SUCCESS: &str =
        "printf '%s' '{\"thread_id\":\"t\",\"text\":\"done\",\"error\":null,\"phase\":\"completed\"}'";

    #[cfg(unix)]
    #[test]
    fn a_failing_before_implement_hook_stops_the_worker() {
        let (home, repo, worker) = hook_fixture(
            r#"{"hooks":{"before_implement":[{"argv":["sh","-c","echo blocked >&2; exit 4"]}]}}"#,
            &format!("touch worker-ran.txt\n{WORKER_SUCCESS}"),
        );
        let (_, result) = run_worker_inner(
            &hooked_task(),
            0,
            "hook-run-before",
            home.path(),
            repo.path(),
            Some(repo.path()),
            None,
            Some(&worker),
            "implementor",
            None,
            None,
            PermissionMode::Default,
        );
        assert_eq!(result.status, "failed");
        let error = result.error.unwrap_or_default();
        assert!(error.contains("before_implement hook failed"), "{error}");
        assert!(error.contains("blocked"), "{error}");
        assert!(
            !result
                .worktree
                .as_ref()
                .expect("worktree")
                .path
                .join("worker-ran.txt")
                .exists(),
            "the worker must not start after a failing hook"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_passing_after_implement_hook_runs_before_the_commit() {
        let (home, repo, worker) = hook_fixture(
            r#"{"hooks":{"after_implement":[{"argv":["sh","-c","echo stamped > hook.txt"]}]}}"#,
            &format!("echo made > worker.txt\n{WORKER_SUCCESS}"),
        );
        let (_, result) = run_worker_inner(
            &hooked_task(),
            0,
            "hook-run-after",
            home.path(),
            repo.path(),
            Some(repo.path()),
            None,
            Some(&worker),
            "implementor",
            None,
            None,
            PermissionMode::Default,
        );
        assert_eq!(result.status, "completed", "{:?}", result.error);
        let commit = result.commit.expect("a commit with the hook output");
        let worktree = result.worktree.expect("worktree");
        let files = Command::new("git")
            .args(["show", "--name-only", "--format=", &commit])
            .current_dir(&worktree.path)
            .output()
            .unwrap();
        assert!(files.status.success());
        let listed = String::from_utf8_lossy(&files.stdout);
        assert!(listed.contains("hook.txt"), "{listed}");
        assert!(listed.contains("worker.txt"), "{listed}");
    }

    #[test]
    fn progress_json_covers_each_orchestration_event() {
        let plan = progress_json(&OrchestrationProgress::PlanReady {
            summary: Some("two tasks".into()),
            tasks: vec![PlanTask {
                title: "one".into(),
                provider: "codex".into(),
                model: Some("model".into()),
                effort: None,
                prompt: "do one".into(),
            }],
        });
        assert_eq!(plan["schema_version"], SCHEMA_VERSION);
        assert_eq!(plan["type"], "plan");
        assert_eq!(plan["tasks"][0]["provider"], "codex");

        let started = progress_json(&OrchestrationProgress::WorkerStarted {
            index: 1,
            title: "one".into(),
            provider: ProviderId::OpenCode,
            model: "model".into(),
            worktree: Some(PathBuf::from("/tmp/worktree")),
        });
        assert_eq!(started["type"], "worker_started");
        assert_eq!(started["provider"], "opencode");
        assert_eq!(started["worktree"], "/tmp/worktree");

        let finished = progress_json(&OrchestrationProgress::WorkerFinished {
            index: 1,
            title: "one".into(),
            status: "completed".into(),
            commit: Some("abc123".into()),
            error: None,
        });
        assert_eq!(finished["type"], "worker_finished");
        assert_eq!(finished["status"], "completed");
        assert_eq!(finished["commit"], "abc123");
    }

    #[test]
    fn single_task_plan_keeps_the_provider_and_overrides() {
        let plan = single_task_plan(
            "fix the login test",
            ProviderId::OpenCode,
            "provider/model".into(),
            Some("high".into()),
        );
        assert_eq!(plan.summary.as_deref(), Some("single subagent"));
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].provider, "opencode");
        assert_eq!(plan.tasks[0].model.as_deref(), Some("provider/model"));
        assert_eq!(plan.tasks[0].effort.as_deref(), Some("high"));
        assert_eq!(plan.tasks[0].prompt, "fix the login test");
        assert!(plan.role_defaults_applied);
    }

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
        let home = tempfile::tempdir().unwrap();
        let registry = crate::roles::load(home.path()).unwrap();
        let implementor = registry.get(RoleId::Implementor);
        let fallback = fallback_plan_from_role("fix the test", implementor);
        let plan =
            plan_after_planner_failure(&fallback, &anyhow::anyhow!("planner returned no tasks"))
                .unwrap();
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].provider, "codex");
        assert_eq!(plan.tasks[0].model.as_deref(), Some("gpt-6.2-sol"));
        assert_eq!(plan.tasks[0].prompt, "fix the test");

        let cancelled =
            plan_after_planner_failure(&fallback, &anyhow::anyhow!(CANCELLED_ERROR)).unwrap_err();
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
    fn greeting_uses_the_orchestrator_role_without_implementor_normalization() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            crate::roles::config_path(home.path()),
            r#"{"roles":{"orchestrator":{"provider":"codex","model":"orch-model"},"implementor":{"provider":"codex","model":"impl-model","effort":"high"}}}"#,
        )
        .unwrap();
        let first_plan = plan(
            "hi",
            home.path(),
            home.path(),
            ProviderId::Claude,
            "planner-model",
            4,
            None,
            PermissionMode::Default,
            None,
        )
        .unwrap();
        assert_eq!(first_plan.tasks[0].provider, "codex");
        assert_eq!(first_plan.tasks[0].model.as_deref(), Some("orch-model"));
        assert!(first_plan.tasks[0].effort.is_none());

        std::fs::write(
            crate::roles::config_path(home.path()),
            r#"{"roles":{"orchestrator":{"provider":"codex","model":"orch-model","effort":"low"},"implementor":{"provider":"codex","model":"impl-model","effort":"high"}}}"#,
        )
        .unwrap();
        let second_plan = plan(
            "hi",
            home.path(),
            home.path(),
            ProviderId::Claude,
            "planner-model",
            4,
            None,
            PermissionMode::Default,
            None,
        )
        .unwrap();
        assert_eq!(second_plan.tasks[0].effort.as_deref(), Some("low"));
        let worker = worker_prompt("implementor prompt", &second_plan.tasks[0].prompt);
        assert!(worker.contains("implementor prompt"), "{worker}");
    }

    #[test]
    fn default_worker_uses_the_implementor_only_when_it_is_installed() {
        assert_eq!(
            default_worker_provider(
                &[ProviderId::Claude, ProviderId::Codex],
                ProviderId::Claude,
                ProviderId::Codex,
            ),
            ProviderId::Codex
        );
        assert_eq!(
            default_worker_provider(&[ProviderId::Claude], ProviderId::Claude, ProviderId::Codex),
            ProviderId::Claude
        );

        let home = tempfile::tempdir().unwrap();
        let registry = crate::roles::load(home.path()).unwrap();
        let coordinator_only = planner_failure_plan("fix it", &[ProviderId::Claude], &registry);
        assert_eq!(coordinator_only.tasks[0].provider, "claude");
        assert_eq!(
            coordinator_only.tasks[0].model.as_deref(),
            Some("claude-opus-5-5")
        );
        let with_implementor = planner_failure_plan(
            "fix it",
            &[ProviderId::Claude, ProviderId::Codex],
            &registry,
        );
        assert_eq!(with_implementor.tasks[0].provider, "codex");
        assert_eq!(
            with_implementor.tasks[0].model.as_deref(),
            Some("gpt-6.2-sol")
        );
    }

    #[test]
    fn coordinator_command_forwards_the_planner_effort() {
        let command = coordinator_command(
            Path::new("baton"),
            Path::new("/repo"),
            ProviderId::Codex,
            "planner-model",
            Some("high"),
            PermissionMode::Default,
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--effort" && pair[1] == "high"));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "-m" && pair[1] == "planner-model"));
        assert!(command.get_envs().any(|(key, value)| {
            key == "BATON_DISABLE_SKILL_DISPATCH"
                && value.map(|value| value.to_string_lossy() == "1").unwrap_or(false)
        }));
    }

    #[test]
    fn plan_only_resolves_files_and_greetings_without_running_workers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.json");
        std::fs::write(
            &path,
            r#"{"summary":"preview","tasks":[{"title":"worker","provider":"codex","model":"preview-model","effort":"high","prompt":"do it"}]}"#,
        )
        .unwrap();
        let file_plan = plan(
            "preview goal",
            dir.path(),
            dir.path(),
            ProviderId::Codex,
            "coordinator-model",
            4,
            Some(&path),
            PermissionMode::Default,
            None,
        )
        .unwrap();
        assert_eq!(file_plan.summary.as_deref(), Some("preview"));
        assert_eq!(file_plan.tasks[0].model.as_deref(), Some("preview-model"));
        assert_eq!(file_plan.tasks[0].effort.as_deref(), Some("high"));

        let greeting = plan(
            "hi",
            dir.path(),
            dir.path(),
            ProviderId::Claude,
            "coordinator-model",
            4,
            None,
            PermissionMode::Default,
            None,
        )
        .unwrap();
        assert_eq!(greeting.tasks.len(), 1);
        assert_eq!(greeting.tasks[0].provider, "claude");
        assert_eq!(greeting.tasks[0].model.as_deref(), Some("claude-opus-5-5"));

        assert!(plan(
            "goal",
            dir.path(),
            dir.path(),
            ProviderId::Codex,
            "coordinator-model",
            0,
            Some(&path),
            PermissionMode::Default,
            None,
        )
        .is_err());

        assert!(plan(
            &"x".repeat(MAX_ORCHESTRATION_GOAL_BYTES + 1),
            dir.path(),
            dir.path(),
            ProviderId::Codex,
            "coordinator-model",
            4,
            Some(&path),
            PermissionMode::Default,
            None,
        )
        .is_err());

        assert!(plan(
            "goal",
            dir.path(),
            dir.path(),
            ProviderId::Codex,
            "coordinator-model",
            MAX_TASKS + 1,
            Some(&path),
            PermissionMode::Default,
            None,
        )
        .is_err());
    }

    #[test]
    fn a_saved_greeting_preview_keeps_its_orchestrator_defaults() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            crate::roles::config_path(home.path()),
            r#"{"roles":{"orchestrator":{"provider":"codex","model":"orch-model"},"implementor":{"provider":"codex","model":"impl-model","effort":"high"}}}"#,
        )
        .unwrap();
        let greeting = plan(
            "hi",
            home.path(),
            home.path(),
            ProviderId::Codex,
            "coordinator-model",
            4,
            None,
            PermissionMode::Default,
            None,
        )
        .unwrap();
        assert_eq!(greeting.tasks[0].model.as_deref(), Some("orch-model"));
        assert_eq!(greeting.tasks[0].effort, None);
        assert!(greeting.role_defaults_applied);

        let path = home.path().join("preview.json");
        std::fs::write(&path, serde_json::to_string(&greeting).unwrap()).unwrap();
        let reloaded = plan(
            "hi",
            home.path(),
            home.path(),
            ProviderId::Codex,
            "coordinator-model",
            4,
            Some(&path),
            PermissionMode::Default,
            None,
        )
        .unwrap();
        assert_eq!(reloaded.tasks[0].model.as_deref(), Some("orch-model"));
        assert_eq!(reloaded.tasks[0].effort, None);
    }

    #[test]
    fn planner_prompt_asks_for_the_smallest_plan_and_lists_available_harnesses() {
        let home = tempfile::tempdir().unwrap();
        let registry = crate::roles::load(home.path()).unwrap();
        let prompt = planner_prompt(
            "fix the bug",
            Path::new("/repo"),
            4,
            &[ProviderId::Codex, ProviderId::Claude],
            &registry,
            ProviderId::Codex,
        );
        assert!(prompt.contains("smallest number of tasks"), "{prompt}");
        assert!(prompt.contains("exactly one task"), "{prompt}");
        assert!(prompt.contains("codex, claude"), "{prompt}");
        assert!(prompt.contains("Default every task to codex"), "{prompt}");
        assert!(prompt.contains("You are Baton's orchestrator"), "{prompt}");
        assert!(prompt.contains("You are Baton's planner"), "{prompt}");
        assert!(prompt.contains("take precedence if they conflict"), "{prompt}");
        assert!(prompt.contains("Never ask a worker to coordinate"), "{prompt}");
        assert!(prompt.contains("explicitly names a model"), "{prompt}");
        assert!(prompt.contains("role default for the default worker provider"), "{prompt}");
        assert!(prompt.contains("explicitly names a reasoning effort"), "{prompt}");
        assert!(prompt.contains("Only codex and claude support effort"), "{prompt}");
        assert!(
            prompt.contains("runs a reviewer after each completed worker"),
            "{prompt}"
        );
    }

    #[test]
    fn implementor_role_defaults_fill_matching_tasks_without_overriding_explicit_values() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            crate::roles::config_path(home.path()),
            r#"{"roles":{"implementor":{"provider":"codex","model":"custom-impl","effort":"xhigh"}}}"#,
        )
        .unwrap();
        let path = home.path().join("plan.json");
        std::fs::write(
            &path,
            r#"{"summary":"roles","tasks":[
                {"title":"inherits","provider":"codex","prompt":"one"},
                {"title":"explicit","provider":"codex","model":"explicit","effort":"low","prompt":"two"},
                {"title":"other","provider":"claude","prompt":"three"}
            ]}"#,
        )
        .unwrap();
        let plan = plan(
            "goal",
            home.path(),
            home.path(),
            ProviderId::Codex,
            "coordinator-model",
            4,
            Some(&path),
            PermissionMode::Default,
            None,
        )
        .unwrap();

        assert_eq!(plan.tasks[0].model.as_deref(), Some("custom-impl"));
        assert_eq!(plan.tasks[0].effort.as_deref(), Some("xhigh"));
        assert_eq!(plan.tasks[1].model.as_deref(), Some("explicit"));
        assert_eq!(plan.tasks[1].effort.as_deref(), Some("low"));
        assert!(plan.tasks[2].model.is_none());
        assert!(plan.tasks[2].effort.is_none());
    }

    #[test]
    fn worker_prompt_keeps_the_preamble_before_the_role_and_task() {
        let prompt = worker_prompt("implementor role", "task body");
        let preamble = prompt.find(WORKER_PREAMBLE).unwrap();
        let role = prompt.find("implementor role").unwrap();
        let task = prompt.find("task body").unwrap();
        assert!(preamble < role && role < task, "{prompt}");
        assert!(prompt.starts_with(WORKER_PREAMBLE), "{prompt}");
    }

    #[test]
    fn planner_prompt_does_not_rewrite_placeholders_inside_user_role_text() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("roles")).unwrap();
        std::fs::write(
            crate::roles::prompt_path(home.path(), RoleId::Orchestrator),
            "orchestrator MAX GOAL CWD",
        )
        .unwrap();
        std::fs::write(
            crate::roles::prompt_path(home.path(), RoleId::Planner),
            "planner PROVIDERS IMPLEMENTOR_PROVIDER",
        )
        .unwrap();
        let registry = crate::roles::load(home.path()).unwrap();
        let prompt = planner_prompt(
            "fix CWD with MAX PROVIDERS",
            Path::new("/repo"),
            4,
            &[ProviderId::Codex],
            &registry,
            ProviderId::Codex,
        );
        assert!(prompt.contains("orchestrator MAX GOAL CWD"), "{prompt}");
        assert!(
            prompt.contains("planner PROVIDERS IMPLEMENTOR_PROVIDER"),
            "{prompt}"
        );
        assert!(
            prompt.contains("Goal: fix CWD with MAX PROVIDERS"),
            "{prompt}"
        );
        assert!(prompt.contains("Source repository: /repo"), "{prompt}");
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

        let oversized = format!(
            r#"{{"tasks":[{{"title":"huge","provider":"codex","prompt":"{}"}}]}}"#,
            "x".repeat(MAX_TASK_PROMPT_BYTES + 1)
        );
        assert!(parse_plan(&oversized, 4)
            .unwrap_err()
            .to_string()
            .contains("larger than"));
    }

    #[test]
    fn plan_task_model_and_effort_are_trimmed_and_optional() {
        let plan = parse_plan(
            r#"{"tasks":[{"title":"custom","provider":"codex","model":"  custom-model  ","effort":" High ","prompt":"do it"},{"title":"default","provider":"codex","model":"   ","effort":"  ","prompt":"do it too"}]}"#,
            4,
        )
        .unwrap();
        assert_eq!(plan.tasks[0].model.as_deref(), Some("custom-model"));
        assert_eq!(plan.tasks[0].effort.as_deref(), Some("high"));
        assert_eq!(plan.tasks[1].model, None);
        assert_eq!(plan.tasks[1].effort, None);

        let invalid = parse_plan(
            r#"{"tasks":[{"title":"bad","provider":"codex","effort":"high effort","prompt":"do it"}]}"#,
            4,
        )
        .unwrap_err();
        assert!(invalid.to_string().contains("invalid reasoning effort"));

        let unsafe_model = parse_plan(
            r#"{"tasks":[{"title":"bad","provider":"codex","model":"bad model\n--foo","prompt":"do it"}]}"#,
            4,
        )
        .unwrap_err();
        assert!(unsafe_model.to_string().contains("invalid model"));
    }

    #[test]
    fn rejects_unknown_provider() {
        let text = r#"{"tasks":[{"title":"a","provider":"not-a-provider","prompt":"do a"}]}"#;
        assert!(parse_plan(text, 4).is_err());
    }

    #[test]
    fn plan_task_model_and_effort_override_the_worker_command() {
        let plan = r#"{"summary":"model override","tasks":[{"title":"worker","provider":"codex","model":"custom-worker-model","effort":"high","prompt":"do it"}]}"#;
        let (_repo, _home, store, report) = run_fixture(
            plan,
            "printf '%s\n' \"$@\" > worker-args.txt\nprintf '%s\n' \"$BATON_DISABLE_SKILL_DISPATCH\" > worker-env.txt",
        );
        assert_eq!(report.status, "completed");
        assert_eq!(report.tasks[0].model, "custom-worker-model");
        assert_eq!(report.tasks[0].effort.as_deref(), Some("high"));
        let worktree = report.tasks[0].worktree.as_ref().unwrap();
        let args = std::fs::read_to_string(worktree.path.join("worker-args.txt")).unwrap();
        assert_eq!(
            std::fs::read_to_string(worktree.path.join("worker-env.txt"))
                .unwrap()
                .trim(),
            "1"
        );
        let lines: Vec<&str> = args.lines().collect();
        let model_index = lines
            .iter()
            .position(|line| *line == "-m")
            .expect("worker command carries -m");
        assert_eq!(lines.get(model_index + 1), Some(&"custom-worker-model"));
        let effort_index = lines
            .iter()
            .position(|line| *line == "--effort")
            .expect("worker command carries --effort");
        assert_eq!(lines.get(effort_index + 1), Some(&"high"));

        let stored = report_from_rows(&store, _repo.path(), &report.run_id).unwrap();
        assert_eq!(stored.tasks[0].effort.as_deref(), Some("high"));
        assert_eq!(to_json(&stored)["tasks"][0]["effort"], "high");
    }

    #[test]
    fn worker_question_marks_needs_input_and_blocks_integration_until_answered() {
        let plan = r#"{"tasks":[{"title":"question worker","provider":"codex","prompt":"ask before finishing"}]}"#;
        let question = "BATON_QUESTION: approve the worker change?";
        let (repo, _home, store, report) = run_fixture_with_worker_text(
            plan,
            "printf 'worker change\n' > worker.txt",
            question,
        );
        assert_eq!(report.status, "needs_input");
        let task = &report.tasks[0];
        assert_eq!(task.status, "needs_input");
        assert!(task.commit.is_some(), "needs_input still commits worker changes");
        assert_eq!(
            worker_question(task.text.as_deref().unwrap()).as_deref(),
            Some("approve the worker change?")
        );
        assert_eq!(
            to_json(&report)["tasks"][0]["question"],
            "approve the worker change?"
        );
        assert!(render(&report).contains("question: approve the worker change?"));

        let error = integrate(&store, repo.path(), &report.run_id, &[]).unwrap_err();
        assert!(error.to_string().contains("waiting for input"), "{error}");
        let error = open_prs(&store, repo.path(), &report.run_id, &[]).unwrap_err();
        assert!(error.to_string().contains("waiting for input"), "{error}");

        let stored = store.list_orchestration_tasks(&report.run_id).unwrap().remove(0);
        store
            .update_orchestration_task_success(&stored.id, Some("answered"), None, false)
            .unwrap();
        store.refresh_orchestration_run_status(&report.run_id).unwrap();
        let run = store
            .find_orchestration_runs(&report.run_id)
            .unwrap()
            .into_iter()
            .find(|run| run.id == report.run_id)
            .unwrap();
        assert_eq!(run.status, "completed");
        let integrated = integrate(&store, repo.path(), &report.run_id, &[]).unwrap();
        assert_eq!(integrated.merged.len(), 1);

        let reloaded = report_from_rows(&store, repo.path(), &report.run_id).unwrap();
        let json = to_json(&reloaded);
        assert_eq!(json["status"], "completed");
        assert_eq!(json["goal"], "test worker question");
        assert!(json["tasks"][0]["created_at"].as_str().is_some());
        assert!(
            json["tasks"][0]["merged_at"].as_str().is_some(),
            "the reloaded report keeps the merge marker"
        );
    }

    #[allow(clippy::type_complexity)]
    fn followup_fixture(
        run_id: &str,
        task_status: &str,
        commit: Option<&str>,
        assistant_text: &str,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        BatonStore,
        crate::store::ThreadRow,
        crate::worktree::WorktreeInfo,
    ) {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let home = tempfile::tempdir().unwrap();
        let repo_root = crate::worktree::repo_root(repo.path()).unwrap();
        let worktree =
            crate::worktree::create(home.path(), &repo_root, run_id, "worker task").unwrap();
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(crate::store::NewThread {
                title: "worker".to_string(),
                cwd: worktree.path.to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: Some("model".to_string()),
            })
            .unwrap();
        store
            .create_orchestration_run(
                run_id,
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
                &format!("task-{run_id}"),
                run_id,
                1,
                "worker task",
                ProviderId::Codex,
                "model",
                "prompt",
                Some(&worktree.path.to_string_lossy()),
                Some(&worktree.branch),
                commit,
                Some(&thread.id),
                task_status,
                Some("old answer"),
                None,
                "now",
                Some("now"),
                None,
                None,
                None,
            )
            .unwrap();
        if commit.is_some() {
            store
                .mark_orchestration_task_merged(&format!("task-{run_id}"), "merged")
                .unwrap();
            store
                .mark_orchestration_task_pr(
                    &format!("task-{run_id}"),
                    "https://example.test/pr/1",
                )
                .unwrap();
        }
        store
            .add_message(&thread.id, 1, "assistant", Some("codex"), None, assistant_text)
            .unwrap();
        (repo, home, store, thread, worktree)
    }

    fn task_row(store: &BatonStore, run_id: &str) -> crate::store::OrchestrationTaskRow {
        store
            .orchestration_task_for_thread(
                &store.list_orchestration_tasks(run_id).unwrap()[0]
                    .thread_id
                    .clone()
                    .unwrap(),
            )
            .unwrap()
            .unwrap()
    }

    fn run_status(store: &BatonStore, run_id: &str) -> String {
        store
            .find_orchestration_runs(run_id)
            .unwrap()
            .into_iter()
            .find(|run| run.id == run_id)
            .unwrap()
            .status
    }

    #[test]
    fn followup_ignores_threads_without_an_orchestration_task() {
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(crate::store::NewThread {
                title: "plain".to_string(),
                cwd: "/tmp".to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let outcome =
            record_worker_followup(&store, &thread.id, true, None, TurnPhase::Completed).unwrap();
        assert!(outcome.is_none());
    }

    #[test]
    fn followup_question_without_new_work_keeps_merge_markers() {
        let (_repo, _home, store, thread, _worktree) = followup_fixture(
            "run-question",
            "needs_input",
            Some("old-commit"),
            "BATON_QUESTION: use green",
        );
        let outcome = record_worker_followup(
            &store,
            &thread.id,
            true,
            None,
            TurnPhase::Completed,
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.status, "needs_input");
        assert_eq!(outcome.question.as_deref(), Some("use green"));
        assert!(outcome.commit.is_none());
        let task = task_row(&store, "run-question");
        assert_eq!(task.status, "needs_input");
        assert_eq!(
            worker_question(task.text.as_deref().unwrap()).as_deref(),
            Some("use green")
        );
        assert_eq!(task.commit_sha.as_deref(), Some("old-commit"));
        assert!(task.merged_at.is_some(), "marker survives a no-op answer");
        assert!(task.pr_url.is_some(), "PR marker survives a no-op answer");
        assert_eq!(run_status(&store, "run-question"), "needs_input");
    }

    #[test]
    fn followup_with_new_work_commits_and_completes_the_task() {
        let (_repo, _home, store, thread, worktree) = followup_fixture(
            "run-complete",
            "needs_input",
            Some("old-commit"),
            "used green",
        );
        std::fs::write(worktree.path.join("answer.txt"), "green\n").unwrap();
        let outcome = record_worker_followup(
            &store,
            &thread.id,
            true,
            None,
            TurnPhase::Completed,
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.status, "completed");
        assert!(outcome.question.is_none());
        let commit = outcome.commit.expect("new work is committed");
        assert_ne!(commit, "old-commit");
        let task = task_row(&store, "run-complete");
        assert_eq!(task.status, "completed");
        assert_eq!(task.commit_sha.as_deref(), Some(commit.as_str()));
        assert_eq!(task.text.as_deref(), Some("used green"));
        assert!(task.merged_at.is_none(), "new work invalidates the merge marker");
        assert!(task.pr_url.is_none(), "new work invalidates the PR marker");
        assert_eq!(
            task.review_status.as_deref(),
            Some(REVIEW_PENDING),
            "new work must be reviewed again"
        );
        assert_eq!(run_status(&store, "run-complete"), "needs_review");
    }

    #[test]
    fn followup_failure_after_a_commit_does_not_roll_the_task_back() {
        let (_repo, _home, store, thread, _worktree) = followup_fixture(
            "run-committed",
            "completed",
            Some("old-commit"),
            "old answer",
        );
        let outcome = record_worker_followup(
            &store,
            &thread.id,
            false,
            Some("follow-up failed"),
            TurnPhase::Failed,
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.status, "completed");
        assert_eq!(outcome.commit.as_deref(), Some("old-commit"));
        assert_eq!(outcome.error.as_deref(), Some("follow-up failed"));
        let task = task_row(&store, "run-committed");
        assert_eq!(task.status, "completed");
        assert_eq!(task.commit_sha.as_deref(), Some("old-commit"));
        assert!(task.error.is_none());
        assert!(task.merged_at.is_some());
        assert!(task.pr_url.is_some());
    }

    #[test]
    fn followup_failure_without_a_commit_marks_the_task_failed() {
        let (_repo, _home, store, thread, _worktree) = followup_fixture(
            "run-failed",
            "running",
            None,
            "",
        );
        let outcome = record_worker_followup(
            &store,
            &thread.id,
            false,
            Some("provider crashed"),
            TurnPhase::Failed,
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.status, "failed");
        assert!(outcome.commit.is_none());
        let task = task_row(&store, "run-failed");
        assert_eq!(task.status, "failed");
        assert_eq!(task.error.as_deref(), Some("provider crashed"));
        assert_eq!(run_status(&store, "run-failed"), "failed");
    }

    #[test]
    fn unsupported_provider_effort_fails_the_task_with_a_clear_error() {
        let plan = r#"{"tasks":[{"title":"worker","provider":"opencode","effort":"high","prompt":"do it"}]}"#;
        let (_repo, _home, _store, report) = run_fixture(
            plan,
            "printf '%s\n' \"$@\" > worker-args.txt",
        );
        assert_eq!(report.status, "failed");
        assert_eq!(report.tasks[0].status, "failed");
        assert!(report.tasks[0].worktree.is_none());
        assert!(report.tasks[0]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("does not support reasoning effort"));
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

    /// These fixtures emulate a single harness turn, so the review gate is
    /// turned off unless a test is about the reviewer itself.
    fn no_review(home: &Path) {
        std::fs::write(home.join("roles.json"), r#"{"review":false}"#).unwrap();
    }

    fn worker_script_for_text(dir: &Path, name: &str, body: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        let result = serde_json::json!({
            "schema_version": 1,
            "thread_id": "fake-thread",
            "ok": true,
            "phase": "completed",
            "error": null,
            "text": text,
        });
        let json = format!("printf '%s\n' '{}'\n", serde_json::to_string(&result).unwrap());
        let script = format!("#!/bin/sh\ncd \"$2\" || exit 1\n{body}\n{json}");
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn worker_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        worker_script_for_text(dir, name, body, "FAKE_OK")
    }

    fn run_fixture_with_worker_text(
        plan: &str,
        worker_body: &str,
        worker_text: &str,
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
        no_review(home.path());
        let worker = worker_script_for_text(home.path(), "question-worker", worker_body, worker_text);
        let plan_path = home.path().join("plan.json");
        std::fs::write(&plan_path, plan).unwrap();
        let store = BatonStore::memory().unwrap();
        let report = run_with_worker_bin_and_progress(
            "test worker question",
            repo.path(),
            home.path(),
            ProviderId::Codex,
            "test-model",
            4,
            Some(&plan_path),
            &store,
            Some(&worker),
            None,
            None,
            PermissionMode::Default,
        )
        .unwrap();
        (repo, home, store, report)
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
        no_review(home.path());
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
    fn dirty_source_snapshot_reaches_workers_without_entering_the_worker_commit() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(repo.path().join("a.txt"), "base\n").unwrap();
        std::fs::write(repo.path().join("b.txt"), "base b\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let base = git(&["rev-parse", "HEAD"]);
        std::fs::write(repo.path().join("a.txt"), "dirty\n").unwrap();
        std::fs::write(repo.path().join("b.txt"), "dirty b\n").unwrap();
        std::fs::write(repo.path().join("source-only.txt"), "source\n").unwrap();

        let home = tempfile::tempdir().unwrap();
        let plan_path = repo.path().join("plan.json");
        std::fs::write(
            &plan_path,
            r#"{"summary":"snapshot worker","tasks":[{"title":"worker","provider":"codex","prompt":"inspect the snapshot"}]}"#,
        )
        .unwrap();
        let store = BatonStore::memory().unwrap();
        no_review(home.path());
        let worker = worker_script(
            home.path(),
            "snapshot-worker",
            "cp a.txt seen.txt\nprintf 'worker final\\n' > a.txt\nprintf 'worker\\n' > worker.txt",
        );
        let report = run_with_worker_bin(
            "test snapshot",
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
        assert!(report.dirty_source);
        let task = &report.tasks[0];
        let worktree = task.worktree.as_ref().unwrap();
        assert_eq!(
            git(&["show", &format!("{}:seen.txt", worktree.branch)]),
            "dirty"
        );
        assert_eq!(
            git(&["show", &format!("{}:worker.txt", worktree.branch)]),
            "worker"
        );
        let source_only = Command::new("git")
            .args(["show", &format!("{}:source-only.txt", worktree.branch)])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(
            !source_only.status.success(),
            "untouched dirty source files must stay out of the worker branch"
        );
        assert_eq!(
            git(&["show", &format!("{}:a.txt", worktree.branch)]),
            "worker final",
            "a worker edit to a dirty file commits the final content"
        );
        assert_eq!(
            git(&["show", &format!("{}:b.txt", worktree.branch)]),
            "base b",
            "an untouched dirty tracked file stays at the source HEAD content"
        );
        assert_eq!(
            git(&["rev-parse", &format!("{}^", worktree.branch)]),
            base,
            "the worker commit stays directly on the source base"
        );
        let worktree_status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&worktree.path)
            .output()
            .unwrap();
        assert!(
            worktree_status.status.success()
                && String::from_utf8_lossy(&worktree_status.stdout).trim().is_empty(),
            "worker worktree is clean after its commit"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
            "dirty\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("b.txt")).unwrap(),
            "dirty b\n"
        );
        assert!(repo.path().join("source-only.txt").exists());
    }

    #[test]
    fn integrates_worker_after_source_commits_snapshot_changes() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(repo.path().join("a.txt"), "base\n").unwrap();
        std::fs::write(repo.path().join("b.txt"), "base b\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::write(repo.path().join("a.txt"), "source dirty\n").unwrap();
        std::fs::write(repo.path().join("b.txt"), "source dirty b\n").unwrap();
        std::fs::write(repo.path().join("source-only.txt"), "source\n").unwrap();

        let home = tempfile::tempdir().unwrap();
        let plan_path = repo.path().join("plan.json");
        std::fs::write(
            &plan_path,
            r#"{"summary":"snapshot worker","tasks":[{"title":"worker","provider":"codex","prompt":"edit the snapshot"}]}"#,
        )
        .unwrap();
        let store = BatonStore::memory().unwrap();
        no_review(home.path());
        let worker = worker_script(
            home.path(),
            "snapshot-integrator",
            "test \"$(cat b.txt)\" = \"source dirty b\"
printf 'worker final\n' > a.txt
printf 'worker\n' > worker.txt",
        );
        let report = run_with_worker_bin(
            "test snapshot integration",
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
        assert!(report.dirty_source);

        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "source snapshot committed"]);

        let stored = report_from_rows(&store, repo.path(), &report.run_id).unwrap();
        assert!(
            stored.tasks[0].snapshot.is_some(),
            "snapshot metadata survives a durable report reload"
        );
        assert!(
            to_json(&stored)["tasks"][0]["snapshot"]["tree"].is_string(),
            "the orchestration JSON exposes the persisted snapshot tree"
        );

        let integrated = integrate(&store, repo.path(), &report.run_id, &[]).unwrap();
        assert!(integrated.conflict.is_none(), "snapshot-aware integration must not conflict");
        assert_eq!(integrated.merged.len(), 1);
        assert_eq!(
            std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
            "worker final\n",
            "the worker edit wins over the unchanged snapshot content"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("b.txt")).unwrap(),
            "source dirty b\n",
            "an untouched snapshot file keeps the source commit's content"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("source-only.txt")).unwrap(),
            "source\n",
            "an untouched snapshot-only file survives integration"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("worker.txt")).unwrap(),
            "worker\n"
        );
    }

    #[test]
    fn snapshot_integration_reports_conflict_without_dirtying_the_source() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(repo.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::write(repo.path().join("a.txt"), "source dirty\n").unwrap();

        let home = tempfile::tempdir().unwrap();
        let plan_path = repo.path().join("plan.json");
        std::fs::write(
            &plan_path,
            r#"{"summary":"snapshot worker","tasks":[{"title":"worker","provider":"codex","prompt":"edit the snapshot"}]}"#,
        )
        .unwrap();
        let store = BatonStore::memory().unwrap();
        no_review(home.path());
        let worker = worker_script(
            home.path(),
            "snapshot-conflict",
            "printf 'worker final\n' > a.txt",
        );
        let report = run_with_worker_bin(
            "test snapshot conflict",
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

        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "source snapshot"]);
        std::fs::write(repo.path().join("a.txt"), "source later\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "source later"]);
        let head = git(&["rev-parse", "HEAD"]);

        let integrated = integrate(&store, repo.path(), &report.run_id, &[]).unwrap();
        assert!(integrated.merged.is_empty());
        let conflict = integrated.conflict.expect("both sides changed a.txt");
        assert_eq!(conflict.index, 1);
        assert!(conflict.files.contains(&"a.txt".to_string()));
        assert_eq!(
            git(&["rev-parse", "HEAD"]),
            head,
            "a conflicted snapshot merge does not move the source HEAD"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
            "source later\n"
        );
        assert_eq!(
            git(&["status", "--porcelain"]),
            "",
            "a conflicted snapshot merge leaves no merge state behind"
        );
    }

    #[test]
    fn panic_fallback_keeps_task_identity_and_position() {
        let fallback = WorkerFallback {
            index: 2,
            title: "second task".to_string(),
            provider: ProviderId::Claude,
            model: "claude-test".to_string(),
            effort: Some("high".to_string()),
            prompt: "do two".to_string(),
        };
        let (index, result) = panicked_worker(&fallback);
        assert_eq!(index, 2, "position survives a panic");
        assert_eq!(result.title, "second task");
        assert_eq!(result.provider, ProviderId::Claude);
        assert_eq!(result.model, "claude-test");
        assert_eq!(result.effort.as_deref(), Some("high"));
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
                    model: None,
                    effort: None,
                    prompt: "a".to_string(),
                },
                PlanTask {
                    title: "b".to_string(),
                    provider: "claude".to_string(),
                    model: None,
                    effort: None,
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
        no_review(home.path());
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
        assert_eq!(report.coordinator_provider, ProviderId::Codex);
        assert_eq!(report.coordinator_model, "test-model");
        assert_eq!(report.tasks.len(), 1);
        assert_eq!(report.tasks[0].model, "gpt-6.2-sol");
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
        assert!(
            report
                .tasks
                .iter()
                .all(|task| task.status == "completed" && task.commit.is_some()),
            "both workers must complete with commits: {:#?}",
            report.tasks
        );

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
        assert_eq!(
            shipped.opened.len(),
            2,
            "both worker branches open a PR: opened={:?} skipped={:?} failed={:?}",
            shipped.opened,
            shipped.skipped,
            shipped.failed
        );
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

    #[test]
    fn review_verdicts_fail_closed() {
        let cases = [
            ("APPROVE", REVIEW_APPROVE),
            ("approve", REVIEW_APPROVE),
            ("Verdict: APPROVE", REVIEW_APPROVE),
            ("REQUEST_CHANGES", REVIEW_REQUEST_CHANGES),
            (
                "request changes: the error path is untested",
                REVIEW_REQUEST_CHANGES,
            ),
            ("Looks good overall\nAPPROVE\n", REVIEW_APPROVE),
            ("I cannot approve this diff", REVIEW_REQUEST_CHANGES),
            ("APPROVES nothing", REVIEW_REQUEST_CHANGES),
            ("", REVIEW_REQUEST_CHANGES),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_review_verdict(text), expected, "{text:?}");
        }
    }

    #[cfg(unix)]
    fn review_fixture(
        roles: Option<&str>,
        reviewer_text: &str,
        reviewer_marker: bool,
    ) -> (tempfile::TempDir, tempfile::TempDir, BatonStore, RunReport) {
        use std::os::unix::fs::PermissionsExt;
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let home = tempfile::tempdir().unwrap();
        let plan_path = home.path().join("plan.json");
        std::fs::write(
            &plan_path,
            r#"{"summary":"one worker","tasks":[{"title":"first task","provider":"codex","prompt":"do the work"}]}"#,
        )
        .unwrap();
        if let Some(roles) = roles {
            std::fs::write(home.path().join("roles.json"), roles).unwrap();
        }
        let worker_json = serde_json::json!({
            "schema_version": 1,
            "thread_id": "worker-thread",
            "ok": true,
            "phase": "completed",
            "error": null,
            "text": "worker done",
        });
        let review_json = serde_json::json!({
            "schema_version": 1,
            "thread_id": "review-thread",
            "ok": true,
            "phase": "completed",
            "error": null,
            "text": reviewer_text,
        });
        let marker = if reviewer_marker {
            "touch \"$BATON_HOME/review-ran.txt\"\n"
        } else {
            ""
        };
        let script = format!(
            "#!/bin/sh\nset -eu\ncd \"$2\" || exit 1\nif [ \"${{BATON_ROLE:-}}\" = reviewer ]; then\n{marker}  printf '%s\\n' '{review_json}'\nelse\n  printf 'work\\n' > worker.txt\n  printf '%s\\n' '{worker_json}'\nfi\n"
        );
        let worker = home.path().join("review-harness");
        std::fs::write(&worker, script).unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let store = BatonStore::memory().unwrap();
        let report = run_with_worker_bin(
            "review fixture",
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
        (repo, home, store, report)
    }

    #[cfg(unix)]
    #[test]
    fn a_reviewer_approval_completes_the_run_and_integration() {
        let (repo, _home, store, report) = review_fixture(None, "APPROVE", false);
        assert_eq!(report.status, "completed");
        let task = &report.tasks[0];
        assert_eq!(task.review_status.as_deref(), Some(REVIEW_APPROVE));
        assert_eq!(task.review_text.as_deref(), Some("APPROVE"));
        let rows = store.list_orchestration_tasks(&report.run_id).unwrap();
        assert_eq!(rows[0].review_status.as_deref(), Some(REVIEW_APPROVE));
        assert!(rows[0].reviewed_at.is_some());
        let integrated = integrate(&store, repo.path(), &report.run_id, &[]).unwrap();
        assert_eq!(integrated.merged.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn request_changes_blocks_integration_and_shipping() {
        let (repo, home, store, report) =
            review_fixture(None, "REQUEST_CHANGES\nThe error path is untested.", false);
        assert_eq!(report.status, "needs_review");
        assert_eq!(
            report.tasks[0].review_status.as_deref(),
            Some(REVIEW_REQUEST_CHANGES)
        );
        let error = integrate(&store, repo.path(), &report.run_id, &[]).unwrap_err();
        let error = error.to_string();
        assert!(error.contains("review did not approve"), "{error}");
        assert!(error.contains("first task"), "{error}");
        let gh = fake_gh(home.path());
        let error =
            open_prs_with_gh(&store, repo.path(), &report.run_id, &[], Some(&gh)).unwrap_err();
        assert!(
            error.to_string().contains("review did not approve"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_followup_commit_stays_pending_until_the_reviewer_reads_it() {
        let (repo, home, store, report) = review_fixture(None, "APPROVE", false);
        let worktree = report.tasks[0].worktree.clone().expect("worktree");
        std::fs::write(worktree.path.join("followup.txt"), "new work\n").unwrap();
        let outcome =
            record_worker_followup(&store, "worker-thread", true, None, TurnPhase::Completed)
                .unwrap()
                .expect("a follow-up for the worker thread");
        assert_eq!(outcome.status, "completed");
        let rows = store.list_orchestration_tasks(&report.run_id).unwrap();
        assert_eq!(rows[0].review_status.as_deref(), Some(REVIEW_PENDING));
        let error = integrate(&store, repo.path(), &report.run_id, &[]).unwrap_err();
        let error = error.to_string();
        assert!(error.contains("review did not approve"), "{error}");
        assert!(error.contains("first task"), "{error}");

        let harness = home.path().join("review-harness");
        let refreshed = review_run_with_bin(
            &store,
            home.path(),
            repo.path(),
            &report.run_id,
            &[],
            None,
            Some(&harness),
        )
        .unwrap();
        assert_eq!(
            refreshed.tasks[0].review_status.as_deref(),
            Some(REVIEW_APPROVE)
        );
        assert_eq!(refreshed.status, "completed");
        let integrated = integrate(&store, repo.path(), &report.run_id, &[]).unwrap();
        assert_eq!(integrated.merged.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn auto_review_followup_approves_a_pending_task() {
        let (repo, home, store, report) = review_fixture(None, "APPROVE", false);
        let worktree = report.tasks[0].worktree.clone().expect("worktree");
        std::fs::write(worktree.path.join("followup.txt"), "new work\n").unwrap();
        let outcome =
            record_worker_followup(&store, "worker-thread", true, None, TurnPhase::Completed)
                .unwrap()
                .expect("follow-up outcome");
        let harness = home.path().join("review-harness");
        let review = auto_review_followup_with_bin(
            &store,
            home.path(),
            repo.path(),
            &outcome,
            None,
            Some(&harness),
        )
        .unwrap()
        .expect("auto review runs");
        assert_eq!(review.status, REVIEW_APPROVE);
        assert_eq!(review.task_id, report.tasks[0].id);
        assert_eq!(review.run_id, report.run_id);
        let rows = store.list_orchestration_tasks(&report.run_id).unwrap();
        assert_eq!(rows[0].review_status.as_deref(), Some(REVIEW_APPROVE));

        let mut missing = outcome.clone();
        missing.task_id = "missing-task".to_string();
        let none = auto_review_followup_with_bin(
            &store,
            home.path(),
            repo.path(),
            &missing,
            None,
            Some(&harness),
        )
        .unwrap();
        assert!(none.is_none(), "an unknown task does not start a review");
    }

    #[cfg(unix)]
    #[test]
    fn auto_review_followup_honors_the_review_setting() {
        let (repo, home, store, report) =
            review_fixture(Some(r#"{"review":false}"#), "APPROVE", false);
        let worktree = report.tasks[0].worktree.clone().expect("worktree");
        std::fs::write(worktree.path.join("followup.txt"), "new work\n").unwrap();
        let outcome =
            record_worker_followup(&store, "worker-thread", true, None, TurnPhase::Completed)
                .unwrap()
                .expect("follow-up outcome");
        let harness = home.path().join("review-harness");
        let none = auto_review_followup_with_bin(
            &store,
            home.path(),
            repo.path(),
            &outcome,
            None,
            Some(&harness),
        )
        .unwrap();
        assert!(none.is_none());
        let rows = store.list_orchestration_tasks(&report.run_id).unwrap();
        assert_eq!(rows[0].review_status.as_deref(), Some(REVIEW_PENDING));
    }

    #[cfg(unix)]
    #[test]
    fn review_run_refuses_when_the_role_disables_review() {
        let (repo, home, store, report) =
            review_fixture(Some(r#"{"review":false}"#), "APPROVE", false);
        let harness = home.path().join("review-harness");
        let error = review_run_with_bin(
            &store,
            home.path(),
            repo.path(),
            &report.run_id,
            &[],
            None,
            Some(&harness),
        )
        .unwrap_err();
        assert!(error.to_string().contains("disabled"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn role_settings_can_disable_the_reviewer() {
        let (repo, home, store, report) =
            review_fixture(Some(r#"{"review":false}"#), "APPROVE", true);
        assert_eq!(report.status, "completed");
        assert_eq!(
            report.tasks[0].review_status.as_deref(),
            Some(REVIEW_SKIPPED)
        );
        assert!(
            !home.path().join("review-ran.txt").exists(),
            "a disabled reviewer must not start a turn"
        );
        let integrated = integrate(&store, repo.path(), &report.run_id, &[]).unwrap();
        assert_eq!(integrated.merged.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_review_hook_fails_the_review_and_blocks_integration() {
        let (repo, _home, store, report) = review_fixture(
            Some(r#"{"hooks":{"before_review":[{"argv":["sh","-c","exit 4"]}]}}"#),
            "APPROVE",
            true,
        );
        assert_eq!(report.status, "needs_review");
        let task = &report.tasks[0];
        assert_eq!(task.review_status.as_deref(), Some(REVIEW_FAILED));
        assert!(
            task.review_text
                .as_deref()
                .unwrap_or_default()
                .contains("before_review"),
            "{:?}",
            task.review_text
        );
        let error = integrate(&store, repo.path(), &report.run_id, &[]).unwrap_err();
        assert!(
            error.to_string().contains("review did not approve"),
            "{error}"
        );
    }
}
