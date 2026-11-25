use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::harness_config::read_harness_config;
use crate::types::{PermissionMode, ProviderId};

pub use crate::types::AuthMode;

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct ProviderAuthFile {
    pub mode: Option<AuthMode>,
    pub api_key: Option<String>,
    /// Where baton reads the key. The child still receives the harness-native
    /// variable, for example `GEMINI_API_KEY` for Antigravity.
    #[serde(default, alias = "apiKeyEnv", alias = "api_key_env")]
    pub api_key_env: Option<String>,
    /// Endpoint root for harnesses that accept a custom base URL.
    #[serde(default, alias = "apiBaseUrl", alias = "api_base_url")]
    pub api_base_url: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl std::fmt::Debug for ProviderAuthFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderAuthFile")
            .field("mode", &self.mode)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("api_key_env", &self.api_key_env)
            .field("api_base_url", &self.api_base_url)
            .field("extra", &self.extra)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotificationsConfig {
    /// Ring the terminal on turn end. Off unless the user turns it on.
    #[serde(default)]
    pub bell: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BatonConfigFile {
    #[serde(default)]
    pub notifications: NotificationsConfig,
    #[serde(default, flatten)]
    pub providers: BTreeMap<String, ProviderAuthFile>,
    #[serde(default, rename = "defaultModels")]
    pub default_models: Option<BTreeMap<String, String>>,
    #[serde(default, rename = "defaultProvider")]
    pub default_provider: Option<String>,
    #[serde(default, rename = "defaultMode")]
    pub default_mode: Option<String>,
}

#[derive(Clone)]
pub struct EffectiveProviderConfig {
    pub provider: ProviderId,
    pub mode: AuthMode,
    pub api_key: Option<String>,
    /// Source variable name for the key, which may be a provider-neutral name
    /// such as `CODEX_ICA_API_KEY`.
    pub api_key_env_name: String,
    pub api_base_url: Option<String>,
    pub model: String,
    pub permission_mode: PermissionMode,
}

impl std::fmt::Debug for EffectiveProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectiveProviderConfig")
            .field("provider", &self.provider)
            .field("mode", &self.mode)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("api_key_env_name", &self.api_key_env_name)
            .field("api_base_url", &self.api_base_url)
            .field("model", &self.model)
            .field("permission_mode", &self.permission_mode)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    pub providers: BTreeMap<ProviderId, EffectiveProviderConfig>,
}

/// Which source supplied the API key that the resolver chose. The resolver
/// checks the Baton config file first, then the harness's own config, then the
/// environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeySource {
    ConfigFile,
    HarnessConfig,
    Environment,
    Missing,
}

impl ApiKeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            ApiKeySource::ConfigFile => "config-file",
            ApiKeySource::HarnessConfig => "harness-config",
            ApiKeySource::Environment => "environment",
            ApiKeySource::Missing => "missing",
        }
    }
}

/// The credential facts a caller may print. The key itself is reduced to a
/// boolean so a report cannot leak it.
#[derive(Debug, Clone)]
pub struct ProviderSecret {
    pub provider: ProviderId,
    pub mode: AuthMode,
    pub key_source: ApiKeySource,
    pub key_variable: String,
    pub key_present: bool,
    pub base_url_present: bool,
}

pub fn default_models() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("claude".to_string(), "claude-sonnet-4-5".to_string()),
        ("codex".to_string(), "gpt-6.1-sol".to_string()),
    ])
}



