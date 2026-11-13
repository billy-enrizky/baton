//! Stable, line-oriented thread status for humans and agents.

use crate::types::{PermissionMode, ProviderId, SessionMode, TurnPhase};

pub const SCHEMA_VERSION: u32 = 1;
pub const STALE_HINT: &str = "if no baton process is attached, this turn is stale and will be marked unverified on the next open";

#[derive(Debug, Clone)]
pub struct TurnStatus {
    pub turn: i64,
    pub phase: TurnPhase,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub error: Option<String>,
    pub provider_turn_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ThreadStatus {
    pub thread_id: String,
    pub title: String,
    pub provider: ProviderId,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub permission_mode: PermissionMode,
    pub session_mode: SessionMode,
    pub goal: Option<String>,
    pub goal_paused: bool,
    pub latest_turn: Option<TurnStatus>,
    pub turn_count: usize,
    pub queued: Vec<String>,
    pub usage: Option<String>,
}

fn preview(text: &str) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    let mut out: String = first.chars().take(80).collect();
    if first.chars().count() > 80 {
        out.push('…');
    }
    out
}

pub fn to_json(status: &ThreadStatus) -> serde_json::Value {
    let open_turn = status
        .latest_turn
        .as_ref()
        .map(|turn| {
            !turn.phase.is_terminal()
        })
        .unwrap_or(false);
    let stale_hint = if open_turn { Some(STALE_HINT) } else { None };
    let latest_turn = status.latest_turn.as_ref().map(|turn| {
        serde_json::json!({
            "turn": turn.turn,
            "phase": turn.phase.as_str(),
            "started_at": turn.started_at,
            "ended_at": turn.ended_at,
            "error": turn.error,
            "provider_turn_id": turn.provider_turn_id,
        })
    });
    serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "thread_id": status.thread_id,
        "title": status.title,
        "provider": status.provider.as_str(),
        "model": status.model,
        "reasoning_effort": status.reasoning_effort,
        "permission_mode": status.permission_mode.as_str(),
        "session_mode": status.session_mode.as_str(),
        "goal": status.goal,
        "goal_paused": status.goal_paused,
        "latest_turn": latest_turn,
        "turn_count": status.turn_count,
        "queue": status.queued,
        "queue_count": status.queued.len(),
        "usage": status.usage,
        "open_turn": open_turn,
        "stale_hint": stale_hint,
    })
}

