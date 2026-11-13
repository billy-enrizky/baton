//! Minimal Orca-style orchestration: a coordinator plans independent tasks,
//! each worker runs in its own git worktree through the normal `baton exec`
//! path, and results are recorded durably.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::resolve_config;
use crate::store::{BatonStore, OrchestrationRunRow, OrchestrationTaskRow};
use crate::types::{PermissionMode, ProviderId};
use crate::worktree::{self, WorktreeInfo};

pub const SCHEMA_VERSION: u32 = 1;

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

fn planner_prompt(goal: &str, cwd: &Path, max_tasks: usize) -> String {
    let template = r#"You are baton's coordinator. Split the goal into at most MAX independent worker tasks.
Each worker runs in its own git worktree, so tasks must not depend on another task's output and must avoid overlapping files.
Choose the best harness for each task from codex, claude, and opencode. Use different harnesses when their strengths differ.
Every worker starts in its own worktree, so each prompt must refer to the current working directory, never to the source checkout path.
Return ONLY a JSON object, with no markdown fences or commentary:
{"summary":"one sentence","tasks":[{"title":"short task name","provider":"codex","prompt":"complete self-contained instructions for the worker"}]}
Goal: GOAL
Source repository: CWD"#;
    template
        .replace("MAX", &max_tasks.to_string())
        .replace("GOAL", goal)
        .replace("CWD", &cwd.display().to_string())
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
    let output = command.arg(prompt).env("BATON_HOME", &temp_home).output();
    let _ = std::fs::remove_dir_all(&temp_home);
    let output = output?;
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
) -> Result<Plan> {
    let base_prompt = planner_prompt(goal, cwd, max_tasks);
    let mut last_error = None;
    for attempt in 0..2 {
        let prompt = if attempt == 0 {
            base_prompt.clone()
        } else {
            format!(
                "{base_prompt}\nYour previous response was not valid JSON. Return only the JSON object."
            )
        };
        let text = run_coordinator(&prompt, cwd, provider, model, permission_mode)?;
        if std::env::var_os("BATON_ORCHESTRATE_DEBUG").is_some() {
            eprintln!("[orchestrate] planner text (attempt {}): {text}", attempt + 1);
        }
        match parse_plan(&text, max_tasks) {
            Ok(plan) => return Ok(plan),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("planner failed")))
}

fn load_plan(path: &Path, max_tasks: usize) -> Result<Plan> {
    let text = std::fs::read_to_string(path)?;
    parse_plan(&text, max_tasks)
}

fn run_worker(
    task: &PlanTask,
    index: usize,
    run_id: &str,
    home: &Path,
    cwd: &Path,
    repo_root: Option<&Path>,
    worker_bin: Option<&Path>,
    permission_mode: PermissionMode,
) -> (usize, WorkerResult) {
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let provider = ProviderId::parse(task.provider.trim()).unwrap_or(ProviderId::Codex);
    let resolved = resolve_config(home, cwd);
    let model = resolved.for_provider(provider).model.clone();
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
    let output = command
        .arg(&effective_prompt)
        .env("BATON_HOME", home)
        .output();
    let (thread_id, text, error, _phase) = match output {
        Ok(output) => {
            let (thread_id, text, error, phase) =
                parse_exec_output(&output.stdout, &output.stderr);
            let ok = output.status.success() && phase.as_deref() == Some("completed");
            (
                thread_id,
                text,
                if ok {
                    None
                } else {
                    error.or_else(|| Some(format!("worker exited with {}", output.status)))
                },
                phase,
            )
        }
        Err(error) => (None, None, Some(format!("cannot run worker: {error}")), None),
    };
    let mut status = if error.is_none() {
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
    run_with_worker_bin(
        goal,
        cwd,
        home,
        coordinator_provider,
        coordinator_model,
        max_tasks,
        plan_path,
        store,
        None,
        permission_mode,
    )
}

#[allow(clippy::too_many_arguments)]
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
    if max_tasks == 0 {
        anyhow::bail!("max tasks must be at least 1");
    }
    let repo_root = worktree::repo_root(cwd).ok();
    let dirty_source = repo_root
        .as_ref()
        .and_then(|root| worktree::is_dirty(root).ok())
        .unwrap_or(false);
    let plan = match plan_path {
        Some(path) => load_plan(path, max_tasks)?,
        None => run_planner(
            goal,
            cwd,
            coordinator_provider,
            coordinator_model,
            max_tasks,
            permission_mode,
        )?,
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
    )?;
    let mut results = std::thread::scope(|scope| {
        let handles = plan
            .tasks
            .iter()
            .enumerate()
            .map(|(index, task)| {
                let run_id = run_id.clone();
                let home = home.to_path_buf();
                let cwd = cwd.to_path_buf();
                let repo_root = repo_root.clone();
                let worker_bin = worker_bin.map(Path::to_path_buf);
                scope.spawn(move || {
                    run_worker(
                        task,
                        index,
                        &run_id,
                        &home,
                        &cwd,
                        repo_root.as_deref(),
                        worker_bin.as_deref(),
                        permission_mode,
                    )
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or_else(|_| {
                    (
                        0,
                        WorkerResult {
                            id: uuid::Uuid::new_v4().to_string(),
                            title: "worker panicked".into(),
                            provider: coordinator_provider,
                            model: coordinator_model.to_string(),
                            prompt: String::new(),
                            worktree: None,
                            commit: None,
                            thread_id: None,
                            status: "failed".into(),
                            text: None,
                            error: Some("worker thread panicked".into()),
                            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                            finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        },
                    )
                })
            })
            .collect::<Vec<_>>()
    });
    results.sort_by_key(|(index, _)| *index);
    let tasks = results.into_iter().map(|(_, result)| result).collect::<Vec<_>>();
    for task in &tasks {
        store.add_orchestration_task(
            &task.id,
            &run_id,
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
    }
    let completed = tasks.iter().filter(|task| task.status == "completed").count();
    let status = if completed == tasks.len() {
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
