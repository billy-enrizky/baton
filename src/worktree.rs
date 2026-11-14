//! Git worktree lifecycle for isolated worker threads.

use anyhow::Result;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub repo_root: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    pub base_ref: String,
    pub shared_paths: Vec<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RepoWorktreeConfig {
    #[serde(default)]
    worktree: RepoWorktreeSection,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RepoWorktreeSection {
    #[serde(default, rename = "sharedDirectories")]
    shared_directories: Vec<String>,
}

fn shared_directories(repo_root: &Path) -> Vec<String> {
    let path = repo_root.join("baton.json");
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<RepoWorktreeConfig>(&contents)
        .map(|config| config.worktree.shared_directories)
        .unwrap_or_default()
}

fn safe_relative(path: &str) -> Option<&Path> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return None;
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return None;
    }
    Some(path)
}

fn is_gitignored(repo_root: &Path, path: &str) -> bool {
    Command::new("git")
        .args(["check-ignore", "-q", "--", path])
        .current_dir(repo_root)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn share_directories(repo_root: &Path, worktree_path: &Path) -> Vec<String> {
    let mut shared = Vec::new();
    for entry in shared_directories(repo_root).into_iter().take(100) {
        let Some(relative) = safe_relative(&entry) else {
            continue;
        };
        let source = repo_root.join(relative);
        if !source.is_dir() || !is_gitignored(repo_root, &entry) {
            continue;
        }
        let destination = worktree_path.join(relative);
        if destination.exists() {
            continue;
        }
        if let Some(parent) = destination.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        #[cfg(unix)]
        let created = std::os::unix::fs::symlink(&source, &destination).is_ok();
        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_dir(&source, &destination).is_ok();
        if created {
            shared.push(entry);
        }
    }
    shared
}

fn git_raw(
    cwd: &Path,
    args: &[&str],
    envs: &[(&str, String)],
    stdin: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd);
    for (key, value) in envs {
        command.env(key, value);
    }
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(input) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(input)?;
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_raw(cwd, args, &[], None)?)
        .trim()
        .to_string())
}

pub fn repo_root(cwd: &Path) -> Result<PathBuf> {
    let root = git(cwd, &["rev-parse", "--show-toplevel"])?;
    if root.is_empty() {
        anyhow::bail!("not inside a git repository: {}", cwd.display());
    }
    Ok(PathBuf::from(root))
}

/// The stable root of the project containing `cwd`. A linked worktree shares
/// the main checkout's root, so memories and other project state do not split
/// between the main tree and its workers.
pub fn project_identity_root(cwd: &Path) -> Option<PathBuf> {
    if let Ok(common) = git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        let common = PathBuf::from(common.trim());
        if common.file_name().and_then(|name| name.to_str()) == Some(".git") {
            return common.parent().map(Path::to_path_buf);
        }
        return Some(common);
    }
    repo_root(cwd).ok()
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

/// A source checkout's dirty state captured without touching HEAD or the real
/// index. `tree` is the snapshot's git tree; `patch` applies that tree to a
/// worktree created at `base_ref`.
#[derive(Debug, Clone)]
pub struct RepoSnapshot {
    pub base_ref: String,
    pub tree: String,
    pub patch: Vec<u8>,
}

fn temp_index_path(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "baton-{prefix}-{}.index",
        uuid::Uuid::new_v4()
    ))
}

fn cleanup_index(path: &Path) {
    let _ = std::fs::remove_file(path);
    let lock = PathBuf::from(format!("{}.lock", path.display()));
    let _ = std::fs::remove_file(lock);
}

fn reset_worktree(path: &Path, target: &str) -> Result<()> {
    git(path, &["reset", "--hard", target])?;
    // Snapshot-only untracked files are not removed by reset. Worker-added
    // files are already in the commit, so a non-ignored clean removes exactly
    // the leftover snapshot files and leaves shared ignored directories.
    git(path, &["clean", "-fd"])?;
    Ok(())
}

fn tree_from_worktree(path: &Path) -> Result<String> {
    let index = temp_index_path("tree");
    let index_text = index.to_string_lossy().to_string();
    let envs = [("GIT_INDEX_FILE", index_text)];
    let result = (|| -> Result<String> {
        git_raw(path, &["read-tree", "HEAD"], &envs, None)?;
        git_raw(path, &["add", "-A"], &envs, None)?;
        let tree = git_raw(path, &["write-tree"], &envs, None)?;
        Ok(String::from_utf8_lossy(&tree).trim().to_string())
    })();
    cleanup_index(&index);
    result
}

