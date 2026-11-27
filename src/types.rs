use std::fmt;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum ProviderId {
    Claude,
    Codex,
    Cursor,
    Grok,
    OpenCode,
    Antigravity,
}

/// A provider name as stored or configured. Built-in harnesses map to
/// `ProviderId`; a configured agent that Baton does not know yet keeps its own
/// name instead of being silently rewritten to a built-in one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderKey(String);

impl ProviderKey {
    /// Validates a name that came from outside Baton: config, CLI, ACP.
    pub fn parse(value: &str) -> Option<ProviderKey> {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.len() > 64 {
            return None;
        }
        let valid = trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/'));
        valid.then(|| ProviderKey(trimmed.to_string()))
    }

    /// A value read back from the store. Storage only ever holds what Baton or
    /// a validated config wrote, so this never rewrites an unknown name.
    pub fn from_stored(value: &str) -> ProviderKey {
        ProviderKey(value.trim().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn builtin(&self) -> Option<ProviderId> {
        ProviderId::parse(&self.0)
    }
}

impl From<ProviderId> for ProviderKey {
    fn from(provider: ProviderId) -> Self {
        ProviderKey(provider.as_str().to_string())
    }
}

impl fmt::Display for ProviderKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
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
            ProviderId::Antigravity => "agy",
        }
    }

    pub fn supports_effort(self) -> bool {
        matches!(self, ProviderId::Codex | ProviderId::Claude)
    }

    pub fn supports_native_rewind(self) -> bool {
        matches!(self, ProviderId::Codex | ProviderId::OpenCode)
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

    /// Privilege order used by a grant ceiling. Plan is read-only, Default
    /// asks before changes, AcceptEdits auto-approves edits, and
    /// BypassPermissions skips checks.
    pub fn privilege(self) -> u8 {
        match self {
            PermissionMode::Plan => 0,
            PermissionMode::Default => 1,
            PermissionMode::AcceptEdits => 2,
            PermissionMode::BypassPermissions => 3,
        }
    }

    /// True when a thread may run under a grant minted with this ceiling.
    pub fn within_ceiling(self, ceiling: PermissionMode) -> bool {
        self.privilege() <= ceiling.privilege()
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RateLimitWindow {
    pub used_percent: i64,
    pub window_duration_mins: Option<i64>,
    /// Unix seconds reported by the provider.
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RateLimitInfo {
    pub plan_type: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub credits_balance: Option<String>,
    pub credits_unlimited: bool,
    /// When Baton received the snapshot, as an RFC 3339 timestamp.
    pub reported_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ApprovalChoice {
    pub id: String,
    pub label: String,
    pub kind: String,
}

impl ApprovalChoice {
    /// The provider decision this choice represents. Custom ACP options keep
    /// their own id so the adapter can map it back to its protocol.
    pub fn decision(&self) -> ApprovalDecision {
        match self.id.as_str() {
            "accept" => ApprovalDecision::Accept,
            "accept_always" => ApprovalDecision::AcceptAlways,
            "deny" => ApprovalDecision::Deny,
            _ => ApprovalDecision::Choose(self.id.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QuestionPrompt {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    pub allow_custom: bool,
    pub multi_select: bool,
    pub secret: bool,
}

impl QuestionPrompt {
    /// A prompt accepts listed option labels and, when it allows custom text,
    /// any other string. An empty answer is allowed so a prompt can be skipped.
    pub fn accepts(&self, answers: &[String]) -> bool {
        if answers.is_empty() {
            return true;
        }
        if !self.multi_select && answers.len() > 1 {
            return false;
        }
        answers.iter().all(|value| {
            self.allow_custom || self.options.iter().any(|option| &option.label == value)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionAnswer {
    pub id: String,
    pub answers: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanKind {
    #[default]
    Items,
    Markdown,
    File,
}

impl PlanKind {
    pub fn is_items(&self) -> bool {
        matches!(self, PlanKind::Items)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: String,
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "PlanKind::is_items")]
    pub kind: PlanKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

impl Default for PlanEntry {
    fn default() -> Self {
        Self {
            content: String::new(),
            status: "pending".to_string(),
            priority: None,
            kind: PlanKind::Items,
            uri: None,
        }
    }
}

impl PlanEntry {
    pub fn item(
        content: impl Into<String>,
        status: impl Into<String>,
        priority: Option<String>,
    ) -> Self {
        Self {
            content: content.into(),
            status: status.into(),
            priority,
            ..Self::default()
        }
    }

    pub fn markdown(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind: PlanKind::Markdown,
            ..Self::default()
        }
    }

    pub fn file(uri: impl Into<String>) -> Self {
        Self {
            uri: Some(uri.into()),
            kind: PlanKind::File,
            ..Self::default()
        }
    }
}

pub fn normalize_plan_status(status: &str) -> String {
    match status.trim() {
        "inProgress" | "in-progress" | "in_progress" => "in_progress".to_string(),
        "completed" => "completed".to_string(),
        "cancelled" | "canceled" => "cancelled".to_string(),
        "pending" => "pending".to_string(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Accept,
    AcceptAlways,
    Deny,
    Choose(String),
}

impl ApprovalDecision {
    /// Stable wire and audit spelling shared by the TUI and the remote host.
    pub fn as_str(&self) -> String {
        match self {
            ApprovalDecision::Accept => "accept".to_string(),
            ApprovalDecision::AcceptAlways => "accept_always".to_string(),
            ApprovalDecision::Deny => "deny".to_string(),
            ApprovalDecision::Choose(option) => format!("choose:{option}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
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
    fn permission_modes_order_under_a_ceiling() {
        assert!(PermissionMode::Plan.within_ceiling(PermissionMode::Default));
        assert!(PermissionMode::Default.within_ceiling(PermissionMode::Default));
        assert!(!PermissionMode::AcceptEdits.within_ceiling(PermissionMode::Default));
        assert!(PermissionMode::AcceptEdits.within_ceiling(PermissionMode::AcceptEdits));
        assert!(!PermissionMode::Default.within_ceiling(PermissionMode::Plan));
        assert!(PermissionMode::Plan.within_ceiling(PermissionMode::Plan));
        assert!(!PermissionMode::BypassPermissions.within_ceiling(PermissionMode::AcceptEdits));
    }

    #[test]
    fn question_prompts_accept_options_custom_text_and_one_choice() {
        let prompt = QuestionPrompt {
            id: "language".into(),
            header: "Language".into(),
            question: "Which language?".into(),
            options: vec![
                QuestionOption {
                    label: "Rust".into(),
                    description: String::new(),
                },
                QuestionOption {
                    label: "Go".into(),
                    description: String::new(),
                },
            ],
            allow_custom: false,
            multi_select: false,
            secret: false,
        };
        assert!(prompt.accepts(&[]));
        assert!(prompt.accepts(&["Rust".into()]));
        assert!(!prompt.accepts(&["Python".into()]));
        assert!(!prompt.accepts(&["Rust".into(), "Go".into()]));

        let custom = QuestionPrompt {
            allow_custom: true,
            ..prompt.clone()
        };
        assert!(custom.accepts(&["Python".into()]));

        let multi = QuestionPrompt {
            multi_select: true,
            ..prompt
        };
        assert!(multi.accepts(&["Rust".into(), "Go".into()]));
    }

    #[test]
    fn approval_choices_map_to_decisions_and_names() {
        let choice = |id: &str| ApprovalChoice {
            id: id.into(),
            label: id.into(),
            kind: "allow_once".into(),
        };
        assert_eq!(choice("accept").decision(), ApprovalDecision::Accept);
        assert_eq!(
            choice("accept_always").decision(),
            ApprovalDecision::AcceptAlways
        );
        assert_eq!(choice("deny").decision(), ApprovalDecision::Deny);
        assert_eq!(
            choice("opt-allow").decision(),
            ApprovalDecision::Choose("opt-allow".into())
        );
        assert_eq!(ApprovalDecision::Accept.as_str(), "accept");
        assert_eq!(ApprovalDecision::AcceptAlways.as_str(), "accept_always");
        assert_eq!(ApprovalDecision::Deny.as_str(), "deny");
        assert_eq!(
            ApprovalDecision::Choose("opt-allow".into()).as_str(),
            "choose:opt-allow"
        );
    }

    #[test]
    fn provider_key_keeps_an_unknown_name_verbatim() {
        let key = ProviderKey::from_stored("custom-agent");
        assert_eq!(key.as_str(), "custom-agent");
        assert_eq!(
            key.builtin(),
            None,
            "an unknown key must not become a built-in"
        );
        assert_eq!(key.to_string(), "custom-agent");
    }

    #[test]
    fn provider_key_maps_builtins_both_ways() {
        for provider in ProviderId::all() {
            let key = ProviderKey::from(*provider);
            assert_eq!(key.as_str(), provider.as_str());
            assert_eq!(key.builtin(), Some(*provider));
        }
    }

    #[test]
    fn provider_key_rejects_empty_and_odd_input() {
        assert!(ProviderKey::parse("").is_none());
        assert!(ProviderKey::parse("   ").is_none());
        assert!(ProviderKey::parse("has space").is_none());
        assert!(ProviderKey::parse("bad$name").is_none());
        assert!(ProviderKey::parse(&"x".repeat(65)).is_none());
        assert_eq!(
            ProviderKey::parse("claude").unwrap().builtin(),
            Some(ProviderId::Claude)
        );
    }

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
        assert_eq!(
            normalize_reasoning_effort(" High "),
            Some("high".to_string())
        );
        assert_eq!(
            normalize_reasoning_effort("x-high"),
            Some("x-high".to_string())
        );
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
    fn plan_statuses_use_one_vocabulary() {
        assert_eq!(normalize_plan_status("inProgress"), "in_progress");
        assert_eq!(normalize_plan_status("in_progress"), "in_progress");
        assert_eq!(normalize_plan_status("completed"), "completed");
        assert_eq!(normalize_plan_status("pending"), "pending");
        assert_eq!(normalize_plan_status("custom"), "custom");
    }

    #[test]
    fn plan_kinds_round_trip_and_keep_the_items_wire_shape() {
        let item = PlanEntry::item("step", "pending", None);
        let value = serde_json::to_value(&item).unwrap();
        assert!(value.get("kind").is_none(), "{value}");
        assert!(value.get("uri").is_none(), "{value}");
        assert_eq!(serde_json::from_value::<PlanEntry>(value).unwrap(), item);

        let markdown = PlanEntry::markdown("# Ship it");
        let value = serde_json::to_value(&markdown).unwrap();
        assert_eq!(
            value.get("kind").and_then(|kind| kind.as_str()),
            Some("markdown")
        );
        assert_eq!(
            serde_json::from_value::<PlanEntry>(value).unwrap(),
            markdown
        );

        let file = PlanEntry::file("file:///workspace/PLAN.md");
        let value = serde_json::to_value(&file).unwrap();
        assert_eq!(
            value.get("kind").and_then(|kind| kind.as_str()),
            Some("file")
        );
        assert_eq!(
            value.get("uri").and_then(|uri| uri.as_str()),
            Some("file:///workspace/PLAN.md")
        );
        assert_eq!(serde_json::from_value::<PlanEntry>(value).unwrap(), file);

        let legacy = serde_json::from_value::<PlanEntry>(serde_json::json!({
            "content": "old",
            "status": "pending",
            "priority": null
        }))
        .unwrap();
        assert_eq!(legacy.kind, PlanKind::Items);
        assert_eq!(legacy.uri, None);
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
