# Baton parity ledger

Baton is a terminal-first harness for coding agents. This ledger tracks T3 Code and Orca capabilities that have a meaningful terminal form, records what Baton already does with source and test evidence, and names the first implementation slice for each gap. The CI and acceptance mechanics are in [ci.md](ci.md).

## Problem

Baton already has a strong local core: durable threads, provider sessions, steering, checkpoints, worktrees, orchestration, and a TUI. The gap is not one missing screen. Users still need to leave the TUI for attention, task, source-control, and recovery workflows, and a few core promises are incomplete, especially cross-provider handoff after switching back to an older provider.

The product problem is: a terminal user should be able to start, supervise, recover, review, and finish agent work without losing the thread to another app, while keeping the terminal fast and predictable.

## Target user and jobs

| User | Job |
|---|---|
| A developer running several coding agents | Start parallel work, see who needs attention, and finish or discard each task safely |
| An agent running Baton through CLI or MCP | Inspect durable state and perform scoped actions without scraping the TUI |
| A maintainer working across multiple harnesses | Switch providers mid-thread without stale context or lost work |
| A contributor using CI | Get honest, fast feedback and a full verification before merge |

## Goals

| Goal | Measurable acceptance |
|---|---|
| Terminal-native parity | Every row in Release 1 has a working TUI or CLI path, a code path, and a `unit`, `CI`, or `local` proof |
| No memorized slash syntax | Every slash command with finite arguments exposes choices; every free-form command shows usage. PR #86 covers the current set. Every new slash command ships its choices or usage hint in its own slice. |
| Thread continuity | Switching away and back to a provider shows the exact delta, redacts configured secrets, offers no-handoff, and sends only after confirmation. No handoff state machine exists yet; today's builder and its defects are recorded in the handoff row. |
| Recoverable work | A closed TUI reopens without losing visibility of queued, running, or needs-input work. A running turn is reconciled to failed or unverified and can be retried; local-only Baton does not promise background continuation. |
| Fast interaction | Warm startup p50 under 250 ms and p95 under 500 ms; keystroke-to-render p95 under 50 ms; idle RSS under 200 MB. The baseline is recorded before the first optimization PR. |
| Honest CI | PR feedback p50 under 60 seconds and p95 under 120 seconds after the CI optimization phase; every non-PR event selects every gate; a merge candidate is proved by an up-to-date PR run plus the `push` run on `main` |
| Safe automation | Every mutating MCP or remote path has a scope, confirmation, and an audit record with a defined schema |

## Non-goals

| Non-goal | Reason |
|---|---|
| Embedded browser or design mode | A terminal has no pointer-driven embedded browser. Agents can use host browser tools. |
| Native mobile app or desktop app chrome | The terminal client and future attach client own the interaction. |
| Full IDE or embedded file editor | Use `$EDITOR` and external tools. Baton only needs file discovery and context references. |
| Plugin marketplace UI | Theme loading is small. A plugin host is a separate product decision. |
| Multi-repository worktree orchestration in Release 1 | Worktree resolution for several repositories is a separate data model; Release 1 resolves one repository per thread |
| Remote hosts, pairing, and guest links in Release 1 | They need one runtime authority; the ownership ADR is a prerequisite and is not in Release 1 |
| Scheduled or unattended work in Release 1 | A scheduler needs a durable owner and an audited trigger path; `serve` does not exist |
| Multiple forges, OAuth providers, and profiles in Release 1 | One forge (`gh`), environment secrets, and one account per provider cover the terminal use case |
| Voice recognition implementation | OS dictation covers most terminal use. An external STT command hook is enough if requested. |
| Slack, Linear, or GitHub bot UI | A headless trigger daemon may be useful later. Chat UI replication is out of scope. |

## Upstream references and method

| Repository | URL | Checkout | Ref | Commit |
|---|---|---|---|---|
| T3 Code | `https://github.com/pingdotgg/t3code` | `/Users/billy/Documents/t3code` | `origin/main` | `7b21adbe7cd633813eeb8a9e3ec4961ec31f98a3` |
| Orca | `https://github.com/stablyai/orca` | `/Users/billy/Documents/orca` | `origin/main` | `52673d4e065ef6e27d0bda3be288ecb6282a2a42` |
| Baton baseline | `https://github.com/billy-enrizky/baton` | current working copy | `origin/main` | `3e2d57891eec34151e4e1a52a4956d009c2e7b31` |

The upstream working trees can be dirty and behind their refs. Read upstream evidence with `git -C <checkout> show <commit>:<path>`, or verify the path exists with `git -C <checkout> cat-file -e <commit>:<path>`. Evidence paths are prefixed with `T3:` or `Orca:`.

Statuses include the baseline plus PR #86, which is open and unmerged: `https://github.com/billy-enrizky/baton/pull/86`. Rows affected by that PR say so in the evidence or first-slice cell.

## Status rubric

| Status | Required evidence |
|---|---|
| `Equivalent` | A Baton source path and a test, PTY probe, or runtime transcript proves the same user-visible outcome |
| `Partial` | At least one working path exists and the gap is named in `First slice`; the proof surface records whether a test covers it |
| `Missing` | No working code path exists; the evidence cell names the source location that proves the absence, checked against source, Cargo dependencies, and the CLI/TUI command inventory |
| `Unmeasured` | The capability exists, but no recorded benchmark or transcript exists, so no performance or parity claim is made |
| `Design decision` | No implementation is authorized yet; the decision owner and acceptance condition are named |
| `N/A in a TUI` | The upstream value depends on a GUI, native device, or pointer surface with no honest terminal form |

Proof surfaces:

- `unit`: a deterministic Rust test that runs with `cargo test --locked`. Available today.
- `CI`: the same deterministic gate, required by the workflow. Not available until Phase 1C lands, so every row that says `CI` today actually means `unit` and becomes `CI` when the workflow is merged.
- `local`: a real provider, `gh`, SSH, or another paid or credential-bearing probe. It is not a CI gate.
- `local bench`: a recorded benchmark run on the developer host. Not deterministic, so it cannot support `Equivalent` on its own.
- `CI+local`: both a deterministic gate and a real-path check.

`unit` and `CI` are the same underlying test. The distinction records whether the workflow currently enforces it, not two kinds of evidence.

Rules the evidence column obeys:

- `unit` means a named test in `tests/` or an inline `#[test]` covers the claim, and the evidence cell names it.
- `none` means no test covers the claim today. The evidence cell still names the source path, so the gap is auditable.
- `Missing`, `Unmeasured`, and `Design decision` rows always use `none`.
- `Equivalent` always uses `unit` and names the test.
- `Partial` may use `none`; that is the signal that the working path is real but unproven, and the first slice adds the test.

## Agents and providers

