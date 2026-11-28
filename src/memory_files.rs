//! Discovery and editing for harness-owned memory files. These are the files
//! Claude Code and Codex load themselves; Baton only lists and opens them.

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub const MAX_MEMORY_FILES: usize = 200;
const MAX_RULE_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEnvironment {
    pub config_dir: PathBuf,
    pub config_dir_is_explicit: bool,
    pub codex_home: PathBuf,
    pub home_dir: PathBuf,
    pub project_dir_name: Option<String>,
    pub remote_memory_dir: Option<PathBuf>,
    pub cowork_memory_path: Option<PathBuf>,
    pub disable_auto_memory: Option<bool>,
    pub remote: bool,
}

impl MemoryEnvironment {
    pub fn from_env() -> Self {
        Self {
            config_dir: crate::paths::claude_global_settings_dir(),
            config_dir_is_explicit: env_string("CLAUDE_CONFIG_DIR").is_some(),
            codex_home: crate::paths::codex_global_home(),
            home_dir: dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
            project_dir_name: env_string("CLAUDE_CODE_PROJECT_DIR_NAME"),
            remote_memory_dir: env_string("CLAUDE_CODE_REMOTE_MEMORY_DIR").map(PathBuf::from),
            cowork_memory_path: env_string("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE").map(PathBuf::from),
            disable_auto_memory: env_string("CLAUDE_CODE_DISABLE_AUTO_MEMORY")
                .and_then(|value| parse_disable_auto_memory(&value)),
            remote: env_truthy("CLAUDE_CODE_REMOTE").unwrap_or(false),
        }
    }

