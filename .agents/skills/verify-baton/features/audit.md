# Audit log

`baton audit list [--limit N] [--json]` shows the append-only audit log, newest first. The default limit is 50 and the accepted range is 1 to 1000.

Each event carries:

- actor: `host_terminal` for the pairing command, `local_cli` for `grant revoke`
- environment: the installation id
- optional thread id
- action: `grant.mint` or `grant.revoke`
- scope: `admin`
- confirmation: `host_terminal` or `local_cli`
- payload digest: SHA-256 of the canonical JSON payload, without the secret
- result: `ok`
- created time
- retention: `durable`

The `audit_events` table rejects UPDATE and DELETE with the `audit_events_append_only` trigger. `BatonStore::insert_grant_with_audit` and `BatonStore::revoke_grant_row_with_audit` commit the mutation and its event in one transaction.

## Proof

- `src/audit.rs` `payload_digest_ignores_object_key_order`, `event_carries_the_action_scope_and_retention`
- `src/store.rs` `a_version_five_store_gains_append_only_audit_events`, `audit_events_are_newest_first_and_append_only`, `a_grant_and_its_audit_event_commit_together`, `a_revoke_and_its_audit_event_commit_together`
- `src/grants.rs` `mint_records_a_host_terminal_audit_event`, `revoke_records_a_local_cli_audit_event`
- `tests/audit_cli.rs::audit_list_reports_grant_events_and_respects_the_limit`
- `local_docs/verification/audit-log-state-machine`: strictly increasing ids, a mutation only with an audit event, and a read cursor that never regresses

## Not yet covered

Turns, approvals, git actions, orchestration, settings, and MCP mutations do not write audit rows yet. `baton_fork` and queue clear remain outside the scope and audit path.
