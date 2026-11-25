//! Read-only aggregation of threads that need a person.

use anyhow::Result;
use serde::Serialize;

use crate::store::BatonStore;
use crate::types::TurnPhase;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

        if let Some(task) = task.as_ref() {
            match task.status.as_str() {
                "needs_input" => reasons.push("worker needs input".to_string()),
                "failed" => reasons.push("worker failed".to_string()),
                "completed" if task.pr_url.is_none() && task.merged_at.is_none() => {
                    reasons.push("worker complete".to_string());
                }
                _ => {}
            }
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
        let completed = thread(&store, "completed worker");
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
        store
            .add_orchestration_task(
                "task-done",
                "run-inbox",
                2,
                "completed task",
                ProviderId::Codex,
                "model",
                "prompt",
                None,
                None,
                Some("0123456789abcdef"),
                Some(&completed.id),
                "completed",
                Some("done"),
                None,
                "2026-11-22T00:00:01Z",
                Some("2026-11-22T00:00:02Z"),
                None,
                None,
                None,
            )
            .unwrap();

        let listed = items(&store).unwrap();
        assert_eq!(listed.len(), 2);
        let waiting_item = listed
            .iter()
            .find(|item| item.thread_id == waiting.id)
            .expect("waiting worker");
        assert_eq!(waiting_item.status, "needs_input");
        assert_eq!(waiting_item.reasons, vec!["worker needs input".to_string()]);
        let completed_item = listed
            .iter()
            .find(|item| item.thread_id == completed.id)
            .expect("completed worker");
        assert_eq!(completed_item.status, "completed");
        assert_eq!(completed_item.reasons, vec!["worker complete".to_string()]);
    }

    #[test]
    fn worker_questions_extract_the_text_and_skip_unanswerable_rows() {
        let store = BatonStore::memory().unwrap();
        let waiting = thread(&store, "waiting worker");
        let done = thread(&store, "done worker");
        store
            .create_orchestration_run(
                "run-questions",
                "goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
            )
            .unwrap();
        fn add(
            store: &BatonStore,
            id: &str,
            position: i64,
            status: &str,
            thread_id: Option<&str>,
            text: Option<&str>,
        ) {
            store
                .add_orchestration_task(
                    id,
                    "run-questions",
                    position,
                    "task",
                    ProviderId::Codex,
                    "model",
                    "prompt",
                    None,
                    None,
                    None,
                    thread_id,
                    status,
                    text,
                    None,
                    "2026-11-22T00:00:00Z",
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
        }
        add(
            &store,
            "task-waiting",
            1,
            "needs_input",
            Some(&waiting.id),
            Some("working\nBATON_QUESTION: choose a database"),
        );
        add(
            &store,
            "task-complete",
            2,
            "completed",
            Some(&done.id),
            Some("done"),
        );
        add(
            &store,
            "task-orphan",
            3,
            "needs_input",
            None,
            Some("BATON_QUESTION: nobody can answer this"),
        );

        let rows = store.list_worker_questions().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].task_id, "task-waiting");
        assert_eq!(rows[0].run_id, "run-questions");
        assert_eq!(rows[0].thread_id, waiting.id);
        assert_eq!(rows[0].title, "task");
        assert_eq!(rows[0].question, "choose a database");
    }
}
