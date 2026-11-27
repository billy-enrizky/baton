//! Append-only audit events for privileged mutations.

use anyhow::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::store::{AuditEventRow, BatonStore, NewAuditEvent};
use crate::types::{QuestionAnswer, QuestionPrompt};

pub const HOST_TERMINAL_ACTOR: &str = "host_terminal";
pub const LOCAL_CLI_ACTOR: &str = "local_cli";
pub const HOST_TERMINAL_CONFIRMATION: &str = "host_terminal";
pub const LOCAL_CLI_CONFIRMATION: &str = "local_cli";
pub const MCP_STDIO_ACTOR: &str = "mcp_stdio";
pub const MCP_STDIO_CONFIRMATION: &str = "stdio_spawner";
pub const HOST_ENGINE_ACTOR: &str = "host_engine";
pub const TUI_ACTOR: &str = "tui";
pub const HOST_SESSION_CONFIRMATION: &str = "host_session";
pub const ADMIN_SCOPE: &str = "admin";
pub const OPERATE_SCOPE: &str = "operate";
pub const APPROVE_SCOPE: &str = "approve";
pub const GIT_SCOPE: &str = "git";
pub const SETTINGS_SCOPE: &str = "settings";
pub const SOCKET_GRANT_CONFIRMATION: &str = "socket_grant";
pub const ORCHESTRATE_SCOPE: &str = "orchestrate";
pub const DURABLE_RETENTION: &str = "durable";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    GrantMint,
    GrantRenew,
    GrantRevoke,
    TurnSubmit,
    ApprovalDecide,
    QuestionAnswer,
    GitCommit,
    GitPush,
    GitPr,
    GitReview,
    GitMerge,
    SettingsWrite,
    OrchestrationStart,
    OrchestrationSettle,
    MemoryAdd,
    MemoryRemove,
    MemoryClear,
    TurnSubmitRequest,
    QueueClear,
    ThreadFork,
}

impl AuditAction {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditAction::GrantMint => "grant.mint",
            AuditAction::GrantRenew => "grant.renew",
            AuditAction::GrantRevoke => "grant.revoke",
            AuditAction::TurnSubmit => "turn.submit",
            AuditAction::ApprovalDecide => "approval.decide",
            AuditAction::QuestionAnswer => "question.answer",
            AuditAction::GitCommit => "git.commit",
            AuditAction::GitPush => "git.push",
            AuditAction::GitPr => "git.pr",
            AuditAction::GitReview => "git.review",
            AuditAction::GitMerge => "git.merge",
            AuditAction::SettingsWrite => "settings.write",
            AuditAction::OrchestrationStart => "orchestration.start",
            AuditAction::OrchestrationSettle => "orchestration.settle",
            AuditAction::MemoryAdd => "memory.add",
            AuditAction::MemoryRemove => "memory.remove",
            AuditAction::MemoryClear => "memory.clear",
            AuditAction::TurnSubmitRequest => "turn.submit_request",
            AuditAction::QueueClear => "queue.clear",
            AuditAction::ThreadFork => "thread.fork",
        }
    }

    /// The scope is a property of the action, so a row cannot claim one scope
    /// while recording an action that belongs to another.
    pub fn scope(self) -> &'static str {
        match self {
            AuditAction::GrantMint
            | AuditAction::GrantRenew
            | AuditAction::GrantRevoke
            | AuditAction::QueueClear
            | AuditAction::ThreadFork => ADMIN_SCOPE,
            AuditAction::TurnSubmit => OPERATE_SCOPE,
            AuditAction::ApprovalDecide | AuditAction::QuestionAnswer => APPROVE_SCOPE,
            AuditAction::GitCommit
            | AuditAction::GitPush
            | AuditAction::GitPr
            | AuditAction::GitReview
            | AuditAction::GitMerge => GIT_SCOPE,
            AuditAction::SettingsWrite => SETTINGS_SCOPE,
            AuditAction::OrchestrationStart => ORCHESTRATE_SCOPE,
            AuditAction::OrchestrationSettle => ADMIN_SCOPE,
            AuditAction::MemoryAdd | AuditAction::MemoryRemove | AuditAction::MemoryClear => {
                SETTINGS_SCOPE
            }
            AuditAction::TurnSubmitRequest => OPERATE_SCOPE,
        }
    }
}

/// The confirmation follows the actor: the local human typed it at the host,
/// the engine or TUI acted inside the host session, and MCP acted through its
/// spawner.
pub fn confirmation_for(actor: &str) -> &'static str {
    match actor {
        HOST_TERMINAL_ACTOR => HOST_TERMINAL_CONFIRMATION,
        LOCAL_CLI_ACTOR => LOCAL_CLI_CONFIRMATION,
        MCP_STDIO_ACTOR => MCP_STDIO_CONFIRMATION,
        _ => HOST_SESSION_CONFIRMATION,
    }
}