| Capability | Upstream evidence | Baton status | Baton evidence and proof surface | Terminal decision | First slice | Plan |
|---|---|---|---|---|---|---|
| Broad provider coverage | T3: `apps/server/src/provider/builtInDrivers.ts`; Orca: `src/shared/agent-launch/catalog/tui-agent-config.ts` | Partial | `src/types.rs` `ProviderId`; `src/adapters/`; `tests/acp_adapter.rs`, `tests/claude_adapter.rs`, `tests/codex_adapter.rs`, `tests/opencode_adapter.rs`; unit | Keep structured adapters for approvals, plans, subagents, and usage. | Add an ACP Registry adapter only, with one fixture server. | D11 |
| Raw terminal or arbitrary CLI agent | Orca: `src/shared/agent-launch/catalog/tui-agent-config.ts`, `docs/site/content/docs/agents/supported.mdx` | Missing | No PTY dependency in `Cargo.toml`; `src/adapters/mod.rs` uses line transports and a closed `ProviderId` enum; no config-declared command adapter; none | Add one shared PTY subsystem, not a separate PTY per provider. | Add a PTY-backed settings-declared command adapter with no structured events. | D8 |
| Provider identity for non-built-in adapters | Orca: `src/shared/agent-launch/catalog/tui-agent-config.ts` | Missing | `src/types.rs` `ProviderId` is a closed six-variant enum; `src/adapters/mod.rs` `BatonAdapter::id()` returns it; `src/store.rs` `provider_sessions` is keyed by it; `src/config.rs` keys auth by provider name; none | One identity model that can name a configured agent without a code change per agent | Widen provider identity to a validated string key with a built-in set, migrate `threads`, `provider_sessions`, and auth keys in one migration, and keep the enum for built-ins. Blocks D8, D11, and Pi import. | D8a |
| Capability-aware provider behavior | T3: `packages/contracts/src/orchestrationV2.ts` (symbol OrchestrationV2ProviderCapabilities) | Partial | `src/models.rs` `ProviderCapabilities`; `src/adapters/mod.rs` optional `steer_handle`, `list_models`, `fork_session`, `revert_session`; `src/types.rs` `supports_effort`; none | Surface existing capabilities in commands, pickers, and notices. | Gate the composer and slash menu from the existing capability methods instead of adding a second type. | D11 |
| Multiple accounts and provider instances | T3: `packages/contracts/src/providerInstance.ts` | Missing | `src/config.rs` stores one `ProviderAuthFile` per provider name with no profile or instance key; `src/store.rs` `provider_sessions` primary key is `(thread_id, provider)`; `src/runtime.rs` `load_provider_models` has no profile parameter and `src/config.rs` has no profile key; none | Named profiles and custom endpoints are useful in a terminal. | Add a `profile` column to `threads` and `provider_sessions`, key auth by `<provider>/<profile>`, and select the profile from `/auth`. | D20 |
| Native session import and resume | T3: `apps/server/src/project/AgentSessionImporter.ts`; Orca: `docs/site/content/docs/agents/session-history.mdx` | Partial | `src/scanner.rs` imports Claude and Codex into a closed `ProviderId`; `src/resume.rs`; `tests/core.rs` scanner tests; unit | Import every provider whose native transcript format is stable. | Pi import needs the provider identity row (D8a) first, then the scanner entry only. | D11 |
| Durable retry and continuation | T3: `apps/server/src/orchestration-v2/RestartContinuation.ts`, `apps/server/src/orchestration-v2/ProviderContinuationService.ts` | Partial | `src/store.rs` `reconcile_running_turns`, called from `src/tui/mod.rs` at startup; `src/runtime.rs` turn settlement; `tests/core.rs` turn reconciliation; unit | A reopened TUI should offer retry or continuation, never guess. | Add `/retry` for failed and unverified turns using the existing turn row. | D15 |
| Subscription limits and reset-aware resume | T3: `packages/contracts/src/providerUsageLimits.ts`, `apps/server/src/orchestration-v2/UsageLimitRecoveryWorker.ts`; Orca: `src/main/rate-limits/service/rate-limit-service.ts` | Missing | `src/usage.rs` records usage only; no limit source or reset worker; none | Show the source of a limit and whether resumption is automatic. | Add `/limits` from provider CLI output where available, with an unknown state elsewhere. | D15 |
| Plan mode and proposed-plan review | T3: `packages/contracts/src/orchestrationV2.ts` (symbol OrchestrationV2PlanningCapabilities); Orca: `docs/site/content/docs/agents/native-chat.mdx` | Partial | `src/tui/mod.rs` `/mode plan` and `/goal`; goal tests; unit | Plans need accept, revise, and reject actions. | Add the `TurnEvent` plan variant and its adapter emission first, then a read-only overlay with Enter to accept and Esc to edit. | D10 |
| Approvals and structured questions | T3: `packages/contracts/src/providerPolicy.ts` (symbol ProviderApprovalOption), `packages/contracts/src/providerRuntime.ts` (symbol UserInputQuestion); Orca: `docs/site/content/docs/agents/native-chat.mdx` | Partial | `src/tui/mod.rs` approvals and `ApprovalState`; `src/adapters/acp.rs` `session/request_permission`; approval tests; unit | Support typed choices, custom answers, and child-thread routing. | Add a question-choice row to the existing approval stack. | D12 |
| Native subagent visibility | T3: `packages/provider-core/src/server/subagentProjection.ts`, `apps/server/src/orchestration-v2/NativeChildWorkQuery.ts`; Orca: `docs/site/content/docs/agents/native-chat.mdx` | Missing | `src/adapters/mod.rs` `TurnEvent` has no child lifecycle; `src/tui/mod.rs` only labels child approvals; none | One `/agents` view should show children without a second source of truth. | Add child start, done, and question variants to `TurnEvent` and emit them from an adapter, then persist a subagent row. | D12 |
| Agent hibernation | Orca: `docs/site/content/docs/agents/hibernation.mdx`, `src/main/agent-hooks/server/server-pane-sleep.ts` | Design decision | `src/adapters/mod.rs` lifecycle; `src/provider_runtime.rs`; no idle sleep; none | Memory savings must never risk session correctness. | Decision owner: Baton maintainer. Revisit after PTY and attach exist. | Deferred |

## Conversation, context, and thread state

