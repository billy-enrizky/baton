//! Git worktree lifecycle for isolated worker threads.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub repo_root: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    pub base_ref: String,
}

fn git(cwd: &Path, args: &[&str]) -> Result<String> {
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

pub fn repo_root(cwd: &Path) -> Result<PathBuf> {
    let root = git(cwd, &["rev-parse", "--show-toplevel"])?;
    if root.is_empty() {
        anyhow::bail!("not inside a git repository: {}", cwd.display());
    }
    Ok(PathBuf::from(root))
}

pub fn is_dirty(repo_root: &Path) -> Result<bool> {
    Ok(!git(repo_root, &["status", "--porcelain"])?.trim().is_empty())
}

pub fn commit_changes(path: &Path, message: &str) -> Result<Option<String>> {
    if git(path, &["status", "--porcelain"])?.trim().is_empty() {
        return Ok(None);
    }
    git(path, &["add", "-A"])?;
    git(path, &["commit", "-m", message])?;
    Ok(Some(git(path, &["rev-parse", "HEAD"])?))
}

pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for character in title.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

/// Creates a real git worktree and branch for one worker. The path is under
/// baton's home, never inside the user's checkout.
pub fn create(
    baton_home: &Path,
    repo_root: &Path,
    run_id: &str,
    title: &str,
) -> Result<WorktreeInfo> {
    let short = run_id.chars().take(8).collect::<String>();
    let slug = slugify(title);
    let branch = format!("baton/{short}/{slug}");
    let path = baton_home.join("worktrees").join(&short).join(&slug);
    if path.exists() {
        anyhow::bail!("worktree path already exists: {}", path.display());
    }
    let base_ref = git(repo_root, &["rev-parse", "HEAD"])?;
    let path_text = path.to_string_lossy().to_string();
    git(
        repo_root,
        &["worktree", "add", "-b", &branch, &path_text, &base_ref],
    )?;
    Ok(WorktreeInfo {
        repo_root: repo_root.to_path_buf(),
        path,
        branch,
        base_ref,
    })
}

#[derive(Debug, Clone)]
pub struct ManagedWorktree {
    pub path: PathBuf,
    pub run: String,
    pub task: String,
    pub repo_root: Option<PathBuf>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub dirty: bool,
    pub merged: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorktreeSkip {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct WorktreeCleanup {
    pub removed: Vec<ManagedWorktree>,
    pub skipped: Vec<WorktreeSkip>,
    pub warnings: Vec<String>,
}

fn git_optional(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).current_dir(cwd).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn repo_root_of_worktree(path: &Path) -> Option<PathBuf> {
    let common = git_optional(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .or_else(|| git_optional(path, &["rev-parse", "--git-common-dir"]))?;
    let common = PathBuf::from(common);
    let common = if common.is_absolute() {
        common
    } else {
        path.join(common)
    };
    common.parent().map(Path::to_path_buf)
}

pub fn list_managed(home: &Path) -> Result<Vec<ManagedWorktree>> {
    let root = home.join("worktrees");
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for run in std::fs::read_dir(&root)? {
        let run = run?;
        if !run.file_type()?.is_dir() {
            continue;
        }
        let run_name = run.file_name().to_string_lossy().to_string();
        for task in std::fs::read_dir(run.path())? {
            let task = task?;
            if !task.file_type()?.is_dir() {
                continue;
            }
            let path = task.path();
            let task_name = task.file_name().to_string_lossy().to_string();
            let repo_root = repo_root_of_worktree(&path);
            let branch = git_optional(&path, &["rev-parse", "--abbrev-ref", "HEAD"])
                .filter(|branch| branch != "HEAD");
            let head = git_optional(&path, &["rev-parse", "HEAD"]);
            let dirty = git_optional(&path, &["status", "--porcelain"])
                .map(|status| !status.is_empty())
                .unwrap_or(true);
            let target = repo_root
                .as_ref()
                .and_then(|root| git_optional(root, &["branch", "--show-current"]))
                .filter(|target| !target.is_empty());
            let merged = match (&repo_root, &head, target.as_deref()) {
                (Some(root), Some(head), Some(target)) => Command::new("git")
                    .args(["merge-base", "--is-ancestor", head, target])
                    .current_dir(root)
                    .output()
                    .map(|output| output.status.success())
                    .unwrap_or(false),
                _ => false,
            };
            let error = if repo_root.is_none() || head.is_none() {
                Some("not a git worktree".to_string())
            } else {
                None
            };
            entries.push(ManagedWorktree {
                path,
                run: run_name.clone(),
                task: task_name,
                repo_root,
                branch,
                head,
                dirty,
                merged,
                error,
            });
        }
    }
    entries.sort_by(|left, right| {
        (&left.run, &left.task).cmp(&(&right.run, &right.task))
    });
    Ok(entries)
}

pub fn run_matches(entry: &ManagedWorktree, filter: &str) -> bool {
    entry.run.starts_with(filter) || filter.starts_with(&entry.run)
}

pub fn clean_managed(
    home: &Path,
    run_filter: Option<&str>,
    force: bool,
) -> Result<WorktreeCleanup> {
    let mut cleanup = WorktreeCleanup {
        removed: Vec::new(),
        skipped: Vec::new(),
        warnings: Vec::new(),
    };
    for entry in list_managed(home)? {
        if let Some(filter) = run_filter {
            if !run_matches(&entry, filter) {
                continue;
            }
        }
        if let Some(error) = &entry.error {
            if !force {
                cleanup.skipped.push(WorktreeSkip {
                    path: entry.path,
                    branch: entry.branch,
                    reason: format!("not a valid worktree: {error}"),
                });
                continue;
            }
            match std::fs::remove_dir_all(&entry.path) {
                Ok(()) => {
                    if let Some(root) = &entry.repo_root {
                        let _ = git(root, &["worktree", "prune"]);
                    }
                    cleanup.removed.push(entry);
                }
                Err(remove_error) => cleanup.skipped.push(WorktreeSkip {
                    path: entry.path,
                    branch: entry.branch,
                    reason: format!("cannot remove directory: {remove_error}"),
                }),
            }
            continue;
        }
        if entry.dirty && !force {
            cleanup.skipped.push(WorktreeSkip {
                path: entry.path,
                branch: entry.branch,
                reason: "dirty".to_string(),
            });
            continue;
        }
        if !entry.merged && !force {
            cleanup.skipped.push(WorktreeSkip {
                path: entry.path,
                branch: entry.branch,
                reason: "branch is not merged into the current branch".to_string(),
            });
            continue;
        }
        let Some(repo_root) = entry.repo_root.clone() else {
            cleanup.skipped.push(WorktreeSkip {
                path: entry.path,
                branch: entry.branch,
                reason: "no repository root".to_string(),
            });
            continue;
        };
        let path_text = entry.path.to_string_lossy().to_string();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&path_text);
        if let Err(error) = git(&repo_root, &args) {
            cleanup.skipped.push(WorktreeSkip {
                path: entry.path,
                branch: entry.branch,
                reason: format!("git worktree remove failed: {error}"),
            });
            continue;
        }
        if let Some(branch) = &entry.branch {
            let flag = if force { "-D" } else { "-d" };
            if let Err(error) = git(&repo_root, &["branch", flag, branch]) {
                cleanup.warnings.push(format!(
                    "removed {} but could not delete branch {branch}: {error}",
                    entry.path.display()
                ));
            }
        }
        cleanup.removed.push(entry);
    }
    Ok(cleanup)
}

/// Removes the worktree directory and its branch. The caller should keep the
/// branch if it may contain work the user still wants; this helper is for
/// explicit cleanup and test teardown.
pub fn remove(worktree: &WorktreeInfo) -> Result<()> {
    let path = worktree.path.to_string_lossy().to_string();
    git(&worktree.repo_root, &["worktree", "remove", "--force", &path])?;
    let _ = git(&worktree.repo_root, &["branch", "-D", &worktree.branch]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q"]).unwrap();
        git(root.path(), &["config", "user.email", "test@test"]).unwrap();
        git(root.path(), &["config", "user.name", "test"]).unwrap();
        std::fs::write(root.path().join("a.txt"), "base\n").unwrap();
        git(root.path(), &["add", "."]).unwrap();
        git(root.path(), &["commit", "-q", "-m", "init"]).unwrap();
        root
    }

    #[test]
    fn creates_isolated_worktrees_and_removes_them() {
        let root = repo();
        let home = tempfile::tempdir().unwrap();
        let repo_root = repo_root(root.path()).unwrap();
        let first = create(home.path(), &repo_root, "run-12345678", "Billing audit").unwrap();
        assert!(first.path.is_dir());
        assert_eq!(first.branch, "baton/run-1234/billing-audit");
        assert_eq!(
            git(&first.path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            first.branch
        );
        std::fs::write(first.path.join("worker.txt"), "worker\n").unwrap();
        assert!(!root.path().join("worker.txt").exists());
        let second = create(home.path(), &repo_root, "run-12345678", "API audit").unwrap();
        assert_ne!(first.path, second.path);
        assert_eq!(second.branch, "baton/run-1234/api-audit");
        remove(&first).unwrap();
        remove(&second).unwrap();
        assert!(!first.path.exists());
        assert!(!second.path.exists());
    }

    #[test]
    fn commits_changes_to_the_worktree_branch() {
        let root = repo();
        let home = tempfile::tempdir().unwrap();
        let repo_root = repo_root(root.path()).unwrap();
        let wt = create(home.path(), &repo_root, "run-commit", "commit check").unwrap();
        assert_eq!(commit_changes(&wt.path, "noop").unwrap(), None);
        std::fs::write(wt.path.join("worker.txt"), "worker\n").unwrap();
        let sha = commit_changes(&wt.path, "baton worker: commit check")
            .unwrap()
            .expect("dirty worktree commits");
        assert_eq!(sha.len(), 40);
        assert_eq!(
            git(&wt.path, &["log", "-1", "--pretty=%s"]).unwrap(),
            "baton worker: commit check"
        );
        assert!(git(&wt.path, &["status", "--porcelain"]).unwrap().is_empty());
        assert_eq!(git(root.path(), &["rev-parse", &wt.branch]).unwrap(), sha);
        remove(&wt).unwrap();
    }

    #[test]
    fn slugify_is_bounded_and_safe() {
        assert_eq!(slugify("  Fix: billing / mobile  "), "fix-billing-mobile");
        assert_eq!(slugify("!!!"), "task");
        assert!(slugify(&"a".repeat(100)).len() <= 40);
    }

    #[test]
    fn lists_and_cleans_managed_worktrees_safely() {
        let root = repo();
        let home = tempfile::tempdir().unwrap();
        let repo_root = repo_root(root.path()).unwrap();
        let merged_wt = create(home.path(), &repo_root, "run-merg", "merged task").unwrap();
        std::fs::write(merged_wt.path.join("merged.txt"), "x\n").unwrap();
        commit_changes(&merged_wt.path, "worker commit").unwrap().unwrap();
        git(
            root.path(),
            &["merge", "--no-ff", "-q", &merged_wt.branch, "-m", "merge worker"],
        )
        .unwrap();

        let dirty_wt = create(home.path(), &repo_root, "run-dirt", "dirty task").unwrap();
        std::fs::write(dirty_wt.path.join("dirty.txt"), "x\n").unwrap();

        let listed = list_managed(home.path()).unwrap();
        assert_eq!(listed.len(), 2);
        let merged_entry = listed.iter().find(|entry| entry.run == "run-merg").unwrap();
        assert!(!merged_entry.dirty);
        assert!(merged_entry.merged);
        let dirty_entry = listed.iter().find(|entry| entry.run == "run-dirt").unwrap();
        assert!(dirty_entry.dirty);

        let cleanup = clean_managed(home.path(), Some("run-merg-extra"), false).unwrap();
        assert_eq!(cleanup.removed.len(), 1);
        assert!(cleanup.skipped.is_empty());
        assert!(!merged_wt.path.exists());
        assert!(dirty_wt.path.exists());
        let merged_ref = format!("refs/heads/{}", merged_wt.branch);
        assert!(
            git(root.path(), &["show-ref", "--verify", &merged_ref]).is_err(),
            "merged branch is deleted"
        );

        let blocked = clean_managed(home.path(), Some("run-dirt"), false).unwrap();
        assert!(blocked.removed.is_empty());
        assert_eq!(blocked.skipped.len(), 1);
        assert_eq!(blocked.skipped[0].reason, "dirty");
        assert!(dirty_wt.path.exists());

        let forced = clean_managed(home.path(), None, true).unwrap();
        assert_eq!(forced.removed.len(), 1);
        assert!(!dirty_wt.path.exists());
        assert!(list_managed(home.path()).unwrap().is_empty());
    }

    #[test]
    fn non_git_directories_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        assert!(repo_root(root.path()).is_err());
    }
}
