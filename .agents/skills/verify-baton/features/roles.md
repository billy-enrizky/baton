# Roles

## Sub-features
- Shows the resolved orchestrator, planner, implementor, and reviewer roles.
- Defaults to Claude `claude-opus-5-5` for orchestrator and planner, Codex `gpt-6.2-sol` for implementor, and Codex `gpt-6.2-astra` for reviewer.
- Reads known-role prompt overrides from `~/.baton/roles/<role>.md` and model settings from `~/.baton/roles.json`.
- Requires provider and model together in an override; effort is rejected for providers that do not support it.
- Embeds the orchestrator and planner prompts in the planner request. The planner prompt instructs tasks to default to the implementor provider when it is installed, otherwise to the coordinator provider. Code fills model and effort only when a task already names the selected role provider.
- Places the implementor prompt after Baton's fixed worker preamble and before the task prompt. The preamble is always first in the request; Baton does not enforce whether the model follows it.
- Parses and validates JSON hook declarations. Hook execution is still pending.
- `/roles` shows the four rows and their detail; `baton roles --json` returns model, effort, prompt source/path, and parsed hook declarations.

## Problem statement

Role behavior was implicit in one coordinator prompt and one worker preamble. Users could not inspect model ownership, provide role-specific markdown, or see hook declarations before a run.

## Goals and non-goals

Goals: one validated role registry, explicit defaults, user prompt and model overrides, role-driven planning and worker execution, a TUI submenu, a machine-readable CLI, and fail-closed malformed configuration.

Still out of scope: executing hooks, reviewer gating, full orchestrator turns for normal conversation, mutating role files from the TUI, and project-scoped roles. A greeting fallback copies only the orchestrator provider, model, and effort.

## Approach

`src/roles.rs` loads optional `roles.json`, validates every role and hook declaration, resolves provider/model/effort, and overlays optional markdown prompts. A missing file uses embedded defaults. A malformed file errors instead of silently falling back.

`src/orchestrate.rs` composes the planner request as the orchestrator prompt, the planner prompt, a precedence sentence, and the coordinator contract. The contract instructs the planner to keep its output format and safety rules if role text conflicts. Plans are capped at 16 tasks and code enforces JSON shape, nonempty fields, safe model tokens, provider existence and installation, and prompt size. A plan file also rejects unknown provider names during parsing and enforces the prompt and model caps, but defers installation and startup checks to the worker preflight. Disjoint file ownership, task independence, and model compliance with the worker preamble remain prompt-level. The planner role supplies the coordinator provider, model, and effort. The prompt defaults tasks to the implementor provider when it is installed, otherwise to the coordinator provider. Any non-greeting plan, including a `--plan-file`, fills each unset task model and effort from the matching role: implementor when installed, otherwise planner. Tasks on other providers use harness config defaults. A resolved plan records `role_defaults_applied: true`, so a saved preview cannot be normalized a second time by a later run. Planner output cannot set that marker itself. A direct plan fallback for an exact greeting copies the orchestrator provider, model, and effort and skips role normalization, including after its preview is saved and reopened. That greeting worker does not receive the orchestrator prompt. Each worker receives the fixed preamble, then the implementor prompt, then the task prompt. `baton spawn` resolves the provider model from config before it creates its one-task plan, so role defaults cannot replace an explicit or configured model.

## Alternatives considered

- One monolithic embedded prompt was rejected because per-role model and prompt ownership would be opaque. Embedded defaults and optional markdown overrides are resolved per role; the planner request concatenates only the orchestrator and planner views, while workers use the implementor view separately.
- Running the orchestrator as the coordinator was rejected because the planner role owns plan shape and the planner model/effort must be independently configurable.
- A planner-only prompt without the orchestrator view was rejected because the user asked for orchestrator policy to guide planning and delegation.
- A plugin host first was rejected because it adds a trust and lifecycle surface before basic roles work.
- Silent fallback on malformed JSON was rejected because it would hide a user's intended model or prompt override.

## Risks and mitigations

- Hooks will eventually execute trusted user code. This slice parses only command argv, timeout, and known event names; it does not run them. A later execution slice must define consent, environment clearing, process limits, and failure policy.
- Model names can drift. Defaults are explicit and Baton's Codex catalog includes both role models, but the selected provider account can still reject a model it does not entitle.
- An uninstalled implementor provider no longer traps every plan. The planner prompt defaults to the coordinator when the implementor binary is missing, and planner failure uses the coordinator provider in that case instead of the implementor.
- If the planner provider is unavailable, planning retries and then falls back to the implementor when installed. If no configured role provider is installed, the fallback worker fails at provider preflight; `baton doctor` lists the missing providers. The failure is visible in the run report, not only in debug logging.
- `installed` means the harness binary is on `PATH`. Authentication and account entitlement are checked when the provider starts, so a broken implementor can still fail after planning. Unsupported custom endpoints are rejected earlier at CLI startup.
- The planner child uses a temporary `BATON_HOME` so it does not write planner threads into the user's store. It inherits harness-native config and environment, not Baton config overrides from the user's home.
- Role prompts are user-controlled text. Empty prompts fail closed. Role markdown and the orchestration goal are capped at 6 KiB each, and planner task prompts at 12 KiB. The two role views plus goal keep the planner request under 21 KiB, and the worker request stays under 19 KiB before quoting; Windows argv escaping is not measured. The worker preamble is always prepended first, and planner role text is inserted outside the template's placeholder replacements. Model compliance with the preamble is not sandboxed.
- Orchestration prompts set `BATON_DISABLE_SKILL_DISPATCH=1`, so `$skill` text in role markdown or a task cannot be moved in front of the safety preamble or planner contract.
- Worker-starting orchestration requires a git repository. Plan-only and preview commands work outside git. A non-git run that would start workers is rejected before a worker starts; the normal TUI composer falls back to a single harness with a notice instead.
- Maximum task count is 16 in both the shared planner path and the TUI. The CLI rejects larger `--max-tasks` values instead of silently truncating, and the TUI rejects `--provider`, `--model`, and `--effort` like the CLI.
- Project role overrides need a trust decision and are deliberately deferred.

