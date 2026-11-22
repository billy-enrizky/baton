//! Derived per-turn activity for the TUI, CLI, and MCP.
//!
//! The builder joins durable turn records, messages, and usage rows. It never
//! copies diff bodies and never invents a completed phase for an imported turn.

use crate::store::{MessageRow, ThreadRow, TurnRow, UsageRow};
use crate::types::ProviderId;
use crate::usage::format_token_count;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub message_id: i64,
    pub text: String,
    pub ok: Option<bool>,
    pub source: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityRow {
    pub turn: i64,
    pub provider: ProviderId,
    pub model: Option<String>,
    pub phase: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub has_diff: bool,
    pub diff_message_id: Option<i64>,
    pub usage: Option<ActivityUsage>,
    pub source: String,
}

#[derive(Default)]
struct RowBuilder {
    turn: i64,
    provider: Option<ProviderId>,
    model: Option<String>,
    phase: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    tool_calls: Vec<ToolCall>,
    diff_message_id: Option<i64>,
    usage: Option<ActivityUsage>,
    source: Option<String>,
}

fn tool_ok(text: &str) -> Option<bool> {
    if text.starts_with('✓') {
        Some(true)
    } else if text.starts_with('✗') {
        Some(false)
    } else {
        None
    }
}

fn add_usage(target: &mut Option<ActivityUsage>, usage: &crate::types::TurnUsage) {
    let entry = target.get_or_insert(ActivityUsage {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        cost_usd: None,
    });
    entry.input_tokens += usage.input_tokens;
    entry.output_tokens += usage.output_tokens;
    entry.cache_read_tokens += usage.cache_read_tokens;
    entry.cache_creation_tokens += usage.cache_creation_tokens;
    if let Some(cost) = usage.cost_usd {
        entry.cost_usd = Some(entry.cost_usd.unwrap_or(0.0) + cost);
    }
}

/// Builds ascending activity rows from durable rows. A turn that exists only
/// in messages becomes an `imported` row with its messages' provider/model and
/// timestamps; it never claims a completed turn.
pub fn build(
    thread: &ThreadRow,
    turns: &[TurnRow],
    messages: &[MessageRow],
    usage: &[UsageRow],
) -> Vec<ActivityRow> {
    let mut builders: BTreeMap<i64, RowBuilder> = BTreeMap::new();

    for turn in turns {
        builders.insert(
            turn.turn,
            RowBuilder {
                turn: turn.turn,
                provider: Some(turn.provider),
                model: turn.model.clone(),
                phase: Some(turn.phase.as_str().to_string()),
                started_at: Some(turn.started_at.clone()),
                ended_at: turn.ended_at.clone(),
                source: Some("turn_record".to_string()),
                ..RowBuilder::default()
            },
        );
    }

    for message in messages {
        if message.turn <= 0 {
            continue;
        }
        let entry = builders.entry(message.turn).or_insert_with(|| RowBuilder {
            turn: message.turn,
            provider: message
                .provider
                .as_deref()
                .and_then(ProviderId::parse)
                .or(Some(thread.current_provider)),
            model: message.model.clone().or_else(|| thread.current_model.clone()),
            phase: Some("imported".to_string()),
            started_at: Some(message.created_at.clone()),
            ended_at: Some(message.created_at.clone()),
            source: Some("messages".to_string()),
            ..RowBuilder::default()
        });
        if entry.source.as_deref() == Some("turn_record") {
            entry.source = Some("mixed".to_string());
        }
        if entry.provider.is_none() {
            entry.provider = message.provider.as_deref().and_then(ProviderId::parse);
        }
        if entry.model.is_none() {
            entry.model = message.model.clone();
        }
        if entry.started_at.is_none() {
            entry.started_at = Some(message.created_at.clone());
        }
        entry.ended_at = Some(message.created_at.clone());
        match message.role.as_str() {
            "tool" => entry.tool_calls.push(ToolCall {
                message_id: message.id,
                text: message.text.clone(),
                ok: tool_ok(&message.text),
                source: "legacy_text",
            }),
            "diff" => {
                entry.diff_message_id = Some(message.id);
            }
            _ => {}
        }
    }

    for row in usage {
        if let Some(entry) = builders.get_mut(&row.turn) {
            add_usage(&mut entry.usage, &row.usage);
        }
    }

    builders
        .into_values()
        .map(|builder| ActivityRow {
            turn: builder.turn,
            provider: builder.provider.unwrap_or(thread.current_provider),
            model: builder.model.or_else(|| thread.current_model.clone()),
            phase: builder.phase.unwrap_or_else(|| "unknown".to_string()),
            started_at: builder.started_at,
            ended_at: builder.ended_at,
            tool_calls: builder.tool_calls,
            has_diff: builder.diff_message_id.is_some(),
            diff_message_id: builder.diff_message_id,
            usage: builder.usage,
            source: builder.source.unwrap_or_else(|| "messages".to_string()),
        })
        .collect()
}

/// One-line summary used by the TUI copy action and the plain CLI.
pub fn summary(row: &ActivityRow) -> String {
    let count = row.tool_calls.len();
    let tool_calls = if count == 1 {
        "1 tool call".to_string()
    } else {
        format!("{count} tool calls")
    };
    let diff = if row.has_diff {
        "1 diff".to_string()
    } else {
        "no diff".to_string()
    };
    let usage = match &row.usage {
        Some(usage) => {
            let input = usage.input_tokens + usage.cache_read_tokens + usage.cache_creation_tokens;
            format!(
                "{} in / {} out",
                format_token_count(input),
                format_token_count(usage.output_tokens)
            )
        }
        None => "usage n/a".to_string(),
    };
    format!(
        "turn {} · {} · {} · {tool_calls} · {diff} · {usage}",
        row.turn,
        row.provider.as_str(),
        row.phase
    )
}

