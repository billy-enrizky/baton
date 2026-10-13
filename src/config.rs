use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::harness_config::read_harness_config;
use crate::types::{PermissionMode, ProviderId};

pub use crate::types::AuthMode;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderAuthFile {
    pub mode: Option<AuthMode>,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BatonConfigFile {
    #[serde(default, flatten)]
    pub providers: BTreeMap<String, ProviderAuthFile>,
    #[serde(default, rename = "defaultModels")]
    pub default_models: Option<BTreeMap<String, String>>,
    #[serde(default, rename = "defaultProvider")]
    pub default_provider: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EffectiveProviderConfig {
    pub mode: AuthMode,
    pub api_key: Option<String>,
    pub api_key_env_name: String,
    pub model: String,
    pub permission_mode: PermissionMode,
}

#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    pub providers: BTreeMap<ProviderId, EffectiveProviderConfig>,
}

pub fn default_models() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("claude".to_string(), "claude-sonnet-4-5".to_string()),
        ("codex".to_string(), "gpt-6.1-sol".to_string()),
    ])
}



pub fn config_path(home: &Path) -> PathBuf {
    home.join("config.json")
}

pub fn load_config_file(home: &Path) -> Option<BatonConfigFile> {
    let path = config_path(home);
    if !path.is_file() {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

pub fn save_config_file(home: &Path, file: &BatonConfigFile) -> Result<()> {
    std::fs::create_dir_all(home)?;
    std::fs::write(config_path(home), format!("{}\n", serde_json::to_string_pretty(file)?))?;
    Ok(())
}

pub fn resolve_provider_config(
    provider: ProviderId,
    home: &Path,
    cwd: &Path,
    claude_dir: Option<&Path>,
    codex_home: Option<&Path>,
) -> EffectiveProviderConfig {
    let file = load_config_file(home);
    let harness = read_harness_config(provider, Some(cwd), claude_dir, codex_home);
    let env_key_name = harness
        .api_key_env_name
        .clone()
        .unwrap_or_else(|| crate::models::api_key_env(provider).to_string());
    let file_provider = file
        .as_ref()
        .and_then(|file| file.providers.get(provider.as_str()))
        .cloned();
    let file_model = file
        .as_ref()
        .and_then(|file| file.default_models.as_ref())
        .and_then(|models| models.get(provider.as_str()))
        .cloned();

    let ambient_key = std::env::var(&env_key_name)
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let mode = file_provider
        .as_ref()
        .and_then(|auth| auth.mode)
        .or(harness.auth_mode)
        .or_else(|| if ambient_key { Some(AuthMode::Api) } else { None })
        .unwrap_or(AuthMode::Subscription);

    let api_key = file_provider
        .and_then(|auth| auth.api_key)
        .filter(|key| !key.trim().is_empty())
        .or(harness.api_key)
        .or_else(|| std::env::var(&env_key_name).ok().filter(|key| !key.trim().is_empty()));

    let fallback_model = crate::models::default_model(provider).to_string();

    EffectiveProviderConfig {
        mode,
        api_key: api_key.filter(|key| !key.trim().is_empty()),
        api_key_env_name: env_key_name,
        model: file_model.or(harness.model).unwrap_or(fallback_model),
        permission_mode: harness.permission_mode.unwrap_or(PermissionMode::Default),
    }
}

pub fn get_default_provider(home: &Path) -> Option<ProviderId> {
    let file = load_config_file(home)?;
    file.default_provider
        .as_deref()
        .and_then(ProviderId::parse)
}

pub fn resolve_config(home: &Path, cwd: &Path) -> EffectiveConfig {
    resolve_config_in(home, cwd, None, None)
}

pub fn resolve_config_in(
    home: &Path,
    cwd: &Path,
    claude_dir: Option<&Path>,
    codex_home: Option<&Path>,
) -> EffectiveConfig {
    let mut providers = BTreeMap::new();
    for provider in ProviderId::all() {
        providers.insert(
            *provider,
            resolve_provider_config(*provider, home, cwd, claude_dir, codex_home),
        );
    }
    EffectiveConfig { providers }
}

impl EffectiveConfig {
    pub fn for_provider(&self, provider: ProviderId) -> &EffectiveProviderConfig {
        self.providers
            .get(&provider)
            .unwrap_or_else(|| panic!("missing config for {provider}"))
    }
}

pub fn provider_auth_env(auth: &EffectiveProviderConfig) -> Vec<(String, String)> {
    if auth.mode != AuthMode::Api {
        return Vec::new();
    }
    let key = auth
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var(&auth.api_key_env_name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    match key {
        Some(key) => vec![(auth.api_key_env_name.clone(), key)],
        None => Vec::new(),
    }
}

pub fn auth_signature(env_additions: &[(String, String)]) -> String {
    let mut entries: Vec<String> = env_additions
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    entries.sort();
    entries.join(";")
}

pub fn describe_auth(auth: &EffectiveProviderConfig) -> String {
    match auth.mode {
        AuthMode::Subscription => "subscription (harness login)".to_string(),
        AuthMode::Api => {
            let has_key = auth.api_key.is_some()
                || std::env::var(&auth.api_key_env_name)
                    .map(|value| !value.trim().is_empty())
                    .unwrap_or(false);
            if has_key {
                format!("api ({})", auth.api_key_env_name)
            } else {
                format!("api (MISSING {})", auth.api_key_env_name)
            }
        }
    }
}
