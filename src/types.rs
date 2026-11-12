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
    use super::TurnPhase;

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
