//! JSONL projection and shared state for `baton exec --stream-json`.

use crate::adapters::TurnEvent;
use crate::status::SCHEMA_VERSION;
use serde_json::{json, Value};

#[derive(Debug, Default)]
pub struct TurnStreamState {
    pub streamed_text: String,
    pub final_text: String,
    pub saw_output: bool,
}

/// Applies one turn event to the stream state. Both the live loop and the
/// post-terminal drain use this, so scheduling cannot change the result text.
pub fn apply_turn_event(state: &mut TurnStreamState, event: &TurnEvent) {
    match event {
        TurnEvent::TextDelta { text } if !text.is_empty() => {
            state.streamed_text.push_str(text);
            state.saw_output = true;
        }
        TurnEvent::AssistantText { text } if !text.is_empty() => {
            state.final_text = text.clone();
            state.saw_output = true;
        }
        TurnEvent::TurnCompleted { final_text, .. } if !final_text.is_empty() => {
            state.final_text = final_text.clone();
            state.saw_output = true;
        }
        _ => {}
    }
}

/// Serializes one live turn event. The exhaustive match keeps a new variant
/// visible until it gets a stream shape.
pub fn turn_event_json(thread_id: &str, event: &TurnEvent) -> Value {
    let mut value = match event {
        TurnEvent::ProviderTurnId { id } => json!({
            "type": "provider_turn_id",
            "id": id,
        }),
        TurnEvent::ProviderSession {
            native_session_id,
            rollout_path,
        } => json!({
            "type": "provider_session",
            "native_session_id": native_session_id,
            "rollout_path": rollout_path,
        }),
        TurnEvent::TextDelta { text } => json!({
            "type": "text_delta",
            "text": text,
        }),
        TurnEvent::AssistantText { text } => json!({
            "type": "assistant_text",
            "text": text,
        }),
        TurnEvent::SubagentStarted { id, name, detail } => json!({
            "type": "subagent_started",
            "id": id,
            "name": name,
            "detail": detail,
        }),
        TurnEvent::SubagentFinished {
            id,
            name,
            status,
            detail,
        } => json!({
            "type": "subagent_finished",
            "id": id,
            "name": name,
            "status": status,
            "detail": detail,
        }),
        TurnEvent::ToolStarted { id, tool, detail } => json!({
            "type": "tool_started",
            "id": id,
            "tool": tool,
            "detail": detail,
        }),
        TurnEvent::ToolCompleted {
            id,
            tool,
            detail,
            exit_code,
        } => json!({
            "type": "tool_completed",
            "id": id,
            "tool": tool,
            "detail": detail,
            "exit_code": exit_code,
        }),
        TurnEvent::ApprovalRequest {
            id,
            kind,
            summary,
            choices,
            ..
        } => json!({
            "type": "approval_request",
            "id": id,
            "kind": kind,
            "summary": summary,
            "choices": choices,
        }),
        TurnEvent::Usage { usage } => json!({
            "type": "usage",
            "usage": usage,
        }),
        TurnEvent::TurnCompleted {
            final_text,
            usage,
            native_session_id,
            ..
        } => json!({
            "type": "turn_completed",
            "text": final_text,
            "usage": usage,
            "native_session_id": native_session_id,
        }),
        TurnEvent::TurnInterrupted { reason } => json!({
            "type": "turn_interrupted",
            "reason": reason,
        }),
        TurnEvent::TurnFailed { error } => json!({
            "type": "turn_failed",
            "error": error,
        }),
    };
    let object = value.as_object_mut().expect("stream events are objects");
    object.insert("schema_version".into(), json!(SCHEMA_VERSION));
    object.insert("thread_id".into(), json!(thread_id));
    value
}

pub struct ExecResult<'a> {
    pub thread_id: &'a str,
    pub effort: Option<&'a str>,
    pub ok: bool,
    pub phase: &'a str,
    pub error: Option<&'a str>,
    pub text: &'a str,
    pub worker_followup: Option<&'a crate::orchestrate::WorkerFollowupOutcome>,
    pub review: Option<&'a crate::orchestrate::FollowupReview>,
}

