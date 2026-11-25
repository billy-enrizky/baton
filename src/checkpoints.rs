use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::store::{BatonStore, ThreadRow};

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
    let output = command
        .output()
        .with_context(|| format!("git {:?}", args))?;
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

/// A turn needs a snapshot only when the tree differs from HEAD, or when the
/// turn moves HEAD. One status call answers both, where a snapshot costs five
/// more git processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeState {
    pub clean: bool,
    pub head: Option<String>,
}

pub fn worktree_state(cwd: &Path) -> Result<WorktreeState> {
    let output = git(
        cwd,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=normal",
        ],
        &[],
    )?;
    let mut state = WorktreeState {
        clean: true,
        head: None,
    };
    for line in output.lines() {
        if let Some(oid) = line.strip_prefix("# branch.oid ") {
            let oid = oid.trim();
            if !oid.is_empty() && oid != "(initial)" {
                state.head = Some(oid.to_string());
            }
        } else if line.starts_with('#') || line.trim().is_empty() {
            continue;
        } else {
            state.clean = false;
        }
    }
    Ok(state)
}

pub fn capture_checkpoint(cwd: &Path, thread_id: &str, turn: i64) -> Result<String> {
    let reference = checkpoint_ref(thread_id, turn);
    capture_checkpoint_at(
        cwd,
        &reference,
        &format!("baton checkpoint {thread_id} turn {turn}"),
    )?;
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

/// A plain `git diff <ref>` ignores untracked files, so a new file that is
/// still on disk looks deleted. Stage the worktree into a temporary index and
/// diff the reference against that, which matches what a checkpoint captures.
pub fn diff_checkpoint_against_worktree(cwd: &Path, reference: &str) -> Result<String> {
    let index_dir = std::env::temp_dir().join(format!("baton-diff-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&index_dir)?;
    let index_path = index_dir.join("index");
    let index_file = index_path.to_string_lossy().to_string();
    let env: Vec<(&str, &str)> = vec![("GIT_INDEX_FILE", index_file.as_str())];
    let result = (|| -> Result<String> {
        git(cwd, &["add", "-A", "--", "."], &env)?;
        git(cwd, &["--no-pager", "diff", "--cached", reference], &env)
    })();
    let _ = std::fs::remove_dir_all(&index_dir);
    match result {
        Ok(diff) => Ok(diff),
        // A worktree that cannot be staged still gets the tracked comparison,
        // which is what this call returned before untracked files were covered.
        Err(_) => Ok(git(cwd, &["--no-pager", "diff", reference], &[])?),
    }
}

pub fn diff_checkpoints(cwd: &Path, ref_a: &str, ref_b: &str) -> Result<String> {
    Ok(git(cwd, &["--no-pager", "diff", ref_a, ref_b], &[])?)
}

pub const SHARED_RESTORE_MESSAGE: &str =
    "file restore requires an isolated worktree; this is the repository's main checkout. Rewind the conversation only, or pass --force";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreConsent {
    /// CLI default: refuse a shared main checkout and a dirty worktree.
    Safe,
    /// The TUI already asked. A dirty worktree is allowed; a shared checkout
    /// is not, because its files may belong to another thread.
    Confirmed,
    /// Explicit `--force`: allow a shared checkout and a dirty worktree.
    Force,
}

/// Rewrites the checkout from `reference`. The guard is deliberately in this
/// function rather than only in the callers, so a new caller cannot skip it.
pub fn restore_checkpoint(cwd: &Path, reference: &str, consent: RestoreConsent) -> Result<()> {
    if consent != RestoreConsent::Force {
        if crate::git_workflow::is_main_worktree(cwd)? {
            anyhow::bail!(SHARED_RESTORE_MESSAGE);
        }
        if consent == RestoreConsent::Safe && !worktree_state(cwd)?.clean {
            anyhow::bail!(
                "working tree has uncommitted changes; commit or stash them, then retry, or pass --force"
            );
        }
    }
    git(cwd, &["read-tree", "-u", "--reset", reference], &[])?;
    Ok(())
}

/// Refuses the restore when any thread under the same repository has durable
/// open work, then applies the checkout guard and restores `reference`.
pub fn restore_thread_checkpoint(
    store: &BatonStore,
    thread: &ThreadRow,
    reference: &str,
    consent: RestoreConsent,
) -> Result<()> {
    let cwd = Path::new(&thread.cwd);
    let root = crate::git_workflow::repository_root(cwd)?;
    if let Some((id, work)) = store.open_work_under(&root)? {
        anyhow::bail!("thread {id} has {work:?} open; finish or stop it before restoring");
    }
    restore_checkpoint(cwd, reference, consent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn repo() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(root.path())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "checkpoint@example.com"]);
        git(&["config", "user.name", "checkpoint"]);
        git(&["config", "commit.gpgsign", "false"]);
        root
    }

    fn head(cwd: &Path) -> String {
        git(cwd, &["rev-parse", "HEAD"], &[])
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn worktree_state_reports_a_clean_head_and_every_dirty_kind() {
        let root = repo();
        std::fs::write(root.path().join("a.txt"), "one\n").unwrap();
        git(root.path(), &["add", "."], &[]).unwrap();
        git(root.path(), &["commit", "-qm", "init"], &[]).unwrap();
        let committed = head(root.path());

        let state = worktree_state(root.path()).unwrap();
        assert!(state.clean);
        assert_eq!(state.head.as_deref(), Some(committed.as_str()));

        std::fs::write(root.path().join("a.txt"), "two\n").unwrap();
        assert!(!worktree_state(root.path()).unwrap().clean, "modified file");
        git(root.path(), &["checkout", "--", "a.txt"], &[]).unwrap();

        std::fs::write(root.path().join("new.txt"), "new\n").unwrap();
        assert!(
            !worktree_state(root.path()).unwrap().clean,
            "untracked file"
        );
        std::fs::remove_file(root.path().join("new.txt")).unwrap();

        std::fs::write(root.path().join("a.txt"), "three\n").unwrap();
        git(root.path(), &["add", "a.txt"], &[]).unwrap();
        assert!(!worktree_state(root.path()).unwrap().clean, "staged file");
        assert!(worktree_state(root.path()).unwrap().head.is_some());
    }

    #[test]
    fn worktree_state_handles_a_repository_without_commits() {
        let root = repo();
        let state = worktree_state(root.path()).unwrap();
        assert!(state.clean, "an empty repository has no changes");
        assert_eq!(state.head, None, "there is no commit to snapshot");

        std::fs::write(root.path().join("only.txt"), "content\n").unwrap();
        let state = worktree_state(root.path()).unwrap();
        assert!(!state.clean);
        assert_eq!(state.head, None);
    }

    #[test]
    fn restore_refuses_the_main_checkout_unless_forced() {
        let root = repo();
        std::fs::write(root.path().join("a.txt"), "one\n").unwrap();
        git(root.path(), &["add", "."], &[]).unwrap();
        git(root.path(), &["commit", "-qm", "init"], &[]).unwrap();

        std::fs::write(root.path().join("a.txt"), "two\n").unwrap();
        let reference = capture_checkpoint(root.path(), "main-guard", 1).unwrap();
        std::fs::write(root.path().join("a.txt"), "three\n").unwrap();

        for consent in [RestoreConsent::Safe, RestoreConsent::Confirmed] {
            let error = restore_checkpoint(root.path(), &reference, consent).unwrap_err();
            assert!(
                format!("{error:#}").contains("isolated worktree"),
                "{consent:?}: {error:#}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
            "three\n",
            "a refused restore must not touch the file"
        );

        restore_checkpoint(root.path(), &reference, RestoreConsent::Force).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
            "two\n"
        );
    }

    #[test]
    fn restore_allows_a_dirty_linked_worktree_after_confirmation() {
        let root = repo();
        std::fs::write(root.path().join("a.txt"), "one\n").unwrap();
        git(root.path(), &["add", "."], &[]).unwrap();
        git(root.path(), &["commit", "-qm", "init"], &[]).unwrap();

        let base = tempfile::tempdir().unwrap();
        let side = base.path().join("side");
        git(
            root.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                side.to_str().unwrap(),
            ],
            &[],
        )
        .unwrap();

        std::fs::write(side.join("a.txt"), "two\n").unwrap();
        let reference = capture_checkpoint(&side, "side-guard", 1).unwrap();
        std::fs::write(side.join("a.txt"), "three\n").unwrap();

        let error = restore_checkpoint(&side, &reference, RestoreConsent::Safe).unwrap_err();
        assert!(
            format!("{error:#}").contains("uncommitted changes"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read_to_string(side.join("a.txt")).unwrap(),
            "three\n"
        );

        restore_checkpoint(&side, &reference, RestoreConsent::Confirmed).unwrap();
        assert_eq!(
            std::fs::read_to_string(side.join("a.txt")).unwrap(),
            "two\n"
        );
    }

    #[test]
    fn restore_refuses_when_another_thread_shares_the_repository() {
        use crate::store::{NewThread, ThreadOpenWork};
        use crate::types::ProviderId;

        let root = repo();
        std::fs::write(root.path().join("a.txt"), "one\n").unwrap();
        git(root.path(), &["add", "."], &[]).unwrap();
        git(root.path(), &["commit", "-qm", "init"], &[]).unwrap();

        let store = BatonStore::memory().unwrap();
        let new_thread = |title: &str| NewThread {
            title: title.to_string(),
            cwd: root.path().to_string_lossy().to_string(),
            current_provider: ProviderId::Codex,
            current_model: None,
        };
        let owner = store.create_thread(new_thread("owner")).unwrap();
        let other = store.create_thread(new_thread("other")).unwrap();
        let reference = capture_checkpoint(root.path(), &owner.id, 1).unwrap();
        store.enqueue_message(&other.id, "queued").unwrap();

        let error = restore_thread_checkpoint(&store, &owner, &reference, RestoreConsent::Force)
            .unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains(&other.id), "{text}");
        assert!(
            text.contains(&format!("{:?}", ThreadOpenWork::Queue)),
            "{text}"
        );
    }
}