    pub fn for_test(config_dir: PathBuf, codex_home: PathBuf, home_dir: PathBuf) -> Self {
        Self {
            config_dir,
            config_dir_is_explicit: true,
            codex_home,
            home_dir,
            project_dir_name: None,
            remote_memory_dir: None,
            cowork_memory_path: None,
            disable_auto_memory: None,
            remote: false,
        }
    }
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_truthy(name: &str) -> Option<bool> {
    let value = env_string(name)?;
    Some(matches!(
        value.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    ))
}

fn parse_disable_auto_memory(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct MemoryListing {
    pub sections: Vec<MemorySection>,
    pub auto_memory: AutoMemoryState,
    pub warnings: Vec<String>,
}

impl MemoryListing {
    pub fn iter(&self) -> impl Iterator<Item = (&MemorySection, &MemoryRow)> {
        self.sections
            .iter()
            .flat_map(|section| section.rows.iter().map(move |row| (section, row)))
    }

    pub fn row(&self, index: usize) -> Option<(&MemorySection, &MemoryRow)> {
        self.iter().nth(index)
    }

    pub fn rows(&self) -> Vec<(&MemorySection, &MemoryRow)> {
        self.iter().collect()
    }

    pub fn row_count(&self) -> usize {
        self.sections.iter().map(|section| section.rows.len()).sum()
    }
}

#[derive(Debug, Clone)]
pub struct MemorySection {
    pub title: &'static str,
    pub rows: Vec<MemoryRow>,
}

#[derive(Debug, Clone)]
pub struct MemoryRow {
    pub label: String,
    pub detail: String,
    pub action: MemoryAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryAction {
    ToggleAutoMemory,
    OpenFile { path: PathBuf },
    OpenFolder { path: PathBuf },
}

#[derive(Debug, Clone)]
pub struct AutoMemoryState {
    pub enabled: bool,
    pub enabled_source: String,
    pub user_enabled: Option<bool>,
    pub directory: PathBuf,
    pub directory_source: String,
    pub can_toggle: bool,
    pub read_only_reason: Option<String>,
}

#[derive(Default)]
struct SettingsScopes {
    user: Option<Value>,
    project: Option<Value>,
    local: Option<Value>,
    has_error: bool,
}

pub fn discover(cwd: &Path, env: &MemoryEnvironment) -> MemoryListing {
    let root = working_root(cwd);
    let memory_root =
        crate::worktree::auto_memory_identity_root(cwd).unwrap_or_else(|| root.clone());
    let local_root = if same_path(&memory_root, &env.home_dir) {
        root.clone()
    } else {
        memory_root.clone()
    };
    let ancestors = ancestor_instruction_dirs(cwd);
    let mut warnings = Vec::new();
    let (user, user_error) = read_settings(
        &env.config_dir.join("settings.json"),
        "user settings",
        &mut warnings,
    );
    let (project, project_error) = read_settings(
        &root.join(".claude").join("settings.json"),
        "project settings",
        &mut warnings,
    );
    let (local, local_error) = read_settings(
        &local_root.join(".claude").join("settings.local.json"),
        "local settings",
        &mut warnings,
    );
    let settings = SettingsScopes {
        user,
        project,
        local,
        has_error: user_error || project_error || local_error,
    };
    let auto_memory = resolve_auto_memory(&memory_root, env, &settings, &mut warnings);

    let mut claude_rows = Vec::new();
    claude_rows.push(memory_file_row(
        "User instructions",
        env.config_dir.join("CLAUDE.md"),
        env,
        "Saved in",
        "Missing · enter creates",
    ));
    for path in collect_markdown_files(&env.config_dir.join("rules"), &mut warnings) {
        claude_rows.push(memory_path_row(
            &display_path(&path, &env.home_dir),
            path,
            "User rule",
        ));
    }
    for dir in &ancestors {
        for path in [dir.join("CLAUDE.md"), dir.join(".claude").join("CLAUDE.md")] {
            if path.is_file() {
                claude_rows.push(memory_file_row(
                    "Parent instructions",
                    path,
                    env,
                    "Claude reads this at",
                    "Missing · enter creates",
                ));
            }
        }
    }
    claude_rows.push(memory_file_row(
        "Project instructions",
        root.join("CLAUDE.md"),
        env,
        "Checked in at",
        "Missing · enter creates",
    ));
    let alternate_project = root.join(".claude").join("CLAUDE.md");
    if alternate_project.exists() {
        claude_rows.push(memory_file_row(
            "Project instructions",
            alternate_project,
            env,
            "Checked in at",
            "Missing · enter creates",
        ));
    }
    let claude_agents = root.join(".claude").join("AGENTS.md");
    if claude_agents.exists() {
        claude_rows.push(memory_file_row(
            "AGENTS.md",
            claude_agents,
            env,
            "Claude reads this only without CLAUDE.md or CLAUDE.local.md at",
            "Missing · enter creates",
        ));
    }
    claude_rows.push(memory_file_row(
        "Local instructions",
        root.join("CLAUDE.local.md"),
        env,
        "Saved in",
        "Missing · enter creates",
    ));
    for path in collect_markdown_files(&root.join(".claude").join("rules"), &mut warnings) {
        claude_rows.push(memory_path_row(
            &display_path(&path, &env.home_dir),
            path,
            "Project rule",
        ));
    }

    let mut codex_rows = Vec::new();
    let user_override = env.codex_home.join("AGENTS.override.md");
    let has_user_override = user_override.is_file();
    if has_user_override {
        codex_rows.push(memory_file_row(
            "User override",
            user_override,
            env,
            "Codex override at",
            "Missing · enter creates",
        ));
    }
    codex_rows.push(memory_file_row(
        "User instructions",
        env.codex_home.join("AGENTS.md"),
        env,
        if has_user_override {
            "Fallback; AGENTS.override.md wins at"
        } else {
            "Saved in"
        },
        "Missing · enter creates",
    ));
    for dir in &ancestors {
        let override_path = dir.join("AGENTS.override.md");
        let has_override = override_path.is_file();
        if has_override {
            codex_rows.push(memory_file_row(
                "Parent override",
                override_path,
                env,
                "Codex override at",
                "Missing · enter creates",
            ));
        }
        let agents = dir.join("AGENTS.md");
        if agents.is_file() {
            codex_rows.push(memory_file_row(
                "Parent instructions",
                agents,
                env,
                if has_override {
                    "Codex fallback; override wins at"
                } else {
                    "Codex reads this at"
                },
                "Missing · enter creates",
            ));
        }
    }
    let project_override = root.join("AGENTS.override.md");
    let has_project_override = project_override.is_file();
    if has_project_override {
        codex_rows.push(memory_file_row(
            "Project override",
            project_override,
            env,
            "Codex override at",
            "Missing · enter creates",
        ));
    }
    codex_rows.push(memory_file_row(
        "Project instructions",
        root.join("AGENTS.md"),
        env,
        if has_project_override {
            "Codex fallback; override wins · Claude reads this only without CLAUDE.md or CLAUDE.local.md at"
        } else {
            "Codex project instructions · Claude reads this only without CLAUDE.md or CLAUDE.local.md at"
        },
        "Missing · enter creates",
    ));
    let mut auto_rows = vec![MemoryRow {
        label: "Auto-memory".to_string(),
        detail: auto_memory_detail(&auto_memory),
        action: MemoryAction::ToggleAutoMemory,
    }];
    auto_rows.push(memory_file_row(
        "Auto-memory index",
        auto_memory.directory.join("MEMORY.md"),
        env,
        "Index",
        "Missing · enter creates",
    ));
    if let Ok(entries) = std::fs::read_dir(&auto_memory.directory) {
        let mut topics = entries
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .filter(|path| path.file_name().and_then(|name| name.to_str()) != Some("MEMORY.md"))
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
            .collect::<Vec<_>>();
        topics.sort();
        if topics.len() > MAX_MEMORY_FILES {
            warnings.push(format!(
                "auto-memory topic list stopped at {MAX_MEMORY_FILES} files"
            ));
            topics.truncate(MAX_MEMORY_FILES);
        }
        for path in topics {
            let label = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| display_path(&path, &env.home_dir));
            let detail = format!("Auto-memory topic · {}", display_path(&path, &env.home_dir));
            auto_rows.push(memory_path_row(&label, path, &detail));
        }
    }
    auto_rows.push(MemoryRow {
        label: "Open auto-memory folder".to_string(),
        detail: format!(
            "{} · {}",
            display_path(&auto_memory.directory, &env.home_dir),
            auto_memory.directory_source
        ),
        action: MemoryAction::OpenFolder {
            path: auto_memory.directory.clone(),
        },
    });

    MemoryListing {
        sections: vec![
            MemorySection {
                title: "Claude Code",
                rows: claude_rows,
            },
            MemorySection {
                title: "Codex",
                rows: codex_rows,
            },
            MemorySection {
                title: "Auto memory",
                rows: auto_rows,
            },
        ],
        auto_memory,
        warnings,
    }
}

fn auto_memory_detail(state: &AutoMemoryState) -> String {
    let user = match state.user_enabled {
        Some(true) => "user on",
        Some(false) => "user off",
        None => "user unset",
    };
    let effective = if state.enabled { "on" } else { "off" };
    let mut detail = format!("{user} · effective {effective} · {}", state.enabled_source);
    if !state.can_toggle {
        if let Some(reason) = state.read_only_reason.as_deref() {
            detail.push_str(&format!(" · read-only: {reason}"));
        }
    }
    detail
}

fn memory_file_row(
    label: &str,
    path: PathBuf,
    env: &MemoryEnvironment,
    existing_detail: &str,
    missing_detail: &str,
) -> MemoryRow {
    let exists = path.is_file();
    let detail = if exists {
        format!("{existing_detail} {}", display_path(&path, &env.home_dir))
    } else {
        format!("{missing_detail} · {}", display_path(&path, &env.home_dir))
    };
    MemoryRow {
        label: label.to_string(),
        detail,
        action: MemoryAction::OpenFile { path },
    }
}

fn memory_path_row(label: &str, path: PathBuf, detail: &str) -> MemoryRow {
    MemoryRow {
        label: label.to_string(),
        detail: detail.to_string(),
        action: MemoryAction::OpenFile { path },
    }
}

fn resolve_auto_memory(
    root: &Path,
    env: &MemoryEnvironment,
    settings: &SettingsScopes,
    warnings: &mut Vec<String>,
) -> AutoMemoryState {
    for (settings, source) in [
        (&settings.user, "user settings"),
        (&settings.project, "project settings"),
        (&settings.local, "local settings"),
    ] {
        setting_type_warnings(settings, source, warnings);
    }
    let mut enabled = true;
    let mut enabled_source = "default".to_string();
    let user_enabled = setting_bool(&settings.user, "user settings").map(|(value, _)| value);
    let mut can_toggle = true;
    let mut read_only_reason = None;
    if settings.has_error {
        can_toggle = false;
        read_only_reason = Some("fix malformed or non-regular settings first".to_string());
    }
    if let Some(forced) = env.disable_auto_memory {
        enabled = !forced;
        enabled_source = "CLAUDE_CODE_DISABLE_AUTO_MEMORY".to_string();
        can_toggle = false;
        read_only_reason = Some(if forced {
            "environment disables auto memory".to_string()
        } else {
            "environment forces auto memory on".to_string()
        });
    } else if env.remote && env.remote_memory_dir.is_none() {
        enabled = false;
        enabled_source = "remote session".to_string();
        can_toggle = false;
        read_only_reason = Some("start a local session to enable auto memory".to_string());
    } else if let Some((value, source)) = setting_bool(&settings.local, "local settings")
        .or_else(|| setting_bool(&settings.project, "project settings"))
        .or_else(|| setting_bool(&settings.user, "user settings"))
    {
        enabled = value;
        enabled_source = source.to_string();
    }

    let (mut directory, mut directory_source) = default_auto_memory_dir(root, env, warnings);
    let mut cowork_applied = false;
    if let Some(path) = env.cowork_memory_path.as_deref() {
        if let Some(path) = absolute_setting_path(path, &env.home_dir) {
            directory = path;
            directory_source = "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE".to_string();
            cowork_applied = true;
        } else {
            warnings.push(
                "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE must be absolute or start with ~/".to_string(),
            );
        }
    }
    if !cowork_applied {
        if let Some((value, source)) = setting_string(&settings.user, "user settings") {
            if let Some(path) = absolute_setting_path(Path::new(&value), &env.home_dir) {
                directory = path;
                directory_source = format!("autoMemoryDirectory in {source}");
            } else {
                warnings.push(format!(
                    "autoMemoryDirectory in {source} must be absolute or start with ~/"
                ));
            }
        }
    }
    for (settings, source) in [
        (&settings.local, "local settings"),
        (&settings.project, "project settings"),
    ] {
        if setting_string(settings, source).is_some() {
            warnings.push(format!(
                "autoMemoryDirectory in {source} is not applied; Baton does not model workspace trust"
            ));
        }
    }

    AutoMemoryState {
        enabled,
        enabled_source,
        user_enabled,
        directory,
        directory_source,
        can_toggle,
        read_only_reason,
    }
}

fn default_auto_memory_dir(
    root: &Path,
    env: &MemoryEnvironment,
    warnings: &mut Vec<String>,
) -> (PathBuf, String) {
    let (base, source) = match env.remote_memory_dir.as_deref() {
        Some(path) if path.is_absolute() => (path.to_path_buf(), "remote memory base".to_string()),
        Some(_) => {
            warnings.push("CLAUDE_CODE_REMOTE_MEMORY_DIR must be an absolute path".to_string());
            (env.config_dir.clone(), "default project memory".to_string())
        }
        None => (env.config_dir.clone(), "default project memory".to_string()),
    };
    let project = project_dir_name(env, warnings).unwrap_or_else(|| encode_project_path(root));
    (base.join("projects").join(project).join("memory"), source)
}

fn project_dir_name(env: &MemoryEnvironment, warnings: &mut Vec<String>) -> Option<String> {
    let name = env.project_dir_name.as_deref()?;
    if !env.config_dir_is_explicit {
        warnings
            .push("CLAUDE_CODE_PROJECT_DIR_NAME is ignored without CLAUDE_CONFIG_DIR".to_string());
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let reserved = matches!(
        lower.as_str(),
        "con"
            | "prn"
            | "aux"
            | "nul"
            | "com1"
            | "com2"
            | "com3"
            | "com4"
            | "com5"
            | "com6"
            | "com7"
            | "com8"
            | "com9"
            | "lpt1"
            | "lpt2"
            | "lpt3"
            | "lpt4"
            | "lpt5"
            | "lpt6"
            | "lpt7"
            | "lpt8"
            | "lpt9"
            | "com0"
            | "lpt0"
    );
    if !(1..=64).contains(&name.len())
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        || reserved
    {
        warnings.push(
            "CLAUDE_CODE_PROJECT_DIR_NAME must be 1-64 letters, digits, hyphens, or underscores and not a Windows device name"
                .to_string(),
        );
        return None;
    }
    Some(name.to_string())
}

fn encode_project_path(root: &Path) -> String {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let text = canonical.to_string_lossy();
    let sanitized = text
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.len() > 200 {
        let prefix = sanitized.chars().take(200).collect::<String>();
        format!("{prefix}-{}", path_hash(&text))
    } else {
        sanitized
    }
}

fn path_hash(path: &str) -> String {
    let mut hash = 0u32;
    for unit in path.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(unit));
    }
    let mut value = hash;
    if value == 0 {
        return "0".to_string();
    }
    let mut encoded = String::new();
    while value > 0 {
        let digit = (value % 36) as u8;
        encoded.insert(
            0,
            match digit {
                0..=9 => char::from(b'0' + digit),
                _ => char::from(b'a' + digit - 10),
            },
        );
        value /= 36;
    }
    encoded
}

fn absolute_setting_path(path: &Path, home: &Path) -> Option<PathBuf> {
    let text = path.to_str()?;
    if let Some(rest) = text.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    path.is_absolute().then(|| path.to_path_buf())
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

fn setting_bool(settings: &Option<Value>, source: &'static str) -> Option<(bool, &'static str)> {
    settings
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|object| object.get("autoMemoryEnabled"))
        .and_then(Value::as_bool)
        .map(|value| (value, source))
}

fn setting_string(
    settings: &Option<Value>,
    source: &'static str,
) -> Option<(String, &'static str)> {
    settings
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|object| object.get("autoMemoryDirectory"))
        .and_then(Value::as_str)
        .map(|value| (value.to_string(), source))
}

