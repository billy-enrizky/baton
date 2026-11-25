use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::types::ProviderId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BatonMcpRegistry {
    #[serde(default, flatten)]
    pub servers: BTreeMap<String, McpServerConfig>,
}

pub fn mcp_registry_path(home: &Path) -> PathBuf {
    home.join("mcp.json")
}

pub fn load_mcp_registry(home: &Path) -> BatonMcpRegistry {
    let path = mcp_registry_path(home);
    if !path.is_file() {
        return BatonMcpRegistry::default();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn save_mcp_registry(home: &Path, registry: &BatonMcpRegistry) -> Result<()> {
    std::fs::create_dir_all(home)?;
    std::fs::write(
        mcp_registry_path(home),
        format!("{}\n", serde_json::to_string_pretty(registry)?),
    )?;
    Ok(())
}

fn claude_global_state_path() -> PathBuf {
    crate::paths::claude_global_settings_dir()
        .parent()
        .map(|parent| parent.join(".claude.json"))
        .unwrap_or_else(|| PathBuf::from(".claude.json"))
}

pub fn sync_mcp_to_claude(registry: &BatonMcpRegistry) -> Result<Vec<String>> {
    let path = claude_global_state_path();
    let mut state: Value = if path.is_file() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    let mcp_json = serde_json::to_value(&registry.servers)?;
    if let Value::Object(map) = &mut state {
        map.insert("mcpServers".to_string(), mcp_json);
    }
    std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&state)?))?;
    Ok(registry.servers.keys().cloned().collect())
}

pub fn sync_mcp_to_codex(registry: &BatonMcpRegistry) -> Result<Vec<String>> {
    let path = crate::paths::codex_global_home().join("config.toml");
    let contents = if path.is_file() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let mut document = contents
        .parse::<toml_edit::DocumentMut>()
        .unwrap_or_default();
    let server_names: Vec<String> = registry.servers.keys().cloned().collect();
    for (name, server) in &registry.servers {
        let section = document
            .entry(format!("mcp_servers.{name}").as_str())
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        if let toml_edit::Item::Table(table) = section {
            table["command"] = toml_edit::value(server.command.clone());
            let args: toml_edit::Array = server.args.iter().map(|arg| arg.as_str()).collect();
            table["args"] = toml_edit::value(args);
            if !server.env.is_empty() {
                let env_table = toml_edit::Table::new();
                table["env"] = toml_edit::Item::Table(env_table);
                if let Some(toml_edit::Item::Table(env)) = table.get_mut("env") {
                    for (key, value) in &server.env {
                        env[key] = toml_edit::value(value.clone());
                    }
                }
            }
        }
    }
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(&path, document.to_string())?;
    Ok(server_names)
}

pub fn opencode_config_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir).join("opencode").join("opencode.json");
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("opencode")
        .join("opencode.json")
}

pub fn sync_mcp_to_opencode(registry: &BatonMcpRegistry) -> Result<Vec<String>> {
    let path = opencode_config_path();
    let mut config: Value = if path.is_file() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    let mcp_json: BTreeMap<String, Value> = registry
        .servers
        .iter()
        .map(|(name, server)| {
            (
                name.clone(),
                serde_json::json!({
                    "type": "local",
                    "command": server.command,
                    "args": server.args,
                    "env": server.env,
                }),
            )
        })
        .collect();
    if let Value::Object(map) = &mut config {
        map.insert("mcp".to_string(), serde_json::to_value(mcp_json)?);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&config)?))?;
    Ok(registry.servers.keys().cloned().collect())
}

pub fn sync_mcp_to_all(registry: &BatonMcpRegistry) -> Result<Vec<(String, Vec<String>)>> {
    let mut results = Vec::new();
    match sync_mcp_to_claude(registry) {
        Ok(names) => results.push(("claude".to_string(), names)),
        Err(error) => results.push(("claude".to_string(), vec![format!("error: {error}")])),
    }
    match sync_mcp_to_codex(registry) {
        Ok(names) => results.push(("codex".to_string(), names)),
        Err(error) => results.push(("codex".to_string(), vec![format!("error: {error}")])),
    }
    match sync_mcp_to_opencode(registry) {
        Ok(names) => results.push(("opencode".to_string(), names)),
        Err(error) => results.push(("opencode".to_string(), vec![format!("error: {error}")])),
    }
    Ok(results)
}

pub struct DiscoveredSkill {
    pub name: String,
    pub path: PathBuf,
    pub source: String,
    pub description: Option<String>,
}