| Capability | Upstream evidence | Baton status | Baton evidence and proof surface | Terminal decision | First slice | Plan |
|---|---|---|---|---|---|---|
| Steering and queueing | T3: `apps/server/src/orchestration-v2/ThreadMessageIntake.ts`, `apps/server/src/orchestration-v2/QueuedRunOrder.ts`; Orca: `docs/site/content/docs/agents/native-chat.mdx` | Partial | `src/tui/mod.rs` steering and queue; `src/store.rs` `queued_messages`; queue tests in `src/tui/mod.rs`; unit | Steering is already correct. Queue editing is the gap. | Add `/queue send <n>` with the existing pop path. | D7 |
| Cross-provider handoff | T3: `apps/server/src/orchestration-v2/ContextHandoffService.ts`, `packages/provider-core/src/server/handoffBudget.ts`, `apps/server/src/orchestration-v2/ProviderSwitchService.ts` | Partial | `src/handoff.rs` `build_handoff` uses a 24,000-byte budget and a 4,000-byte per-message cap; `src/runtime.rs` builds a handoff only when `session.is_none()`; `tests/core.rs` covers composition and UTF-8 truncation, not the returning-provider delta; unit | Core to Baton's promise and a provider-to-provider egress path | D4a fixes the remaining correctness defect: switching back to a provider that already has a session gets no delta, because `provider_sessions` has no covered-turn marker. PR #89 already fixed the duplicate prompt and the byte-offset panic. D4b adds transcript redaction and the handoff egress inventory. D4c adds the preview, no-handoff, and confirmation. | D4a, D4b, D4c |
| Compaction and context telemetry | T3: `apps/server/src/orchestration-v2/IdleCompactionService.ts`, `packages/contracts/src/providerRuntime.ts` (symbol ThreadTokenUsageSnapshot) | Partial | `src/tui/mod.rs` `/compact` and `/context`; `src/usage.rs` records input, output, cache tokens, and cost; it does not record model capacity; none | Show model-relative context use before a safe compaction point. | Add a context-window field to `ProviderCapabilities` with an explicit unknown state, then add percent used to `/context`; without the capability field there is no capacity source. | D5 |
| Rich context references and attachments | T3: `packages/contracts/src/composerContext.ts`; Orca: `docs/site/content/docs/editing/file-explorer.mdx` | Partial | `src/tui/mod.rs` composer text; `src/memory.rs` `expand_memories`; `src/handoff.rs`; none | Text paths and quoted excerpts cover most terminal needs. | Add `@file` completion that serializes absolute paths. | D10 |
| Side chats and merge-back | T3: `apps/server/src/orchestration-v2/ThreadForkService.ts`, `apps/server/src/orchestration-v2/testkit/ThreadMergeBack.integration.test.ts` | Partial | `src/tui/mod.rs` `/fork`; `src/cli.rs` `baton fork`; `src/store.rs` `fork_thread_through_turn`; fork tests; unit | Wrap the existing fork path rather than build a second thread model. | Add `/btw` as a fork with a side-chat marker and no source mutation. Add `/merge-back` as a separate follow-up. | D16 |
| Thread organization | T3: `apps/server/src/orchestration-v2/ThreadSettlementService.ts`, `apps/server/src/orchestration-v2/ThreadTitleRegenerationService.ts`; Orca: `docs/site/content/docs/activity.mdx` | Partial | `src/store.rs` `ThreadRow` and `search_messages`; `src/tui/mod.rs` `/threads`; none | Pins, snooze, and archive are useful without a sidebar. | Add `/thread pin <id>` with one migration and a store test. | D16 |
| Unified search and command palette | T3: `apps/server/src/orchestration-v2/ThreadSearch.ts`, `apps/web/src/components/CommandPalette.tsx`; Orca: `src/renderer/src/components/file-actions/quick-open/QuickOpen.tsx` | Partial | `src/store.rs` `search_messages`; `src/tui/mod.rs` `/find`; `src/cli.rs` `search`; none | Start with actions, threads, and files. | Add a `Ctrl-K` palette that lists built-in actions and `/open` thread choices. | D17 |
| Cross-thread attention inbox | T3: `packages/client-runtime/src/state/threadInbox.ts` (symbol createInboxReturnTracker); Orca: `docs/site/content/docs/activity.mdx` | Missing | `src/tui/mod.rs` `/activity`, `/status`, and `/queue` are per-thread; `baton threads` lists threads but does not merge failures, questions, and completed workers; none | One inbox should merge failures, questions, and completed workers. | Add `baton inbox` over existing turn and orchestration queries. | D16 |
| Usage and cost history | T3: `apps/server/src/usage/UsageService.ts`; Orca: `src/main/rate-limits/service/rate-limit-service.ts` | Partial | `src/usage.rs`; `/usage`; `baton usage`; usage tests; unit | Token, cache, and cost history belongs in JSON and the TUI. | Add `--by day`, `--by model`, and `--by provider` to `baton usage`. | D5 |
| Quick commands | Orca: `docs/site/content/docs/terminal.mdx`, `src/shared/terminal/quick-commands/terminal-quick-commands.ts` | Partial | `src/tui/mod.rs` `discover_custom_commands`; `{{args}}` and `{{cwd}}` expansion; source only, no management test; none | Extend the existing custom command registry. | Add `/commands add <name> <template>` to the existing `.baton/commands` loader. | D17 |
| File navigation and quick open | T3: `apps/web/src/components/search/ProjectContentSearchDialog.tsx`; Orca: `src/renderer/src/components/file-actions/quick-open/QuickOpen.tsx` | Missing | `/open` opens threads only; no source path reads `$EDITOR` or `VISUAL`; none | Use `$EDITOR`; do not embed an editor. | Add `baton files` and `baton open <path>` backed by `rg --files` and `$EDITOR`. | D17 |

## Worktrees, source control, and delivery

