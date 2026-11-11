use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::types::{AuthMode, PermissionMode, ProviderId};

#[derive(Debug, Clone, Default)]
pub struct HarnessProviderConfig {
    pub model: Option<String>,
    pub auth_mode: Option<AuthMode>,
    pub api_key: Option<String>,
    pub api_key_env_name: Option<String>,
    pub permission_mode: Option<PermissionMode>,
    pub model_provider: Option<String>,
}

fn read_json_if_exists(path: &Path) -> Option<Value> {
    if !path.is_file() {
        return None;
    }
    std::fs::read_to_string(path).ok().and_then(|contents| serde_json::from_str(&contents).ok())
}

fn read_toml_if_exists(path: &Path) -> Option<toml::Value> {
    if !path.is_file() {
        return None;
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| toml::from_str::<toml::Table>(&contents).ok())
        .map(toml::Value::Table)
}

fn claude_permission_mode_map(value: &str) -> Option<PermissionMode> {
    match value {
        "default" => Some(PermissionMode::Default),
        "acceptEdits" => Some(PermissionMode::AcceptEdits),
        "plan" => Some(PermissionMode::Plan),
        "bypassPermissions" | "dangerously-skip-permissions" => Some(PermissionMode::BypassPermissions),
        "dontAsk" => Some(PermissionMode::AcceptEdits),
        _ => None,
    }
}

fn codex_approval_policy_map(value: &str) -> Option<PermissionMode> {
    match value {
        "untrusted" | "on-request" => Some(PermissionMode::Default),
        "on-failure" => Some(PermissionMode::AcceptEdits),
        "never" => Some(PermissionMode::BypassPermissions),
        _ => None,
    }
}

pub fn read_claude_harness_config(global_dir: &Path, project_root: Option<&Path>) -> HarnessProviderConfig {
    let mut layers: Vec<Value> = Vec::new();
    if let Some(value) = read_json_if_exists(&global_dir.join("settings.json")) {
        layers.push(value);
    }
    if let Some(root) = project_root {
        if let Some(value) = read_json_if_exists(&root.join(".claude").join("settings.json")) {
            layers.push(value);
        }
        if let Some(value) = read_json_if_exists(&root.join(".claude").join("settings.local.json")) {
            layers.push(value);
        }
    }
    if layers.is_empty() {
        return HarnessProviderConfig::default();
    }

    let mut merged: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut merged_env: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut default_mode: Option<String> = None;
    for layer in &layers {
        if let Value::Object(map) = layer {
            for (key, value) in map {
                merged.insert(key.clone(), value.clone());
            }
            if let Some(Value::Object(env)) = map.get("env") {
                for (key, value) in env {
                    merged_env.insert(key.clone(), value.clone());
                }
            }
            if let Some(Value::Object(permissions)) = map.get("permissions") {
                if let Some(Value::String(mode)) = permissions.get("defaultMode") {
                    if !mode.is_empty() {
                        default_mode = Some(mode.clone());
                    }
                }
            }
        }
    }

    let env_model = merged_env
        .get("ANTHROPIC_MODEL")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let settings_model = merged
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let api_key = merged_env
        .get("ANTHROPIC_API_KEY")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    HarnessProviderConfig {
        model: env_model.or(settings_model),
        auth_mode: if api_key.is_some() { Some(AuthMode::Api) } else { None },
        api_key,
        api_key_env_name: None,
        permission_mode: default_mode.as_deref().and_then(claude_permission_mode_map),
        model_provider: None,
    }
}

pub fn read_codex_harness_config(global_home: &Path, project_root: Option<&Path>) -> HarnessProviderConfig {    let global = read_toml_if_exists(&global_home.join("config.toml"));
    let project = project_root.and_then(|root| read_toml_if_exists(&root.join(".codex").join("config.toml")));
    if global.is_none() && project.is_none() {
        return HarnessProviderConfig::default();
    }

    let mut merged = global.clone().unwrap_or(toml::Value::Table(Default::default()));
    if let Some(project) = project {
        if let toml::Value::Table(project_table) = project {
            if let toml::Value::Table(merged_table) = &mut merged {
                for (key, value) in project_table {
                    match (merged_table.get_mut(&key), &value) {
                        (Some(toml::Value::Table(existing)), toml::Value::Table(incoming)) => {
                            for (sub_key, sub_value) in incoming {
                                existing.insert(sub_key.clone(), sub_value.clone());
                            }
                        }
                        _ => {
                            merged_table.insert(key, value);
                        }
                    }
                }
            }
        }
    }

    let model = merged
        .get("model")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let model_provider = merged
        .get("model_provider")
        .and_then(toml::Value::as_str)
        .unwrap_or("openai")
        .to_string();
    let env_key_name = merged
        .get("model_providers")
        .and_then(|providers| providers.get(&model_provider))
        .and_then(|provider| provider.get("env_key"))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let preferred_auth = merged.get("preferred_auth_method").and_then(toml::Value::as_str);

    let auth_mode = match preferred_auth {
        Some("apikey") => Some(AuthMode::Api),
        Some("chatgpt") => Some(AuthMode::Subscription),
        _ => {
            if model_provider != "openai" && env_key_name.is_some() {
                Some(AuthMode::Api)
            } else {
                None
            }
        }
    };

    let permission_mode = merged
        .get("approval_policy")
        .and_then(toml::Value::as_str)
        .and_then(codex_approval_policy_map);

    HarnessProviderConfig {
        model,
        auth_mode,
        api_key: None,
        api_key_env_name: env_key_name,
        permission_mode,
        model_provider: Some(model_provider),
    }
}

