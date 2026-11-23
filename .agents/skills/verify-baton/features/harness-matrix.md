# Harness matrix

## Problem and non-goals

Baton advertises Codex, Claude Code, Cursor, Grok, OpenCode, and Antigravity. A green unit suite does not prove that each installed CLI can start a real turn through Baton with the user's endpoint. This journey drives every advertised harness, records one normalized row per provider, and distinguishes a working turn from an external account block. It does not install or update user tools, change vendor account status, or hide a provider failure behind a generic readiness label.

## Goals

- Prove a real turn through Codex, Grok, OpenCode, and Antigravity with the ICA endpoint and `CODEX_ICA_API_KEY`.
- Record Claude's native subscription state and, only with explicit consent, its live turn result. The matrix clears ambient Anthropic API, Bedrock, and Vertex overrides for this check. A credit or usage limit is blocked, not passed.
- Verify Cursor installation, login state, and Baton's explicit preflight rejection of a custom endpoint. Cursor cannot use ICA.
- Give every provider the same normalized evidence fields and partial-run behavior.
- Keep Baton database and thread state isolated in a throwaway `BATON_HOME`; OpenCode also gets isolated XDG config, data, state, and cache directories. Codex runs the trivial turn with a read-only sandbox.

## Non-goals

- Installing or updating harness binaries.
- Logging into a vendor account or supplying a missing account credit.
- Treating a capability check as a live turn.
- Persisting API keys or raw provider prompts in the matrix summary.
- Proving Cursor or Claude can use the ICA endpoint; neither vendor path supports it.

## Proposed architecture

`harness_matrix_probe.py` is the entry point. It runs a provider list in order: Codex, Claude, Grok, OpenCode, Antigravity, then Cursor. Codex, Claude, OpenCode, and Cursor use in-process runners because they need isolated harness config, a consent gate, or a capability-only path. Grok and Antigravity delegate to their focused probes. Cursor performs a capability check with a recognized `cursor-agent status` result and a temporary Baton config; Baton's preflight must reject the custom endpoint before a turn starts.

The entry point isolates each provider runner and converts a timeout, malformed payload, or process error into a failed row with its configured mode, model, endpoint, and version, then lets the remaining providers run. The entry point writes one normalized JSON file per provider plus `summary.json`. Statuses are `passed`, `blocked`, `skipped`, `capability_verified`, or `failed`. The matrix passes when no row is `failed`; `blocked`, `skipped`, and `capability_verified` are never reported as live passes.

## Sub-features

- Codex runs through the ICA Responses endpoint with a throwaway `CODEX_HOME` and the default `rits/deepseek-ai/DeepSeek-V4.1-Flash-1017` model.
- Grok runs through `grok agent stdio` with the ICA `/v1` base and `gemini-3.7-flash`.
- Antigravity runs through ACP with the ICA Gemini endpoint and the same model.
- OpenCode runs through an isolated OpenAI-compatible provider config and `gemini-3.7-flash`.
- Claude runs no live turn by default. A logged-in account is skipped without `--include-claude`; a logged-out account is blocked; unusable auth output is failed. With consent, the live row records the harness default model and auth method.
- Cursor reports capability verification, unknown status failure, or an explicit login block. Baton rejects its custom endpoint before a turn.
- Every normalized row records provider, mode, version, effective model and endpoint where applicable, turn exit code or null when no turn ran, seconds, redacted response text or bounded error, tracked processes or null when no turn ran, status, reason, and bounded provider detail where useful.

## How to get to it (user POV)

```sh
export CODEX_ICA_API_KEY=...
.agents/skills/verify-baton/scripts/harness_matrix_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The default run performs four small ICA turns and no live Claude turn. Add `--include-claude` only when the user explicitly approves a live Claude subscription turn. `--codex-model` selects the Codex model; `--model` applies to Grok, OpenCode, and Antigravity. Claude uses its harness default, and Cursor performs no model call. `--base-url` is the endpoint root; the matrix appends `/v1` for Codex, Grok, OpenCode, and Cursor and uses the root unchanged for Antigravity.

## Driving it with the PTY/CLI harness

```sh
.agents/skills/verify-baton/scripts/harness_matrix_probe.py \
  --binary target/debug/baton \
  --key-env CODEX_ICA_API_KEY \
  --base-url https://api.servicesessentials.ibm.com \
  --model gemini-3.7-flash \
  --evidence-dir local_docs/verification/evidence

