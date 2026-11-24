//! GitHub issue tasks: read an issue, then create a linked worktree.
//!
//! The forge is `gh`, bound to the `origin` repository exactly like the git
//! pull request flow, so a user-level default repository cannot redirect it.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::git_workflow::{
    gh_binary, gh_command, git_text, remote_url_for_gh, run_command_with_timeout,
};
use crate::worktree::WorktreeInfo;

pub const MAX_TASK_BODY_CHARS: usize = 1200;
pub const MAX_TASK_ISSUES: usize = 50;
const GH_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub body: String,
    pub state: String,
    pub labels: Vec<String>,
}

impl Issue {
    /// The composer prefill. The body is bounded so a long issue does not
    /// take over the input; the URL is always present for the full text.
    pub fn prompt(&self) -> String {
        let body = self.body.trim();
        let mut prompt = format!(
            "Work on issue #{}: {}\n{}\n",
            self.number, self.title, self.url
        );
        if !body.is_empty() {
            prompt.push('\n');
            let mut taken = String::new();
            for character in body.chars().take(MAX_TASK_BODY_CHARS) {
                taken.push(if character.is_control() && character != '\n' {
                    ' '
                } else {
                    character
                });
            }
            prompt.push_str(taken.trim_end());
            if body.chars().count() > MAX_TASK_BODY_CHARS {
                prompt.push_str(&format!(
                    "\n\n[body cut at {MAX_TASK_BODY_CHARS} characters; read the rest at {}]",
                    self.url
                ));
            }
            prompt.push('\n');
        }
        prompt
    }

