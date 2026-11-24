use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::types::ProviderId;

pub const MAX_CACHED_MODELS: usize = 200;
const MAX_MODEL_CACHE_BYTES: u64 = 64 * 1024;

/// One loaded catalog and where each part came from, so a notice can say
/// whether the listing was live, cached, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalog {
    pub models: Vec<String>,
    pub live: usize,
    pub cached: usize,
    pub error: Option<String>,
}

pub fn catalog(provider: ProviderId) -> &'static [&'static str] {
    match provider {
        ProviderId::Claude => &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5"],
        ProviderId::Codex => &[
            "gpt-6.1-sol",
            "gpt-6.2-sol",
            "gpt-6.2-astra",
            "gpt-5.6-luna",
            "gpt-5.6-terra",
            "gpt-6-astra",
        ],
        ProviderId::Cursor => &["composer-2.5"],
        ProviderId::Grok => &["grok-build", "grok-4.6", "grok-4.5"],
        ProviderId::OpenCode => &["anthropic/claude-sonnet-4-5", "openai/gpt-6.1-sol"],
        ProviderId::Antigravity => &["gemini-3.7-flash", "gemini-3.6-flash", "gemini-3.1-pro", "gemini-4-carbon", "gemini-4-hydrogen", "gemini-4-argon"],
    }
}

pub fn default_model(provider: ProviderId) -> &'static str {
    catalog(provider).first().copied().unwrap_or("default")
}

/// Keep the adapter's live order, then append known models the live probe
/// omitted. A short or degraded listing must not hide a model the picker
/// already knew.
pub fn merge_live_catalog(provider: ProviderId, live: Vec<String>) -> Vec<String> {
    merge_catalogs(provider, live, &[])
}

/// Keep the live order, then the last cached full list, then the known
/// catalog. A short or degraded live probe must not hide a model the picker
/// saw before, and the static list must not hide a newer cached one.
pub fn merge_catalogs(provider: ProviderId, live: Vec<String>, cached: &[String]) -> Vec<String> {
    let mut merged = cleaned_models(live);
    for model in cleaned_models(cached.iter().cloned()) {
        if !merged.contains(&model) {
            merged.push(model);
        }
    }
    for known in catalog(provider) {
        if !merged.iter().any(|existing| existing == known) {
            merged.push((*known).to_string());
        }
    }
    merged
}

fn cleaned_models(models: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut cleaned: Vec<String> = Vec::new();
    for model in models {
        let model = model.trim();
        if model.is_empty() || model.chars().any(char::is_control) {
            continue;
        }
        if !cleaned.iter().any(|existing| existing == model) {
            cleaned.push(model.to_string());
        }
        if cleaned.len() == MAX_CACHED_MODELS {
            break;
        }
    }
    cleaned
}

pub fn model_cache_path(home: &Path, provider: ProviderId) -> PathBuf {
    home.join("model-cache")
        .join(format!("{}.json", provider.as_str()))
}

