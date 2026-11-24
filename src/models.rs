use crate::types::ProviderId;

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
    let mut merged: Vec<String> = Vec::new();
    for model in live {
        let model = model.trim();
        if !model.is_empty() && !merged.iter().any(|existing| existing == model) {
            merged.push(model.to_string());
        }
    }
    for known in catalog(provider) {
        if !merged.iter().any(|existing| existing == known) {
            merged.push((*known).to_string());
        }
    }
    merged
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
    fn live_catalog_merge_returns_the_known_catalog_when_live_is_empty() {
        let merged = merge_live_catalog(ProviderId::Antigravity, Vec::new());
        let expected = catalog(ProviderId::Antigravity)
            .iter()
            .map(|model| (*model).to_string())
            .collect::<Vec<_>>();
        assert_eq!(merged, expected);
    }
}
