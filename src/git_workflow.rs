//! Local git inspection and commit flow for normal threads. Push, pull
//! requests, and history rewrites are deliberately out of scope.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_MESSAGE_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    pub root: PathBuf,
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub staged: Vec<GitChange>,
    pub unstaged: Vec<GitChange>,
    pub untracked: Vec<String>,
    pub operation: Option<String>,
    pub conflicts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitChange {
    pub status: String,
    pub path: String,
    pub original: Option<String>,
}

struct StatusEntries {
    staged: Vec<GitChange>,
    unstaged: Vec<GitChange>,
    untracked: Vec<String>,
    conflicts: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitMode {
    All,
    Staged,
}

impl CommitMode {
    pub fn label(self) -> &'static str {
        match self {
            CommitMode::All => "all changes",
            CommitMode::Staged => "staged changes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPlan {
    pub root: PathBuf,
    pub branch: String,
    pub head: Option<String>,
    pub mode: CommitMode,
    pub message: String,
    pub files: Vec<String>,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitOutcome {
    pub sha: String,
    pub files: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTarget {
    pub remote: String,
    pub local_branch: String,
    pub remote_branch: String,
    pub remote_ref: String,
    pub upstream: bool,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushPlan {
    pub root: PathBuf,
    pub branch: String,
    pub head: String,
    pub remote: String,
    pub remote_branch: String,
    pub remote_ref: String,
    pub upstream: bool,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    pub remote: String,
    pub remote_branch: String,
    pub upstream: bool,
}

impl GitStatus {
    pub fn changed_files(&self) -> Vec<String> {
        let mut files = self
            .staged
            .iter()
            .chain(self.unstaged.iter())
            .map(|change| change.path.clone())
            .chain(self.untracked.iter().cloned())
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        files
    }

    pub fn staged_files(&self) -> Vec<String> {
        let mut files = self
            .staged
            .iter()
            .map(|change| change.path.clone())
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        files
    }

    pub fn is_clean(&self) -> bool {
        self.changed_files().is_empty()
    }

    pub fn display_changes(&self) -> Vec<String> {
        let mut rows = self
            .staged
            .iter()
            .chain(self.unstaged.iter())
            .map(|change| format!("{} {}", change.status, change.path))
            .chain(self.untracked.iter().map(|path| format!("?? {path}")))
            .collect::<Vec<_>>();
        rows.sort();
        rows.dedup();
        rows
    }
}

pub fn inspect(cwd: &Path) -> Result<GitStatus> {
    let root = repo_root(cwd)?;
    let head_ref = git_text(&root, &["symbolic-ref", "--quiet", "HEAD"])
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| value.starts_with("refs/heads/"));
    let branch = match head_ref.as_deref() {
        Some(head_ref) => head_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(head_ref)
            .to_string(),
        None => {
            let sha = git_text(&root, &["rev-parse", "--short", "HEAD"])?;
            format!("detached at {}", sha.trim())
        }
    };
    let mut upstream = git_text(
        &root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty());
    let (ahead, behind) = match upstream.as_deref() {
        Some(name) => match revision_counts(&root, name) {
            Ok(counts) => counts,
            Err(_) => {
                upstream = None;
                (0, 0)
            }
        },
        None => (0, 0),
    };
    let operation = in_progress_operation(&root)?;
    let entries = status_entries(&root)?;

    Ok(GitStatus {
        root,
        branch,
        upstream,
        ahead,
        behind,
        staged: entries.staged,
        unstaged: entries.unstaged,
        untracked: entries.untracked,
        operation,
        conflicts: entries.conflicts,
    })
}

pub fn plan_commit(cwd: &Path, mode: CommitMode, message: Option<&str>) -> Result<CommitPlan> {
    let status = inspect(cwd)?;
    if let Some(operation) = status.operation.as_deref() {
        bail!("{operation} is in progress; finish it before committing");
    }
    if status.conflicts {
        bail!("unresolved conflicts; resolve them before committing");
    }
    let files = match mode {
        CommitMode::All => status.changed_files(),
        CommitMode::Staged => status.staged_files(),
    };
    if files.is_empty() {
        bail!(match mode {
            CommitMode::All => "nothing to commit",
            CommitMode::Staged => "nothing staged to commit",
        });
    }
    let message = match message.map(str::trim).filter(|value| !value.is_empty()) {
        Some(message) => validate_message(message)?,
        None => generate_message(&files),
    };
    let head = head_sha(&status.root)?;
    let fingerprint = tree_fingerprint(&status.root, mode)?;
    Ok(CommitPlan {
        root: status.root,
        branch: status.branch,
        head,
        mode,
        message,
        files,
        fingerprint,
    })
}

pub fn commit(plan: &CommitPlan) -> Result<CommitOutcome> {
    let current = inspect(&plan.root)?;
    if let Some(operation) = current.operation.as_deref() {
        bail!("{operation} is in progress; finish it before committing");
    }
    if current.conflicts {
        bail!("unresolved conflicts; resolve them before committing");
    }
    if current.branch != plan.branch {
        bail!(
            "branch changed from {} to {}; review the commit again",
            plan.branch,
            current.branch
        );
    }
    if current_head(&plan.root)? != plan.head {
        bail!("HEAD moved since the confirmation; review the commit again");
    }
    let current_files = match plan.mode {
        CommitMode::All => current.changed_files(),
        CommitMode::Staged => current.staged_files(),
    };
    if current_files != plan.files {
        bail!("changes changed since the confirmation; review the commit again");
    }
    if tree_fingerprint(&plan.root, plan.mode)? != plan.fingerprint {
        bail!(
            "{} changed after the confirmation; review the commit again",
            plan.mode.label()
        );
    }
    if plan.mode == CommitMode::All {
        git_text(&plan.root, &["add", "-A", "--", "."])?;
        if tree_fingerprint(&plan.root, CommitMode::Staged)? != plan.fingerprint {
            bail!(
                "all changes changed while staging; all changes are now staged; review the commit again"
            );
        }
    }
    let before_commit = inspect(&plan.root)?;
    if before_commit.branch != plan.branch {
        bail!(
            "branch changed from {} to {}; review the commit again",
            plan.branch,
            before_commit.branch
        );
    }
    if current_head(&plan.root)? != plan.head {
        bail!("HEAD moved since the confirmation; review the commit again");
    }
    git_text(
        &plan.root,
        &["commit", "--quiet", "--message", &plan.message],
    )
    .with_context(|| {
        if plan.mode == CommitMode::All {
            format!(
                "cannot commit on {}; all changes are now staged",
                plan.branch
            )
        } else {
            format!("cannot commit on {}", plan.branch)
        }
    })?;
    let sha = git_text(&plan.root, &["rev-parse", "--short", "HEAD"])?;
    let committed_tree = git_text(&plan.root, &["rev-parse", "HEAD^{tree}"])?;
    if committed_tree.trim() != plan.fingerprint {
        bail!(
            "hooks changed the committed tree; commit {} has tree {}; review the hook and reset if needed",
            sha.trim(),
            committed_tree.trim()
        );
    }
    Ok(CommitOutcome {
        sha: sha.trim().to_string(),
        files: plan.files.len(),
        message: plan.message.clone(),
    })
}

pub fn list_push_targets(cwd: &Path) -> Result<Vec<PushTarget>> {
    let status = inspect(cwd)?;
    if status.branch.starts_with("detached at ") {
        bail!("detached HEAD; check out a branch before pushing");
    }
    if let Some(operation) = status.operation.as_deref() {
        bail!("{operation} is in progress; finish it before pushing");
    }
    if status.conflicts {
        bail!("unresolved conflicts; resolve them before pushing");
    }
    let remotes = git_text(&status.root, &["remote"])?
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if remotes.is_empty() {
        bail!("no git remote is configured");
    }
    let (upstream_remote, upstream_branch) = upstream_target(&status.root, &status.branch)
        .map(|(remote, branch)| (Some(remote), Some(branch)))
        .unwrap_or_default();
    let upstream_resolved = status.upstream.is_some();
    let mut targets = remotes
        .into_iter()
        .map(|remote| {
            let upstream = upstream_resolved
                && upstream_remote.as_deref() == Some(remote.as_str())
                && upstream_branch.is_some();
            let remote_branch = if upstream {
                upstream_branch
                    .clone()
                    .unwrap_or_else(|| status.branch.clone())
            } else {
                status.branch.clone()
            };
            let remote_ref = format!("refs/heads/{remote_branch}");
            let remote_branch = remote_ref
                .strip_prefix("refs/heads/")
                .unwrap_or(&remote_ref)
                .to_string();
            PushTarget {
                remote,
                local_branch: status.branch.clone(),
                remote_branch,
                remote_ref,
                upstream,
                ahead: if upstream { status.ahead } else { 0 },
                behind: if upstream { status.behind } else { 0 },
            }
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| {
        (!left.upstream, &left.remote).cmp(&(!right.upstream, &right.remote))
    });
    Ok(targets)
}

fn upstream_target(root: &Path, branch: &str) -> Option<(String, String)> {
    let remote = git_text(
        root,
        &["config", "--get", &format!("branch.{branch}.remote")],
    )
    .ok()?
    .trim()
    .to_string();
    if remote.is_empty() || remote == "." {
        return None;
    }
    let merge = git_text(
        root,
        &["config", "--get", &format!("branch.{branch}.merge")],
    )
    .ok()?;
    let merge = merge.trim();
    let branch = merge.strip_prefix("refs/heads/").unwrap_or(merge);
    if branch.is_empty() {
        return None;
    }
    Some((remote, branch.to_string()))
}

pub fn plan_push(cwd: &Path, remote: &str) -> Result<PushPlan> {
    let targets = list_push_targets(cwd)?;
    let target = targets
        .into_iter()
        .find(|target| target.remote == remote)
        .with_context(|| format!("remote {remote} is not configured"))?;
    let root = repo_root(cwd)?;
    let head = head_sha(&root)?.context("the branch has no commits to push")?;
    if target.upstream {
        if target.behind > 0 {
            bail!(
                "upstream {} has {} commit(s) you do not; pull or merge before pushing",
                target.remote,
                target.behind
            );
        }
        if target.ahead == 0 {
            bail!("nothing to push; the branch matches its upstream");
        }
    }
    Ok(PushPlan {
        root,
        branch: target.local_branch,
        head,
        remote: target.remote,
        remote_branch: target.remote_branch,
        remote_ref: target.remote_ref,
        upstream: target.upstream,
        ahead: target.ahead,
        behind: target.behind,
    })
}

pub fn push(plan: &PushPlan) -> Result<PushOutcome> {
    let status = inspect(&plan.root)?;
    if status.branch != plan.branch {
        bail!(
            "branch changed from {} to {}; review the push again",
            plan.branch,
            status.branch
        );
    }
    if let Some(operation) = status.operation.as_deref() {
        bail!("{operation} is in progress; finish it before pushing");
    }
    if status.conflicts {
        bail!("unresolved conflicts; resolve them before pushing");
    }
    if current_head(&plan.root)?.as_deref() != Some(plan.head.as_str()) {
        bail!("HEAD moved since the confirmation; review the push again");
    }
    let arguments = push_arguments(plan);
    let output = Command::new("git")
        .args(&arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(&plan.root)
        .output()
        .with_context(|| format!("cannot run git {}", arguments.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            arguments.join(" "),
            std::str::from_utf8(&output.stderr)
                .unwrap_or("push failed")
                .trim()
        );
    }
    Ok(PushOutcome {
        remote: plan.remote.clone(),
        remote_branch: plan.remote_branch.clone(),
        upstream: plan.upstream,
    })
}

fn push_arguments(plan: &PushPlan) -> Vec<String> {
    vec![
        "-c".to_string(),
        "push.autoSetupRemote=false".to_string(),
        "-c".to_string(),
        "push.recurseSubmodules=check".to_string(),
        "push".to_string(),
        "--porcelain".to_string(),
        "--no-follow-tags".to_string(),
        "--".to_string(),
        plan.remote.clone(),
        format!("{}:{}", plan.head, plan.remote_ref),
    ]
}

pub fn generate_message(files: &[String]) -> String {
    let first = files.first().map(String::as_str).unwrap_or("changes");
    let base = first
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(first);
    let base = sanitize_message(base);
    let message = if files.len() == 1 {
        format!("baton: update {base}")
    } else {
        format!("baton: update {} files", files.len())
    };
    message.chars().take(MAX_MESSAGE_CHARS).collect()
}

fn head_sha(root: &Path) -> Result<Option<String>> {
    match git_text(root, &["rev-parse", "--verify", "HEAD"]) {
        Ok(sha) if !sha.trim().is_empty() => Ok(Some(sha.trim().to_string())),
        _ => Ok(None),
    }
}

fn current_head(root: &Path) -> Result<Option<String>> {
    head_sha(root)
}

fn tree_fingerprint(root: &Path, mode: CommitMode) -> Result<String> {
    if mode == CommitMode::Staged {
        return Ok(git_text(root, &["write-tree"])?.trim().to_string());
    }
    let index = std::env::temp_dir().join(format!("baton-git-index-{}", uuid::Uuid::new_v4()));
    let index_text = index.to_string_lossy().to_string();
    let result = (|| -> Result<String> {
        let real_index = git_text(root, &["rev-parse", "--git-path", "index"])?;
        let real_index = PathBuf::from(real_index.trim());
        let real_index = if real_index.is_absolute() {
            real_index
        } else {
            root.join(real_index)
        };
        if real_index.is_file() {
            std::fs::copy(&real_index, &index)
                .with_context(|| format!("cannot copy {}", real_index.display()))?;
        } else {
            git_text_with_env(
                root,
                &["read-tree", "--empty"],
                &[("GIT_INDEX_FILE", &index_text)],
            )?;
        }
        git_text_with_env(
            root,
            &["add", "-A", "--", "."],
            &[("GIT_INDEX_FILE", &index_text)],
        )?;
        let tree = git_text_with_env(root, &["write-tree"], &[("GIT_INDEX_FILE", &index_text)])?;
        Ok(tree.trim().to_string())
    })();
    let _ = std::fs::remove_file(&index);
    let _ = std::fs::remove_file(index.with_extension("lock"));
    result
}

fn validate_message(message: &str) -> Result<String> {
    if message.chars().any(char::is_control) {
        bail!("commit message cannot contain control characters");
    }
    let message = message.chars().take(MAX_MESSAGE_CHARS).collect::<String>();
    if message.trim().is_empty() {
        bail!("commit message cannot be empty");
    }
    Ok(message)
}

fn sanitize_message(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_MESSAGE_CHARS)
        .collect()
}

fn repo_root(cwd: &Path) -> Result<PathBuf> {
    let inside = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("cannot run git in {}", cwd.display()))?;
    if !inside.status.success() || String::from_utf8_lossy(&inside.stdout).trim() != "true" {
        bail!("not inside a git repository: {}", cwd.display());
    }
    let output = git_bytes(cwd, &["rev-parse", "--show-toplevel"])?;
    let text = std::str::from_utf8(&output)
        .context("git reported a repository root that is not valid UTF-8")?;
    let root = text.strip_suffix('\n').unwrap_or(text);
    if root.is_empty() {
        bail!("not inside a git repository: {}", cwd.display());
    }
    Ok(PathBuf::from(root))
}

fn revision_counts(root: &Path, upstream: &str) -> Result<(usize, usize)> {
    let value = git_text(
        root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{upstream}...HEAD"),
        ],
    )?;
    let mut parts = value.split_whitespace();
    let behind = parts.next().and_then(|value| value.parse().ok());
    let ahead = parts.next().and_then(|value| value.parse().ok());
    match (ahead, behind) {
        (Some(ahead), Some(behind)) => Ok((ahead, behind)),
        _ => bail!("cannot parse revision counts from git"),
    }
}

fn in_progress_operation(root: &Path) -> Result<Option<String>> {
    for name in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
        "BISECT_LOG",
    ] {
        let path = git_text(root, &["rev-parse", "--git-path", name])?;
        let path = PathBuf::from(path.trim());
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        if path.exists() {
            return Ok(Some(match name {
                "MERGE_HEAD" => "a merge".to_string(),
                "CHERRY_PICK_HEAD" => "a cherry-pick".to_string(),
                "REVERT_HEAD" => "a revert".to_string(),
                "rebase-merge" | "rebase-apply" => "a rebase".to_string(),
                "sequencer" => "a cherry-pick or revert sequence".to_string(),
                "BISECT_LOG" => "a bisect".to_string(),
                _ => name.to_string(),
            }));
        }
    }
    Ok(None)
}

fn status_entries(root: &Path) -> Result<StatusEntries> {
    let output = git_bytes(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    )?;
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicts = false;
    let mut entries = output.split(|byte| *byte == 0);
    while let Some(entry) = entries.next() {
        if entry.is_empty() {
            continue;
        }
        if entry.len() < 4 {
            continue;
        }
        let x = entry[0] as char;
        let y = entry[1] as char;
        if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
            conflicts = true;
        }
        let path = std::str::from_utf8(&entry[3..])
            .context("git reported a path that is not valid UTF-8")?
            .to_string();
        if x == '?' && y == '?' {
            untracked.push(path);
            continue;
        }
        let original = if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            match entries.next() {
                Some(value) => Some(
                    std::str::from_utf8(value)
                        .context("git reported a rename source that is not valid UTF-8")?
                        .to_string(),
                ),
                None => None,
            }
        } else {
            None
        };
        let change = GitChange {
            status: format!("{x}{y}"),
            path,
            original,
        };
        if x != ' ' {
            staged.push(change.clone());
        }
        if y != ' ' {
            unstaged.push(change);
        }
    }
    Ok(StatusEntries {
        staged,
        unstaged,
        untracked,
        conflicts,
    })
}