fn setting_type_warnings(settings: &Option<Value>, source: &str, warnings: &mut Vec<String>) {
    let Some(object) = settings.as_ref().and_then(Value::as_object) else {
        return;
    };
    if object
        .get("autoMemoryEnabled")
        .is_some_and(|value| !value.is_boolean())
    {
        warnings.push(format!(
            "autoMemoryEnabled in {source} must be true or false"
        ));
    }
    if object
        .get("autoMemoryDirectory")
        .is_some_and(|value| !value.is_string())
    {
        warnings.push(format!("autoMemoryDirectory in {source} must be a string"));
    }
}

fn read_settings(path: &Path, label: &str, warnings: &mut Vec<String>) -> (Option<Value>, bool) {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (None, false),
        Err(error) => {
            warnings.push(format!("cannot inspect {}: {error}", path.display()));
            return (None, true);
        }
    };
    if !metadata.file_type().is_file() {
        warnings.push(format!("{label} {} is not a regular file", path.display()));
        return (None, true);
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            warnings.push(format!("cannot read {}: {error}", path.display()));
            return (None, true);
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(object)) => (Some(Value::Object(object)), false),
        Ok(_) => {
            warnings.push(format!("{label} {} is not a JSON object", path.display()));
            (None, true)
        }
        Err(error) => {
            warnings.push(format!("cannot parse {}: {error}", path.display()));
            (None, true)
        }
    }
}