/// One result shape for normal completion and timeout. Callers add
/// `"type": "result"` for the JSONL stream; `--json` keeps its old shape.
pub fn result_json(result: &ExecResult<'_>) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "thread_id": result.thread_id,
        "effort": result.effort,
        "ok": result.ok,
        "phase": result.phase,
        "error": result.error,
        "text": result.text,
        "worker_followup": result.worker_followup.map(|outcome| json!({
            "run_id": outcome.run_id,
            "task_id": outcome.task_id,
            "status": outcome.status,
            "commit": outcome.commit,
            "question": outcome.question,
            "error": outcome.error,
        })),
        "review": result.review.map(|review| json!({
            "run_id": review.run_id,
            "task_id": review.task_id,
            "status": review.status,
            "text": review.text,
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ApprovalChoice, TurnUsage};

    fn completed(final_text: &str) -> TurnEvent {
        TurnEvent::TurnCompleted {
            final_text: final_text.into(),
            usage: None,
            native_session_id: None,
            rollout_path: None,
        }
    }

    #[test]
    fn drained_deltas_still_count_as_output() {
        let mut state = TurnStreamState::default();
        apply_turn_event(
            &mut state,
            &TurnEvent::TextDelta {
                text: "hello".into(),
            },
        );
        apply_turn_event(&mut state, &completed(""));
        assert_eq!(state.streamed_text, "hello");
        assert!(state.saw_output);
        assert!(state.final_text.is_empty());
    }

    #[test]
    fn assistant_text_is_a_fallback_and_completion_wins() {
        let mut state = TurnStreamState::default();
        apply_turn_event(
            &mut state,
            &TurnEvent::AssistantText {
                text: "draft".into(),
            },
        );
        assert_eq!(state.final_text, "draft");
        assert!(state.saw_output);
        apply_turn_event(&mut state, &completed("final"));
        assert_eq!(state.final_text, "final");
    }

    #[test]
    fn live_events_carry_the_schema_and_thread() {
        let value = turn_event_json(
            "thread-1",
            &TurnEvent::TextDelta {
                text: "hello".into(),
            },
        );
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["thread_id"], "thread-1");
        assert_eq!(value["type"], "text_delta");
        assert_eq!(value["text"], "hello");
    }

    #[test]
    fn tool_and_usage_events_keep_their_fields() {
        let tool = turn_event_json(
            "thread-1",
            &TurnEvent::ToolCompleted {
                id: Some("call-1".into()),
                tool: "shell".into(),
                detail: Some("ls".into()),
                exit_code: Some(0),
            },
        );
        assert_eq!(tool["type"], "tool_completed");
        assert_eq!(tool["id"], "call-1");
        assert_eq!(tool["exit_code"], 0);

        let usage = turn_event_json(
            "thread-1",
            &TurnEvent::Usage {
                usage: TurnUsage {
                    input_tokens: 3,
                    output_tokens: 2,
                    cache_read_tokens: 1,
                    cache_creation_tokens: 4,
                    cost_usd: Some(0.25),
                },
            },
        );
        assert_eq!(usage["usage"]["input_tokens"], 3);
        assert_eq!(usage["usage"]["cache_creation_tokens"], 4);
        assert_eq!(usage["usage"]["cost_usd"], 0.25);
    }

    #[test]
    fn terminal_events_keep_their_verdict_shape() {
        let completed = turn_event_json(
            "thread-1",
            &TurnEvent::TurnCompleted {
                final_text: "done".into(),
                usage: Some(TurnUsage {
                    input_tokens: 3,
                    output_tokens: 2,
                    ..Default::default()
                }),
                native_session_id: Some("native-1".into()),
                rollout_path: None,
            },
        );
        assert_eq!(completed["type"], "turn_completed");
        assert_eq!(completed["text"], "done");
        assert_eq!(completed["native_session_id"], "native-1");

        let failed = turn_event_json(
            "thread-1",
            &TurnEvent::TurnFailed {
                error: "boom".into(),
            },
        );
        assert_eq!(failed["type"], "turn_failed");
        assert_eq!(failed["error"], "boom");
    }

    #[test]
    fn approval_events_do_not_leak_the_responder() {
        let (respond, _receiver) = tokio::sync::oneshot::channel();
        let value = turn_event_json(
            "thread-1",
            &TurnEvent::ApprovalRequest {
                id: "approval-1".into(),
                kind: "command".into(),
                summary: "rm -rf /tmp/x".into(),
                choices: vec![ApprovalChoice {
                    id: "accept".into(),
                    label: "Allow once".into(),
                    kind: "allow_once".into(),
                }],
                respond,
            },
        );
        assert_eq!(value["type"], "approval_request");
        assert_eq!(value["id"], "approval-1");
        assert_eq!(value["choices"][0]["label"], "Allow once");
        assert!(value.get("respond").is_none());
    }

    #[test]
    fn normal_and_timeout_results_share_one_shape() {
        let normal = result_json(&ExecResult {
            thread_id: "thread-1",
            effort: Some("high"),
            ok: true,
            phase: "completed",
            error: None,
            text: "done",
            worker_followup: None,
            review: None,
        });
        let timeout = result_json(&ExecResult {
            thread_id: "thread-1",
            effort: None,
            ok: false,
            phase: "unverified",
            error: Some("exec timed out after 300s"),
            text: "",
            worker_followup: None,
            review: None,
        });
        let normal_keys: Vec<&String> = normal.as_object().unwrap().keys().collect();
        let timeout_keys: Vec<&String> = timeout.as_object().unwrap().keys().collect();
        assert_eq!(normal_keys, timeout_keys);
        assert!(normal["worker_followup"].is_null());
    }

    #[test]
    fn result_json_includes_the_followup_review() {
        let review = crate::orchestrate::FollowupReview {
            run_id: "run-1".into(),
            task_id: "task-1".into(),
            status: "approve".into(),
            text: "APPROVE".into(),
        };
        let value = result_json(&ExecResult {
            thread_id: "thread-1",
            effort: None,
            ok: true,
            phase: "completed",
            error: None,
            text: "done",
            worker_followup: None,
            review: Some(&review),
        });
        assert_eq!(value["review"]["run_id"], "run-1");
        assert_eq!(value["review"]["task_id"], "task-1");
        assert_eq!(value["review"]["status"], "approve");
        assert_eq!(value["review"]["text"], "APPROVE");
    }
}
