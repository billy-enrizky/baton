use crate::types::ProviderId;

pub fn catalog(provider: ProviderId) -> &'static [&'static str] {
    match provider {
        ProviderId::Claude => &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5"],
        ProviderId::Codex => &["gpt-6.1-sol", "gpt-5.6-luna", "gpt-5.6-terra", "gpt-6-astra"],
        ProviderId::Cursor => &["composer-2.5"],
        ProviderId::Grok => &["grok-build", "grok-4.6", "grok-4.5"],
        ProviderId::OpenCode => &["anthropic/claude-sonnet-4-5", "openai/gpt-6.1-sol"],
        ProviderId::Antigravity => &["gemini-3.7-flash", "gemini-3.6-flash", "gemini-3.1-pro", "gemini-4-carbon", "gemini-4-hydrogen", "gemini-4-argon"],
    }
}

pub fn default_model(provider: ProviderId) -> &'static str {
    catalog(provider).first().copied().unwrap_or("default")
}

/// The environment variable the harness itself reads for the API key. A
/// custom `apiKeyEnv` changes where baton reads the key, not where the child
/// expects it.
pub fn native_api_key_env(provider: ProviderId) -> &'static str {
    api_key_env(provider)
}

/// The environment variable a harness reads for a custom API base URL, when
/// the harness supports one. Antigravity appends its own `/v1beta` paths, so
/// the value is the endpoint root, not the OpenAI `/v1` base.
pub fn base_url_env(provider: ProviderId) -> Option<&'static str> {
    match provider {
        ProviderId::Antigravity => Some("GOOGLE_GEMINI_BASE_URL"),
        _ => None,
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
