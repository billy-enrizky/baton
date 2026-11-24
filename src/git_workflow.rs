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
    let branch = match git_text(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(branch) if !branch.trim().is_empty() => branch.trim().to_string(),
        _ => {
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
    Ok(CommitOutcome {
        sha: sha.trim().to_string(),
        files: plan.files.len(),
        message: plan.message.clone(),
    })
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
    let root = std::str::from_utf8(&output)
        .context("git reported a repository root that is not valid UTF-8")?
        .trim();
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
        _ => Ok((0, 0)),
    }
}

fn in_progress_operation(root: &Path) -> Result<Option<String>> {
    for name in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
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
}