| Capability | Upstream evidence | Baton status | Baton evidence and proof surface | Terminal decision | First slice | Plan |
|---|---|---|---|---|---|---|
| Single-repository worktrees | T3: `apps/server/src/orchestration-v2/ThreadLaunchService.ts`; Orca: `src/main/git/worktrees/creation/worktree-add.ts` | Equivalent | `src/worktree.rs`; `/worktree`; `baton worktrees`; worktree tests in `src/tui/mod.rs` and `src/worktree.rs`; unit | Keep the existing worktree and shared-directory model. | None. | - |
| Multi-repository worktrees | T3: `apps/server/src/workspace/TaskFolders.ts` | Missing | `src/worktree.rs` resolves one `repo_root`; `baton.json` supports only `worktree.sharedDirectories`; none | One thread may need several repositories | Resolve and operate on a list of repositories per thread, with one worktree per repository. Setup hooks are the separate trust row. | D22 |
| Commit, push, and open a PR from a normal thread | T3: `apps/server/src/git/GitWorkflowService.ts`; Orca: `src/renderer/src/components/right-sidebar/source-control/commit/use-commit-flows.ts`, `src/main/github/client/create/create-github-pull-request.ts` | Partial | `src/orchestrate.rs` `open_prs_with_gh` commits worker branches, runs `git push -u`, and calls `gh pr create`; no normal-thread git command; `CI+local`; none | Normal threads need the same finish path as workers. | Add `/git commit` for the current thread worktree with a generated message. `/git push` and `/git pr` are separate follow-ups. | D14 |
| PR checks, review, merge, and multiple forges | T3: `apps/server/src/sourceControl/SourceControlProviderRegistry.ts`; Orca: `src/main/github/client/check/get-pr-checks.ts` | Missing | `src/orchestrate.rs` only creates GitHub PRs; no checks, review, or merge path; none | Start with GitHub and `gh`. | Add `/git checks` backed by `gh pr checks`. | D14 |
| Diff review and navigation | T3: `apps/web/src/components/DiffPanel.tsx`; Orca: `src/renderer/src/components/diff-comments/DiffCommentCard.tsx` | Partial | `src/tui/mod.rs` `/review`, `/diff`; `src/store.rs` review notes; review tests; unit | Add sources and search without replacing the existing review overlay. | Add `/` search inside the current review rows. | D10 |
| Checkpoint restore safety | T3: `apps/server/src/orchestration-v2/CheckpointRestoreSafety.ts`, `apps/server/src/orchestration-v2/CheckpointRollbackService.ts` | Partial | `src/checkpoints.rs`; `tests/harness.rs` checkpoint and diff tests; unit | Restore must refuse shared concurrent work rather than guess. | Add a pre-restore cleanliness and worktree-ownership check. | D15 |
| Task dependency graph | Orca: `src/main/runtime/orchestration/coordinator/coordinator.ts`, `src/cli/orchestration/orchestration-handlers.ts` | Missing | `src/orchestrate.rs` `PlanTask` has no `dependsOn` field and launches independent tasks; `src/tui/mod.rs` orchestration report; orchestration tests cover a flat plan only; none | Dependencies change admission, cancellation, and merge order. | Add `dependsOn` to the plan schema, reject cycles, and gate task admission on completed dependencies. | D13 |
| Worker question mailbox | Orca: `src/cli/orchestration/orchestration-handlers.ts`; T3 native child question flow | Equivalent | `src/orchestrate.rs` persists the question in `orchestration_tasks.text` with status `needs_input`; `src/store.rs` task read; `src/main.rs` `exec -t` answers through `record_worker_followup`; inline test `followup_question_without_new_work_keeps_merge_markers` in `src/orchestrate.rs`; unit | A worker question must survive TUI close and be answerable from `baton exec -t`. That already holds. | None for the terminal acceptance. A multi-question inbox is a separate later row. | D13 |
| Project onboarding and actions | T3: `apps/server/src/project/ProjectService.ts`, `apps/server/src/project/ProjectSetupScriptRunner.ts` | Partial | `src/main.rs` starts from cwd; `src/cli.rs` has no project command; no action runner; none | Use git and shell for clone. Add only project selection and trusted actions. | Add `baton project add <path>` and a recent-project list. | D20 |
| GitHub and Linear task loops | Orca: `src/main/github/issues/issues.ts`, `src/main/linear/issues/linear-issue-listing.ts`, `src/cli/workspaces/worktree-linear-issue-link.ts` | Missing | No issue or task module; `src/orchestrate.rs` only creates PRs; none | Start with a GitHub issue because `gh` is already a dependency. | Add `baton task <github-issue>` that reads the issue and creates a linked worktree. Linear is a later follow-up. | D14 |
| Remote SSH worktrees | Orca: `src/main/ipc/ssh/ssh-connect-flow.ts`, `src/main/ssh/application/ssh-application.ts` | Missing | No host abstraction in `src/cli.rs` or `src/main.rs`; none | Do not build remote hosts before the runtime ownership ADR. | Add one `--ssh <host>` path to `exec`, not orchestrate or a daemon. | D24 |

## Execution surfaces and operations

| Capability | Upstream evidence | Baton status | Baton evidence and proof surface | Terminal decision | First slice | Plan |
|---|---|---|---|---|---|---|
| Integrated shell terminal | T3: `apps/server/src/terminal/Manager.ts`, `apps/server/src/terminal/NodePtyAdapter.ts`; Orca: `docs/site/content/docs/model/tabs-panes-splits.mdx` | Missing | No PTY dependency; `src/tui/mod.rs` has no shell overlay; none | One PTY subsystem, one bounded scrollback, one context-insert path | Add the sanitizer and its escape-sequence fixtures first (D9a), then `!` command execution with captured, sanitized output and no interactive PTY. | D9 |
| Tabs and split panes | Orca: `src/renderer/src/runtime/workspace-layout/commands/layout-apply-send.ts`, `docs/site/content/docs/model/tabs-panes-splits.mdx` | Missing | `src/tui/mod.rs` uses one vertical `Layout`; none | Tabs first, then one secondary pane. | Add thread tabs above the current composer. | D21 |
| Attention notifications | T3: `apps/web/src/components/ThreadNotificationCoordinator.tsx`; Orca: `src/main/notifications/notification-delivery-service.ts` | Missing | No BEL, OSC, or desktop notification path in `src/tui/mod.rs`; none | Notify on completion, failure, input, and approval; never on every delta | Emit BEL from the trusted notification writer only, behind a config flag, after the D9a sanitizer proves child output cannot emit BEL or OSC. Test the raw PTY bytes and the mute path. | D6 |
| Schedules and automations | T3: `apps/server/src/scheduledTasks/ScheduledTaskService.ts`; Orca: `src/main/automations/automation-service.ts` | Missing | No schedule table or command; none | One scheduler owner: `baton serve` claims due work; there is no separate `run-due` cron path | After the host exists, add `baton schedule add --cron --prompt` plus a single-writer claim inside `serve`. A `run-due` command is rejected because it would give two owners for the same due work. | D23 |
| Persistent host and remote attach | T3: `apps/server/src/auth/PairingGrantStore.ts`, `apps/server/src/cloud/CloudLink.ts`; Orca: `docs/site/content/docs/ways-to-run.mdx` | Missing | No `serve`, `pair`, or `attach` in `src/cli.rs`; none | The host becomes the only authority before remote control | D18b is the runtime ownership ADR alone, reviewed and signed off on its own. D18 then adds a local `serve --stdio` skeleton. Pairing and attach are separate follow-ups. | D18b, then D18 |
| Guest access | T3: `docs/user/guest-links.md`, `docs/user/t3-connect-troubleshooting.md` | Design decision | No host service; guest links require T3 Connect and host approval; no Baton equivalent; none | Guests can send messages and attachments with approval, not read-only viewing only. | Decision owner: Baton maintainer. Revisit after authenticated attach and scopes. | Deferred |
| MCP transports and OAuth | T3: `apps/server/src/auth/McpOAuth.ts`, `apps/server/src/mcp/McpHttpServer.ts` | Partial | `src/sync.rs` `McpServerConfig` has `command`, `args`, and `env` only; `src/mcp.rs` is stdio; none | Add HTTP after the local stdio path has a transport abstraction | Add an HTTP entry whose bearer token Baton consumes from a mode-0600 file or the platform keychain, with an explicit statement of who issues it, how expiry is checked, and how revocation takes effect. A static environment token cannot enforce scopes or revocation, so scopes are enforced by Baton per tool, not by the token. OAuth discovery is a follow-up. | D19 |
| Skills discovery and install | T3: `apps/server/src/provider/Drivers/ClaudeSkills.ts`; Orca: `src/cli/skills/skills.ts` | Partial | `src/sync.rs` scans global Claude and Codex skills; `/skills` lists; no install or insert path; none | Use one composer insertion path and one install ownership model. | Add `/skills` Enter insertion into the composer. | D10 |
| Agent-facing control plane | T3: `apps/server/src/mcp/OrchestratorMcpService.ts`; Orca: `src/cli/index.ts` | Partial | `src/mcp.rs` exposes `baton_fork` and queue clear; `src/cli.rs` has over twenty subcommands including `New`, `Exec`, `Spawn`, `Rewind`, and `Restore`, and none exposes orchestration controls over MCP; none | Reuse service methods and add scopes, not a second action layer | First close the existing mutators: `baton_fork` and queue clear get a scope, a confirmation requirement, and an audit row. Then add a scoped `baton_send` that targets an existing thread. | D19 |
| Settings, policy, and keymap | T3: `packages/contracts/src/settings.ts`, `packages/contracts/src/keybindings.ts`, `docs/user/managed-policy.md`; Orca: `docs/site/content/docs/settings.mdx` | Partial | `src/tui/mod.rs` `/settings` is display-only; `src/config.rs` stores user config; unit | Make `/settings` writable and keep precedence explicit. | Add one writable setting: default session mode. | D20 |
| Secrets and external secret providers | T3: `docs/user/secrets.md`, `docs/user/secrets-1password-connect.md`, `docs/user/secrets-1password-service-account.md` | Partial | `src/config.rs` reads API keys from env or config; no secret store; none | Prefer external providers; do not invent encryption before the host model. | Add `baton secret check` that reports env and config sources without values. | D18a |
| Themes and plugins | Orca: `src/main/plugins/contributions/plugin-theme-registries.ts`, `docs/reference/plugin-themes.md` | Design decision | `src/tui/mod.rs` has a fixed `COLORS` constant; no loader; none | Load a palette, not a plugin host. | Decision owner: Baton maintainer. Revisit after tabs and settings. | Deferred |
| Voice and dictation | T3: `apps/mobile/src/features/voice-input/environmentTranscriber.ts`; Orca: `mobile/packages/expo-two-way-audio/android/src/main/java/expo/modules/twowayaudio/OnDeviceSpeech.kt` | Design decision | No STT code or input hook; none | OS dictation is the default. | Decision owner: Baton maintainer. Add only an external command hook if requested. | Deferred |
| Slack, Linear, or GitHub bots | Orca: `docs/site/content/docs/remote-servers.mdx`, `src/renderer/src/components/bots/AddConnectorDialog.tsx` | Design decision | No webhook or daemon entrypoint; none | A headless trigger daemon may fit; chat UI does not. | Decision owner: Baton maintainer. Revisit after persistent attach. | Deferred |