fn is_skill_dir(path: &Path) -> bool {
    path.join("SKILL.md").is_file() || path.join("skill.md").is_file()
}

/// Reads the frontmatter description from a skill file. Only the first few
/// kilobytes are read, and only a single-line scalar is recognized.
fn skill_description(path: &Path) -> Option<String> {
    let file = if path.join("SKILL.md").is_file() {
        path.join("SKILL.md")
    } else {
        path.join("skill.md")
    };
    let bytes = std::fs::read(&file).ok()?;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines.take(60) {
        let line = line.trim();
        if line == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix("description:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'').trim();
            if value.is_empty() {
                return None;
            }
            return Some(value.chars().take(160).collect());
        }
    }
    None
}

fn collect_skill_dirs(path: &Path, source: &str, depth: usize, skills: &mut Vec<DiscoveredSkill>) {
    if !path.is_dir() {
        return;
    }
    if is_skill_dir(path) {
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            skills.push(DiscoveredSkill {
                name: name.to_string(),
                path: path.to_path_buf(),
                source: source.to_string(),
                description: skill_description(path),
            });
        }
        return;
    }
    if depth > 0 {
        return;
    }
    if let Ok(children) = std::fs::read_dir(path) {
        for child in children.flatten() {
            collect_skill_dirs(&child.path(), source, depth + 1, skills);
        }
    }
}

fn scan_skill_root(root: &Path, source: &str) -> Vec<DiscoveredSkill> {
    let mut skills = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            collect_skill_dirs(&entry.path(), source, 0, &mut skills);
        }
    }
    skills.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.path.cmp(&right.path))
    });
    skills.dedup_by(|left, right| left.name == right.name);
    skills
}