pub fn notifications(home: &Path) -> NotificationsConfig {
    load_config_file(home).map(|file| file.notifications).unwrap_or_default()
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

/// Reads config for a writer. A malformed file is an error, not an empty config
/// to overwrite, because it may contain hand-edited provider keys.
pub fn load_config_file_strict(home: &Path) -> Result<Option<BatonConfigFile>> {
    let path = config_path(home);
    if !path.is_file() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let file = serde_json::from_str(&contents)
        .with_context(|| format!("cannot parse {}", path.display()))?;
    Ok(Some(file))
}

/// Rejects provider overrides the harness cannot honor before a turn starts.
pub fn validate_provider_overrides(home: &Path) -> Result<()> {
    let Some(file) = load_config_file(home) else {
        return Ok(());
    };
    for (name, auth) in &file.providers {
        let Some(provider) = ProviderId::parse(name) else {
            continue;
        };
        if auth.api_base_url.is_some() && !crate::models::capabilities(provider).custom_base_url {
            match provider {
                ProviderId::Cursor => anyhow::bail!(
                    "Cursor does not support a custom endpoint; remove the cursor apiBaseUrl and run `cursor-agent login`"
                ),
                _ => anyhow::bail!(
                    "{} does not support a custom endpoint; remove its apiBaseUrl",
                    provider
                ),
            }
        }
    }
    Ok(())
}

pub fn save_config_file(home: &Path, file: &BatonConfigFile) -> Result<()> {
    std::fs::create_dir_all(home)
        .with_context(|| format!("cannot create {}", home.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
    }
    let path = config_path(home);
    let temp = home.join(format!(".config.json.{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options
            .open(&temp)
            .with_context(|| format!("cannot write {}", temp.display()))?;
        handle.write_all(format!("{}\n", serde_json::to_string_pretty(file)?).as_bytes())?;
        handle.sync_all()?;
        std::fs::rename(&temp, &path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        #[cfg(unix)]
        std::fs::File::open(home)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

pub fn resolve_provider_config(
    provider: ProviderId,
    home: &Path,
    cwd: &Path,
    claude_dir: Option<&Path>,
    codex_home: Option<&Path>,
    opencode_dir: Option<&Path>,
) -> EffectiveProviderConfig {
    resolve_provider_config_with_source(provider, home, cwd, claude_dir, codex_home, opencode_dir).0
}

pub fn provider_secret(provider: ProviderId, home: &Path, cwd: &Path) -> ProviderSecret {
    let (config, key_source) =
        resolve_provider_config_with_source(provider, home, cwd, None, None, None);
    ProviderSecret {
        provider,
        mode: config.mode,
        key_source,
        key_variable: config.api_key_env_name,
        key_present: config.api_key.is_some(),
        base_url_present: config.api_base_url.is_some(),
    }
}

fn resolve_provider_config_with_source(
    provider: ProviderId,
    home: &Path,
    cwd: &Path,
    claude_dir: Option<&Path>,
    codex_home: Option<&Path>,
    opencode_dir: Option<&Path>,
) -> (EffectiveProviderConfig, ApiKeySource) {
    let file = load_config_file(home);
    let harness = read_harness_config(provider, Some(cwd), claude_dir, codex_home, opencode_dir);
    let file_provider = file
        .as_ref()
        .and_then(|file| file.providers.get(provider.as_str()))
        .cloned();
    let env_key_name = file_provider
        .as_ref()
        .and_then(|auth| auth.api_key_env.clone())
        .filter(|name| !name.trim().is_empty())
        .or_else(|| harness.api_key_env_name.clone())
        .unwrap_or_else(|| crate::models::api_key_env(provider).to_string());
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

    let file_key = file_provider
        .as_ref()
        .and_then(|auth| auth.api_key.clone())
        .filter(|key| !key.trim().is_empty());
    let harness_key = harness.api_key.clone().filter(|key| !key.trim().is_empty());
    let env_key = std::env::var(&env_key_name)
        .ok()
        .filter(|key| !key.trim().is_empty());
    let (api_key, key_source) = if file_key.is_some() {
        (file_key, ApiKeySource::ConfigFile)
    } else if harness_key.is_some() {
        (harness_key, ApiKeySource::HarnessConfig)
    } else if env_key.is_some() {
        (env_key, ApiKeySource::Environment)
    } else {
        (None, ApiKeySource::Missing)
    };

    let fallback_model = crate::models::default_model(provider).to_string();

    let effective = EffectiveProviderConfig {
        provider,
        mode,
        api_key,
        api_key_env_name: env_key_name,
        api_base_url: file_provider
            .as_ref()
            .and_then(|auth| auth.api_base_url.clone())
            .filter(|base| !base.trim().is_empty()),
        model: file_model.or(harness.model).unwrap_or(fallback_model),
        permission_mode: harness.permission_mode.unwrap_or(PermissionMode::Default),
    };
    (effective, key_source)
}

/// New sessions orchestrate by default. A harness is still used as the
/// coordinator, but the prompt is split into worker tasks and each worker can
/// run a different provider. Set `defaultMode` to `single` to opt out.
pub fn get_default_session_mode(home: &Path) -> crate::types::SessionMode {
    load_config_file(home)
        .and_then(|file| file.default_mode)
        .and_then(|value| crate::types::SessionMode::parse(&value))
        .unwrap_or(crate::types::SessionMode::Orchestrate)
}

/// Chooses the harness a new thread starts on. An explicit config choice wins,
/// then an installed harness with API auth, then the first installed harness.
/// Claude remains the last resort so the missing-harness hint can explain it.
pub fn pick_default_provider(
    configured: Option<ProviderId>,
    installed: &[ProviderId],
    api: &[ProviderId],
) -> ProviderId {
    if let Some(provider) = configured {
        return provider;
    }
    let order = ProviderId::all();
    order
        .iter()
        .copied()
        .find(|provider| installed.contains(provider) && api.contains(provider))
        .or_else(|| {
            order
                .iter()
                .copied()
                .find(|provider| installed.contains(provider))
        })
        .unwrap_or(ProviderId::Claude)
}

pub fn get_default_provider(home: &Path) -> Option<ProviderId> {
    let file = load_config_file(home)?;
    file.default_provider
        .as_deref()
        .and_then(ProviderId::parse)
}

pub fn resolve_config(home: &Path, cwd: &Path) -> EffectiveConfig {
    resolve_config_in(home, cwd, None, None, None)
}

pub fn resolve_config_in(
    home: &Path,
    cwd: &Path,
    claude_dir: Option<&Path>,
    codex_home: Option<&Path>,
    opencode_dir: Option<&Path>,
) -> EffectiveConfig {
    let mut providers = BTreeMap::new();
    for provider in ProviderId::all() {
        providers.insert(
            *provider,
            resolve_provider_config(*provider, home, cwd, claude_dir, codex_home, opencode_dir),
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
    let mut env = Vec::new();
    if let Some(key) = key {
        // The child expects the harness-native variable even when the key was
        // read from a provider-neutral one such as CODEX_ICA_API_KEY.
        env.push((
            crate::models::native_api_key_env(auth.provider).to_string(),
            key,
        ));
    }
    if let Some(base) = auth
        .api_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        for name in crate::models::base_url_envs(auth.provider) {
            env.push((name.to_string(), base.to_string()));
        }
    }
    env
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_preserves_extra_fields_and_private_permissions() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            config_path(home.path()),
            r#"{"notifications":{"bell":false,"desktop":true},"defaultModels":{"claude":"custom"},"claude":{"apiKeyEnv":"KEY_A","api_key":"secret","future":7}}"#,
        )
        .unwrap();

        let mut file = load_config_file_strict(home.path()).unwrap().unwrap();
        file.notifications.bell = true;
        save_config_file(home.path(), &file).unwrap();

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(config_path(home.path())).unwrap())
                .unwrap();
        assert_eq!(value["notifications"]["desktop"], true);
        assert_eq!(value["notifications"]["bell"], true);
        assert_eq!(value["claude"]["future"], 7);
        assert_eq!(value["claude"]["api_key"], "secret");
        assert_eq!(value["defaultModels"]["claude"], "custom");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = std::fs::metadata(config_path(home.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let home_mode = std::fs::metadata(home.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(file_mode, 0o600);
            assert_eq!(home_mode, 0o700);
        }
    }

    #[test]
    fn explicit_default_wins_even_when_missing() {
        assert_eq!(
            pick_default_provider(Some(ProviderId::Cursor), &[], &[]),
            ProviderId::Cursor
        );
    }

    #[test]
    fn installed_api_provider_beats_subscription_provider() {
        assert_eq!(
            pick_default_provider(
                None,
                &[ProviderId::Claude, ProviderId::Codex],
                &[ProviderId::Codex]
            ),
            ProviderId::Codex
        );
    }

    #[test]
    fn falls_back_to_the_first_installed_provider() {
        assert_eq!(
            pick_default_provider(None, &[ProviderId::OpenCode], &[]),
            ProviderId::OpenCode
        );
    }

    #[test]
    fn falls_back_to_claude_when_nothing_is_installed() {
        assert_eq!(
            pick_default_provider(None, &[], &[]),
            ProviderId::Claude
        );
    }

    #[test]
    fn provider_secret_reports_the_winning_source_without_the_key() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"codex":{"mode":"api","api_key":"config-secret-value","api_key_env":"BATON_TEST_SECRET_CONFIG_KEY"}}"#,
        )
        .unwrap();

        let secret = provider_secret(ProviderId::Codex, home.path(), home.path());
        assert_eq!(secret.key_source, ApiKeySource::ConfigFile);
        assert!(secret.key_present);
        assert_eq!(secret.key_variable, "BATON_TEST_SECRET_CONFIG_KEY");
        assert!(!format!("{secret:?}").contains("config-secret-value"));

        std::fs::write(
            &path,
            r#"{"cursor":{"mode":"api","apiKeyEnv":"BATON_TEST_SECRET_SOURCE_KEY"}}"#,
        )
        .unwrap();
        std::env::set_var("BATON_TEST_SECRET_SOURCE_KEY", "environment-secret-value");
        let secret = provider_secret(ProviderId::Cursor, home.path(), home.path());
        assert_eq!(secret.key_source, ApiKeySource::Environment);
        assert!(secret.key_present);
        assert_eq!(secret.key_variable, "BATON_TEST_SECRET_SOURCE_KEY");
        assert!(!format!("{secret:?}").contains("environment-secret-value"));

        std::env::remove_var("BATON_TEST_SECRET_SOURCE_KEY");
        let secret = provider_secret(ProviderId::Cursor, home.path(), home.path());
        assert_eq!(secret.key_source, ApiKeySource::Missing);
        assert!(!secret.key_present);
    }

    #[test]
    fn debug_output_redacts_provider_keys() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "codex".to_string(),
            ProviderAuthFile {
                mode: Some(AuthMode::Api),
                api_key: Some("sk-debug-secret-123456".to_string()),
                ..Default::default()
            },
        );
        let file = BatonConfigFile {
            providers,
            ..Default::default()
        };
        let text = format!("{file:?}");
        assert!(!text.contains("sk-debug-secret-123456"), "{text}");
        assert!(text.contains("[redacted]"), "{text}");

        let effective = EffectiveProviderConfig {
            provider: ProviderId::Codex,
            mode: AuthMode::Api,
            api_key: Some("sk-debug-secret-123456".to_string()),
            api_key_env_name: "OPENAI_API_KEY".to_string(),
            api_base_url: None,
            model: "gpt-6.1-sol".to_string(),
            permission_mode: PermissionMode::Default,
        };
        let text = format!("{effective:?}");
        assert!(!text.contains("sk-debug-secret-123456"), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
    }
}
