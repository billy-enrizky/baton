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
}

pub fn scan_skills() -> Vec<DiscoveredSkill> {
    let mut skills = Vec::new();
    let claude_skills = crate::paths::claude_global_settings_dir().join("skills");
    if let Ok(entries) = std::fs::read_dir(&claude_skills) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    skills.push(DiscoveredSkill {
                        name: name.to_string(),
                        path: path.clone(),
                        source: "claude".to_string(),
                    });
                }
            }
        }
    }
    let codex_skills = crate::paths::codex_global_home().join("skills");
    if let Ok(entries) = std::fs::read_dir(&codex_skills) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    if !skills.iter().any(|skill| skill.name == name) {
                        skills.push(DiscoveredSkill {
                            name: name.to_string(),
                            path: path.clone(),
                            source: "codex".to_string(),
                        });
                    }
                }
            }
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