pub fn payload_digest(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn event(
    actor: &str,
    environment: &str,
    target: Option<&str>,
    thread_id: Option<&str>,
    action: AuditAction,
    payload: &Value,
) -> Result<NewAuditEvent> {
    Ok(NewAuditEvent {
        actor: actor.to_string(),
        environment: environment.to_string(),
        target: target.map(str::to_string),
        thread_id: thread_id.map(str::to_string),
        action: action.as_str().to_string(),
        scope: action.scope().to_string(),
        confirmation: confirmation_for(actor).to_string(),
        payload_digest: payload_digest(payload)?,
        result: "ok".to_string(),
        retention: DURABLE_RETENTION.to_string(),
    })
}

/// A request made by a paired client. The actor is the grant id, and the
/// confirmation names the socket grant rather than the host session.
pub fn grant_event(
    actor: &str,
    environment: &str,
    target: Option<&str>,
    thread_id: Option<&str>,
    action: AuditAction,
    payload: &Value,
) -> Result<NewAuditEvent> {
    let mut event = event(actor, environment, target, thread_id, action, payload)?;
    event.confirmation = SOCKET_GRANT_CONFIRMATION.to_string();
    Ok(event)
}

/// The prompt count and option labels an answer touches. Free-text answers are
/// counted as custom and never written, so a secret typed by the user stays out
/// of the durable log.
pub fn question_answer_payload(
    request_id: &str,
    questions: &[QuestionPrompt],
    answers: &[QuestionAnswer],
) -> Value {
    let answers = answers
        .iter()
        .map(|answer| {
            let chosen: Vec<&str> = questions
                .iter()
                .find(|prompt| prompt.id == answer.id)
                .map(|prompt| {
                    answer
                        .answers
                        .iter()
                        .filter(|value| prompt.options.iter().any(|option| &option.label == *value))
                        .map(String::as_str)
                        .collect()
                })
                .unwrap_or_default();
            serde_json::json!({
                "prompt_id": answer.id,
                "answers": answer.answers.len(),
                "options": chosen,
                "custom": answer.answers.len() > chosen.len(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "request_id": request_id,
        "prompts": questions.len(),
        "answers": answers,
    })
}

pub fn list(store: &BatonStore, limit: usize) -> Result<Vec<AuditEventRow>> {
    store.list_audit_events(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_digest_ignores_object_key_order() {
        let first = serde_json::json!({"b": 2, "a": 1});
        let second = serde_json::json!({"a": 1, "b": 2});
        let digest = payload_digest(&first).unwrap();
        assert_eq!(digest, payload_digest(&second).unwrap());
        assert_eq!(digest.len(), 64);
        assert!(digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
    }

    #[test]
    fn question_payload_keeps_options_and_counts_custom_text() {
        let questions = vec![
            QuestionPrompt {
                id: "language".into(),
                header: "Language".into(),
                question: "Which language?".into(),
                options: vec![
                    crate::types::QuestionOption {
                        label: "Rust".into(),
                        description: String::new(),
                    },
                    crate::types::QuestionOption {
                        label: "Go".into(),
                        description: String::new(),
                    },
                ],
                allow_custom: false,
                multi_select: false,
                secret: false,
            },
            QuestionPrompt {
                id: "note".into(),
                header: "Note".into(),
                question: "Anything else?".into(),
                options: Vec::new(),
                allow_custom: true,
                multi_select: false,
                secret: true,
            },
        ];
        let answers = vec![
            QuestionAnswer {
                id: "language".into(),
                answers: vec!["Rust".into()],
            },
            QuestionAnswer {
                id: "note".into(),
                answers: vec!["a secret".into()],
            },
        ];
        let payload = question_answer_payload("req-1", &questions, &answers);
        assert_eq!(payload["request_id"], "req-1");
        assert_eq!(payload["prompts"], 2);
        assert_eq!(payload["answers"][0]["options"][0], "Rust");
        assert_eq!(payload["answers"][0]["custom"], false);
        assert_eq!(
            payload["answers"][1]["options"].as_array().unwrap().len(),
            0
        );
        assert_eq!(payload["answers"][1]["custom"], true);
        assert!(!payload.to_string().contains("a secret"));
    }

    #[test]
    fn every_action_maps_to_its_scope() {
        assert_eq!(AuditAction::GrantMint.scope(), ADMIN_SCOPE);
        assert_eq!(AuditAction::GrantRenew.scope(), ADMIN_SCOPE);
        assert_eq!(AuditAction::GrantRevoke.scope(), ADMIN_SCOPE);
        assert_eq!(AuditAction::QueueClear.scope(), ADMIN_SCOPE);
        assert_eq!(AuditAction::ThreadFork.scope(), ADMIN_SCOPE);
        assert_eq!(AuditAction::TurnSubmit.scope(), OPERATE_SCOPE);
        assert_eq!(AuditAction::ApprovalDecide.scope(), APPROVE_SCOPE);
        assert_eq!(AuditAction::QuestionAnswer.scope(), APPROVE_SCOPE);
        assert_eq!(AuditAction::GitCommit.scope(), GIT_SCOPE);
        assert_eq!(AuditAction::GitPush.scope(), GIT_SCOPE);
        assert_eq!(AuditAction::GitPr.scope(), GIT_SCOPE);
        assert_eq!(AuditAction::GitReview.scope(), GIT_SCOPE);
        assert_eq!(AuditAction::GitMerge.scope(), GIT_SCOPE);
        assert_eq!(AuditAction::SettingsWrite.scope(), SETTINGS_SCOPE);
        assert_eq!(AuditAction::OrchestrationStart.scope(), ORCHESTRATE_SCOPE);
        assert_eq!(AuditAction::OrchestrationSettle.scope(), ADMIN_SCOPE);
        assert_eq!(AuditAction::MemoryAdd.scope(), SETTINGS_SCOPE);
        assert_eq!(AuditAction::MemoryRemove.scope(), SETTINGS_SCOPE);
        assert_eq!(AuditAction::MemoryClear.scope(), SETTINGS_SCOPE);
        assert_eq!(AuditAction::TurnSubmitRequest.scope(), OPERATE_SCOPE);
    }

    #[test]
    fn every_actor_maps_to_its_confirmation() {
        assert_eq!(
            confirmation_for(HOST_TERMINAL_ACTOR),
            HOST_TERMINAL_CONFIRMATION
        );
        assert_eq!(confirmation_for(LOCAL_CLI_ACTOR), LOCAL_CLI_CONFIRMATION);
        assert_eq!(confirmation_for(MCP_STDIO_ACTOR), MCP_STDIO_CONFIRMATION);
        assert_eq!(
            confirmation_for(HOST_ENGINE_ACTOR),
            HOST_SESSION_CONFIRMATION
        );
        assert_eq!(confirmation_for(TUI_ACTOR), HOST_SESSION_CONFIRMATION);
    }

    #[test]
    fn event_carries_the_action_scope_and_retention() {
        let event = event(
            HOST_TERMINAL_ACTOR,
            "installation",
            Some("grant"),
            Some("thread"),
            AuditAction::GrantMint,
            &serde_json::json!({"grant_id": "grant"}),
        )
        .unwrap();
        assert_eq!(event.actor, HOST_TERMINAL_ACTOR);
        assert_eq!(event.environment, "installation");
        assert_eq!(event.target.as_deref(), Some("grant"));
        assert_eq!(event.thread_id.as_deref(), Some("thread"));
        assert_eq!(event.action, "grant.mint");
        assert_eq!(event.scope, "admin");
        assert_eq!(event.confirmation, "host_terminal");
        assert_eq!(event.result, "ok");
        assert_eq!(event.retention, DURABLE_RETENTION);
        assert_eq!(event.payload_digest.len(), 64);
        assert_eq!(AuditAction::GrantRenew.as_str(), "grant.renew");
        assert_eq!(AuditAction::TurnSubmit.as_str(), "turn.submit");
        assert_eq!(AuditAction::ApprovalDecide.as_str(), "approval.decide");
        assert_eq!(AuditAction::QuestionAnswer.as_str(), "question.answer");
        assert_eq!(AuditAction::GitCommit.as_str(), "git.commit");
        assert_eq!(AuditAction::GitMerge.as_str(), "git.merge");
        assert_eq!(AuditAction::SettingsWrite.as_str(), "settings.write");
        assert_eq!(
            AuditAction::OrchestrationStart.as_str(),
            "orchestration.start"
        );
        assert_eq!(
            AuditAction::OrchestrationSettle.as_str(),
            "orchestration.settle"
        );
        assert_eq!(AuditAction::MemoryAdd.as_str(), "memory.add");
        assert_eq!(
            AuditAction::TurnSubmitRequest.as_str(),
            "turn.submit_request"
        );
        assert_eq!(AuditAction::QueueClear.as_str(), "queue.clear");
        assert_eq!(AuditAction::ThreadFork.as_str(), "thread.fork");
    }
}
