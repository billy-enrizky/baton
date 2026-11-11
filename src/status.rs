//! Stable, line-oriented thread status for humans and agents.

use crate::types::{PermissionMode, ProviderId, TurnPhase};

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
    pub permission_mode: PermissionMode,
    pub goal: Option<String>,
    pub goal_paused: bool,
    pub latest_turn: Option<TurnStatus>,
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

pub fn render(status: &ThreadStatus) -> String {
    let mut lines = Vec::new();
    lines.push(format!("thread: {}  {}", status.thread_id, status.title));
    lines.push(format!("provider: {}/{}", status.provider, status.model));
    lines.push(format!("mode: {}", status.permission_mode));
    match &status.goal {
        Some(goal) => {
            let suffix = if status.goal_paused { " (paused)" } else { "" };
            lines.push(format!("goal: {goal}{suffix}"));
        }
        None => lines.push("goal: none".to_string()),
    }
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
                lines.push(format!("error: {error}"));
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
            permission_mode: PermissionMode::Plan,
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
            queued: vec!["follow up".into(), "second\nline".into()],
            usage: Some("in 1.0k, out 100, cache 50%".into()),
        }
    }

    #[test]
    fn renders_stable_lines_for_agents() {
        let text = render(&status());
        assert!(text.contains("thread: thread-1  fix login"));
        assert!(text.contains("provider: codex/model-1"));
        assert!(text.contains("mode: plan"));
        assert!(text.contains("goal: ship the fix (paused)"));
        assert!(text.contains("turn: 3 failed (started 2026-11-11T00:00:00Z, ended 2026-11-11T00:00:05Z)"));
        assert!(text.contains("provider turn: provider-turn-3"));
        assert!(text.contains("error: provider died"));
        assert!(text.contains("queue: 2 waiting"));
        assert!(text.contains("  - follow up"));
        assert!(text.contains("  - second"));
        assert!(text.contains("usage: in 1.0k, out 100, cache 50%"));
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