/// Capture tracked and untracked non-ignored changes from a source checkout.
pub fn snapshot(repo_root: &Path) -> Result<Option<RepoSnapshot>> {
    if !is_dirty(repo_root)? {
        return Ok(None);
    }
    let base_ref = git(repo_root, &["rev-parse", "HEAD"])?;
    let index = temp_index_path("snapshot");
    let index_text = index.to_string_lossy().to_string();
    let envs = [("GIT_INDEX_FILE", index_text)];
    let tree = (|| -> Result<String> {
        git_raw(repo_root, &["read-tree", "HEAD"], &envs, None)?;
        git_raw(repo_root, &["add", "-A"], &envs, None)?;
        let tree = git_raw(repo_root, &["write-tree"], &envs, None)?;
        Ok(String::from_utf8_lossy(&tree).trim().to_string())
    })();
    cleanup_index(&index);
    let tree = tree?;
    let patch = git_raw(
        repo_root,
        &["diff", "--binary", "--no-color", "--no-ext-diff", "--no-textconv", &base_ref, &tree],
        &[],
        None,
    )?;
    Ok(Some(RepoSnapshot {
        base_ref,
        tree,
        patch,
    }))
}

/// Apply a captured source snapshot to a worker worktree.
pub fn apply_snapshot(worktree_path: &Path, snapshot: &RepoSnapshot) -> Result<()> {
    if snapshot.patch.is_empty() {
        return Ok(());
    }
    git_raw(
        worktree_path,
        &["apply", "--binary", "--whitespace=nowarn", "-"],
        &[],
        Some(&snapshot.patch),
    )?;
    Ok(())
}

/// Commit only paths the worker changed relative to the snapshot. Files that
/// were dirty in the source but untouched by the worker stay out of the worker
/// branch. For touched files, the final worktree content is committed so the
/// branch contains the complete result.
pub fn commit_changes_after_snapshot(
    path: &Path,
    snapshot_tree: &str,
    message: &str,
) -> Result<Option<String>> {
    let final_tree = tree_from_worktree(path)?;
    let changed = git_raw(
        path,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--no-renames",
            "-z",
            snapshot_tree,
            &final_tree,
        ],
        &[],
        None,
    )?;
    if changed.is_empty() {
        reset_worktree(path, "HEAD")?;
        return Ok(None);
    }

    let head_tree = git(path, &["rev-parse", "HEAD^{tree}"])?;
    let worker_tree = overlay_tree_paths(path, &head_tree, &final_tree, &changed)?;
    if worker_tree == head_tree {
        reset_worktree(path, "HEAD")?;
        return Ok(None);
    }
    let commit = git(
        path,
        &["commit-tree", &worker_tree, "-p", "HEAD", "-m", message],
    )?;
    reset_worktree(path, &commit)?;
    Ok(Some(commit))
}

fn overlay_tree_paths(
    cwd: &Path,
    base_tree: &str,
    source_tree: &str,
    changed: &[u8],
) -> Result<String> {
    let mut index_info = Vec::new();
    for raw_path in changed.split(|byte| *byte == 0) {
        if raw_path.is_empty() {
            continue;
        }
        let file = String::from_utf8_lossy(raw_path).to_string();
        let entry = git_raw(cwd, &["ls-tree", "-z", source_tree, "--", &file], &[], None)?;
        let Some(record) = entry.split(|byte| *byte == 0).find(|part| !part.is_empty()) else {
            index_info.extend_from_slice(
                format!("0 0000000000000000000000000000000000000000\t{file}\0").as_bytes(),
            );
            continue;
        };
        let record = String::from_utf8_lossy(record);
        let Some((meta, _)) = record.split_once('\t') else {
            anyhow::bail!("cannot parse ls-tree entry for {file}");
        };
        let mut fields = meta.split_whitespace();
        let mode = fields
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing mode for {file}"))?;
        let _kind = fields
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing type for {file}"))?;
        let object = fields
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing object for {file}"))?;
        index_info.extend_from_slice(format!("{mode} {object}\t{file}\0").as_bytes());
    }

    let index = temp_index_path("overlay");
    let index_text = index.to_string_lossy().to_string();
    let envs = [("GIT_INDEX_FILE", index_text)];
    let tree = (|| -> Result<String> {
        git_raw(cwd, &["read-tree", base_tree], &envs, None)?;
        git_raw(
            cwd,
            &["update-index", "-z", "--index-info"],
            &envs,
            Some(&index_info),
        )?;
        let tree = git_raw(cwd, &["write-tree"], &envs, None)?;
        Ok(String::from_utf8_lossy(&tree).trim().to_string())
    })();
    cleanup_index(&index);
    tree
}