fn collect_markdown_files(root: &Path, warnings: &mut Vec<String>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(metadata) = std::fs::symlink_metadata(root) {
        if metadata.file_type().is_symlink() {
            warnings.push(format!(
                "memory rule directory is a symlink; not followed: {}",
                root.display()
            ));
            return files;
        }
    }
    let mut truncated = false;
    let mut depth_limited = false;
    let rule_root = std::fs::canonicalize(root).ok();
    collect_markdown_files_inner(
        root,
        rule_root.as_deref(),
        0,
        &mut files,
        &mut truncated,
        &mut depth_limited,
        warnings,
    );
    if truncated {
        warnings.push(format!(
            "memory file list stopped at {MAX_MEMORY_FILES} files"
        ));
    }
    if depth_limited {
        warnings.push(format!(
            "memory rule scan stopped at depth {MAX_RULE_DEPTH}"
        ));
    }
    files
}

fn collect_markdown_files_inner(
    directory: &Path,
    rule_root: Option<&Path>,
    depth: usize,
    files: &mut Vec<PathBuf>,
    truncated: &mut bool,
    depth_limited: &mut bool,
    warnings: &mut Vec<String>,
) {
    if *truncated {
        return;
    }
    if depth > MAX_RULE_DEPTH {
        *depth_limited = true;
        return;
    }
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            warnings.push(format!("cannot read {}: {error}", directory.display()));
            return;
        }
    };
    let mut entries = entries
        .filter_map(std::result::Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if files.len() >= MAX_MEMORY_FILES {
            *truncated = true;
            return;
        }
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                warnings.push(format!("cannot inspect {}: {error}", path.display()));
                continue;
            }
        };
        if file_type.is_dir() {
            collect_markdown_files_inner(
                &path,
                rule_root,
                depth + 1,
                files,
                truncated,
                depth_limited,
                warnings,
            );
        } else if file_type.is_symlink() {
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let Some(root) = rule_root else {
                continue;
            };
            match std::fs::canonicalize(&path) {
                Ok(target) if target.is_file() && target.starts_with(root) => files.push(path),
                Ok(target) if target.is_file() => warnings.push(format!(
                    "ignored memory rule symlink outside {}: {}",
                    root.display(),
                    path.display()
                )),
                Ok(_) => warnings.push(format!(
                    "ignored memory rule symlink that is not a file: {}",
                    path.display()
                )),
                Err(error) => warnings.push(format!(
                    "ignored memory rule symlink that cannot be resolved: {}: {error}",
                    path.display()
                )),
            }
        } else if file_type.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md")
        {
            files.push(path);
        }
    }
}

fn working_root(cwd: &Path) -> PathBuf {
    std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf())
}

/// Directories between the project identity root and `cwd`, outermost first.
/// `cwd` itself is excluded because the project rows already cover it. A path
/// outside a repository has no bounded parent chain, so it returns nothing
/// instead of walking to the filesystem root.
fn ancestor_instruction_dirs(cwd: &Path) -> Vec<PathBuf> {
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let Some(anchor) = crate::worktree::project_identity_root(&cwd) else {
        return Vec::new();
    };
    let anchor = std::fs::canonicalize(&anchor).unwrap_or(anchor);
    if cwd == anchor || !cwd.starts_with(&anchor) {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    let mut current = cwd.parent();
    while let Some(dir) = current {
        if !dir.starts_with(&anchor) {
            break;
        }
        dirs.push(dir.to_path_buf());
        if dir == anchor {
            break;
        }
        current = dir.parent();
    }
    dirs.reverse();
    dirs
}

fn display_path(path: &Path, home: &Path) -> String {
    let rendered = path.to_string_lossy();
    if let Ok(relative) = path.strip_prefix(home) {
        if relative.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", relative.display())
        }
    } else {
        rendered.into_owned()
    }
}

pub fn ensure_memory_file(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => return Ok(()),
        Ok(metadata) if metadata.file_type().is_symlink() && path.is_file() => return Ok(()),
        Ok(_) => anyhow::bail!("{} is not a regular file", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", path.display()))
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let header = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("# {name}\n\n"))
        .unwrap_or_default();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(header.as_bytes())?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_file() => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot create {}", path.display())),
    }
}

pub fn ensure_memory_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))?;
    if !path.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }
    Ok(())
}

