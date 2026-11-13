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
    fn slugify_is_bounded_and_safe() {
        assert_eq!(slugify("  Fix: billing / mobile  "), "fix-billing-mobile");
        assert_eq!(slugify("!!!"), "task");
        assert!(slugify(&"a".repeat(100)).len() <= 40);
    }

    #[test]
    fn non_git_directories_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        assert!(repo_root(root.path()).is_err());
    }
}
