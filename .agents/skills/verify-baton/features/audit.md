# Audit log

`baton audit list [--limit N] [--json]` shows the append-only audit log, newest first. The default limit is 50 and the accepted range is 1 to 1000.

Each event carries:

- actor: `host_terminal` for the pairing command, `local_cli` for `grant renew` and `grant revoke`, `host_engine` for accepted turns, `local_cli` for orchestration start and settle, `tui` for approval answers, question answers, git actions, settings writes, and the thread-worktree run, `mcp_stdio` for MCP fork and queue clear
- environment: the installation id
- target: what changed, such as a grant id, thread id, provider request id, commit sha, or pull request number
- optional thread id
- action: `grant.mint`, `grant.renew`, `grant.revoke`, `turn.submit`, `approval.decide`, `question.answer`, `git.commit`, `git.push`, `git.pr`, `git.review`, `git.merge`, `settings.write`, `orchestration.start`, `orchestration.settle`, `queue.clear`, or `thread.fork`
- scope: `admin` for grants, MCP fork, and queue clear; `operate` for turn submission; `approve` for approval and question answers; `git` for repository actions; `settings` for global settings writes; `orchestrate` for run start; `admin` for run settlement
- confirmation: `host_terminal`, `local_cli`, `host_session`, or `stdio_spawner`
- payload digest: SHA-256 of the canonical JSON payload, without the secret
- result: `ok`
- created time
- retention: `durable`

The `audit_events` table rejects UPDATE and DELETE with the `audit_events_append_only` trigger. `insert_grant_with_audit`, `renew_grant_row_with_audit`, `revoke_grant_row_with_audit`, `submit_turn_with_audit`, `clear_queued_messages_with_audit`, and `fork_thread_through_turn_with_audit` commit the mutation and its event in one transaction. The renewal event payload digests the grant id, the previous expiry, and the requested expiry. The turn event digests the thread id, provider, model, and queue-drain flag, so prompt text never reaches the log. The approval event digests the request id, kind, decision, and chosen label, and the summary text is deliberately excluded. The question event digests the request id, prompt count, and chosen option labels, counts free-text answers, and never commits their text. Git events digest identifiers such as the commit sha, branch, remote, pull request number, review action, and merge method. Settings events target the config key and digest the key and the new value. Orchestration events target the run id and digest the coordinator, task count, verdict, and force flag. The fork event thread id is the new thread id; its payload digest covers the source, title, cutoff, and native flag.

## Proof

- `src/audit.rs` `payload_digest_ignores_object_key_order`, `event_carries_the_action_scope_and_retention`
- `src/store.rs` `a_version_five_store_gains_append_only_audit_events`, `a_version_six_store_gains_the_audit_target`, `audit_events_are_newest_first_and_append_only`, `a_grant_and_its_audit_event_commit_together`, `a_renewal_and_its_audit_event_commit_together`, `a_revoke_and_its_audit_event_commit_together`
- `src/grants.rs` `mint_records_a_host_terminal_audit_event`, `renew_extends_only_forward_and_keeps_the_credential_working`, `revoke_records_a_local_cli_audit_event`
- `src/runtime.rs` `runtime::tests::a_completed_turn_is_recorded` asserts one `turn.submit` row per accepted engine turn
- `src/tui/mod.rs` `approvals_stack_over_pickers_without_losing_them` asserts one row per answer with distinct digests, `an_approval_that_cannot_be_recorded_is_denied` proves the denial fallback, and `approval_decision_names_cover_every_choice` fixes the decision names
- `src/tui/mod.rs` `question_picker_answers_each_question_in_order` asserts one row per request, `question_payload_records_labels_and_never_custom_text` proves typed text stays out, and `a_question_that_cannot_be_recorded_sends_no_answers` proves the empty-answer fallback
- `src/tui/mod.rs` `git_action_rows_name_the_action_and_the_thread`, `a_commit_is_recorded_after_it_lands` on a real temporary repository, and `a_commit_that_cannot_be_recorded_still_lands_and_says_so`
- `src/tui/mod.rs` `settings_picker_writes_global_values_and_preserves_other_config` asserts both settings rows with their keys, and `a_setting_that_cannot_be_recorded_still_saves_and_says_so` proves the notice
- `src/store.rs` `a_run_and_its_audit_event_commit_together` and `a_settlement_and_its_audit_event_commit_together` prove the transaction rollback, and `src/orchestrate.rs` `dirty_source_snapshot_reaches_workers_without_entering_the_worker_commit` asserts the start row on a real run
- `.agents/skills/verify-baton/scripts/approval_probe.py` drives the real TUI through a fake ACP approval and two questions, then asserts one `approval.decide` row and two `question.answer` rows with their actor, scope, and thread ids, plus a real installation id on the approval row
- `tests/audit_cli.rs::audit_list_reports_grant_events_and_respects_the_limit`
- `mcp::tests::messages_queue_and_diff_are_readable` and `mcp::tests::fork_creates_a_branch_through_a_completed_turn` for MCP audit rows
- `store::tests::a_turn_and_its_audit_event_commit_together`, `store::tests::a_queue_clear_and_its_audit_event_commit_together`, and `store::tests::a_fork_and_its_audit_event_commit_together`
- `local_docs/verification/audit-log-state-machine`: strictly increasing ids, a mutation only with an audit event, and a read cursor that never regresses
- `local_docs/verification/grant-renew-state-machine`: a renewal only from live, never shortening, exactly one event, and a refusal that writes none

## Not yet covered

Push, pull request, review, and merge rows share one helper but are exercised by the payload test rather than a live remote. Automatic run finish and abandonment, MCP mutations beyond fork and queue clear, and changes made outside the TUI or CLI do not write audit rows yet. Settings writes from the CLI or a hand-edited config file are not audited. Approval and question rows are written by the TUI, so an answer that arrives without a TUI is not covered. The engine records `unclaimed` as the environment when no installation id exists yet, which only happens on an unclaimed store. The MCP stdio server records its spawner as `stdio_spawner` with `admin` scope, but HTTP and remote scope enforcement remain.