.agents/skills/verify-baton/scripts/harness_matrix_probe.py \
  --binary target/debug/baton \
  --include-claude \
  --evidence-dir local_docs/verification/evidence
```

The second command is the explicit Claude consent path. Evidence lands in one `harness-matrix-<timestamp>-<unique>` directory. The focused Grok and Antigravity probes also keep their own evidence directories.

## Pass contract

A live pass requires turn exit 0, the expected exact text, no provider error, and no tracked processes. A blocked row requires an explicit external cause such as `cursor-agent login`, a Claude credit or rate limit, or an ICA rate limit. A skipped row is an intentional policy gate. Cursor's `capability_verified` row proves installation, a recognized vendor login status, and Baton's preflight rejection of a custom Cursor endpoint only, not a live turn. Unrecognized Cursor status output is failed.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| A provider echoes a secret into output | The matrix and focused probes replace the configured key value with `[redacted]` in text, errors, status output, and saved stdout. The matrix stores bounded normalized fields rather than full turn stdout. Evidence remains local and must not be attached to a public PR without review. |
| A harness hangs | Each turn has a 180-second timeout and each focused probe has a 240-second timeout. On timeout, the matrix terminates that command's process group, writes a failed row, and continues with the remaining providers. The total run can still approach the sum of those timeouts. |
| A harness is missing or broken | The provider row fails with its version or bounded error. Missing installation is not converted into an account block. |
| An endpoint or model changes | Every ICA-backed row records its effective model and endpoint; Claude records its harness default model, auth method, and native-subscription mode; Cursor records its attempted endpoint. A failure is reproducible from the summary and the recorded arguments. |
| Live runs spend account quota or hit a rate limit | The default matrix skips Claude and performs no Cursor turn. The four ICA turns are small. An explicit rate-limit response becomes blocked, not passed. `--include-claude` is explicit consent, not a default, and the Claude check clears ambient API and cloud-provider overrides. |
| Concurrent runs collide | The matrix directory includes a timestamp and random suffix, and focused-probe evidence now lives inside it. |
| Claude auth output changes | Only explicit logged-out JSON becomes blocked. A nonzero status, malformed JSON, or unexpected shape becomes failed. |
| Cursor's status output changes | Only recognized logged-in or logged-out text is accepted. Unknown output is failed, not `capability_verified`. |
| A harness writes outside its temporary cwd | Codex gets a throwaway `CODEX_HOME` and read-only sandbox; OpenCode gets throwaway XDG config, data, state, and cache; Baton database and thread state use a throwaway `BATON_HOME`. Claude, Grok, Antigravity, and OpenCode still read vendor configuration or may access the host filesystem according to their own policy. Run the matrix only in a workspace where a provider turn is acceptable. |

## Alternatives considered

- **One uniform subprocess probe for all six.** Rejected because Codex, Claude, and OpenCode need isolated harness configuration or a consent gate that the focused probe contract does not expose. The hybrid reuses the two existing live probes for Grok and Antigravity and keeps provider-specific setup near its runner.
- **Run Claude by default.** Rejected because the verification skill forbids an unapproved live Claude turn. The opt-in preserves the check without surprising quota use.
- **Require a live Cursor turn.** Rejected because Cursor rejects the ICA endpoint and the installed account may not be logged in. A capability check still proves the adapter's actionable behavior and error.
- **Fixture-only coverage.** Rejected because fixtures cannot prove the installed CLI, endpoint, model, or account path. Unit and adapter fixtures remain the CI gate; this matrix is a local live proof.

## Gotchas

- Cursor validates vendor-issued credentials and cannot use the ICA endpoint. Baton rejects that override before a turn. Use `cursor-agent login`; do not put the ICA key in a Cursor provider override.
- Claude Code uses its native subscription or `ANTHROPIC_API_KEY`. An account with no credit is an external block, not an ICA failure.
- OpenCode accepts a custom OpenAI-compatible provider through its own config; Baton does not map a custom base URL for OpenCode.
- Codex Gemini requests fail at ICA with an unsupported `prompt_cache_key` parameter. Keep the known Codex ICA model or add a harness-specific request option before changing the matrix default.
- Never print the API key. The script records the source variable name only and redacts the value from captured output.
