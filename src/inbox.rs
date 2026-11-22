//! Read-only aggregation of threads that need a person.

use anyhow::Result;

use crate::store::BatonStore;
use crate::types::TurnPhase;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxItem {
    pub thread_id: String,
    pub title: String,
    pub status: String,
    pub reasons: Vec<String>,
    pub updated_at: String,
}

/// One row per thread with failed, unverified, interrupted, needs-input, or
/// queued work. The newest thread comes first.
pub fn items(store: &BatonStore) -> Result<Vec<InboxItem>> {
    let mut items = Vec::new();
    for thread in store.list_threads(None)? {
        let turns = store.list_turn_records(&thread.id).unwrap_or_default();
        let last_phase = turns.last().map(|turn| turn.phase);
        let task = store
            .orchestration_task_for_thread(&thread.id)
            .ok()
            .flatten();
        let mut reasons = Vec::new();

        match task.as_ref().map(|task| task.status.as_str()) {
            Some("needs_input") => reasons.push("worker needs input".to_string()),
            Some("failed") => reasons.push("worker failed".to_string()),
            _ => {}
        }
        match last_phase {
            Some(TurnPhase::Failed) => reasons.push("turn failed".to_string()),
            Some(TurnPhase::Unverified) => reasons.push("turn unverified".to_string()),
            Some(TurnPhase::Interrupted) => reasons.push("turn interrupted".to_string()),
            _ => {}
        }
        let queued = store.list_queued_messages(&thread.id).unwrap_or_default();
        if !queued.is_empty() {
            reasons.push(format!("{} queued", queued.len()));
        }
        if reasons.is_empty() {
            continue;
        }

        let status = task
            .map(|task| task.status)
            .or_else(|| last_phase.map(|phase| phase.as_str().to_string()))
            .unwrap_or_else(|| "new".to_string());
        items.push(InboxItem {
            thread_id: thread.id,
            title: thread.title,
            status,
            reasons,
            updated_at: thread.updated_at,
        });
    }
    items.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{BatonStore, NewThread};
    use crate::types::ProviderId;

    fn thread(store: &BatonStore, title: &str) -> crate::store::ThreadRow {
        store
            .create_thread(NewThread {
                title: title.to_string(),
                cwd: "/tmp/inbox-test".to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap()
    }

    #[test]
    fn collects_only_threads_that_need_attention() {
        let store = BatonStore::memory().unwrap();
        let failed = thread(&store, "failed work");
        store
            .start_turn_record(&failed.id, 1, ProviderId::Codex, None)
            .unwrap();
        store
            .finish_turn_record(&failed.id, 1, TurnPhase::Failed, Some("boom"))
            .unwrap();
        store
            .enqueue_message(&failed.id, "queued follow-up")
            .unwrap();

        let clean = thread(&store, "finished work");
        store
            .start_turn_record(&clean.id, 1, ProviderId::Codex, None)
            .unwrap();
        store
            .finish_turn_record(&clean.id, 1, TurnPhase::Completed, None)
            .unwrap();

        let listed = items(&store).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].thread_id, failed.id);
        assert_eq!(listed[0].status, "failed");
        assert_eq!(
            listed[0].reasons,
            vec!["turn failed".to_string(), "1 queued".to_string()]
        );
    }

    #[test]
    fn includes_a_worker_waiting_for_an_answer() {
        let store = BatonStore::memory().unwrap();
        let waiting = thread(&store, "waiting worker");
        store
            .create_orchestration_run(
                "run-inbox",
                "goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
            )
            .unwrap();
        store
            .add_orchestration_task(
                "task-inbox",
                "run-inbox",
                1,
                "waiting task",
                ProviderId::Codex,
                "model",
                "prompt",
                None,
                None,
                None,
                Some(&waiting.id),
                "needs_input",
                Some("BATON_QUESTION: choose"),
                None,
                "2026-11-22T00:00:00Z",
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let listed = items(&store).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, "needs_input");
        assert_eq!(listed[0].reasons, vec!["worker needs input".to_string()]);
    }
}