fn git_text(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = git_bytes(cwd, args)?;
    Ok(String::from_utf8_lossy(&output).trim().to_string())
}

fn git_text_with_env(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .with_context(|| format!("cannot run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("cannot run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        let hooks = std::env::temp_dir().join("baton-test-no-hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        git(
            root.path(),
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        );
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        git(root.path(), &["config", "push.autoSetupRemote", "false"]);
        git(root.path(), &["config", "status.showUntrackedFiles", "all"]);
        let excludes = std::env::temp_dir().join("baton-test-empty-excludes");
        std::fs::write(&excludes, "").unwrap();
        git(
            root.path(),
            &["config", "core.excludesFile", excludes.to_str().unwrap()],
        );
        git(root.path(), &["config", "user.email", "test@test"]);
        git(root.path(), &["config", "user.name", "test"]);
        std::fs::write(root.path().join("base.txt"), "base\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-q", "-m", "init"]);
        root
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn bare_remote() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--bare", "-q", "-b", "main"]);
        root
    }

    fn add_remote(repo: &Path, name: &str, remote: &Path) {
        git(repo, &["remote", "add", name, remote.to_str().unwrap()]);
    }

    #[test]
    fn inspect_reports_clean_and_dirty_worktrees() {
        let repo = repo();
        let status = inspect(repo.path()).unwrap();
        assert!(status.is_clean());
        assert!(status.operation.is_none());
        assert!(status.staged.is_empty());

        std::fs::write(repo.path().join("new.txt"), "new\n").unwrap();
        let status = inspect(repo.path()).unwrap();
        assert!(!status.is_clean());
        assert_eq!(status.changed_files(), vec!["new.txt".to_string()]);
        assert_eq!(status.untracked, vec!["new.txt".to_string()]);
    }

    #[test]
    fn inspect_separates_staged_and_unstaged_changes() {
        let repo = repo();
        std::fs::write(repo.path().join("base.txt"), "staged\n").unwrap();
        git(repo.path(), &["add", "base.txt"]);
        std::fs::write(repo.path().join("base.txt"), "unstaged\n").unwrap();
        let status = inspect(repo.path()).unwrap();
        assert_eq!(status.staged_files(), vec!["base.txt".to_string()]);
        assert_eq!(status.unstaged.len(), 1);
        assert_eq!(status.unstaged[0].path, "base.txt");
    }

    #[test]
    fn inspect_reports_upstream_ahead_and_behind() {
        let repo = repo();
        std::fs::write(repo.path().join("base.txt"), "second\n").unwrap();
        git(repo.path(), &["commit", "-qam", "second"]);
        std::fs::write(repo.path().join("base.txt"), "third\n").unwrap();
        git(repo.path(), &["commit", "-qam", "third"]);
        git(repo.path(), &["checkout", "-q", "-b", "other", "HEAD~2"]);
        std::fs::write(repo.path().join("other.txt"), "other\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "other"]);
        let other = git_text(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        git(repo.path(), &["checkout", "-q", "main"]);
        git(
            repo.path(),
            &["remote", "add", "origin", "https://example.com/repo.git"],
        );
        git(
            repo.path(),
            &["update-ref", "refs/remotes/origin/main", other.trim()],
        );
        git(
            repo.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        let status = inspect(repo.path()).unwrap();
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.ahead, status.behind), (2, 1));

        git(
            repo.path(),
            &["update-ref", "-d", "refs/remotes/origin/main"],
        );
        assert!(inspect(repo.path()).unwrap().upstream.is_none());
    }

    #[test]
    fn plan_refuses_a_clean_worktree_and_an_operation_in_progress() {
        let repo = repo();
        assert!(plan_commit(repo.path(), CommitMode::All, None).is_err());

        std::fs::write(repo.path().join("dirty.txt"), "dirty\n").unwrap();
        std::fs::write(repo.path().join(".git").join("MERGE_HEAD"), "deadbeef\n").unwrap();
        let error = plan_commit(repo.path(), CommitMode::All, None).unwrap_err();
        assert!(error.to_string().contains("merge"));
        std::fs::remove_file(repo.path().join(".git").join("MERGE_HEAD")).unwrap();

        for (marker, label) in [
            ("CHERRY_PICK_HEAD", "cherry-pick"),
            ("REVERT_HEAD", "revert"),
            ("rebase-merge", "rebase"),
            ("rebase-apply", "rebase"),
            ("sequencer", "sequence"),
            ("BISECT_LOG", "bisect"),
        ] {
            std::fs::write(repo.path().join(".git").join(marker), "x\n").unwrap();
            let error = plan_commit(repo.path(), CommitMode::All, None).unwrap_err();
            assert!(error.to_string().contains(label), "{marker}: {error}");
            std::fs::remove_file(repo.path().join(".git").join(marker)).unwrap();
        }
    }

    #[test]
    fn plan_refuses_an_unmerged_index_without_a_merge_marker() {
        let repo = repo();
        std::fs::write(repo.path().join("base.txt"), "stash\n").unwrap();
        git(repo.path(), &["stash", "push", "-q"]);
        std::fs::write(repo.path().join("base.txt"), "main\n").unwrap();
        git(repo.path(), &["commit", "-qam", "main"]);
        let pop = Command::new("git")
            .args(["stash", "pop"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(!pop.status.success(), "the fixture must conflict");
        let status = inspect(repo.path()).unwrap();
        assert!(status.conflicts);
        let error = plan_commit(repo.path(), CommitMode::All, None).unwrap_err();
        assert!(error.to_string().contains("unresolved conflicts"));
    }

    #[test]
    fn commit_all_stages_modified_and_untracked_files() {
        let repo = repo();
        std::fs::write(repo.path().join("base.txt"), "changed\n").unwrap();
        std::fs::write(repo.path().join("new.txt"), "new\n").unwrap();
        std::fs::write(repo.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(repo.path().join("ignored.txt"), "ignored\n").unwrap();

        let plan = plan_commit(repo.path(), CommitMode::All, None).unwrap();
        assert_eq!(plan.files.len(), 3);
        let outcome = commit(&plan).unwrap();
        assert_eq!(outcome.files, 3);
        assert!(outcome.sha.len() >= 4);
        assert!(outcome.message.starts_with("baton: update"));

        let committed =
            git_text(repo.path(), &["show", "--name-only", "--format=", "HEAD"]).unwrap();
        assert!(committed.contains("base.txt"));
        assert!(committed.contains("new.txt"));
        assert!(committed.contains(".gitignore"));
        assert!(!committed.contains("ignored.txt"));
        assert!(inspect(repo.path()).unwrap().is_clean());
    }

    #[test]
    fn commit_staged_leaves_unstaged_changes_in_the_worktree() {
        let repo = repo();
        std::fs::write(repo.path().join("base.txt"), "staged\n").unwrap();
        git(repo.path(), &["add", "base.txt"]);
        std::fs::write(repo.path().join("base.txt"), "unstaged\n").unwrap();
        std::fs::write(repo.path().join("other.txt"), "other\n").unwrap();

        assert!(plan_commit(repo.path(), CommitMode::Staged, Some("bad\nmessage")).is_err());
        let long = plan_commit(
            repo.path(),
            CommitMode::Staged,
            Some(&"x".repeat(MAX_MESSAGE_CHARS + 1)),
        )
        .unwrap();
        assert_eq!(long.message.len(), MAX_MESSAGE_CHARS);

        let plan = plan_commit(repo.path(), CommitMode::Staged, Some("test: staged only")).unwrap();
        assert_eq!(plan.files, vec!["base.txt".to_string()]);
        let outcome = commit(&plan).unwrap();
        assert_eq!(
            git_text(repo.path(), &["rev-parse", "--short", "HEAD"]).unwrap(),
            outcome.sha
        );
        assert_eq!(
            git_text(repo.path(), &["log", "-1", "--format=%s"]).unwrap(),
            "test: staged only"
        );

        let status = inspect(repo.path()).unwrap();
        assert_eq!(status.unstaged.len(), 1);
        assert_eq!(status.unstaged[0].path, "base.txt");
        assert_eq!(status.untracked, vec!["other.txt".to_string()]);
    }

    #[test]
    fn generated_messages_name_one_file_and_count_many() {
        assert_eq!(
            generate_message(&["src/parser/mod.rs".to_string()]),
            "baton: update mod.rs"
        );
        assert_eq!(
            generate_message(&["a.txt".to_string(), "b.txt".to_string()]),
            "baton: update 2 files"
        );
    }

    #[test]
    fn commit_refuses_a_plan_from_another_branch() {
        let repo = repo();
        std::fs::write(repo.path().join("dirty.txt"), "dirty\n").unwrap();
        let plan = plan_commit(repo.path(), CommitMode::All, None).unwrap();
        git(repo.path(), &["checkout", "-q", "-b", "other"]);
        let error = commit(&plan).unwrap_err();
        assert!(error.to_string().contains("branch changed"));
        assert!(!inspect(repo.path()).unwrap().is_clean());
    }

    #[test]
    fn commit_refuses_a_plan_after_the_changes_move() {
        let repo = repo();
        std::fs::write(repo.path().join("dirty.txt"), "dirty\n").unwrap();
        let plan = plan_commit(repo.path(), CommitMode::All, None).unwrap();
        std::fs::write(repo.path().join("later.txt"), "later\n").unwrap();
        let error = commit(&plan).unwrap_err();
        assert!(error.to_string().contains("changes changed"));
        assert!(!inspect(repo.path()).unwrap().is_clean());
    }

    #[test]
    fn commit_refuses_a_content_change_after_the_plan() {
        let repo = repo();
        std::fs::write(repo.path().join("base.txt"), "planned\n").unwrap();
        let plan = plan_commit(repo.path(), CommitMode::All, None).unwrap();
        std::fs::write(repo.path().join("base.txt"), "changed later\n").unwrap();
        let error = commit(&plan).unwrap_err();
        assert!(error.to_string().contains("changed after the confirmation"));
        assert!(!inspect(repo.path()).unwrap().is_clean());
        assert!(
            inspect(repo.path()).unwrap().staged_files().is_empty(),
            "a pre-staging refusal must not mutate the index"
        );
    }

    #[test]
    fn commit_all_includes_a_force_added_ignored_file() {
        let repo = repo();
        std::fs::write(repo.path().join(".gitignore"), "secret.txt\n").unwrap();
        git(repo.path(), &["add", ".gitignore"]);
        git(repo.path(), &["commit", "-q", "-m", "ignore"]);
        std::fs::write(repo.path().join("secret.txt"), "secret\n").unwrap();
        git(repo.path(), &["add", "-f", "secret.txt"]);

        let plan = plan_commit(repo.path(), CommitMode::All, None).unwrap();
        assert!(
            plan.files.contains(&"secret.txt".to_string()),
            "{:?}",
            plan.files
        );
        commit(&plan).unwrap();
        let committed =
            git_text(repo.path(), &["show", "--name-only", "--format=", "HEAD"]).unwrap();
        assert!(
            committed.lines().any(|line| line == "secret.txt"),
            "{committed}"
        );
    }

    #[test]
    fn commit_refuses_a_head_move_on_the_same_branch() {
        let repo = repo();
        std::fs::write(repo.path().join("dirty.txt"), "dirty\n").unwrap();
        let plan = plan_commit(repo.path(), CommitMode::All, None).unwrap();
        git(
            repo.path(),
            &["commit", "--allow-empty", "-q", "-m", "moved"],
        );
        let error = commit(&plan).unwrap_err();
        assert!(error.to_string().contains("HEAD moved"));
    }

    #[test]
    fn failed_commit_reports_the_staged_index() {
        let repo = repo();
        std::fs::write(repo.path().join("dirty.txt"), "dirty\n").unwrap();
        git(repo.path(), &["config", "core.hooksPath", ".git/hooks"]);
        let hook = repo.path().join(".git").join("hooks").join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let plan = plan_commit(repo.path(), CommitMode::All, None).unwrap();
        let error = commit(&plan).unwrap_err();
        assert!(error.to_string().contains("all changes are now staged"));
        assert!(!inspect(repo.path()).unwrap().staged_files().is_empty());
    }

    #[test]
    fn successful_hook_tree_changes_are_reported() {
        let repo = repo();
        std::fs::write(repo.path().join("dirty.txt"), "dirty\n").unwrap();
        git(repo.path(), &["config", "core.hooksPath", ".git/hooks"]);
        let hook = repo.path().join(".git").join("hooks").join("pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho hook >> hook.txt\ngit add hook.txt\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let plan = plan_commit(repo.path(), CommitMode::All, None).unwrap();
        let error = commit(&plan).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("hooks changed the committed tree"),
            "{error}"
        );
        let committed =
            git_text(repo.path(), &["show", "--name-only", "--format=", "HEAD"]).unwrap();
        assert!(
            committed.lines().any(|line| line == "hook.txt"),
            "{committed}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn repository_root_preserves_a_trailing_space() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("repo ");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        assert_eq!(
            repo_root(&root).unwrap(),
            std::fs::canonicalize(&root).unwrap()
        );
    }

    #[test]
    fn display_changes_keeps_staged_and_unstaged_codes() {
        let repo = repo();
        std::fs::write(repo.path().join("base.txt"), "staged\n").unwrap();
        git(repo.path(), &["add", "base.txt"]);
        std::fs::write(repo.path().join("base.txt"), "unstaged\n").unwrap();
        std::fs::write(repo.path().join("new.txt"), "new\n").unwrap();
        let status = inspect(repo.path()).unwrap();
        let rows = status.display_changes();
        assert!(
            rows.iter().any(|row| row.starts_with("MM base.txt")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.starts_with("?? new.txt")),
            "{rows:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_paths_are_rejected_instead_of_collapsed() {
        use std::os::unix::ffi::OsStringExt;

        let repo = repo();
        let name = std::ffi::OsString::from_vec(b"bad-\xff.txt".to_vec());
        std::fs::write(repo.path().join(name), "bad\n").unwrap();
        let error = inspect(repo.path()).unwrap_err();
        assert!(error.to_string().contains("not valid UTF-8"), "{error}");
    }

    #[test]
    fn detached_head_is_reported_as_detached() {
        let repo = repo();
        git(repo.path(), &["checkout", "-q", "--detach", "HEAD"]);
        let status = inspect(repo.path()).unwrap();
        assert!(status.branch.starts_with("detached at "));
        std::fs::write(repo.path().join("detached.txt"), "detached\n").unwrap();
        let plan = plan_commit(repo.path(), CommitMode::All, None).unwrap();
        commit(&plan).unwrap();
        assert!(inspect(repo.path()).unwrap().is_clean());
    }

    #[test]
    fn push_targets_prefer_the_configured_upstream() {
        let repo = repo();
        let bare = bare_remote();
        add_remote(repo.path(), "origin", bare.path());
        git(
            repo.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        git(
            repo.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        std::fs::write(repo.path().join("ahead.txt"), "ahead\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "ahead"]);
        add_remote(repo.path(), "backup", bare.path());

        let targets = list_push_targets(repo.path()).unwrap();
        assert_eq!(targets[0].remote, "origin");
        assert!(targets[0].upstream);
        assert_eq!(targets[0].ahead, 1);
        assert_eq!(targets[0].behind, 0);
        assert_eq!(targets[1].remote, "backup");
        assert!(!targets[1].upstream);
    }

    #[test]
    fn push_targets_handle_a_remote_name_with_a_slash() {
        let repo = repo();
        let bare = bare_remote();
        add_remote(repo.path(), "team/origin", bare.path());
        git(
            repo.path(),
            &["update-ref", "refs/remotes/team/origin/main", "HEAD"],
        );
        git(
            repo.path(),
            &["branch", "--set-upstream-to=team/origin/main", "main"],
        );
        let targets = list_push_targets(repo.path()).unwrap();
        assert_eq!(targets[0].remote, "team/origin");
        assert!(targets[0].upstream);
    }

    #[test]
    fn push_target_uses_a_different_upstream_branch() {
        let repo = repo();
        let bare = bare_remote();
        add_remote(repo.path(), "origin", bare.path());
        git(repo.path(), &["checkout", "-q", "-b", "feature"]);
        git(
            repo.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        git(
            repo.path(),
            &["branch", "--set-upstream-to=origin/main", "feature"],
        );
        std::fs::write(repo.path().join("ahead.txt"), "ahead\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "ahead"]);
        let targets = list_push_targets(repo.path()).unwrap();
        assert_eq!(targets[0].remote, "origin");
        assert!(targets[0].upstream);
        assert_eq!(targets[0].remote_branch, "main");
        assert_eq!(targets[0].remote_ref, "refs/heads/main");
        assert_eq!(targets[0].ahead, 1);
    }

    #[test]
    fn missing_tracking_ref_does_not_fabricate_an_upstream() {
        let repo = repo();
        let bare = bare_remote();
        add_remote(repo.path(), "origin", bare.path());
        git(
            repo.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        git(
            repo.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        git(
            repo.path(),
            &["update-ref", "-d", "refs/remotes/origin/main"],
        );
        let targets = list_push_targets(repo.path()).unwrap();
        assert!(!targets[0].upstream);
        assert_eq!(targets[0].remote_branch, "main");
        assert_eq!(targets[0].ahead, 0);
        assert_eq!(targets[0].behind, 0);
    }

    #[test]
    fn push_sends_the_branch_and_never_forces() {
        let repo = repo();
        let bare = bare_remote();
        add_remote(repo.path(), "origin", bare.path());
        std::fs::write(repo.path().join("pushed.txt"), "pushed\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "pushed"]);
        git(repo.path(), &["config", "push.recurseSubmodules", "only"]);
        git(repo.path(), &["config", "push.followTags", "true"]);
        git(repo.path(), &["tag", "-a", "v-test", "-m", "tag"]);

        let plan = plan_push(repo.path(), "origin").unwrap();
        let arguments = push_arguments(&plan);
        assert!(!arguments.iter().any(|argument| argument.contains("force")));
        assert_eq!(
            arguments,
            vec![
                "-c",
                "push.autoSetupRemote=false",
                "-c",
                "push.recurseSubmodules=check",
                "push",
                "--porcelain",
                "--no-follow-tags",
                "--",
                "origin",
                &format!("{}:refs/heads/main", plan.head)
            ]
        );
        push(&plan).unwrap();
        let branch_remote = Command::new("git")
            .args(["config", "--get", "branch.main.remote"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        let branch_merge = Command::new("git")
            .args(["config", "--get", "branch.main.merge"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(
            !branch_remote.status.success(),
            "push must not set upstream"
        );
        assert!(!branch_merge.status.success(), "push must not set upstream");
        let local = git_text(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        let remote = git_text(bare.path(), &["rev-parse", "refs/heads/main"]).unwrap();
        assert_eq!(local, remote);
        let tags = Command::new("git")
            .args(["show-ref", "--tags"])
            .current_dir(bare.path())
            .output()
            .unwrap();
        assert!(!tags.status.success(), "push must not follow tags");
    }

    #[test]
    fn push_refuses_nothing_to_push_and_a_behind_branch() {
        let repo = repo();
        let bare = bare_remote();
        add_remote(repo.path(), "origin", bare.path());
        git(repo.path(), &["push", "-q", "-u", "origin", "main"]);
        let error = plan_push(repo.path(), "origin").unwrap_err();
        assert!(error.to_string().contains("nothing to push"), "{error}");

        git(repo.path(), &["checkout", "-q", "-b", "remote-work"]);
        std::fs::write(repo.path().join("remote.txt"), "remote\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "remote"]);
        let remote_head = git_text(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        git(repo.path(), &["checkout", "-q", "main"]);
        git(
            repo.path(),
            &["update-ref", "refs/remotes/origin/main", remote_head.trim()],
        );
        let error = plan_push(repo.path(), "origin").unwrap_err();
        assert!(error.to_string().contains("pull or merge"), "{error}");
    }

    #[test]
    fn push_refuses_no_remote_detached_head_and_an_operation() {
        let repo = repo();
        let error = list_push_targets(repo.path()).unwrap_err();
        assert!(error.to_string().contains("no git remote"), "{error}");

        let bare = bare_remote();
        add_remote(repo.path(), "origin", bare.path());
        git(repo.path(), &["checkout", "-q", "--detach", "HEAD"]);
        let error = list_push_targets(repo.path()).unwrap_err();
        assert!(error.to_string().contains("detached HEAD"), "{error}");
        git(repo.path(), &["checkout", "-q", "main"]);
        std::fs::write(repo.path().join(".git").join("MERGE_HEAD"), "x\n").unwrap();
        let error = list_push_targets(repo.path()).unwrap_err();
        assert!(error.to_string().contains("merge"), "{error}");
    }

    #[test]
    fn push_refuses_a_moved_head() {
        let repo = repo();
        let bare = bare_remote();
        add_remote(repo.path(), "origin", bare.path());
        std::fs::write(repo.path().join("first.txt"), "first\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "first"]);
        let plan = plan_push(repo.path(), "origin").unwrap();
        std::fs::write(repo.path().join("second.txt"), "second\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "second"]);
        let error = push(&plan).unwrap_err();
        assert!(error.to_string().contains("HEAD moved"), "{error}");
    }

    #[test]
    fn push_remote_named_like_an_option_stays_a_remote() {
        let repo = repo();
        let bare = bare_remote();
        git(
            repo.path(),
            &[
                "config",
                "remote.--force.url",
                bare.path().to_str().unwrap(),
            ],
        );
        git(
            repo.path(),
            &[
                "config",
                "remote.--force.fetch",
                "+refs/heads/*:refs/remotes/--force/*",
            ],
        );
        std::fs::write(repo.path().join("pushed.txt"), "pushed\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "pushed"]);
        let targets = list_push_targets(repo.path()).unwrap();
        assert!(targets.iter().any(|target| target.remote == "--force"));
        let plan = plan_push(repo.path(), "--force").unwrap();
        let arguments = push_arguments(&plan);
        assert_eq!(arguments[7], "--");
        assert_eq!(arguments[8], "--force");
        assert_eq!(arguments[9], format!("{}:refs/heads/main", plan.head));
        push(&plan).unwrap();
        let local = git_text(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        let remote = git_text(bare.path(), &["rev-parse", "refs/heads/main"]).unwrap();
        assert_eq!(local, remote);
    }

    #[test]
    fn push_preserves_a_branch_name_that_starts_with_refs_heads() {
        let repo = repo();
        let bare = bare_remote();
        add_remote(repo.path(), "origin", bare.path());
        git(repo.path(), &["checkout", "-q", "-b", "refs/heads/topic"]);
        let targets = list_push_targets(repo.path()).unwrap();
        assert_eq!(targets[0].remote_branch, "refs/heads/topic");
        assert_eq!(targets[0].remote_ref, "refs/heads/refs/heads/topic");
        let plan = plan_push(repo.path(), "origin").unwrap();
        let arguments = push_arguments(&plan);
        assert_eq!(
            arguments.last().unwrap(),
            &format!("{}:refs/heads/refs/heads/topic", plan.head)
        );
        push(&plan).unwrap();
        assert!(git_text(bare.path(), &["rev-parse", "refs/heads/refs/heads/topic"]).is_ok());
    }

    #[test]
    fn push_rejects_a_non_fast_forward_update_without_forcing() {
        let repo = repo();
        let bare = bare_remote();
        add_remote(repo.path(), "origin", bare.path());
        git(repo.path(), &["push", "-q", "origin", "main"]);
        git(repo.path(), &["checkout", "-q", "-b", "remote-work"]);
        std::fs::write(repo.path().join("remote.txt"), "remote\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "remote"]);
        git(repo.path(), &["push", "-q", "origin", "remote-work:main"]);
        let remote_before = git_text(bare.path(), &["rev-parse", "refs/heads/main"]).unwrap();
        git(repo.path(), &["checkout", "-q", "main"]);
        std::fs::write(repo.path().join("local.txt"), "local\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "local"]);
        for key in ["branch.main.remote", "branch.main.merge"] {
            let _ = Command::new("git")
                .args(["config", "--unset-all", key])
                .current_dir(repo.path())
                .output();
        }
        let plan = plan_push(repo.path(), "origin").unwrap();
        let error = push(&plan).unwrap_err();
        let message = error.to_string().to_ascii_lowercase();
        assert!(
            message.contains("rejected") || message.contains("non-fast-forward"),
            "{error}"
        );
        assert_eq!(
            git_text(bare.path(), &["rev-parse", "refs/heads/main"]).unwrap(),
            remote_before
        );
    }

    #[test]
    fn plan_push_refuses_an_unborn_branch_and_a_missing_remote() {
        let repo = repo();
        let bare = bare_remote();
        add_remote(repo.path(), "origin", bare.path());
        let error = plan_push(repo.path(), "missing").unwrap_err();
        assert!(error.to_string().contains("not configured"), "{error}");

        let unborn = tempfile::tempdir().unwrap();
        git(unborn.path(), &["init", "-q", "-b", "main"]);
        add_remote(unborn.path(), "origin", bare.path());
        let error = plan_push(unborn.path(), "origin").unwrap_err();
        assert!(error.to_string().contains("no commits"), "{error}");
    }
}