pub fn render(status: &ThreadStatus) -> String {
    let mut lines = Vec::new();
    lines.push(format!("thread: {}  {}", status.thread_id, status.title));
    lines.push(format!("provider: {}/{}", status.provider, status.model));
    if let Some(effort) = &status.reasoning_effort {
        lines.push(format!("effort: {effort}"));
    }
    lines.push(format!("session: {}", status.session_mode));
    lines.push(format!("mode: {}", status.permission_mode));
    match &status.goal {
        Some(goal) => {
            let suffix = if status.goal_paused { " (paused)" } else { "" };
            lines.push(format!("goal: {goal}{suffix}"));
        }
        None => lines.push("goal: none".to_string()),
    }
    lines.push(format!("turns: {}", status.turn_count));
    match &status.latest_turn {
        Some(turn) => {
            let ended = turn.ended_at.as_deref().unwrap_or("running");
            lines.push(format!(
                "turn: {} {} (started {}, ended {})",
                turn.turn,
                turn.phase.as_str(),
                turn.started_at,
                ended
            ));
            if let Some(provider_turn_id) = &turn.provider_turn_id {
                lines.push(format!("provider turn: {provider_turn_id}"));
            }
            if let Some(error) = &turn.error {
                lines.push(format!("error: {}", preview(error)));
            }
            if !turn.phase.is_terminal() {
                lines.push(
                    "note: this turn is still open; if no baton process is running it is stale and will be marked unverified on the next open"
                        .to_string(),
                );
            }
        }
        None => lines.push("turn: none".to_string()),
    }
    if status.queued.is_empty() {
        lines.push("queue: empty".to_string());
    } else {
        lines.push(format!("queue: {} waiting", status.queued.len()));
        for item in &status.queued {
            lines.push(format!("  - {}", preview(item)));
        }
    }
    if let Some(usage) = &status.usage {
        lines.push(format!("usage: {usage}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> ThreadStatus {
        ThreadStatus {
            thread_id: "thread-1".into(),
            title: "fix login".into(),
            provider: ProviderId::Codex,
            model: "model-1".into(),
            reasoning_effort: Some("high".into()),
            permission_mode: PermissionMode::Plan,
            session_mode: SessionMode::Orchestrate,
            goal: Some("ship the fix".into()),
            goal_paused: true,
            latest_turn: Some(TurnStatus {
                turn: 3,
                phase: TurnPhase::Failed,
                started_at: "2026-11-11T00:00:00Z".into(),
                ended_at: Some("2026-11-11T00:00:05Z".into()),
                error: Some("provider died".into()),
                provider_turn_id: Some("provider-turn-3".into()),
            }),
            turn_count: 3,
            queued: vec!["follow up".into(), "second\nline".into()],
            usage: Some("in 1.0k, out 100, cache 50%".into()),
        }
    }

    #[test]
    fn renders_stable_lines_for_agents() {
        let text = render(&status());
        assert!(text.contains("thread: thread-1  fix login"));
        assert!(text.contains("provider: codex/model-1"));
        assert!(text.contains("effort: high"));
        assert!(text.contains("mode: plan"));
        assert!(text.contains("goal: ship the fix (paused)"));
        assert!(text.contains("turns: 3"));
        assert!(text.contains("turn: 3 failed (started 2026-11-11T00:00:00Z, ended 2026-11-11T00:00:05Z)"));
        assert!(text.contains("provider turn: provider-turn-3"));
        assert!(text.contains("error: provider died"));
        assert!(text.contains("queue: 2 waiting"));
        assert!(text.contains("  - follow up"));
        assert!(text.contains("  - second"));
        assert!(text.contains("usage: in 1.0k, out 100, cache 50%"));
    }

    #[test]
    fn json_exposes_the_same_state() {
        let value = to_json(&status());
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["thread_id"], "thread-1");
        assert_eq!(value["reasoning_effort"], "high");
        assert_eq!(value["permission_mode"], "plan");
        assert_eq!(value["latest_turn"]["phase"], "failed");
        assert_eq!(value["turn_count"], 3);
        assert_eq!(value["queue_count"], 2);
        assert_eq!(value["queue"][0], "follow up");
        assert_eq!(value["open_turn"], false);
        assert_eq!(value["stale_hint"], serde_json::Value::Null);
    }

    #[test]
    fn warns_that_an_open_turn_may_be_stale() {
        let mut value = status();
        value.latest_turn.as_mut().unwrap().phase = TurnPhase::Running;
        value.latest_turn.as_mut().unwrap().ended_at = None;
        let text = render(&value);
        assert!(text.contains("turn: 3 running"));
        assert!(text.contains("still open"));
        assert!(text.contains("marked unverified"));
        let json = to_json(&value);
        assert_eq!(json["open_turn"], true);
        assert!(json["stale_hint"].as_str().unwrap().contains("stale"));
    }

    #[test]
    fn renders_idle_thread_with_empty_queue() {
        let mut value = status();
        value.goal = None;
        value.latest_turn = None;
        value.queued.clear();
        value.usage = None;
        let text = render(&value);
        assert!(text.contains("goal: none"));
        assert!(text.contains("turn: none"));
        assert!(text.contains("queue: empty"));
        assert!(!text.contains("usage:"));
    }

    #[test]
    fn queue_previews_stay_single_line_and_bounded() {
        let mut value = status();
        value.queued = vec![format!("{}\nsecond", "x".repeat(120))];
        let text = render(&value);
        let queue_line = text.lines().find(|line| line.starts_with("  - ")).unwrap();
        assert!(queue_line.ends_with('…'));
        assert!(queue_line.chars().count() < 90);
        assert!(!queue_line.contains("second"));
    }
}