## Terminal quality, privacy, and trust

| Capability | Upstream evidence | Baton status | Baton evidence and proof surface | Terminal decision | First slice | Plan |
|---|---|---|---|---|---|---|
| Startup performance | T3: `docs/operations/development.md`, `scripts/profile-startup.mjs` | Unmeasured | `src/main.rs` startup reconciliation and `src/tui/mod.rs` startup hydrate; no benchmark exists; none | Enforce warm p50 under 250 ms and p95 under 500 ms | Add `baton bench startup --json` over an isolated home, record the baseline, then fail a regression above 10 percent. | D5 |
| Input and render latency | Orca: `src/renderer/src/diagnostics/typing-latency/diagnostic.ts`, `src/renderer/src/diagnostics/typing-latency/renderer-long-tasks.ts` | Unmeasured | `src/tui/mod.rs` 32 ms poll loop; markdown and diff caches; no end-to-end benchmark exists; none | Enforce keystroke-to-render p95 under 50 ms | Add a PTY benchmark for typing and `/` completion, record the baseline, then fail a regression above 10 percent. | D5 |
| Memory and process bounds | T3: `docs/internals/resource-telemetry.md`, `apps/server/src/diagnostics/ProcessResourceMonitor.ts` | Unmeasured | Provider process tracking in `src/process.rs`; no RSS measurement or budget exists; none | Bound idle RSS under 200 MB and bound the adapter process count in the benchmark fixture | Record RSS at idle, during a turn, and after quit, then fail a regression above 10 percent. | D5 |
| Shell completion | T3: `apps/server/src/cli/install.ts`, `apps/server/src/cli/install.test.ts` | Missing | `clap` is present but no completion command or install path; none | Generate from the clap command graph. | Add `baton completions <shell>`. | D17 |
| Stdin, stdout, and exit codes | T3: `apps/server/src/binCli.test.ts`, `apps/server/src/cli/consoleStdin.ts` | Partial | `src/cli.rs` JSON and stream JSON; `src/mcp.rs` stdin server; `std::process::exit(130)` is reached only for a cancelled orchestration report, while an interrupted `exec` turn exits 1 and a TUI Ctrl+C does not exit; none | Keep machine output on stdout and progress on stderr | Define the cancellation contract explicitly, then add an integration test for `baton status --json` with a closed stdin and one for an interrupted `exec`. | D17 |
| Terminal capability detection | T3: `apps/server/src/cli/updateProgress.ts`, `apps/server/src/cli/connect.ts` | Partial | `crossterm` and `std::io::stderr().is_terminal()`; no color or width capability model; none | Detect color and width, then degrade. | Add a `NO_COLOR` and non-TTY rendering test. | D5 |
| Accessibility | T3: `docs/user/keyboard-focus.md`, `apps/web/src/lib/accessibilityAnnouncements.ts` | Design decision | Ratatui renders plain text but no screen-reader contract or focus announcement; none | Plain text, stable order, and no color-only meaning are the terminal baseline. | Owner: TUI maintainer. D5 is measurement only: no-color, reduced-motion, and 60x20 narrow-terminal checks. A full accessibility claim needs a separate decision. | D5 |
| Output sanitization | Orca: `src/shared/terminal/output/terminal-safe-text.ts`, `src/shared/terminal/protocol/escape/ansi-escape-sequences.ts` | Missing | Child text is rendered through Ratatui, but no escape-sequence sanitization exists and no test covers it; none | Never emit raw child control or escape sequences into the host terminal | Sanitize decoded Unicode scalar values, not bytes: allow newline and tab below U+0020, reject every other C0 control, reject DEL, reject the C1 range U+0080 to U+009F, reject bidi controls (U+202A to U+202E, U+2066 to U+2069) and the zero-width space, non-joiner, and word-joiner (U+200B, U+200C, U+2060) while preserving the zero-width joiner U+200D so emoji sequences survive, and reject the escape introducer so OSC, CSI, DCS, APC, PM, and SOS cannot begin. A byte-level 0x80 to 0x9F filter would corrupt UTF-8 continuation bytes. Fixtures must cover UTF-8, emoji, C1, and a BEL attempt. | D9a |
| Telemetry and data egress | T3: `docs/user/telemetry.md`, `apps/server/src/telemetry/AnalyticsService.ts`; Orca privacy settings | Missing | No analytics module; local SQLite in `src/store.rs`; `src/mcp.rs` is local stdio; no egress inventory or proof exists; none | Baton should remain local by default, but every non-local path needs an inventory | Add an egress inventory covering provider endpoints, handoff, MCP, `gh pr list` and `gh pr create`, `git push` in `src/orchestrate.rs`, and `cargo install` in `baton update`, plus a test for the Baton-owned paths. Arbitrary child-process and hook egress is delegated and cannot be enumerated by a test; the inventory must say so instead of implying coverage. | D18a |
| Secret redaction | Orca: `src/shared/observability/redactor.ts`, `src/shared/crash-reporting/crash-report-redaction.ts` | Missing | `src/config.rs` reads API keys into provider environments and `ProviderAuthFile` derives `Debug`, so a resolved key can reach a log; no redaction code or test exists; none | Never print a resolved key | Implement redaction in doctor and debug output, including the `ProviderAuthFile` Debug path, then add the fake-key test. The transcript redaction test belongs to D4b. | D18a |
| Repo-controlled hooks and actions | T3: `apps/server/src/project/ProjectSetupScriptRunner.ts`, `t3.json` | Missing | No action runner; `src/worktree.rs` reads `baton.json` for `worktree.sharedDirectories` only; none | A repo hook is executable code and needs trust before first run | Write a hook-execution ADR first: canonical repository identity, digest over the executed bytes, symlink and TOCTOU handling, default-deny sandbox or an explicit statement that none exists, process limits, environment allowlist, and audit semantics. Then add `worktree.setup` and `actions` sections to `baton.json`, not a second config file. | D22 |
| Remote and guest authority | T3: `docs/internals/environment-auth.md`, `apps/server/src/guest/GuestShareService.ts` | Missing | No host or token store; none | One authority, short-lived tokens, TLS or SSH transport, and revocation | Define the token scope model in the D18b ownership ADR before any transport code. | D18b |
| MCP mutation and spend | T3: `apps/server/src/mcp/McpToolAccess.ts`, `apps/server/src/mcp/toolkits/orchestrator/tools.ts` | Partial | `baton_fork` and queue clear mutate with no scope, confirmation, or audit record; no spend cap exists; none | Every mutating tool gets a scope, a confirmation, and an audit row | Add the audit event schema and the scope taxonomy, retrofit `baton_fork` and queue clear, then add `baton_send` with a thread scope, a spend confirmation, and no implicit orchestration. | D19 |