/// Machine contract. Provider and phase are lowercase because the runtime
/// enums do not serialize in the wire shape.
pub fn to_json(thread_id: &str, rows: &[ActivityRow]) -> Value {
    let turns: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "turn": row.turn,
                "provider": row.provider.as_str(),
                "model": row.model,
                "phase": row.phase,
                "started_at": row.started_at,
                "ended_at": row.ended_at,
                "source": row.source,
                "tool_calls": row.tool_calls.iter().map(|call| json!({
                    "message_id": call.message_id,
                    "text": call.text,
                    "ok": call.ok,
                    "source": call.source,
                })).collect::<Vec<_>>(),
                "has_diff": row.has_diff,
                "diff_message_id": row.diff_message_id,
                "usage": row.usage.as_ref().map(|usage| json!({
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_read_tokens": usage.cache_read_tokens,
                    "cache_creation_tokens": usage.cache_creation_tokens,
                    "cost_usd": usage.cost_usd,
                })),
            })
        })
        .collect();
    json!({
        "schema_version": 1,
        "thread_id": thread_id,
        "count": rows.len(),
        "turns": turns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TurnPhase, TurnUsage};

    fn thread() -> ThreadRow {
        ThreadRow {
            id: "thread-1".into(),
            title: "activity".into(),
            cwd: "/project".into(),
            current_provider: ProviderId::Codex,
            current_model: Some("model".into()),
            reasoning_effort: None,
            permission_mode: crate::types::PermissionMode::Default,
            goal: None,
            goal_paused: false,
            session_mode: crate::types::SessionMode::Single,
            pinned: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn message(id: i64, turn: i64, role: &str, text: &str) -> MessageRow {
        MessageRow {
            id,
            thread_id: "thread-1".into(),
            turn,
            role: role.into(),
            provider: Some("codex".into()),
            model: Some("model".into()),
            text: text.into(),
            created_at: format!("2026-01-01T00:00:0{id}Z"),
        }
    }

    fn turn(turn: i64, phase: TurnPhase) -> TurnRow {
        TurnRow {
            thread_id: "thread-1".into(),
            turn,
            provider: ProviderId::Codex,
            model: Some("model".into()),
            phase,
            error: None,
            provider_turn_id: None,
            started_at: format!("2026-01-01T00:00:0{turn}Z"),
            ended_at: Some(format!("2026-01-01T00:01:0{turn}Z")),
        }
    }

    #[test]
    fn joins_tools_diff_and_usage_per_turn() {
        let messages = vec![
            message(1, 1, "user", "hello"),
            message(2, 1, "tool", "✓ shell cargo test"),
            message(3, 1, "tool", "✗ shell cargo build"),
            message(4, 1, "diff", "diff --git a/a b/a"),
            message(5, 1, "assistant", "done"),
        ];
        let usage = vec![UsageRow {
            thread_id: "thread-1".into(),
            turn: 1,
            provider: ProviderId::Codex,
            model: Some("model".into()),
            usage: TurnUsage {
                input_tokens: 100,
                output_tokens: 20,
                cache_read_tokens: 50,
                cache_creation_tokens: 10,
                cost_usd: None,
            },
            created_at: String::new(),
        }];
        let rows = build(&thread(), &[turn(1, TurnPhase::Completed)], &messages, &usage);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tool_calls.len(), 2);
        assert_eq!(rows[0].tool_calls[0].ok, Some(true));
        assert_eq!(rows[0].tool_calls[1].ok, Some(false));
        assert!(rows[0].has_diff);
        assert_eq!(rows[0].diff_message_id, Some(4));
        assert_eq!(rows[0].usage.as_ref().unwrap().input_tokens, 100);
        assert_eq!(rows[0].source, "mixed");
        let summary = summary(&rows[0]);
        assert!(summary.contains("2 tool calls · 1 diff · 160 in / 20 out"), "{summary}");
    }

    #[test]
    fn message_only_turns_are_imported_and_not_completed() {
        let messages = vec![
            message(1, 1, "user", "imported question"),
            message(2, 1, "assistant", "imported answer"),
            message(3, 2, "user", "another question"),
        ];
        let rows = build(&thread(), &[], &messages, &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].phase, "imported");
        assert_eq!(rows[0].source, "messages");
        assert_eq!(rows[0].model.as_deref(), Some("model"));
        assert!(rows[0].tool_calls.is_empty());
        assert!(!rows[0].has_diff);
        assert!(summary(&rows[0]).contains("0 tool calls · no diff · usage n/a"));
    }

    #[test]
    fn json_uses_lowercase_provider_and_phase_and_nulls() {
        let rows = build(&thread(), &[turn(1, TurnPhase::Running)], &[], &[]);
        let value = to_json("thread-1", &rows);
        assert_eq!(value["turns"][0]["provider"], "codex");
        assert_eq!(value["turns"][0]["phase"], "running");
        assert!(value["turns"][0]["usage"].is_null());
        assert!(value["turns"][0]["diff_message_id"].is_null());
    }
}
