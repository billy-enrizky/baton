use crate::types::ProviderId;

pub fn catalog(provider: ProviderId) -> &'static [&'static str] {
    match provider {
        ProviderId::Claude => &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5"],
        ProviderId::Codex => &["gpt-6.1-sol", "gpt-5.6-luna", "gpt-5.6-terra", "gpt-6-astra"],
        ProviderId::Cursor => &["composer-2.5"],
        ProviderId::Grok => &["grok-build", "grok-4.6", "grok-4.5"],
        ProviderId::OpenCode => &["anthropic/claude-sonnet-4-5", "openai/gpt-6.1-sol"],
        ProviderId::Antigravity => &["gemini-3.8-flash-high", "gemini-3.8-flash-medium", "gemini-2.5-pro", "gemini-2.5-flash"],
    }
}

pub fn default_model(provider: ProviderId) -> &'static str {
    catalog(provider).first().copied().unwrap_or("default")
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