## How to get to it (user POV)
Type `/roles` in the TUI to open the picker. Enter on a row shows the resolved provider/model/effort, config source, prompt source, prompt path, and global hook events. Run `baton roles` for a non-interactive table, or `baton roles --json` for the full registry including `prompt_path` and `hook_events`. A normal CLI or TUI orchestration uses the planner role for its coordinator turn and the implementor role for matching worker tasks. The banner labels the current single-turn harness as `thread`; `/roles` lists the resolved roles and the orchestration preview shows the coordinator provider/model. The direct-plan greeting fallback copies the orchestrator provider, model, and effort, but it is not a full orchestrator turn: the worker still receives the implementor prompt. The TUI also routes normal conversational greetings through the current thread's single harness. Routing every conversational turn through the full orchestrator role remains a follow-up. `orchestrate` rejects `--provider`, `--model`, and `--effort`; change the role settings in `roles.json` instead.

## Verification paths

### PTY harness
```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The slash probe seeds an all-Codex role override, opens `/roles`, requires all four rows and the override model, and checks detail-only `config:` and `prompt:` fields after Enter.

### Unit and CLI

The tests cover role prompt ordering, placeholder preservation, implementor defaults, explicit task overrides, the distinct orchestrator greeting fallback, coordinator selection in the TUI, prompt caps, git-only worker execution, and registry validation:

```sh
cargo test --locked roles::tests
cargo test --locked models::tests::codex_catalog_includes_the_default_role_models
cargo test --locked --test core roles_json_reports_overrides_and_hook_declarations
cargo test --locked --test core orchestrate_plan_only_uses_the_orchestrator_role_for_greetings
cargo test --locked orchestrate::tests::planner_prompt_asks_for_the_smallest_plan_and_lists_available_harnesses
cargo test --locked orchestrate::tests::planner_prompt_does_not_rewrite_placeholders_inside_user_role_text
cargo test --locked orchestrate::tests::implementor_role_defaults_fill_matching_tasks_without_overriding_explicit_values
cargo test --locked orchestrate::tests::worker_prompt_keeps_the_preamble_before_the_role_and_task
cargo test --locked orchestrate::tests::greeting_uses_the_orchestrator_role_without_implementor_normalization
cargo test --locked orchestrate::tests::default_worker_uses_the_implementor_only_when_it_is_installed
cargo test --locked orchestrate::tests::coordinator_command_forwards_the_planner_effort
cargo test --locked orchestrate::tests::plan_only_resolves_files_and_greetings_without_running_workers
cargo test --locked orchestrate::tests::a_saved_greeting_preview_keeps_its_orchestrator_defaults
cargo test --locked orchestrate::tests::single_task_plan_keeps_the_provider_and_overrides
cargo test --locked orchestrate::tests::plan_task_model_and_effort_are_trimmed_and_optional
cargo test --locked tui::tests::orchestrate_args_parse_goal_and_flags
cargo test --locked tui::tests::orchestration_coordinator_role_matches_the_goal_and_plan_source
cargo test --locked tui::tests::orchestrate_command_routes_a_greeting_to_one_turn
cargo test --locked tui::tests::greeting_in_orchestrate_mode_uses_a_single_turn
cargo test --locked tui::tests::banner_shows_version_cwd_mode_and_permissions
cargo test --locked tui::tests::orchestrate_banner_labels_the_visible_model_as_thread_harness
BATON_HOME=$(mktemp -d) target/debug/baton roles --json
BATON_HOME=$(mktemp -d) target/debug/baton orchestrate --plan-only --json hi
```

### Live acceptance

For live acceptance, use a temporary `BATON_HOME` and git repository, set all four roles to an exact installed Codex model, and let the implementor markdown own the task instruction. The task prompt should not repeat the file name or contents. Run:

```sh
BATON_HOME="$home" target/debug/baton -C "$repo" --yolo orchestrate \
  --plan-file "$repo/plan.json" --json "role injection check"
```

The 2026-11-23 acceptance run used `rits/deepseek-ai/DeepSeek-V4.1-Flash-1017` through `CODEX_ICA_API_KEY`. The worker completed, committed, and created `ROLE_MARKER_OK` as directed only by `roles/implementor.md`. The key does not appear in the captured evidence.

A second live run exercised the real planner path with `planner.effort = high`, no plan file, and no greeting fallback. `orchestrate --plan-only --json` returned one model-written implementation task with a non-fallback summary. The prompt unit test proves role composition and the no-reviewer sentence, and the coordinator command unit test proves effort forwarding.

## Gotchas
- The reviewer role is visible and configurable, but reviewer gating and hook execution are not active yet.
- Hooks are declared globally in `roles.json`; the `/roles` Enter detail and each `baton roles --json` role object repeat the same event names. Picker rows do not show them.
- `~/.baton/roles.json` is read as strict JSON. A typo fails the command instead of using defaults.
- A role markdown file overrides only that role's prompt; provider, model, and effort stay in JSON.
- Model IDs are accepted as safe provider tokens even when they are not in the static catalog. Use the exact slug from the provider catalog: the live ICA run accepted `rits/deepseek-ai/DeepSeek-V4.1-Flash-1017`; shorter aliases may not resolve. The provider preflight and account entitlement decide whether the model can start.
