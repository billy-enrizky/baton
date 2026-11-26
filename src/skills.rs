use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_MAX_FILES: usize = 2048;
const DEFAULT_MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TREE_DEPTH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillInstallMode {
    Install,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillInstallOutcome {
    Installed { destination: PathBuf, updated: bool },
    AlreadyInstalled(PathBuf),
    NotInstalled(PathBuf),
    SourceInProject,
}

#[derive(Debug, Clone, Copy)]
struct CopyLimits {
    files: usize,
    bytes: u64,
}

impl Default for CopyLimits {
    fn default() -> Self {
        Self {
            files: DEFAULT_MAX_FILES,
            bytes: DEFAULT_MAX_BYTES,
        }
    }
}

struct CopyProgress {
    files: usize,
    bytes: u64,
}

/// Skill names become directory names and composer tokens, so both uses share
/// one conservative ASCII rule.
pub fn is_safe_skill_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut has_letter = first.is_ascii_alphabetic();
    let valid = characters.all(|character| {
        has_letter |= character.is_ascii_alphabetic();
        character.is_ascii_alphanumeric() || matches!(character, ':' | '_' | '-')
    });
    valid && has_letter
}

pub fn install_skill(
    cwd: &Path,
    source: &Path,
    harness: &str,
    mode: SkillInstallMode,
) -> Result<SkillInstallOutcome> {
    install_skill_with_limits(cwd, source, harness, mode, CopyLimits::default())
}

fn install_skill_with_limits(
    cwd: &Path,
    source: &Path,
    harness: &str,
    mode: SkillInstallMode,
    limits: CopyLimits,
) -> Result<SkillInstallOutcome> {
    if !matches!(harness, "claude" | "codex") {
        bail!("skill harness must be claude or codex");
    }
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| is_safe_skill_name(name))
        .ok_or_else(|| anyhow::anyhow!("skill directory name cannot be installed"))?;
    let source_meta = fs::symlink_metadata(source)
        .with_context(|| format!("cannot inspect skill source {}", source.display()))?;
    if source_meta.file_type().is_symlink() {
        bail!("skill source is a symlink");
    }
    if !source_meta.is_dir() {
        bail!("skill source is not a directory");
    }
    if !source.join("SKILL.md").is_file() && !source.join("skill.md").is_file() {
        bail!("skill source has no SKILL.md");
    }
    let cwd_meta = fs::metadata(cwd)
        .with_context(|| format!("cannot inspect project directory {}", cwd.display()))?;
    if !cwd_meta.is_dir() {
        bail!("project directory is not a directory");
    }

    let cwd_canonical = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let source_canonical = fs::canonicalize(source)
        .with_context(|| format!("cannot resolve skill source {}", source.display()))?;
    let project_roots = [
        cwd_canonical.join(".claude").join("skills"),
        cwd_canonical.join(".codex").join("skills"),
    ];
    if project_roots
        .iter()
        .any(|root| source_canonical.starts_with(root))
    {
        return Ok(SkillInstallOutcome::SourceInProject);
    }

    let harness_dir = if harness == "claude" {
        ".claude"
    } else {
        ".codex"
    };
    let root = cwd.join(harness_dir).join("skills");
    reject_symlinked_destination(&cwd.join(harness_dir))?;
    reject_symlinked_destination(&root)?;
    let destination = root.join(name);
    let destination_meta = match fs::symlink_metadata(&destination) {
        Ok(meta) => Some(meta),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", destination.display()))
        }
    };
    if let Some(meta) = destination_meta.as_ref() {
        if meta.file_type().is_symlink() || !meta.is_dir() {
            bail!("project skill destination is not a regular directory");
        }
    }
    match (mode, destination_meta.is_some()) {
        (SkillInstallMode::Install, true) => {
            return Ok(SkillInstallOutcome::AlreadyInstalled(destination));
        }
        (SkillInstallMode::Update, false) => {
            return Ok(SkillInstallOutcome::NotInstalled(destination));
        }
        _ => {}
    }

    fs::create_dir_all(&root)
        .with_context(|| format!("cannot create skill root {}", root.display()))?;
    let staging = root.join(format!(".baton-staging-{}", uuid::Uuid::new_v4().simple()));
    let mut progress = CopyProgress { files: 0, bytes: 0 };
    if let Err(error) = copy_tree(source, &staging, limits, &mut progress, 0) {
        remove_owned_tree(&staging);
        return Err(error);
    }

    if mode == SkillInstallMode::Install {
        if let Err(error) = fs::rename(&staging, &destination) {
            remove_owned_tree(&staging);
            return Err(error).with_context(|| {
                format!(
                    "cannot move staged skill {} into {}",
                    staging.display(),
                    destination.display()
                )
            });
        }
        return Ok(SkillInstallOutcome::Installed {
            destination,
            updated: false,
        });
    }

    let backup = root.join(format!(".baton-backup-{}", uuid::Uuid::new_v4().simple()));
    fs::rename(&destination, &backup).with_context(|| {
        format!(
            "cannot move the existing project skill {} out of the way",
            destination.display()
        )
    })?;
    if let Err(error) = fs::rename(&staging, &destination) {
        remove_owned_tree(&staging);
        let restore = fs::rename(&backup, &destination);
        if let Err(restore_error) = restore {
            bail!(
                "cannot install the updated skill and cannot restore the previous copy; the previous copy is at {}: {error}; restore error: {restore_error}",
                backup.display()
            );
        }
        return Err(error).with_context(|| {
            format!(
                "cannot move staged skill {} into {}",
                staging.display(),
                destination.display()
            )
        });
    }
    remove_owned_tree(&backup);
    Ok(SkillInstallOutcome::Installed {
        destination,
        updated: true,
    })
}

