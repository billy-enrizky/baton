use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub fn checkpoint_ref(thread_id: &str, turn: i64) -> String {
    format!("refs/baton/checkpoints/{thread_id}/{turn}")
}

/// Worktree state as it was before a turn started; the matching
/// `checkpoint_ref` holds the state after the turn ended.
pub fn checkpoint_start_ref(thread_id: &str, turn: i64) -> String {
    format!("refs/baton/checkpoints/{thread_id}/{turn}.start")
}

fn git(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().with_context(|| format!("git {:?}", args))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {:?} failed: {}", args, stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn resolve_head(cwd: &Path) -> Result<String> {
    match git(cwd, &["rev-parse", "--verify", "HEAD"], &[]) {
        Ok(head) => Ok(head.trim().to_string()),
        Err(_) => Ok(EMPTY_TREE.to_string()),
    }
}

pub fn capture_checkpoint(cwd: &Path, thread_id: &str, turn: i64) -> Result<String> {
    let reference = checkpoint_ref(thread_id, turn);
    capture_checkpoint_at(cwd, &reference, &format!("baton checkpoint {thread_id} turn {turn}"))?;
    Ok(reference)
}

/// Captures the current worktree state (tracked + untracked files) under the
/// given reference without touching HEAD, the index, or the working tree.
pub fn capture_checkpoint_at(cwd: &Path, reference: &str, message: &str) -> Result<()> {
    let head = resolve_head(cwd)?;
    let index_dir = std::env::temp_dir().join(format!("baton-index-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&index_dir)?;
    let index_path = index_dir.join("index");
    let index_file = index_path.to_string_lossy().to_string();
    let index_env: Vec<(&str, &str)> = vec![("GIT_INDEX_FILE", index_file.as_str())];
    let result = (|| -> Result<()> {
        git(cwd, &["read-tree", &head], &index_env)?;
        git(cwd, &["add", "-A", "--", "."], &index_env)?;
        let tree = git(cwd, &["write-tree"], &index_env)?.trim().to_string();
        let mut commit_args = vec!["commit-tree", &tree, "-m", message];
        if head != EMPTY_TREE {
            commit_args.push("-p");
            commit_args.push(&head);
        }
        let commit_env = [
            ("GIT_AUTHOR_NAME", "baton".to_string()),
            ("GIT_AUTHOR_EMAIL", "baton@local".to_string()),
            ("GIT_COMMITTER_NAME", "baton".to_string()),
            ("GIT_COMMITTER_EMAIL", "baton@local".to_string()),
        ];
        let commit_env_refs: Vec<(&str, &str)> = commit_env
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let commit = git(cwd, &commit_args, &commit_env_refs)?.trim().to_string();
        git(cwd, &["update-ref", reference, &commit], &[])?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&index_dir);
    result
}

pub fn diff_checkpoint_against_worktree(cwd: &Path, reference: &str) -> Result<String> {
    Ok(git(cwd, &["--no-pager", "diff", reference], &[])?)
}

pub fn diff_checkpoints(cwd: &Path, ref_a: &str, ref_b: &str) -> Result<String> {
    Ok(git(cwd, &["--no-pager", "diff", ref_a, ref_b], &[])?)
}

pub fn restore_checkpoint(cwd: &Path, reference: &str) -> Result<()> {
    git(cwd, &["read-tree", "-u", "--reset", reference], &[])?;
    Ok(())
}