## Alternatives considered

| Choice | Decision | Rejected alternative | Reason |
|---|---|---|---|
| Terminal parity target | Terminal-native workflows | Clone each T3 and Orca GUI | A TUI cannot reproduce pointer, browser, and native-device interactions; the ledger records N/A where that is true |
| Provider reach | One PTY subsystem plus structured adapters | One dedicated adapter per CLI | Dedicated adapters are needed for approvals, plans, and usage, but a PTY covers the long tail |
| Shell execution | Native `!` overlay and context insertion | Tell users to open tmux | The context insertion and thread continuity are the value; tmux remains an escape hatch |
| Remote control | `serve`, `pair`, and `attach` after an ownership ADR | Shell out to SSH and tmux with no product support | Remote work needs one authority, token scopes, and recovery |
| Schedules | Durable schedule rows plus `run-due` | Always-running Baton daemon | System cron is simpler until the host exists |
| Secrets | Environment and external providers first | Local encrypted store in the first PR | Key management and backups are a product of their own |
| File navigation | `$EDITOR` and `rg --files` | Embedded editor | Editors are already installed and better than a TUI clone |
| Quick commands | Extend `.baton/commands` | New second command registry | Two registries drift and confuse the slash menu |
| CI first step | Full-suite bootstrap and real measurements | Shard count chosen from local macOS data | No Actions run history exists; shards must follow CI data |
| Merge queue | Required `verify` now, official merge queue after ownership change | Fake queue with a custom bot | A fake queue can hide failures and needs its own server |
| Handoff content | Delta preview with redaction, confirmation, and a no-handoff option | Full transcript replay | Replay leaks every prior provider's content and spends cache budget; a preview makes the egress explicit |
| Remote authority | One Baton host after the ownership ADR | SSH plus tmux only | SSH and tmux remain the fallback; the product value is one owner for queues, schedules, and attach |
| Upstream relationship | Build a terminal-native Baton and use T3 Code and Orca as references | Embed or fork a GUI app | Embedding a GUI or a second server violates the terminal-first constraint and inherits a much larger surface |
| Child output | Allowlist printable text, newline, and tab | Denylist BEL, OSC, and CSI | The terminal control set is larger than three sequences; an allowlist is auditable |
| CI workflow changes | Base-owned comparison with no PR-level override | An explicit label that permits command changes | A label is a blanket bypass: it turns off exactly the comparison that makes a weakened command visible. CI changes land through the owner-only `ci-repair` path instead |
| Runtime ownership in Release 1 | Local-first until an explicit ownership ADR exists | Host-first from the start | Local-first keeps one writer while the TUI and CLI share the store. Host-first needs scopes, recovery, and shutdown before any of that is proved, and it delays every Release 1 row |
| Project configuration | Extend `baton.json` with `worktree.setup` and `actions` | A second `.baton/worktree.json` registry | Two project configs with two trust models drift, for the same reason a second command registry was rejected |
| Private-repo spend | Hosted minutes with an owner-approved cap | Buy a paid runner before measurement | No CI data exists yet; a paid runner is justified only after the hosted p50 or p95 misses |

### Reversal conditions

Every row above has one. A decision is reopened when its condition is met, not when someone dislikes it.

- Local-first runtime: reopen if any Release 1 slice needs a second writer for turns, queues, or schedules. The rework then is the ownership and scope work in D18b plus a migration of the TUI's engine handle, so it is cheaper before D19 than after.
- Delta handoff: reopen if the covered-turn marker cannot be stored per provider in `provider_sessions` without a second migration. The fallback is a thread-level marker, which is coarser and may resend a turn.
- Allowlist sanitizer: reopen if a legitimate agent output needs a control sequence, for example a progress bar. The answer is an explicit allowlisted sequence with its own test, never a denylist.
- Base-owned CI tools: reopen if a base tool bug blocks every PR. That is the `ci-repair` path, not a label that permits head command changes.
- Hosted runners: reopen when recorded p50 or p95 misses both targets.

## Risks and mitigations

