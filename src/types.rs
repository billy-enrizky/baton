use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum ProviderId {
    Claude,
    Codex,
    Cursor,
    Grok,
    OpenCode,
    Antigravity,
}

impl ProviderId {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderId::Claude => "claude",
            ProviderId::Codex => "codex",
            ProviderId::Cursor => "cursor",
            ProviderId::Grok => "grok",
            ProviderId::OpenCode => "opencode",
            ProviderId::Antigravity => "antigravity",
        }
    }

    pub fn parse(value: &str) -> Option<ProviderId> {
        match value {
            "claude" => Some(ProviderId::Claude),
            "codex" => Some(ProviderId::Codex),
            "cursor" => Some(ProviderId::Cursor),
            "grok" => Some(ProviderId::Grok),
            "opencode" => Some(ProviderId::OpenCode),
            "antigravity" => Some(ProviderId::Antigravity),
            _ => None,
        }
    }

    pub fn binary_name(self) -> &'static str {
        match self {
            ProviderId::Claude => "claude",
            ProviderId::Codex => "codex",
            ProviderId::Cursor => "cursor-agent",
            ProviderId::Grok => "grok",
            ProviderId::OpenCode => "opencode",
            ProviderId::Antigravity => "antigravity",
        }
    }

    pub fn supports_effort(self) -> bool {
        matches!(self, ProviderId::Codex | ProviderId::Claude)
    }

    pub fn all() -> &'static [ProviderId] {
        &[
            ProviderId::Claude,
            ProviderId::Codex,
            ProviderId::Cursor,
            ProviderId::Grok,
            ProviderId::OpenCode,
            ProviderId::Antigravity,
        ]
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Reasoning effort values are passed to provider CLIs as single tokens. Keep
/// the accepted alphabet narrow so a plan file cannot smuggle argument syntax.
pub fn is_effort_clear(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "default" | "off" | "none"
    )
}

pub fn normalize_reasoning_effort(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    BypassPermissions,
    Plan,
}

impl PermissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::Plan => "plan",
        }
    }

    pub fn parse(value: &str) -> Option<PermissionMode> {
        match value {
            "default" => Some(PermissionMode::Default),
            "acceptEdits" => Some(PermissionMode::AcceptEdits),
            "bypassPermissions" => Some(PermissionMode::BypassPermissions),
            "plan" => Some(PermissionMode::Plan),
            _ => None,
        }
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionMode {
    #[default]
    Single,
    Orchestrate,
}

impl SessionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionMode::Single => "single",
            SessionMode::Orchestrate => "orchestrate",
        }
    }

    pub fn parse(value: &str) -> Option<SessionMode> {
        match value {
            "single" => Some(SessionMode::Single),
            "orchestrate" => Some(SessionMode::Orchestrate),
            _ => None,
        }
    }
}

impl fmt::Display for SessionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    Subscription,
    Api,
}

impl AuthMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthMode::Subscription => "subscription",
            AuthMode::Api => "api",
        }
    }

    pub fn parse(value: &str) -> Option<AuthMode> {
        match value {
            "subscription" => Some(AuthMode::Subscription),
            "api" => Some(AuthMode::Api),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnPhase {
    Queued,
    Running,
    Settling,
    Completed,
    Failed,
    Interrupted,
    Unverified,
}

impl TurnPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            TurnPhase::Queued => "queued",
            TurnPhase::Running => "running",
            TurnPhase::Settling => "settling",
            TurnPhase::Completed => "completed",
            TurnPhase::Failed => "failed",
            TurnPhase::Interrupted => "interrupted",
            TurnPhase::Unverified => "unverified",
        }
    }

    pub fn parse(value: &str) -> Option<TurnPhase> {
        match value {
            "queued" => Some(TurnPhase::Queued),
            "running" => Some(TurnPhase::Running),
            "settling" => Some(TurnPhase::Settling),
            "completed" => Some(TurnPhase::Completed),
            "failed" => Some(TurnPhase::Failed),
            "interrupted" => Some(TurnPhase::Interrupted),
            "unverified" => Some(TurnPhase::Unverified),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TurnPhase::Completed
                | TurnPhase::Failed
                | TurnPhase::Interrupted
                | TurnPhase::Unverified
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Accept,
    AcceptAlways,
    Deny,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TurnUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct NativeSessionRef {
    pub native_session_id: String,
    pub rollout_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_terminal_phases_report_terminal() {
        for phase in [TurnPhase::Queued, TurnPhase::Running, TurnPhase::Settling] {
            assert!(!phase.is_terminal(), "{} must stay open", phase.as_str());
        }
        for phase in [
            TurnPhase::Completed,
            TurnPhase::Failed,
            TurnPhase::Interrupted,
            TurnPhase::Unverified,
        ] {
            assert!(phase.is_terminal(), "{} must be terminal", phase.as_str());
        }
    }

    #[test]
    fn reasoning_effort_is_normalized_and_provider_gated() {
        assert_eq!(normalize_reasoning_effort(" High "), Some("high".to_string()));
        assert_eq!(normalize_reasoning_effort("x-high"), Some("x-high".to_string()));
        assert_eq!(normalize_reasoning_effort(""), None);
        assert_eq!(normalize_reasoning_effort("high effort"), None);
        assert!(is_effort_clear(" default "));
        assert!(is_effort_clear("OFF"));
        assert!(!is_effort_clear("high"));
        assert!(ProviderId::Codex.supports_effort());
        assert!(ProviderId::Claude.supports_effort());
        assert!(!ProviderId::OpenCode.supports_effort());
        assert!(!ProviderId::Cursor.supports_effort());
    }

    #[test]
    fn phase_strings_round_trip() {
        for phase in [
            TurnPhase::Queued,
            TurnPhase::Running,
            TurnPhase::Settling,
            TurnPhase::Completed,
            TurnPhase::Failed,
            TurnPhase::Interrupted,
            TurnPhase::Unverified,
        ] {
            assert_eq!(TurnPhase::parse(phase.as_str()), Some(phase));
        }
        assert_eq!(TurnPhase::parse("unknown"), None);
    }
}