pub fn toggle_auto_memory(env: &MemoryEnvironment, enabled: bool) -> Result<PathBuf> {
    if env.disable_auto_memory.is_some() {
        anyhow::bail!("CLAUDE_CODE_DISABLE_AUTO_MEMORY controls this session");
    }
    if enabled && env.remote && env.remote_memory_dir.is_none() {
        anyhow::bail!("start a local session to enable auto memory");
    }
    let path = env.config_dir.join("settings.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if !metadata.file_type().is_file() {
            anyhow::bail!("{} is not a regular file", path.display());
        }
    }
    let mut object = if path.is_file() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        match serde_json::from_str::<Value>(&text)
            .with_context(|| format!("cannot parse {}", path.display()))?
        {
            Value::Object(object) => object,
            _ => anyhow::bail!("{} is not a JSON object", path.display()),
        }
    } else {
        Map::new()
    };
    object.insert("autoMemoryEnabled".to_string(), Value::Bool(enabled));
    let temp = path.with_file_name(format!(
        ".settings.json.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .with_context(|| format!("cannot write {}", temp.display()))?;
        file.write_all(
            format!(
                "{}\n",
                serde_json::to_string_pretty(&Value::Object(object))?
            )
            .as_bytes(),
        )?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(root: &Path) -> MemoryEnvironment {
        MemoryEnvironment::for_test(root.join("claude"), root.join("codex"), root.to_path_buf())
    }

    #[test]
    fn discovery_lists_user_project_codex_and_auto_memory_files() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut env = environment(temp.path());
        env.project_dir_name = Some("test-project".to_string());
        std::fs::create_dir_all(env.config_dir.join("rules")).unwrap();
        std::fs::write(env.config_dir.join("CLAUDE.md"), "user").unwrap();
        std::fs::write(env.config_dir.join("rules").join("a.md"), "rule").unwrap();
        std::fs::write(project.join("CLAUDE.md"), "project").unwrap();
        std::fs::write(project.join("AGENTS.md"), "agents").unwrap();
        std::fs::write(project.join("AGENTS.override.md"), "override").unwrap();
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::write(project.join(".claude").join("CLAUDE.md"), "alternate").unwrap();
        std::fs::write(project.join(".claude").join("AGENTS.md"), "claude agents").unwrap();
        std::fs::create_dir_all(project.join(".codex")).unwrap();
        std::fs::write(project.join(".codex").join("AGENTS.md"), "not a codex path").unwrap();
        std::fs::create_dir_all(env.codex_home.clone()).unwrap();
        std::fs::write(env.codex_home.join("AGENTS.md"), "codex").unwrap();
        std::fs::write(env.codex_home.join("AGENTS.override.md"), "codex override").unwrap();
        let memory = env
            .config_dir
            .join("projects")
            .join("test-project")
            .join("memory");
        std::fs::create_dir_all(&memory).unwrap();
        std::fs::write(memory.join("MEMORY.md"), "index").unwrap();
        std::fs::write(memory.join("topic.md"), "topic").unwrap();

        let listing = discover(&project, &env);
        let project = std::fs::canonicalize(&project).unwrap();
        let labels = listing
            .rows()
            .into_iter()
            .map(|(_, row)| row.label.clone())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"User instructions".to_string()));
        assert!(labels.contains(&"Project instructions".to_string()));
        assert!(labels.contains(&"Auto-memory".to_string()));
        assert!(labels.contains(&"Auto-memory index".to_string()));
        assert!(labels.iter().any(|label| label.ends_with("topic.md")));
        assert!(labels.contains(&"Open auto-memory folder".to_string()));
        let paths = listing
            .rows()
            .into_iter()
            .filter_map(|(_, row)| match &row.action {
                MemoryAction::OpenFile { path } => Some(path.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(paths.contains(&project.join(".claude").join("AGENTS.md")));
        assert!(paths.contains(&project.join(".claude").join("CLAUDE.md")));
        assert!(paths.contains(&project.join("AGENTS.override.md")));
        assert!(paths.contains(&env.codex_home.join("AGENTS.override.md")));
        assert!(!paths.contains(&project.join(".codex").join("AGENTS.md")));
        let rows = listing.rows();
        let project_agents = rows
            .iter()
            .find(|(_, row)| {
                row.action
                    == MemoryAction::OpenFile {
                        path: project.join("AGENTS.md"),
                    }
            })
            .expect("project AGENTS.md row");
        assert!(project_agents.1.detail.contains("override wins"));
        let user_agents = rows
            .iter()
            .find(|(_, row)| {
                row.action
                    == MemoryAction::OpenFile {
                        path: env.codex_home.join("AGENTS.md"),
                    }
            })
            .expect("user AGENTS.md row");
        assert!(user_agents.1.detail.contains("AGENTS.override.md wins"));
        assert!(listing
            .auto_memory
            .directory
            .ends_with("projects/test-project/memory"));
    }

    #[cfg(unix)]
    #[test]
    fn discovery_lists_existing_ancestor_claude_files_in_order() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(status.success(), "git init failed");
        std::fs::write(repo.join("CLAUDE.md"), "root").unwrap();
        std::fs::write(repo.join("src/CLAUDE.md"), "src").unwrap();
        std::fs::write(repo.join("src/deep/CLAUDE.md"), "deep").unwrap();

        let env = environment(temp.path());
        let listing = discover(&repo.join("src/deep"), &env);
        let parents = listing
            .rows()
            .into_iter()
            .filter(|(_, row)| row.label == "Parent instructions")
            .map(|(_, row)| row.action.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            parents,
            vec![
                MemoryAction::OpenFile {
                    path: std::fs::canonicalize(repo.join("CLAUDE.md")).unwrap(),
                },
                MemoryAction::OpenFile {
                    path: std::fs::canonicalize(repo.join("src/CLAUDE.md")).unwrap(),
                },
            ],
            "{parents:?}"
        );
        let deep = std::fs::canonicalize(repo.join("src/deep/CLAUDE.md")).unwrap();
        assert!(
            listing.rows().into_iter().any(|(_, row)| {
                row.label == "Project instructions"
                    && row.action == MemoryAction::OpenFile { path: deep.clone() }
            }),
            "{:?}",
            listing.rows()
        );
    }

    #[test]
    fn discovery_lists_existing_ancestor_codex_files_in_order() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(status.success(), "git init failed");
        std::fs::write(repo.join("AGENTS.override.md"), "root override").unwrap();
        std::fs::write(repo.join("AGENTS.md"), "root agents").unwrap();
        std::fs::write(repo.join("src/AGENTS.md"), "src agents").unwrap();
        std::fs::write(repo.join("src/deep/AGENTS.md"), "deep agents").unwrap();

        let env = environment(temp.path());
        let listing = discover(&repo.join("src/deep"), &env);
        let parents = listing
            .rows()
            .into_iter()
            .filter(|(section, row)| {
                section.title == "Codex"
                    && (row.label == "Parent override" || row.label == "Parent instructions")
            })
            .map(|(_, row)| (row.label.clone(), row.action.clone(), row.detail.clone()))
            .collect::<Vec<_>>();
        assert_eq!(parents.len(), 3, "{parents:?}");
        let root_override = std::fs::canonicalize(repo.join("AGENTS.override.md")).unwrap();
        let root_agents = std::fs::canonicalize(repo.join("AGENTS.md")).unwrap();
        let src_agents = std::fs::canonicalize(repo.join("src/AGENTS.md")).unwrap();
        assert_eq!(parents[0].0, "Parent override");
        assert_eq!(
            parents[0].1,
            MemoryAction::OpenFile {
                path: root_override
            }
        );
        assert_eq!(parents[1].0, "Parent instructions");
        assert_eq!(parents[1].1, MemoryAction::OpenFile { path: root_agents });
        assert!(parents[1].2.contains("fallback"), "{:?}", parents[1]);
        assert_eq!(parents[2].1, MemoryAction::OpenFile { path: src_agents });
        assert!(!parents.iter().any(|(_, action, _)| {
            action
                == &MemoryAction::OpenFile {
                    path: std::fs::canonicalize(repo.join("src/deep/AGENTS.md")).unwrap(),
                }
        }));
    }

    #[test]
    fn discovery_skips_rule_symlinks_outside_the_rule_root() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let rules = project.join(".claude").join("rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("inside.md"), "inside").unwrap();
        std::fs::write(temp.path().join("outside.md"), "outside").unwrap();
        symlink(rules.join("inside.md"), rules.join("linked.md")).unwrap();
        symlink(temp.path().join("outside.md"), rules.join("outside.md")).unwrap();

        let env = environment(temp.path());
        let listing = discover(&project, &env);
        let labels = listing
            .rows()
            .into_iter()
            .map(|(_, row)| row.label.clone())
            .collect::<Vec<_>>();
        assert!(labels.iter().any(|label| label.ends_with("inside.md")));
        assert!(labels.iter().any(|label| label.ends_with("linked.md")));
        assert!(!labels.iter().any(|label| label.ends_with("outside.md")));
        assert!(listing
            .warnings
            .iter()
            .any(|warning| warning.contains("symlink outside")));
    }

    #[cfg(unix)]
    #[test]
    fn discovery_does_not_follow_a_symlinked_rule_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let external_rules = temp.path().join("external-rules");
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::create_dir_all(&external_rules).unwrap();
        std::fs::write(external_rules.join("outside.md"), "outside").unwrap();
        symlink(&external_rules, project.join(".claude").join("rules")).unwrap();

        let env = environment(temp.path());
        let listing = discover(&project, &env);
        let labels = listing
            .rows()
            .into_iter()
            .map(|(_, row)| row.label.clone())
            .collect::<Vec<_>>();
        assert!(!labels.iter().any(|label| label.ends_with("outside.md")));
        assert!(listing
            .warnings
            .iter()
            .any(|warning| warning.contains("rule directory is a symlink")));
    }

    #[test]
    fn auto_memory_directory_honors_absolute_and_home_settings() {
        let temp = tempfile::tempdir().unwrap();
        let env = environment(temp.path());
        std::fs::create_dir_all(&env.config_dir).unwrap();
        std::fs::write(
            env.config_dir.join("settings.json"),
            r#"{"autoMemoryEnabled":false,"autoMemoryDirectory":"~/custom-memory"}"#,
        )
        .unwrap();
        let listing = discover(temp.path(), &env);
        assert!(!listing.auto_memory.enabled);
        assert_eq!(
            listing.auto_memory.directory,
            temp.path().join("custom-memory")
        );
        assert!(listing
            .auto_memory
            .directory_source
            .contains("user settings"));

        std::fs::write(
            env.config_dir.join("settings.json"),
            r#"{"autoMemoryDirectory":"relative/path"}"#,
        )
        .unwrap();
        let listing = discover(temp.path(), &env);
        assert!(listing
            .warnings
            .iter()
            .any(|warning| warning.contains("must be absolute")));

        std::fs::write(
            env.config_dir.join("settings.json"),
            r#"{"autoMemoryDirectory":"~"}"#,
        )
        .unwrap();
        let listing = discover(temp.path(), &env);
        assert!(listing
            .warnings
            .iter()
            .any(|warning| warning.contains("must be absolute")));

        std::fs::write(
            env.config_dir.join("settings.json"),
            r#"{"autoMemoryEnabled":"yes","autoMemoryDirectory":42}"#,
        )
        .unwrap();
        let listing = discover(temp.path(), &env);
        assert!(listing
            .warnings
            .iter()
            .any(|warning| warning.contains("autoMemoryEnabled")
                && warning.contains("true or false")));
        assert!(listing
            .warnings
            .iter()
            .any(|warning| warning.contains("autoMemoryDirectory") && warning.contains("string")));
    }

    #[test]
    fn disable_auto_memory_environment_values_are_tri_state() {
        for value in ["1", "true", "YES", "on"] {
            assert_eq!(parse_disable_auto_memory(value), Some(true));
        }
        for value in ["0", "false", "NO", "off"] {
            assert_eq!(parse_disable_auto_memory(value), Some(false));
        }
        assert_eq!(parse_disable_auto_memory("maybe"), None);
    }

    #[test]
    fn disable_auto_memory_environment_controls_the_effective_state() {
        let temp = tempfile::tempdir().unwrap();
        let mut env = environment(temp.path());
        env.disable_auto_memory = Some(true);
        let listing = discover(temp.path(), &env);
        assert!(!listing.auto_memory.enabled);
        assert_eq!(
            listing.auto_memory.enabled_source,
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY"
        );
        assert!(!listing.auto_memory.can_toggle);

        env.disable_auto_memory = Some(false);
        let listing = discover(temp.path(), &env);
        assert!(listing.auto_memory.enabled);
        assert_eq!(
            listing.auto_memory.enabled_source,
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY"
        );
        assert!(!listing.auto_memory.can_toggle);
    }

    #[test]
    fn remote_memory_base_must_be_absolute_and_reports_its_source() {
        let temp = tempfile::tempdir().unwrap();
        let mut env = environment(temp.path());
        env.remote_memory_dir = Some(PathBuf::from("relative"));
        let listing = discover(temp.path(), &env);
        assert!(listing.auto_memory.directory.starts_with(&env.config_dir));
        assert_eq!(
            listing.auto_memory.directory_source,
            "default project memory"
        );
        assert!(listing
            .warnings
            .iter()
            .any(|warning| warning.contains("must be an absolute path")));

        let remote = temp.path().join("remote-memory");
        env.remote_memory_dir = Some(remote.clone());
        let listing = discover(temp.path(), &env);
        assert!(listing.auto_memory.directory.starts_with(&remote));
        assert_eq!(listing.auto_memory.directory_source, "remote memory base");
    }

    #[test]
    fn cowork_override_wins_over_user_auto_memory_directory() {
        let temp = tempfile::tempdir().unwrap();
        let mut env = environment(temp.path());
        std::fs::create_dir_all(&env.config_dir).unwrap();
        std::fs::write(
            env.config_dir.join("settings.json"),
            r#"{"autoMemoryDirectory":"~/user-memory"}"#,
        )
        .unwrap();
        let cowork = temp.path().join("cowork-memory");
        env.cowork_memory_path = Some(cowork.clone());

        let listing = discover(temp.path(), &env);
        assert_eq!(listing.auto_memory.directory, cowork);
        assert_eq!(
            listing.auto_memory.directory_source,
            "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE"
        );
    }

    #[test]
    fn project_auto_memory_directory_is_ignored_without_workspace_trust() {
        let temp = tempfile::tempdir().unwrap();
        let env = environment(temp.path());
        std::fs::create_dir_all(temp.path().join(".claude")).unwrap();
        std::fs::write(
            temp.path().join(".claude").join("settings.json"),
            r#"{"autoMemoryDirectory":"/tmp/repository-chosen-memory"}"#,
        )
        .unwrap();
        let listing = discover(temp.path(), &env);
        assert_ne!(
            listing.auto_memory.directory,
            PathBuf::from("/tmp/repository-chosen-memory")
        );
        assert!(listing
            .warnings
            .iter()
            .any(|warning| warning.contains("does not model workspace trust")));
    }

    #[test]
    fn home_repository_reads_local_settings_from_the_working_directory() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("home");
        std::fs::create_dir_all(&repo).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(
            repo.join(".claude").join("settings.local.json"),
            r#"{"autoMemoryEnabled":false}"#,
        )
        .unwrap();
        let subdirectory = repo.join("packages").join("api");
        std::fs::create_dir_all(subdirectory.join(".claude")).unwrap();
        std::fs::write(
            subdirectory.join(".claude").join("settings.local.json"),
            r#"{"autoMemoryEnabled":true}"#,
        )
        .unwrap();
        let mut env = environment(temp.path());
        env.home_dir = repo.clone();

        let listing = discover(&subdirectory, &env);
        assert!(listing.auto_memory.enabled);
        assert_eq!(listing.auto_memory.enabled_source, "local settings");
    }

    #[test]
    fn project_settings_override_user_auto_memory() {
        let temp = tempfile::tempdir().unwrap();
        let env = environment(temp.path());
        std::fs::create_dir_all(&env.config_dir).unwrap();
        std::fs::write(
            env.config_dir.join("settings.json"),
            r#"{"autoMemoryEnabled":true}"#,
        )
        .unwrap();
        std::fs::create_dir_all(temp.path().join(".claude")).unwrap();
        std::fs::write(
            temp.path().join(".claude").join("settings.json"),
            r#"{"autoMemoryEnabled":false}"#,
        )
        .unwrap();
        let listing = discover(temp.path(), &env);
        assert!(!listing.auto_memory.enabled);
        assert_eq!(listing.auto_memory.enabled_source, "project settings");
        assert_eq!(listing.auto_memory.user_enabled, Some(true));
    }

    #[test]
    fn local_settings_override_project_auto_memory() {
        let temp = tempfile::tempdir().unwrap();
        let env = environment(temp.path());
        std::fs::create_dir_all(&env.config_dir).unwrap();
        std::fs::write(
            env.config_dir.join("settings.json"),
            r#"{"autoMemoryEnabled":true}"#,
        )
        .unwrap();
        std::fs::create_dir_all(temp.path().join(".claude")).unwrap();
        std::fs::write(
            temp.path().join(".claude").join("settings.json"),
            r#"{"autoMemoryEnabled":true}"#,
        )
        .unwrap();
        std::fs::write(
            temp.path().join(".claude").join("settings.local.json"),
            r#"{"autoMemoryEnabled":false}"#,
        )
        .unwrap();
        let listing = discover(temp.path(), &env);
        assert!(!listing.auto_memory.enabled);
        assert_eq!(listing.auto_memory.enabled_source, "local settings");
    }

    #[test]
    fn subdirectory_reads_local_settings_from_the_repository_root() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::create_dir_all(repo.path().join(".claude")).unwrap();
        std::fs::write(
            repo.path().join(".claude").join("settings.local.json"),
            r#"{"autoMemoryEnabled":false}"#,
        )
        .unwrap();
        let subdir = repo.path().join("packages").join("api");
        std::fs::create_dir_all(&subdir).unwrap();
        let mut env = environment(repo.path());
        env.home_dir = repo.path().join("home");
        let listing = discover(&subdir, &env);
        assert!(!listing.auto_memory.enabled);
        assert_eq!(listing.auto_memory.enabled_source, "local settings");
    }

    #[test]
    fn project_directory_encoding_matches_claude_code() {
        assert_eq!(
            encode_project_path(Path::new("/Users/me/my_project")),
            "-Users-me-my-project"
        );
        assert_eq!(
            encode_project_path(Path::new("/Users/me/café")),
            "-Users-me-caf-"
        );
        let long = Path::new(
            "/private/var/folders/ff/t7w_y9t56p5dl9pyhkq2t5p40000gn/T/tmp.0NdBol9QnZ/abcdefghijklmnopqrstuvwxyz0123456789/abcdefghijklmnopqrstuvwxyz0123456789/abcdefghijklmnopqrstuvwxyz0123456789/abcdefghijklmnopqrstuvwxyz0123456789/abcdefghijklmnopqrstuvwxyz0123456789",
        );
        assert_eq!(
            encode_project_path(long),
            "-private-var-folders-ff-t7w-y9t56p5dl9pyhkq2t5p40000gn-T-tmp-0NdBol9QnZ-abcdefghijklmnopqrstuvwxyz0123456789-abcdefghijklmnopqrstuvwxyz0123456789-abcdefghijklmnopqrstuvwxyz0123456789-abcdefghijklmnopq-4xxi74"
        );
    }

    #[test]
    fn project_directory_name_requires_the_documented_pair_and_alphabet() {
        let temp = tempfile::tempdir().unwrap();
        let mut env = environment(temp.path());
        env.project_dir_name = Some("work_1".to_string());
        let listing = discover(temp.path(), &env);
        assert!(listing
            .auto_memory
            .directory
            .ends_with("projects/work_1/memory"));

        for name in ["con", "com0", "lpt0", "bad name", &"x".repeat(65)] {
            let mut env = environment(temp.path());
            env.project_dir_name = Some(name.to_string());
            let listing = discover(temp.path(), &env);
            assert!(!listing
                .auto_memory
                .directory
                .ends_with(format!("projects/{name}/memory")));
            assert!(listing
                .warnings
                .iter()
                .any(|warning| warning.contains("CLAUDE_CODE_PROJECT_DIR_NAME")));
        }

        let mut env = environment(temp.path());
        env.config_dir_is_explicit = false;
        env.project_dir_name = Some("work".to_string());
        let listing = discover(temp.path(), &env);
        assert!(listing
            .warnings
            .iter()
            .any(|warning| warning.contains("without CLAUDE_CONFIG_DIR")));
    }

    #[test]
    fn toggle_auto_memory_preserves_unknown_settings_and_private_mode() {
        let temp = tempfile::tempdir().unwrap();
        let env = environment(temp.path());
        std::fs::create_dir_all(&env.config_dir).unwrap();
        std::fs::write(
            env.config_dir.join("settings.json"),
            r#"{"theme":"dark","nested":{"x":1}}"#,
        )
        .unwrap();
        let path = toggle_auto_memory(&env, false).unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["nested"]["x"], 1);
        assert_eq!(value["autoMemoryEnabled"], false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(env.config_dir.join("settings.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn malformed_settings_fail_closed_on_toggle() {
        let temp = tempfile::tempdir().unwrap();
        let env = environment(temp.path());
        std::fs::create_dir_all(&env.config_dir).unwrap();
        std::fs::write(env.config_dir.join("settings.json"), "not json").unwrap();
        assert!(toggle_auto_memory(&env, false).is_err());

        std::fs::write(env.config_dir.join("settings.json"), "[]").unwrap();
        assert!(toggle_auto_memory(&env, false).is_err());

        std::fs::remove_file(env.config_dir.join("settings.json")).unwrap();
        std::fs::create_dir_all(env.config_dir.join("settings.json")).unwrap();
        assert!(toggle_auto_memory(&env, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn toggle_refuses_a_symlinked_settings_file() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let env = environment(temp.path());
        std::fs::create_dir_all(&env.config_dir).unwrap();
        let real = temp.path().join("real-settings.json");
        std::fs::write(&real, r#"{"autoMemoryEnabled":false,"keep":1}"#).unwrap();
        symlink(&real, env.config_dir.join("settings.json")).unwrap();

        let error = toggle_auto_memory(&env, true).unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            r#"{"autoMemoryEnabled":false,"keep":1}"#
        );
    }

    #[test]
    fn malformed_settings_in_any_scope_make_auto_memory_read_only() {
        fn settings_path(root: &Path, env: &MemoryEnvironment, scope: &str) -> PathBuf {
            match scope {
                "user" => env.config_dir.join("settings.json"),
                "project" => root.join(".claude").join("settings.json"),
                _ => root.join(".claude").join("settings.local.json"),
            }
        }

        for scope in ["user", "project", "local"] {
            for invalid in ["not json", "[]"] {
                let temp = tempfile::tempdir().unwrap();
                let env = environment(temp.path());
                let path = settings_path(temp.path(), &env, scope);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, invalid).unwrap();
                let listing = discover(temp.path(), &env);
                assert!(!listing.auto_memory.can_toggle, "{scope} {invalid}");
                assert!(
                    listing.warnings.iter().any(|warning| {
                        warning.contains("parse") || warning.contains("JSON object")
                    }),
                    "{scope} {invalid}: {:?}",
                    listing.warnings
                );
                assert!(listing
                    .auto_memory
                    .read_only_reason
                    .as_deref()
                    .unwrap_or("")
                    .contains("malformed"));
            }

            let temp = tempfile::tempdir().unwrap();
            let env = environment(temp.path());
            let path = settings_path(temp.path(), &env, scope);
            std::fs::create_dir_all(&path).unwrap();
            let listing = discover(temp.path(), &env);
            assert!(!listing.auto_memory.can_toggle, "{scope} directory");
            assert!(listing
                .warnings
                .iter()
                .any(|warning| warning.contains("is not a regular file")));
        }
    }

    #[test]
    fn missing_files_create_parent_directories_and_refuse_directories() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("rules").join("one.md");
        ensure_memory_file(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# one.md\n\n");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir_all(&path).unwrap();
        assert!(ensure_memory_file(&path).is_err());
    }

    #[test]
    fn worktree_project_files_use_the_active_tree_while_auto_memory_is_shared() {
        let main = tempfile::tempdir().unwrap();
        let worktree_parent = tempfile::tempdir().unwrap();
        let worktree = worktree_parent.path().join("worker");
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(main.path())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(main.path().join("CLAUDE.md"), "main instructions").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        git(&[
            "worktree",
            "add",
            worktree.to_str().unwrap(),
            "-b",
            "worker",
        ]);
        std::fs::write(worktree.join("CLAUDE.md"), "worktree instructions").unwrap();

        let env = environment(main.path());
        let main_listing = discover(main.path(), &env);
        let worktree_listing = discover(&worktree, &env);
        assert_eq!(
            main_listing.auto_memory.directory, worktree_listing.auto_memory.directory,
            "auto-memory is shared by the git repository"
        );
        let project_path = worktree_listing
            .iter()
            .find(|(_, row)| row.label == "Project instructions")
            .and_then(|(_, row)| match &row.action {
                MemoryAction::OpenFile { path } => Some(path.clone()),
                _ => None,
            })
            .expect("project instructions");
        assert_eq!(
            project_path,
            std::fs::canonicalize(&worktree).unwrap().join("CLAUDE.md")
        );
    }
}