fn reject_symlinked_destination(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                bail!(
                    "skill destination path is not a regular directory: {}",
                    path.display()
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("cannot inspect skill destination {}", path.display())),
    }
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    limits: CopyLimits,
    progress: &mut CopyProgress,
    depth: usize,
) -> Result<()> {
    if depth > MAX_TREE_DEPTH {
        bail!("skill tree is deeper than {MAX_TREE_DEPTH} directories");
    }
    fs::create_dir(destination)
        .with_context(|| format!("cannot create staging directory {}", destination.display()))?;
    for entry in fs::read_dir(source)
        .with_context(|| format!("cannot read skill directory {}", source.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if file_type.is_symlink() {
            bail!("skill contains a symlink: {}", path.display());
        }
        if file_type.is_dir() {
            copy_tree(&path, &target, limits, progress, depth + 1)?;
            continue;
        }
        if !file_type.is_file() {
            bail!("skill contains a non-regular file: {}", path.display());
        }
        progress.files += 1;
        if progress.files > limits.files {
            bail!("skill has more than {} files", limits.files);
        }
        let expected = entry.metadata()?.len();
        if expected > limits.bytes.saturating_sub(progress.bytes) {
            bail!("skill is larger than {} bytes", limits.bytes);
        }
        let copied = fs::copy(&path, &target)
            .with_context(|| format!("cannot copy skill file {}", path.display()))?;
        progress.bytes = progress.bytes.saturating_add(copied);
        if progress.bytes > limits.bytes {
            bail!("skill is larger than {} bytes", limits.bytes);
        }
    }
    Ok(())
}

fn remove_owned_tree(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(root: &Path, name: &str, body: &str) -> PathBuf {
        let path = root.join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), body).unwrap();
        path
    }

    #[test]
    fn install_copies_a_global_skill_into_the_project() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let global = root.path().join("global");
        fs::create_dir_all(&cwd).unwrap();
        let source = skill(&global, "demo", "version one\n");
        fs::write(source.join("notes.txt"), "asset\n").unwrap();

        let outcome = install_skill(&cwd, &source, "codex", SkillInstallMode::Install).unwrap();
        let SkillInstallOutcome::Installed {
            destination,
            updated,
        } = outcome
        else {
            panic!("expected install");
        };
        assert!(!updated);
        assert_eq!(destination, cwd.join(".codex/skills/demo"));
        assert_eq!(
            fs::read_to_string(destination.join("SKILL.md")).unwrap(),
            "version one\n"
        );
        assert_eq!(
            fs::read_to_string(destination.join("notes.txt")).unwrap(),
            "asset\n"
        );
    }

    #[test]
    fn install_refuses_an_existing_project_copy() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let global = root.path().join("global");
        fs::create_dir_all(&cwd).unwrap();
        let source = skill(&global, "demo", "new\n");
        let existing = skill(&cwd.join(".claude/skills"), "demo", "old\n");

        let outcome = install_skill(&cwd, &source, "claude", SkillInstallMode::Install).unwrap();
        assert_eq!(
            outcome,
            SkillInstallOutcome::AlreadyInstalled(existing.clone())
        );
        assert_eq!(
            fs::read_to_string(existing.join("SKILL.md")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn update_replaces_the_project_copy() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let global = root.path().join("global");
        fs::create_dir_all(&cwd).unwrap();
        let source = skill(&global, "demo", "version one\n");
        fs::write(source.join("old.txt"), "old\n").unwrap();
        install_skill(&cwd, &source, "claude", SkillInstallMode::Install).unwrap();

        fs::write(source.join("SKILL.md"), "version two\n").unwrap();
        fs::remove_file(source.join("old.txt")).unwrap();
        fs::write(source.join("new.txt"), "new\n").unwrap();
        let outcome = install_skill(&cwd, &source, "claude", SkillInstallMode::Update).unwrap();
        let SkillInstallOutcome::Installed {
            destination,
            updated,
        } = outcome
        else {
            panic!("expected update");
        };
        assert!(updated);
        assert_eq!(
            fs::read_to_string(destination.join("SKILL.md")).unwrap(),
            "version two\n"
        );
        assert!(!destination.join("old.txt").exists());
        assert_eq!(
            fs::read_to_string(destination.join("new.txt")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn update_requires_an_existing_project_copy() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let global = root.path().join("global");
        fs::create_dir_all(&cwd).unwrap();
        let source = skill(&global, "demo", "new\n");
        let outcome = install_skill(&cwd, &source, "codex", SkillInstallMode::Update).unwrap();
        assert_eq!(
            outcome,
            SkillInstallOutcome::NotInstalled(cwd.join(".codex/skills/demo"))
        );
    }

    #[test]
    fn a_project_skill_is_not_an_install_source() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let source = skill(&cwd.join(".claude/skills"), "demo", "project\n");
        let outcome = install_skill(&cwd, &source, "claude", SkillInstallMode::Install).unwrap();
        assert_eq!(outcome, SkillInstallOutcome::SourceInProject);
    }

    #[test]
    fn a_source_without_a_skill_file_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let source = root.path().join("global/demo");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        let error = install_skill(&cwd, &source, "codex", SkillInstallMode::Install).unwrap_err();
        assert!(error.to_string().contains("SKILL.md"), "{error}");
    }

    #[test]
    fn a_missing_project_directory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("missing");
        let global = root.path().join("global");
        let source = skill(&global, "demo", "one\n");
        let error = install_skill(&cwd, &source, "codex", SkillInstallMode::Install).unwrap_err();
        assert!(error.to_string().contains("project directory"), "{error}");
        assert!(!cwd.exists());
    }

    #[test]
    fn the_file_limit_is_enforced_before_a_destination_exists() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let global = root.path().join("global");
        fs::create_dir_all(&cwd).unwrap();
        let source = skill(&global, "demo", "one\n");
        fs::write(source.join("second.txt"), "two\n").unwrap();
        let error = install_skill_with_limits(
            &cwd,
            &source,
            "codex",
            SkillInstallMode::Install,
            CopyLimits {
                files: 1,
                bytes: 1024,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("more than 1 files"), "{error}");
        assert!(!cwd.join(".codex/skills/demo").exists());
        let root = cwd.join(".codex/skills");
        if root.exists() {
            assert!(fs::read_dir(root).unwrap().next().is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_the_source_is_refused() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let global = root.path().join("global");
        fs::create_dir_all(&cwd).unwrap();
        let source = skill(&global, "demo", "one\n");
        symlink("/etc/passwd", source.join("secret.txt")).unwrap();
        let error = install_skill(&cwd, &source, "codex", SkillInstallMode::Install).unwrap_err();
        assert!(error.to_string().contains("symlink"), "{error}");
        assert!(!cwd.join(".codex/skills/demo").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_project_directory_is_refused() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let global = root.path().join("global");
        let outside = root.path().join("outside");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let source = skill(&global, "demo", "one\n");
        symlink(&outside, cwd.join(".codex")).unwrap();
        let error = install_skill(&cwd, &source, "codex", SkillInstallMode::Install).unwrap_err();
        assert!(
            error.to_string().contains("symlink")
                || error.to_string().contains("regular directory"),
            "{error}"
        );
        assert!(!outside.join("skills/demo").exists());
    }
}