| Risk | Impact | Mitigation | Owner |
|---|---|---|---|
| Provider protocol drift | A provider turn silently loses events | Adapter fixtures, live local probes, and a provider capability matrix | Adapter maintainer |
| Handoff returns stale context | The core thread promise breaks | Covered-turn ranges, delta handoff, formal gate, and transcript fixture | Runtime maintainer |
| Handoff discloses prior provider content | Provider-to-provider data egress without consent | Show the exact delta, redact configured secrets, offer no-handoff, and require confirmation | Privacy owner |
| Unattended work spends unexpectedly | A schedule or MCP action starts paid work | One host authority, scoped tokens, confirmation, budget cap, and audit rows before schedules or new MCP mutations | Host maintainer |
| Private-repo CI exhausts included minutes | CI stops or creates an unexpected bill | Owner-approved monthly cap, billable-minute measurement, and no paid runner without a new approval | Repository owner |
| CI workflow is weakened by a PR | Tests or checks no longer run | Base-owned scripts and command comparison on every event, no PR-level override, gate-integrity suppression checks, and an owner-only `ci-repair` merge override that is recorded in the PR | CI maintainer |
| Base-owned CI tool is wrong | Every PR fails, including the fix | `ci/selftest.sh` fixtures, a local full-gate run before merging a `ci-repair` PR, and the `push` run on `main` as the recovery record | CI maintainer |
| Selector skips tests a diff can reach | A regression merges green | Reverse transitive closure, full-suite fallback for unmapped modules and `lib.rs`, and a union check against the discovered inventory | CI maintainer |
| A budget is wrong for the runner | First `main` run fails on time, not on code | Provisional budgets from local cold estimates, Phase 2 recalibration from recorded Linux p95, and a rule that a budget may only shrink | CI maintainer |
| Running work is mistaken for background work | A closed TUI leaves a running turn unverified | Reopen visibility, failed/unverified reconciliation, and explicit retry; no local-only continuation promise | Runtime maintainer |
| PTY and OSC output reaches the host terminal | Terminal escape injection or a stuck screen | Unicode-level allowlist in D9a before any PTY or BEL slice, with UTF-8, emoji, C1, OSC, and BEL fixtures | TUI maintainer |
| Repo hooks execute untrusted code | Code execution on open or worktree create | Bind trust to repository, hook digest, and config version; re-prompt on change; default deny; sanitized environment; audit row | Worktree maintainer |
| Remote or MCP mutation exceeds scope | Unauthorized spend or thread changes | A durable append-only audit event (actor, environment, thread, action, scope, confirmation, payload digest, result, timestamp, retention) defined before D19, plus scoped tokens, confirmation, and a spend cap | Host maintainer |
| Secret leakage in logs or doctor output | Credential exposure | Redaction test with a fake key in every output path | CLI maintainer |
| CI false green | A broken change merges | Full suite in merge candidate, selector self-test, independent selection check | CI maintainer |
| Cache miss turns a check red | Flaky and slow PR feedback | Cold and warm budgets, previous-lock restore keys, and a measured cache audit | CI maintainer |
| Scope grows faster than delivery | No parity row reaches Equivalent | One first slice per PR and a capped delivery order | Product maintainer |

## Runtime model decision

Baton remains local-first until `baton serve` exists.

- The TUI owns one orchestration run and the selected thread.
- A future `baton serve` process becomes the only owner of turns, queues, and schedules. A TUI attach client sends commands to it and does not start a second engine.
- Schedules, guest access, and mutating MCP actions are not scheduled before the host authority exists.
- The D18b ADR must define command ownership, concurrent-run limits, token scopes, shutdown, and recovery before D18, D19, D23, or D24 starts.

## Not applicable in a TUI

| Feature | Reason |
|---|---|
| Embedded browser and design mode | No pointer-driven embedded browser; agents can use host browser tools |
| Native mobile companion | The future attach client and SSH/tmux cover remote terminal access |
| Desktop app chrome and split editors | The terminal owns its layout; use `$EDITOR` |
| Computer use and mobile emulator | GUI automation belongs to host tools |
| Visual replies and HTML render panes | A TUI can show text and links; agents can write files for an external browser |
| Browser session import and default-browser capture | These depend on a desktop browser integration |

## Release 1 boundary

Release 1 is the smallest set that makes the terminal core trustworthy: correct slash completion, an honest CI gate, a sanitizer before any slice renders child bytes, notifications and queue control, a handoff that neither duplicates nor leaks content, and measurement before optimization. A new surface such as the shell overlay, and anything that needs a second runtime owner, is not in Release 1.

| In Release 1 | Out of Release 1 (backlog) |
|---|---|
| D1, D2, D3a, D3b, D3c, D9a, D6, D7, D4a, D4b, D4c, D5 | D8a, D8, D10 to D17, D18a, D18b, D18, D19 to D24 |

Work in progress, capped at four slices. A fifth slice waits even if it is ready.

| Slice | PR | Done when |
|---|---|---|
| D1 | #86, open | Every finite-argument command exposes choices and the PTY probe passes |
| D2 | #88, open | Both checkers report zero missing and two Goldfish rounds are clean |
| D3b | #87, open | `ci/selftest.sh`, the budget negative test, and the formal gate pass |
| D4a | #89, open | The duplicate prompt and the UTF-8 truncation are fixed; the covered-turn marker is a second PR |

The cap is four: one product feature, two records and tools, and one defect fix, all review-ready and independent. Starting a fifth slice before one of these merges is a regression against this document.

## Delivery order

The table order is the execution order. The D identifiers are stable labels, not a second ordering, so a slice that gains a prerequisite keeps its name and moves in the table.

Each row is one small PR. A row with a letter suffix is the smallest unit that can be verified on its own. A row without a suffix is one PR unless its `First slice` text names several steps, in which case the steps land in order as separate PRs and the row is not done until the last one lands. The capability tables' `First slice` cell is the first step of that sequence, not the whole capability.