/// The last full live listing for this provider. A symlink, a directory, an
/// oversized file, or invalid JSON yields an empty cache instead of an error:
/// the picker still opens with the static catalog.
pub fn read_cached_catalog(home: &Path, provider: ProviderId) -> Vec<String> {
    let path = model_cache_path(home, provider);
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return Vec::new();
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_MODEL_CACHE_BYTES {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let models = value
        .get("models")
        .and_then(|models| models.as_array())
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    cleaned_models(models)
}

/// Replace the cache with a live listing. The caller decides when a listing is
/// full enough to replace the previous one; this function only writes.
pub fn write_cached_catalog(home: &Path, provider: ProviderId, models: &[String]) -> Result<()> {
    let cleaned = cleaned_models(models.iter().cloned());
    if cleaned.is_empty() {
        return Ok(());
    }
    let directory = home.join("model-cache");
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("cannot create {}", directory.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let path = model_cache_path(home, provider);
    let temp = directory.join(format!(".{}.{}.tmp", provider.as_str(), std::process::id()));
    let payload = format!(
        "{}\n",
        serde_json::to_string_pretty(&serde_json::json!({ "models": cleaned }))?
    );
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write as _;
        let mut handle = options
            .open(&temp)
            .with_context(|| format!("cannot write {}", temp.display()))?;
        handle.write_all(payload.as_bytes())?;
        handle.sync_all()?;
        std::fs::rename(&temp, &path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// The environment variable the harness itself reads for the API key. A
/// custom `apiKeyEnv` changes where baton reads the key, not where the child
/// expects it.
pub fn native_api_key_env(provider: ProviderId) -> &'static str {
    api_key_env(provider)
}

/// The environment variables a harness reads for a custom API base URL. Some
/// CLIs use separate chat and model-list variables. Antigravity appends its
/// own `/v1beta` paths, so its value is the endpoint root; Cursor and Grok
/// use the OpenAI-compatible `/v1` base.
pub fn base_url_envs(provider: ProviderId) -> &'static [&'static str] {
    match provider {
        ProviderId::Antigravity => &["GOOGLE_GEMINI_BASE_URL"],
        ProviderId::Grok => &["GROK_XAI_API_BASE_URL", "GROK_MODELS_BASE_URL"],
        _ => &[],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// The harness accepts a provider-neutral API base URL.
    pub custom_base_url: bool,
    /// The harness can use an API key.
    pub api_key: bool,
    /// The harness has its own interactive login.
    pub vendor_login: bool,
}

pub fn capabilities(provider: ProviderId) -> ProviderCapabilities {
    match provider {
        ProviderId::Antigravity => ProviderCapabilities { custom_base_url: true, api_key: true, vendor_login: false },
        ProviderId::Grok => ProviderCapabilities { custom_base_url: true, api_key: true, vendor_login: true },
        // Cursor validates its own keys and has no custom-endpoint mode.
        ProviderId::Cursor => ProviderCapabilities { custom_base_url: false, api_key: true, vendor_login: true },
        ProviderId::Claude | ProviderId::Codex | ProviderId::OpenCode => {
            ProviderCapabilities { custom_base_url: false, api_key: true, vendor_login: true }
        }
    }
}

pub fn api_key_env(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Claude => "ANTHROPIC_API_KEY",
        ProviderId::Codex => "OPENAI_API_KEY",
        ProviderId::Cursor => "CURSOR_API_KEY",
        ProviderId::Grok => "XAI_API_KEY",
        ProviderId::OpenCode => "OPENCODE_API_KEY",
        ProviderId::Antigravity => "GEMINI_API_KEY",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_harnesses_with_a_custom_endpoint_mode_advertise_one() {
        assert!(!capabilities(ProviderId::Cursor).custom_base_url);
        assert!(capabilities(ProviderId::Grok).custom_base_url);
        assert!(capabilities(ProviderId::Antigravity).custom_base_url);
        assert!(base_url_envs(ProviderId::Cursor).is_empty());
        assert!(base_url_envs(ProviderId::Grok).contains(&"GROK_XAI_API_BASE_URL"));
    }

    #[test]
    fn codex_catalog_includes_the_default_role_models() {
        let catalog = catalog(ProviderId::Codex);
        assert!(catalog.contains(&"gpt-6.2-sol"));
        assert!(catalog.contains(&"gpt-6.2-astra"));
    }

    #[test]
    fn live_catalog_merge_keeps_live_order_and_adds_known_models() {
        let merged = merge_live_catalog(
            ProviderId::Codex,
            vec![
                " gpt-6.1-sol ".to_string(),
                "new-model".to_string(),
                "gpt-6.1-sol".to_string(),
                String::new(),
            ],
        );
        assert_eq!(merged[0], "gpt-6.1-sol");
        assert_eq!(merged[1], "new-model");
        assert!(merged.contains(&"gpt-6.2-sol".to_string()));
        assert_eq!(
            merged
                .iter()
                .filter(|model| model.as_str() == "gpt-6.1-sol")
                .count(),
            1
        );
    }

    #[test]
    fn merge_catalogs_keeps_live_then_cached_then_known() {
        let merged = merge_catalogs(
            ProviderId::Codex,
            vec!["live-only".to_string(), "shared".to_string()],
            &["cached-only".to_string(), "shared".to_string()],
        );
        assert_eq!(merged[0], "live-only");
        assert_eq!(merged[1], "shared");
        assert_eq!(merged[2], "cached-only");
        assert!(merged.contains(&"gpt-6.2-sol".to_string()));
        assert_eq!(merged.iter().filter(|model| *model == "shared").count(), 1);
    }

    #[test]
    fn model_cache_round_trips_and_rejects_unsafe_files() {
        let home = tempfile::tempdir().unwrap();
        assert!(read_cached_catalog(home.path(), ProviderId::Codex).is_empty());

        write_cached_catalog(
            home.path(),
            ProviderId::Codex,
            &[
                " gpt-6.1-sol ".to_string(),
                "new-model".to_string(),
                "new-model".to_string(),
                "bad\nmodel".to_string(),
                String::new(),
            ],
        )
        .unwrap();
        assert_eq!(
            read_cached_catalog(home.path(), ProviderId::Codex),
            vec!["gpt-6.1-sol".to_string(), "new-model".to_string()]
        );

        let path = model_cache_path(home.path(), ProviderId::Codex);
        std::fs::write(&path, vec![b'x'; (MAX_MODEL_CACHE_BYTES + 1) as usize]).unwrap();
        assert!(
            read_cached_catalog(home.path(), ProviderId::Codex).is_empty(),
            "an oversized file is ignored"
        );

        std::fs::remove_file(&path).unwrap();
        let target = home.path().join("target.json");
        std::fs::write(&target, r#"{"models":["from-a-symlink"]}"#).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(
                read_cached_catalog(home.path(), ProviderId::Codex).is_empty(),
                "a symlink is ignored"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn model_cache_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        write_cached_catalog(home.path(), ProviderId::Codex, &["gpt-6.1-sol".to_string()]).unwrap();
        let directory = std::fs::metadata(home.path().join("model-cache"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let file = std::fs::metadata(model_cache_path(home.path(), ProviderId::Codex))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory, 0o700);
        assert_eq!(file, 0o600);
    }

    #[test]
    fn live_catalog_merge_returns_the_known_catalog_when_live_is_empty() {
        let merged = merge_live_catalog(ProviderId::Antigravity, Vec::new());
        let expected = catalog(ProviderId::Antigravity)
            .iter()
            .map(|model| (*model).to_string())
            .collect::<Vec<_>>();
        assert_eq!(merged, expected);
    }
}
