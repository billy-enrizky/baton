//! Append-only audit events for privileged mutations.

use anyhow::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::store::{AuditEventRow, BatonStore, NewAuditEvent};

pub const HOST_TERMINAL_ACTOR: &str = "host_terminal";
pub const LOCAL_CLI_ACTOR: &str = "local_cli";
pub const HOST_TERMINAL_CONFIRMATION: &str = "host_terminal";
pub const LOCAL_CLI_CONFIRMATION: &str = "local_cli";
pub const MCP_STDIO_ACTOR: &str = "mcp_stdio";
pub const MCP_STDIO_CONFIRMATION: &str = "stdio_spawner";
pub const ADMIN_SCOPE: &str = "admin";
pub const DURABLE_RETENTION: &str = "durable";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    GrantMint,
    GrantRevoke,
    QueueClear,
    ThreadFork,
}

impl AuditAction {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditAction::GrantMint => "grant.mint",
            AuditAction::GrantRevoke => "grant.revoke",
            AuditAction::QueueClear => "queue.clear",
            AuditAction::ThreadFork => "thread.fork",
        }
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
    thread_id: Option<&str>,
    action: AuditAction,
    scope: &str,
    confirmation: &str,
    payload: &Value,
) -> Result<NewAuditEvent> {
    Ok(NewAuditEvent {
        actor: actor.to_string(),
        environment: environment.to_string(),
        thread_id: thread_id.map(str::to_string),
        action: action.as_str().to_string(),
        scope: scope.to_string(),
        confirmation: confirmation.to_string(),
        payload_digest: payload_digest(payload)?,
        result: "ok".to_string(),
        retention: DURABLE_RETENTION.to_string(),
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
    fn event_carries_the_action_scope_and_retention() {
        let event = event(
            HOST_TERMINAL_ACTOR,
            "installation",
            Some("thread"),
            AuditAction::GrantMint,
            ADMIN_SCOPE,
            HOST_TERMINAL_CONFIRMATION,
            &serde_json::json!({"grant_id": "grant"}),
        )
        .unwrap();
        assert_eq!(event.actor, HOST_TERMINAL_ACTOR);
        assert_eq!(event.environment, "installation");
        assert_eq!(event.thread_id.as_deref(), Some("thread"));
        assert_eq!(event.action, "grant.mint");
        assert_eq!(event.scope, "admin");
        assert_eq!(event.confirmation, "host_terminal");
        assert_eq!(event.result, "ok");
        assert_eq!(event.retention, DURABLE_RETENTION);
        assert_eq!(event.payload_digest.len(), 64);
        assert_eq!(AuditAction::QueueClear.as_str(), "queue.clear");
        assert_eq!(AuditAction::ThreadFork.as_str(), "thread.fork");
    }
}