| ID | Change | Why now | Proof |
|---|---|---|---|
| D1 | Slash argument choices and usage hints | Removes memorized syntax and establishes the completion contract every later command must follow | PR #86, unit tests, PTY slash probe |
| D2 | Parity ledger and CI design record | Makes scope, evidence, and acceptance explicit | This review, `scripts/check_parity_paths.py`, `scripts/check_doc_links.py` |
| D3a | CI Phase 1A documentation | Records the gate before any code exists | Doc link check, EGM review record |
| D3b | CI Phase 1B trusted helpers, skills, and formal model | Creates the gate scripts and their fixtures | `ci/selftest.sh`, budget exit 124, formal gate |
| D3c | CI Phase 1C full-suite workflow | Creates the first honest CI run and real data | Workflow run, workflow guard, executed-test inventory |
| D9a | Unicode output sanitizer | Nothing that renders child bytes can be safe before this exists, so it precedes every such slice including BEL | Escape-sequence, UTF-8, C1, and ZWJ fixture suite |
| D6 | Attention notifications | Small and user-visible once the sanitizer owns the BEL decision | PTY byte test and mute test |
| D7 | Queue edit, send-now, and reorder | Uses the existing durable queue | Store and TUI tests |
| D4a | Handoff correctness | Two defects are already fixed in PR #89; the returning-provider delta remains | Defect tests for the duplicate prompt, UTF-8 truncation, and a covered-turn marker |
| D4b | Transcript redaction and the handoff egress inventory | A handoff is provider-to-provider egress; it cannot send before this exists | Fake-key transcript test and an inventory test over the Baton-owned paths |
| D4c | Handoff preview, no-handoff, and confirmation | Makes the egress explicit to the user, on top of a redacted payload | Handoff state-machine gate plus a transcript fixture |
| D5 | Startup, input-to-render, and memory benchmarks plus context and usage telemetry, plus the NO_COLOR, non-TTY, and 60x20 checks | Sets numeric baselines and surfaces the resource state those baselines measure | Local benchmark artifact and a regression threshold |
| D8a | Provider identity for non-built-in adapters | Blocks D8, D11, and Pi import; the closed enum is a hard dependency | One migration plus store, adapter, and auth-key tests |
| D8 | PTY core and config-declared command adapter | Unlocks the long-tail CLI agents | PTY lifecycle test and process cleanup |
| D9 | Shell overlay | Highest-value terminal surface, after the sanitizer owns the output path | PTY probe with the sanitizer fixtures |
| D10 | Skills insertion, plan review, diff search, and `@file` completion | Extends the completion and overlay surfaces | TUI and PTY tests |
| D11 | ACP Registry, capability-gated commands, and Pi transcript import | Expands provider reach without a new engine | Adapter fixture, capability tests, one local live turn |
| D12 | Native subagent and question view | Makes delegated work observable | Provider fixture and TUI state test |
| D13 | Orchestration dependency graph | The flagship orchestration gap, after the already-Equivalent mailbox | Orchestration state-machine gate |
| D14 | GitHub task, commit, checks, and PR loop, where the row is done only when `/git commit`, `/git push`, `/git checks`, and `/git pr` all exist | Closes the delivery loop in one capability, not one command | Local `gh` fixture and CI-independent tests |
| D15 | Retry, `/limits`, reset recovery, and checkpoint restore safety | Recovers failed, rate-limited, or risky-restore work without a host | Turn and settlement gates |
| D16 | Thread organization, inbox, side chat, and merge-back | Makes many threads manageable | Store migration, TUI tests, handoff proof |
| D17 | Command palette, search, quick commands, file open, shell completion, and CLI contracts | Removes shell round trips and makes machine output predictable | TUI and CLI tests |
| D18a | Secret storage, file permissions, and doctor and debug redaction | Must exist before unattended or remote authority, and before any key can reach a log | Redaction test, file-mode test |
| D18b | Runtime ownership ADR | A decision-only slice with explicit sign-off; it gates D18, D19, D23, and D24 | Reviewed ADR naming the single owner for turns, queues, schedules, and tokens |
| D18 | `serve --stdio` skeleton | Unlocks safe remote control after the ADR | Integration test for the skeleton only; pairing and attach are separate |
| D19 | Audit schema, scoped mutation, and MCP HTTP | Lets agents act without a second control plane | MCP schema, scope, confirmation, and audit tests |
| D20 | Writable settings, profiles, keymap, project onboarding, and trusted actions | Makes operation explicit without mixing in secrets | Config precedence and keymap tests |
| D21 | Thread tabs and one secondary pane | Improves parallel observation after data-model stability | PTY layout probe |
| D22 | Multi-repo worktrees, then trusted hooks | Completes advanced worktree parity without bundling a trust decision into a feature | Hook-execution ADR first, then worktree and trust tests |
| D23 | Schedules owned by `serve` | Unattended work needs one owner and an audited trigger path | Scheduler gate plus single-writer test |
| D24 | SSH exec transport | Useful before a daemon exists, and it needs the same ownership rules, so it follows the ADR | Local SSH fixture and a process-cleanup test |

Deferred rows: agent hibernation, guest access, themes and plugin host, voice, and bots. They are not scheduled until their decision owner signs off.

Every new slash command introduced by D6 to D24 must ship its argument choices or a usage hint in the same slice. The completion contract is defined in PR #86 and enforced by the `every_argument_command_has_choices_or_a_usage_hint` test.

## Sync procedure

1. Fetch `origin/main` in `/Users/billy/Documents/t3code` and `/Users/billy/Documents/orca` without changing their working trees.
2. Run `git -C <checkout> rev-parse origin/main` and update the Upstream references table.
3. Review the user docs, provider registry, command surface, and release notes for terminal-relevant changes.
4. Resolve every evidence path with `git -C <checkout> cat-file -e <commit>:<path>`.
5. Run `uv run python scripts/check_parity_paths.py` and `uv run python scripts/check_doc_links.py`. Both must print `0 missing`.
6. Update the affected row, its Baton evidence, and its proof surface.
7. Add or update a test, PTY probe, or recorded transcript before changing a status to `Equivalent`.
8. Open one small implementation PR per first slice.
9. Run the affected formal gate before and after stateful work, and record the gate result.

## Review record

| Field | Value |
|---|---|
| EGM round | Round 6 findings addressed; Round 7 pending |
| Reviewers | Round 1 (parity and CI): `codebase_details`, `design_critic`, `comprehension`, `implementation_readiness`. Round 2: `design_critic` on parity, `implementation_readiness` and `design_critic` on CI, plus a parity `codebase_details` retry that returned no report and was rerun in Round 3. Round 3: `codebase_details` and `design_critic` on parity; `implementation_readiness` and `design_critic` on CI. |
| Targets | `docs/parity.md`, `docs/ci.md`, `docs/ci-acceptance.md`, `ci/**`, `.github/workflows/ci.yml` |
| Findings report | `local_docs/goldfish_review_report.md`, which is gitignored. Durable conclusions live in this table and in the documents themselves. |
| Accepted fixes | Baton evidence column and proof rubric; problem, goals, non-goals, alternatives, and risks; Release 1 boundary and the four-slice in-flight cap; corrected statuses for multiple accounts, task dependencies, attention inbox, worker mailbox, and the three performance rows; provider identity prerequisite (D8a) ahead of PTY, ACP, and Pi; sanitizer (D9a) ahead of BEL and PTY; egress and redaction ahead of cross-provider send; audit schema and hook-execution ADR as prerequisites; single scheduler owner; handoff defects recorded as the first slice; CI Phase 1A/1B/1C split; base-owned tools on every event; no PR-level CI override; gate-integrity suppression checks; test inventory floor; unbudgeted-command rule; provisional budgets with a shrink-only rule |
| Rejected fixes | A `ci-change` label that permits command changes (a label is a blanket bypass, so CI changes use the owner-only `ci-repair` path); a fake merge queue; a paid runner before measurement; a second project config file for hooks and actions |
| Open escalated items | Official merge queue needs an organization on GitHub Enterprise Cloud; private-repository Actions minutes and rate are unconfirmed; branch protection is not yet configured |