/// Strips // and /* */ comments from JSONC text without touching string literals.
fn strip_jsonc_comments(contents: &str) -> String {
    let mut output = String::with_capacity(contents.len());
    let mut chars = contents.chars().peekable();
    let mut in_string = false;
    while let Some(ch) = chars.next() {
        if in_string {
            output.push(ch);
            if ch == '\\' {
                if let Some(escaped) = chars.next() {
                    output.push(escaped);
                }
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                output.push(ch);
            }
            '/' => match chars.peek() {
                Some('/') => {
                    for skip in chars.by_ref() {
                        if skip == '\n' {
                            output.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    let mut previous = '\0';
                    for skip in chars.by_ref() {
                        if previous == '*' && skip == '/' {
                            break;
                        }
                        if skip == '\n' {
                            output.push('\n');
                        }
                        previous = skip;
                    }
                }
                _ => output.push(ch),
            },
            _ => output.push(ch),
        }
    }
    output
}

/// Removes commas followed only by whitespace and a closing brace or bracket.
fn strip_jsonc_trailing_commas(contents: &str) -> String {
    let chars: Vec<char> = contents.chars().collect();
    let mut output = String::with_capacity(contents.len());
    let mut in_string = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if in_string {
            output.push(ch);
            if ch == '\\' && index + 1 < chars.len() {
                index += 1;
                output.push(chars[index]);
            } else if ch == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            output.push(ch);
            index += 1;
            continue;
        }
        if ch == ',' {
            let mut lookahead = index + 1;
            while lookahead < chars.len() && chars[lookahead].is_whitespace() {
                lookahead += 1;
            }
            if lookahead < chars.len() && (chars[lookahead] == '}' || chars[lookahead] == ']') {
                index += 1;
                continue;
            }
        }
        output.push(ch);
        index += 1;
    }
    output
}

fn parse_jsonc(contents: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str(contents) {
        return Some(value);
    }
    let stripped = strip_jsonc_trailing_commas(&strip_jsonc_comments(contents));
    serde_json::from_str(&stripped).ok()
}

fn read_opencode_config_file(path: &Path) -> Option<Value> {
    if !path.is_file() {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    parse_jsonc(&contents)
}

/// Reads the opencode config (`opencode.json`/`opencode.jsonc`, global then
/// project) for the default `model` slug, e.g. `myprovider/rits/zai-org/glm-5-3`.
/// Auth stays opencode's own concern: provider API keys live inside its config,
/// so baton neither injects keys nor overrides the billing mode.
pub fn read_opencode_harness_config(global_dir: &Path, project_root: Option<&Path>) -> HarnessProviderConfig {
    let global = read_opencode_config_file(&global_dir.join("opencode.json"))
        .or_else(|| read_opencode_config_file(&global_dir.join("opencode.jsonc")));
    let project = project_root.and_then(|root| {
        read_opencode_config_file(&root.join("opencode.json"))
            .or_else(|| read_opencode_config_file(&root.join("opencode.jsonc")))
    });
    let model = project
        .as_ref()
        .and_then(|layer| layer.get("model").and_then(Value::as_str))
        .or_else(|| global.as_ref().and_then(|layer| layer.get("model").and_then(Value::as_str)))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    HarnessProviderConfig {
        model,
        auth_mode: None,
        api_key: None,
        api_key_env_name: None,
        permission_mode: None,
        model_provider: None,
    }
}

pub fn read_harness_config(
    provider: ProviderId,
    project_root: Option<&Path>,
    claude_dir: Option<&Path>,
    codex_home: Option<&Path>,
    opencode_dir: Option<&Path>,
) -> HarnessProviderConfig {
    match provider {
        ProviderId::Claude => read_claude_harness_config(
            claude_dir.unwrap_or(&crate::paths::claude_global_settings_dir()),
            project_root,
        ),
        ProviderId::Codex => read_codex_harness_config(
            codex_home.unwrap_or(&crate::paths::codex_global_home()),
            project_root,
        ),
        ProviderId::OpenCode => read_opencode_harness_config(
            opencode_dir.unwrap_or(&crate::paths::opencode_global_config_dir()),
            project_root,
        ),
        ProviderId::Cursor | ProviderId::Grok | ProviderId::Antigravity => {
            HarnessProviderConfig::default()
        }
    }
}

pub fn default_config_dirs() -> (PathBuf, PathBuf) {
    (crate::paths::claude_global_settings_dir(), crate::paths::codex_global_home())
}