pub fn scan_skill_roots(claude_root: &Path, codex_root: &Path) -> Vec<DiscoveredSkill> {
    let mut skills = scan_skill_root(claude_root, "claude");
    for skill in scan_skill_root(codex_root, "codex") {
        if !skills.iter().any(|existing| existing.name == skill.name) {
            skills.push(skill);
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

pub fn scan_skills() -> Vec<DiscoveredSkill> {
    scan_skill_roots(
        &crate::paths::claude_global_settings_dir().join("skills"),
        &crate::paths::codex_global_home().join("skills"),
    )
}

/// Project skills shadow global skills with the same name. Both harness
/// spellings are read from the working directory; the source keeps the
/// harness that owns the skill so the invocation and switch hint stay right.
pub fn scan_skills_in(cwd: &Path) -> Vec<DiscoveredSkill> {
    scan_skills_with(
        cwd,
        &crate::paths::claude_global_settings_dir().join("skills"),
        &crate::paths::codex_global_home().join("skills"),
    )
}

fn scan_skills_with(cwd: &Path, claude_root: &Path, codex_root: &Path) -> Vec<DiscoveredSkill> {
    let mut skills = scan_skill_root(&cwd.join(".claude").join("skills"), "claude");
    for skill in scan_skill_root(&cwd.join(".codex").join("skills"), "codex") {
        if !skills.iter().any(|existing| existing.name == skill.name) {
            skills.push(skill);
        }
    }
    for skill in scan_skill_roots(claude_root, codex_root) {
        if !skills.iter().any(|existing| existing.name == skill.name) {
            skills.push(skill);
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

pub fn settings_for_provider(provider: ProviderId) -> Vec<(&'static str, Vec<&'static str>)> {
    match provider {
        ProviderId::Claude => vec![
            ("permission mode", vec!["default", "acceptEdits", "bypassPermissions", "plan"]),
            ("api key", vec!["ANTHROPIC_API_KEY"]),
        ],
        ProviderId::Codex => vec![
            ("approval policy", vec!["untrusted", "on-request", "on-failure", "never"]),
            ("sandbox mode", vec!["read-only", "workspace-write", "danger-full-access"]),
            ("api key", vec!["OPENAI_API_KEY"]),
        ],
        ProviderId::Cursor => vec![
            ("mode", vec!["agent", "plan"]),
            ("api key", vec!["CURSOR_API_KEY"]),
        ],
        ProviderId::Grok => vec![
            ("permission mode", vec!["default", "auto", "always-approve"]),
            ("api key", vec!["XAI_API_KEY"]),
        ],
        ProviderId::OpenCode => vec![
            ("model slug", vec!["provider/model"]),
            ("api key", vec!["OPENCODE_API_KEY"]),
        ],
        ProviderId::Antigravity => vec![
            ("mode", vec!["default", "auto_edit", "yolo"]),
            ("api key", vec!["GEMINI_API_KEY"]),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(root: &Path, name: &str, file: &str) {
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join(file), "---\nname: test\n---\n").unwrap();
    }

    #[test]
    fn skill_descriptions_come_from_frontmatter() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        let path = claude.path().join("described");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("SKILL.md"),
            "---\nname: described\ndescription: \"Summarize a change quickly\"\n---\nbody\n",
        )
        .unwrap();
        let path = claude.path().join("plain");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("SKILL.md"), "---\nname: plain\n---\n").unwrap();
        let path = claude.path().join("no-frontmatter");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("SKILL.md"), "body only\n").unwrap();

        let skills = scan_skill_roots(claude.path(), codex.path());
        let described = skills
            .iter()
            .find(|skill| skill.name == "described")
            .unwrap();
        assert_eq!(
            described.description.as_deref(),
            Some("Summarize a change quickly")
        );
        assert!(skills
            .iter()
            .find(|skill| skill.name == "plain")
            .unwrap()
            .description
            .is_none());
        assert!(skills
            .iter()
            .find(|skill| skill.name == "no-frontmatter")
            .unwrap()
            .description
            .is_none());
    }

    #[test]
    fn skill_description_is_capped_and_reads_the_lowercase_file() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        let path = claude.path().join("long");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("skill.md"),
            format!("---\ndescription: {}\n---\n", "x".repeat(200)),
        )
        .unwrap();
        let skills = scan_skill_roots(claude.path(), codex.path());
        let description = skills[0].description.as_ref().unwrap();
        assert_eq!(description.chars().count(), 160);
    }

    #[test]
    fn skill_roots_scan_flat_and_container_directories() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        skill(claude.path(), "flat", "SKILL.md");
        skill(claude.path(), "lower", "skill.md");
        skill(&claude.path().join(".system"), "nested", "SKILL.md");
        std::fs::create_dir_all(claude.path().join("empty")).unwrap();
        std::fs::create_dir_all(claude.path().join("container").join("deep")).unwrap();
        std::fs::write(
            claude
                .path()
                .join("container")
                .join("deep")
                .join("SKILL.md"),
            "---\nname: deep\n---\n",
        )
        .unwrap();

        let skills = scan_skill_roots(claude.path(), codex.path());
        let names = skills
            .iter()
            .map(|skill| (skill.name.as_str(), skill.source.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                ("deep", "claude"),
                ("flat", "claude"),
                ("lower", "claude"),
                ("nested", "claude"),
            ]
        );
    }

    #[test]
    fn project_skills_shadow_global_skills_and_keep_their_harness() {
        let cwd = tempfile::tempdir().unwrap();
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        skill(
            &cwd.path().join(".claude").join("skills"),
            "shared",
            "SKILL.md",
        );
        skill(
            &cwd.path().join(".codex").join("skills"),
            "codex-project",
            "SKILL.md",
        );
        skill(claude.path(), "shared", "SKILL.md");
        skill(claude.path(), "global-only", "SKILL.md");

        let skills = scan_skills_with(cwd.path(), claude.path(), codex.path());
        let names = skills
            .iter()
            .map(|skill| (skill.name.as_str(), skill.source.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                ("codex-project", "codex"),
                ("global-only", "claude"),
                ("shared", "claude"),
            ]
        );
        let shared = skills.iter().find(|skill| skill.name == "shared").unwrap();
        assert!(
            shared.path.starts_with(cwd.path()),
            "the project skill must win: {:?}",
            shared.path
        );
    }

    #[test]
    fn a_project_codex_skill_is_listed_when_no_claude_skill_exists() {
        let cwd = tempfile::tempdir().unwrap();
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        skill(
            &cwd.path().join(".codex").join("skills"),
            "project",
            "SKILL.md",
        );

        let skills = scan_skills_with(cwd.path(), claude.path(), codex.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "project");
        assert_eq!(skills[0].source, "codex");
    }

    #[test]
    fn claude_skills_win_duplicate_names_and_codex_adds_the_rest() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        skill(claude.path(), "shared", "SKILL.md");
        skill(codex.path(), "shared", "SKILL.md");
        skill(codex.path(), "codex-only", "SKILL.md");
        skill(&codex.path().join(".system"), "built-in", "SKILL.md");

        let skills = scan_skill_roots(claude.path(), codex.path());
        let names = skills
            .iter()
            .map(|skill| (skill.name.as_str(), skill.source.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                ("built-in", "codex"),
                ("codex-only", "codex"),
                ("shared", "claude"),
            ]
        );
    }
}