    pub fn label_summary(&self) -> String {
        if self.labels.is_empty() {
            String::new()
        } else {
            format!(" · {}", self.labels.join(", "))
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskPlan {
    pub root: PathBuf,
    pub repo: String,
    pub issue: Issue,
}

pub struct IssueList {
    pub root: PathBuf,
    pub repo: String,
    pub issues: Vec<Issue>,
}

pub fn plan_task(cwd: &Path, number: u64) -> Result<TaskPlan> {
    plan_task_with(cwd, number, &gh_binary())
}

pub(crate) fn plan_task_with(cwd: &Path, number: u64, gh: &Path) -> Result<TaskPlan> {
    let (root, repo) = repo_identity(cwd, gh)?;
    let issue = read_issue_with(&root, &repo, number, gh)?;
    if !issue.state.eq_ignore_ascii_case("OPEN") {
        bail!(
            "issue #{} is {}; only open issues become tasks",
            number,
            issue.state.to_lowercase()
        );
    }
    Ok(TaskPlan { root, repo, issue })
}

pub fn list_open_issues(cwd: &Path, limit: usize) -> Result<IssueList> {
    list_open_issues_with(cwd, limit, &gh_binary())
}

pub(crate) fn list_open_issues_with(cwd: &Path, limit: usize, gh: &Path) -> Result<IssueList> {
    let (root, repo) = repo_identity(cwd, gh)?;
    let limit = limit.clamp(1, MAX_TASK_ISSUES).to_string();
    let output = run_gh(
        &root,
        gh,
        &[
            "issue",
            "list",
            "--repo",
            &repo,
            "--state",
            "open",
            "--limit",
            &limit,
            "--json",
            "number,title,url,body,state,labels",
        ],
    )?;
    let issues = parse_issues(&output)?;
    Ok(IssueList { root, repo, issues })
}

pub fn create_worktree(home: &Path, plan: &TaskPlan) -> Result<WorktreeInfo> {
    let run_id = uuid::Uuid::new_v4().to_string();
    crate::worktree::create(
        home,
        &plan.root,
        &run_id,
        &format!("issue-{}", plan.issue.number),
    )
}

pub(crate) fn repo_identity(cwd: &Path, gh: &Path) -> Result<(PathBuf, String)> {
    let root = crate::worktree::repo_root(cwd).context("a task needs a git repository")?;
    let remotes = git_text(&root, &["remote"])?;
    if !remotes.lines().any(|remote| remote.trim() == "origin") {
        bail!("no origin remote; a task reads the issue from origin");
    }
    let url = remote_url_for_gh(&git_text(&root, &["remote", "get-url", "origin"])?);
    let output = run_gh(
        &root,
        gh,
        &["repo", "view", &url, "--json", "nameWithOwner"],
    )?;
    let value: serde_json::Value =
        serde_json::from_slice(&output).context("gh repo view returned invalid JSON")?;
    let repo = value
        .get("nameWithOwner")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|repo| !repo.is_empty())
        .context("gh repo view did not return a repository name")?
        .to_string();
    Ok((root, repo))
}

pub(crate) fn read_issue_with(root: &Path, repo: &str, number: u64, gh: &Path) -> Result<Issue> {
    let output = run_gh(
        root,
        gh,
        &[
            "issue",
            "view",
            &number.to_string(),
            "--repo",
            repo,
            "--json",
            "number,title,url,body,state,labels",
        ],
    )?;
    let value: serde_json::Value =
        serde_json::from_slice(&output).context("gh issue view returned invalid JSON")?;
    parse_issue(&value).context("gh issue view did not return a usable issue")
}

fn run_gh(root: &Path, gh: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let label = format!("{} {}", gh.display(), args.join(" "));
    let output = run_command_with_timeout(gh_command(root, gh, args), &label, GH_TIMEOUT)?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    bail!(
        "gh {} failed: {}",
        args.first().copied().unwrap_or("command"),
        if stderr.is_empty() {
            "no error output"
        } else {
            &stderr
        }
    )
}

fn parse_issues(output: &[u8]) -> Result<Vec<Issue>> {
    let value: serde_json::Value =
        serde_json::from_slice(output).context("gh issue list returned invalid JSON")?;
    let list = value
        .as_array()
        .context("gh issue list did not return an array")?;
    Ok(list.iter().filter_map(parse_issue).collect())
}

fn parse_issue(value: &serde_json::Value) -> Option<Issue> {
    let number = value.get("number").and_then(serde_json::Value::as_u64)?;
    let title = value
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())?;
    let url = value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let body = value
        .get("body")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let state = value
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("OPEN")
        .to_string();
    let labels = value
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.get("name").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(Issue {
        number,
        title: title.to_string(),
        url,
        body,
        state,
        labels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_gh(root: &Path, body: &str) -> PathBuf {
        let path = root.join("fake-gh-task");
        std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn repo() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root.path())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "task@example.com"]);
        git(&["config", "user.name", "task"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(root.path().join("README.md"), "# task\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        let origin = root.path().to_string_lossy().to_string();
        git(&["remote", "add", "origin", &origin]);
        root
    }

    fn open_issue_gh(root: &Path) -> PathBuf {
        fake_gh(
            root,
            r#"if [ "$1" = "repo" ] && [ "$2" = "view" ]; then
  printf '%s\n' '{"nameWithOwner":"example/repo"}'
  exit 0
fi
if [ "$1" = "issue" ] && [ "$2" = "view" ]; then
  printf '%s\n' '{"number":12,"title":"Fix the parser","url":"https://github.com/example/repo/issues/12","body":"The parser drops tokens.","state":"OPEN","labels":[{"name":"bug"}]}'
  exit 0
fi
exit 1"#,
        )
    }

    #[test]
    fn a_task_reads_an_open_issue_bound_to_origin() {
        let repo = repo();
        let gh = open_issue_gh(repo.path());
        let plan = plan_task_with(repo.path(), 12, &gh).unwrap();
        assert_eq!(plan.repo, "example/repo");
        assert_eq!(plan.issue.number, 12);
        assert_eq!(plan.issue.title, "Fix the parser");
        assert_eq!(plan.issue.labels, vec!["bug".to_string()]);
        let prompt = plan.issue.prompt();
        assert!(
            prompt.contains("Work on issue #12: Fix the parser"),
            "{prompt}"
        );
        assert!(
            prompt.contains("https://github.com/example/repo/issues/12"),
            "{prompt}"
        );
        assert!(prompt.contains("The parser drops tokens."), "{prompt}");
    }

    #[test]
    fn a_closed_issue_and_a_missing_origin_are_refused() {
        let repo = repo();
        let closed = fake_gh(
            repo.path(),
            r#"if [ "$1" = "repo" ] && [ "$2" = "view" ]; then
  printf '%s\n' '{"nameWithOwner":"example/repo"}'
  exit 0
fi
if [ "$1" = "issue" ] && [ "$2" = "view" ]; then
  printf '%s\n' '{"number":12,"title":"Done","url":"u","body":"","state":"CLOSED","labels":[]}'
  exit 0
fi
exit 1"#,
        );
        let error = plan_task_with(repo.path(), 12, &closed).unwrap_err();
        assert!(error.to_string().contains("only open issues"), "{error}");

        let gh = open_issue_gh(repo.path());
        let status = std::process::Command::new("git")
            .args(["remote", "remove", "origin"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(status.success());
        let error = plan_task_with(repo.path(), 12, &gh).unwrap_err();
        assert!(error.to_string().contains("no origin"), "{error}");
    }

    #[test]
    fn the_prompt_bounds_a_long_body_and_marks_the_cut() {
        let issue = Issue {
            number: 7,
            title: "Long".to_string(),
            url: "https://example/7".to_string(),
            body: "z".repeat(MAX_TASK_BODY_CHARS + 50),
            state: "OPEN".to_string(),
            labels: Vec::new(),
        };
        let prompt = issue.prompt();
        assert!(prompt.contains("body cut at"), "{prompt}");
        assert_eq!(prompt.matches('z').count(), MAX_TASK_BODY_CHARS);
    }

    #[test]
    fn open_issues_are_listed_from_the_origin_repository() {
        let repo = repo();
        let gh = fake_gh(
            repo.path(),
            r#"if [ "$1" = "repo" ] && [ "$2" = "view" ]; then
  printf '%s\n' '{"nameWithOwner":"example/repo"}'
  exit 0
fi
if [ "$1" = "issue" ] && [ "$2" = "list" ]; then
  printf '%s\n' '[{"number":3,"title":"Add docs","url":"https://example/3","body":"","state":"OPEN","labels":[]},{"number":4,"title":"Fix bug","url":"https://example/4","body":"","state":"OPEN","labels":[{"name":"p1"}]}]'
  exit 0
fi
exit 1"#,
        );
        let list = list_open_issues_with(repo.path(), 10, &gh).unwrap();
        assert_eq!(list.issues.len(), 2);
        assert_eq!(list.issues[0].number, 3);
        assert_eq!(list.issues[1].labels, vec!["p1".to_string()]);
    }

    #[test]
    fn a_task_worktree_is_created_under_baton_home() {
        let repo = repo();
        let gh = open_issue_gh(repo.path());
        let plan = plan_task_with(repo.path(), 12, &gh).unwrap();
        let home = tempfile::tempdir().unwrap();
        let worktree = create_worktree(home.path(), &plan).unwrap();
        assert!(worktree.path.is_dir());
        assert_eq!(
            worktree.repo_root,
            repo.path().canonicalize().unwrap(),
            "the worktree records the canonical repository root"
        );
        assert!(worktree.branch.contains("issue-12"), "{}", worktree.branch);
        assert!(worktree.path.starts_with(home.path()));
        crate::worktree::remove(&worktree).unwrap();
    }
}