/// Builds the tree a worker would have had if its branch commit contained the
/// source snapshot. The worker branch intentionally excludes untouched dirty
/// files, so a three-way merge must use this tree as "theirs", with the
/// snapshot tree as the merge base.
pub fn snapshot_result_tree(
    repo_root: &Path,
    base_ref: &str,
    snapshot_tree: &str,
    branch: &str,
) -> Result<String> {
    let branch_tree = git(repo_root, &["rev-parse", &format!("{branch}^{{tree}}")])?;
    let changed = git_raw(
        repo_root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--no-renames",
            "-z",
            base_ref,
            branch,
        ],
        &[],
        None,
    )?;
    overlay_tree_paths(repo_root, snapshot_tree, &branch_tree, &changed)
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
    let base_ref = git(repo_root, &["rev-parse", "HEAD"])?;
    create_at(baton_home, repo_root, run_id, title, &base_ref)
}

/// Creates a worktree at an explicit commit. Source snapshots use the commit
/// they were captured from so a concurrent source commit cannot desync the
/// patch baseline.
pub fn create_at(
    baton_home: &Path,
    repo_root: &Path,
    run_id: &str,
    title: &str,
    base_ref: &str,
) -> Result<WorktreeInfo> {
    let short = run_id.chars().take(8).collect::<String>();
    let slug = slugify(title);
    let branch = format!("baton/{short}/{slug}");
    let path = baton_home.join("worktrees").join(&short).join(&slug);
    if path.exists() {
        anyhow::bail!("worktree path already exists: {}", path.display());
    }
    let path_text = path.to_string_lossy().to_string();
    git(
        repo_root,
        &["worktree", "add", "-b", &branch, &path_text, base_ref],
    )?;
    let shared_paths = share_directories(repo_root, &path);
    Ok(WorktreeInfo {
        repo_root: repo_root.to_path_buf(),
        path,
        branch,
        base_ref: base_ref.to_string(),
        shared_paths,
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
    shipped_branches: &[String],
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
        let shipped = entry
            .branch
            .as_ref()
            .map(|branch| shipped_branches.iter().any(|shipped| shipped == branch))
            .unwrap_or(false);
        if !entry.merged && !shipped && !force {
            cleanup.skipped.push(WorktreeSkip {
                path: entry.path,
                branch: entry.branch,
                reason: "branch is not merged and has no opened PR".to_string(),
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
            let flag = if force || !entry.merged { "-D" } else { "-d" };
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
    fn snapshot_applies_dirty_tracked_and_untracked_files() {
        let root = repo();
        std::fs::write(root.path().join(".gitignore"), "ignored.log\n").unwrap();
        git(root.path(), &["add", ".gitignore"]).unwrap();
        git(root.path(), &["commit", "-q", "-m", "ignore"]).unwrap();
        let head = git(root.path(), &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(root.path().join("a.txt"), "dirty\n").unwrap();
        std::fs::write(root.path().join("b.txt"), "untracked\n").unwrap();
        std::fs::write(root.path().join("bin.dat"), [0_u8, 1, 2, 255]).unwrap();
        std::fs::write(root.path().join("ignored.log"), "ignored\n").unwrap();
        let repo_root = repo_root(root.path()).unwrap();
        let snapshot = snapshot(&repo_root).unwrap().expect("dirty source snapshots");
        assert_eq!(snapshot.base_ref, head);
        assert_eq!(git(root.path(), &["rev-parse", "HEAD"]).unwrap(), head);
        assert!(!snapshot.patch.is_empty());

        let home = tempfile::tempdir().unwrap();
        let wt = create_at(
            home.path(),
            &repo_root,
            "run-snapshot",
            "snapshot check",
            &snapshot.base_ref,
        )
        .unwrap();
        apply_snapshot(&wt.path, &snapshot).unwrap();
        assert_eq!(std::fs::read_to_string(wt.path.join("a.txt")).unwrap(), "dirty\n");
        assert_eq!(
            std::fs::read_to_string(wt.path.join("b.txt")).unwrap(),
            "untracked\n"
        );
        assert_eq!(
            std::fs::read(wt.path.join("bin.dat")).unwrap(),
            vec![0_u8, 1, 2, 255]
        );
        assert!(!wt.path.join("ignored.log").exists());
        assert!(git(&wt.path, &["status", "--porcelain"]).unwrap().contains("a.txt"));
        assert_eq!(std::fs::read_to_string(root.path().join("a.txt")).unwrap(), "dirty\n");
        assert!(root.path().join("b.txt").exists());
        remove(&wt).unwrap();
    }

    #[test]
    fn snapshot_worker_commit_excludes_untouched_source_changes() {
        let root = repo();
        let head = git(root.path(), &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(root.path().join("a.txt"), "dirty\n").unwrap();
        std::fs::write(root.path().join("source-only.txt"), "source\n").unwrap();
        let repo_root = repo_root(root.path()).unwrap();
        let snapshot = snapshot(&repo_root).unwrap().unwrap();
        let home = tempfile::tempdir().unwrap();
        let wt = create_at(
            home.path(),
            &repo_root,
            "run-worker-snapshot",
            "worker snapshot",
            &snapshot.base_ref,
        )
        .unwrap();
        apply_snapshot(&wt.path, &snapshot).unwrap();

        std::fs::write(wt.path.join("a.txt"), "worker final\n").unwrap();
        std::fs::write(wt.path.join("worker-only.txt"), "worker\n").unwrap();
        let commit = commit_changes_after_snapshot(
            &wt.path,
            &snapshot.tree,
            "baton worker: snapshot check",
        )
        .unwrap()
        .expect("worker changes commit");
        assert_eq!(git(&wt.path, &["status", "--porcelain"]).unwrap(), "");
        assert_eq!(
            git(&wt.path, &["show", &format!("{}:a.txt", wt.branch)]).unwrap(),
            "worker final"
        );
        assert_eq!(
            git(&wt.path, &["show", &format!("{}:worker-only.txt", wt.branch)]).unwrap(),
            "worker"
        );
        assert!(
            git(&wt.path, &["show", &format!("{}:source-only.txt", wt.branch)]).is_err(),
            "an untouched source-only file must not enter the worker branch"
        );
        assert!(
            !wt.path.join("source-only.txt").exists(),
            "snapshot-only files are cleared from the worker checkout after commit"
        );
        let result_tree = snapshot_result_tree(
            &repo_root,
            &snapshot.base_ref,
            &snapshot.tree,
            &wt.branch,
        )
        .unwrap();
        assert_eq!(
            git(root.path(), &["show", &format!("{result_tree}:a.txt")]).unwrap(),
            "worker final"
        );
        assert_eq!(
            git(root.path(), &["show", &format!("{result_tree}:source-only.txt")]).unwrap(),
            "source",
            "the merge result keeps untouched snapshot files"
        );

        assert_eq!(
            git(root.path(), &["rev-parse", &format!("{}^", wt.branch)]).unwrap(),
            head,
            "the initial worker commit stays directly on the source base"
        );
        assert_eq!(commit, git(root.path(), &["rev-parse", &wt.branch]).unwrap());
        std::fs::write(wt.path.join("follow-up.txt"), "later\n").unwrap();
        commit_changes(&wt.path, "baton worker: follow-up")
            .unwrap()
            .expect("follow-up commits");
        let result_tree = snapshot_result_tree(
            &repo_root,
            &snapshot.base_ref,
            &snapshot.tree,
            &wt.branch,
        )
        .unwrap();
        assert_eq!(
            git(root.path(), &["show", &format!("{result_tree}:follow-up.txt")]).unwrap(),
            "later",
            "the result tree follows the current branch tip after a follow-up commit"
        );
        assert_eq!(
            git(root.path(), &["show", &format!("{result_tree}:source-only.txt")]).unwrap(),
            "source",
            "follow-up commits do not drop untouched snapshot files"
        );
        assert!(root.path().join("source-only.txt").exists());
        assert_eq!(std::fs::read_to_string(root.path().join("a.txt")).unwrap(), "dirty\n");
        remove(&wt).unwrap();
    }

    #[test]
    fn snapshot_only_changes_do_not_create_a_worker_commit() {
        let root = repo();
        let head = git(root.path(), &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(root.path().join("a.txt"), "dirty\n").unwrap();
        std::fs::write(root.path().join("source-only.txt"), "source\n").unwrap();
        let repo_root = repo_root(root.path()).unwrap();
        let snapshot = snapshot(&repo_root).unwrap().unwrap();
        let home = tempfile::tempdir().unwrap();
        let wt = create_at(
            home.path(),
            &repo_root,
            "run-no-worker",
            "no worker changes",
            &snapshot.base_ref,
        )
        .unwrap();
        apply_snapshot(&wt.path, &snapshot).unwrap();
        let commit =
            commit_changes_after_snapshot(&wt.path, &snapshot.tree, "baton worker: noop").unwrap();
        assert_eq!(commit, None);
        assert_eq!(git(&wt.path, &["status", "--porcelain"]).unwrap(), "");
        assert!(!wt.path.join("source-only.txt").exists());
        assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]).unwrap(), head);
        remove(&wt).unwrap();
    }

    #[test]
    fn shares_only_existing_gitignored_directories() {
        let root = repo();
        let home = tempfile::tempdir().unwrap();
        let repo_root = repo_root(root.path()).unwrap();
        std::fs::create_dir_all(root.path().join("node_modules/pkg")).unwrap();
        std::fs::write(root.path().join("node_modules/pkg/index.js"), "x\n").unwrap();
        std::fs::create_dir_all(root.path().join("notignored")).unwrap();
        std::fs::write(root.path().join("notignored/file.txt"), "x\n").unwrap();
        std::fs::write(root.path().join(".gitignore"), "node_modules\n").unwrap();
        git(root.path(), &["add", ".gitignore"]).unwrap();
        git(root.path(), &["commit", "-q", "-m", "ignore"]).unwrap();
        std::fs::write(
            root.path().join("baton.json"),
            r#"{"worktree":{"sharedDirectories":["node_modules","notignored","missing","../outside"]}}"#,
        )
        .unwrap();

        let wt = create(home.path(), &repo_root, "run-share", "shared task").unwrap();
        assert_eq!(wt.shared_paths, vec!["node_modules"]);
        let linked = wt.path.join("node_modules");
        assert!(std::fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_link(&linked).unwrap(),
            root.path().canonicalize().unwrap().join("node_modules")
        );
        assert!(!wt.path.join("notignored").exists());
        assert!(git(&wt.path, &["status", "--porcelain"]).unwrap().is_empty());
        remove(&wt).unwrap();
        assert!(root.path().join("node_modules").is_dir());
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

        let cleanup = clean_managed(home.path(), Some("run-merg-extra"), false, &[]).unwrap();
        assert_eq!(cleanup.removed.len(), 1);
        assert!(cleanup.skipped.is_empty());
        assert!(!merged_wt.path.exists());
        assert!(dirty_wt.path.exists());
        let merged_ref = format!("refs/heads/{}", merged_wt.branch);
        assert!(
            git(root.path(), &["show-ref", "--verify", &merged_ref]).is_err(),
            "merged branch is deleted"
        );

        let blocked = clean_managed(home.path(), Some("run-dirt"), false, &[]).unwrap();
        assert!(blocked.removed.is_empty());
        assert_eq!(blocked.skipped.len(), 1);
        assert_eq!(blocked.skipped[0].reason, "dirty");
        assert!(dirty_wt.path.exists());

        let forced = clean_managed(home.path(), None, true, &[]).unwrap();
        assert_eq!(forced.removed.len(), 1);
        assert!(!dirty_wt.path.exists());
        assert!(list_managed(home.path()).unwrap().is_empty());
    }

    #[test]
    fn cleans_clean_unmerged_worktrees_only_when_the_branch_has_a_pr() {
        let root = repo();
        let home = tempfile::tempdir().unwrap();
        let repo_root = repo_root(root.path()).unwrap();
        let shipped = create(home.path(), &repo_root, "run-ship", "shipped task").unwrap();
        std::fs::write(shipped.path.join("shipped.txt"), "x\n").unwrap();
        commit_changes(&shipped.path, "worker commit").unwrap().unwrap();
        let unshipped = create(home.path(), &repo_root, "run-keep", "unshipped task").unwrap();
        std::fs::write(unshipped.path.join("unshipped.txt"), "x\n").unwrap();
        commit_changes(&unshipped.path, "worker commit").unwrap().unwrap();

        let cleanup = clean_managed(
            home.path(),
            None,
            false,
            std::slice::from_ref(&shipped.branch),
        )
        .unwrap();
        assert_eq!(cleanup.removed.len(), 1);
        assert_eq!(cleanup.removed[0].branch.as_deref(), Some(shipped.branch.as_str()));
        assert!(!shipped.path.exists());
        assert!(unshipped.path.exists(), "a branch without a PR is kept");
        assert_eq!(cleanup.skipped.len(), 1);
        assert_eq!(
            cleanup.skipped[0].reason,
            "branch is not merged and has no opened PR"
        );
        let shipped_ref = format!("refs/heads/{}", shipped.branch);
        assert!(
            git(root.path(), &["show-ref", "--verify", &shipped_ref]).is_err(),
            "the local shipped branch is deleted after the worktree is removed"
        );
    }

    #[test]
    fn non_git_directories_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        assert!(repo_root(root.path()).is_err());
    }
}
