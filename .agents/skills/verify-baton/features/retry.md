# Retry a failed turn

## Sub-features
- `/retry` opens a picker of failed and unverified turns in the current thread.
- Each row shows the turn number, phase, provider/model, and stored prompt preview. Same-harness rows show session coverage; cross-harness rows show the switch instruction. The selected row detail adds the bounded error preview.
- `/retry <turn>` opens the picker on that row, and the slash completion list uses the same eligible rows.
- Enter submits the stored first user message as one new turn on the failed turn's harness, which must match the current provider. It uses the effective current model and permission mode, without a handoff preview. Orchestrate mode does not turn this into a coordinator run.
- The completion list keeps an exact `/retry <turn>` first, so `/retry 1` does not become `/retry 10`; a highlighted row from bare `/retry ` is still honored.
- Prompt and error previews replace terminal control characters before rendering.
- Completed, interrupted, open, missing, or promptless turns are refused.
- Retry reconciles an orchestration whose owner process is gone, then refuses while this TUI or the durable store reports running, queued, open, or pending-native-rewind work. An ownerless legacy run remains blocked because its liveness cannot be proven.
- A retry rejected by the engine is shown as a refusal and is never added to the durable queue.

## How to get to it (user POV)
Open a thread with a failed or unverified turn, type `/retry`, select the row, and press Enter. The transcript gains a new turn with the same prompt.

## Driving it with the PTY/CLI harness
```sh
.agents/skills/verify-baton/scripts/retry_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe creates an isolated Git repository and thread, seeds failed turns 1 and
10 directly in its temporary store, and puts a fake Claude harness on `PATH`.
The fake harness records the prompt line it receives. It requires exact
`/retry 1 ` and `/retry 10 ` completion rows, then types the exact `/retry 1`
form to prove it wins over the `/retry 10` prefix collision. It requires the picker
to show the failed turn, error, `current mode default`,
`claude/claude-sonnet-5-5`, and `session through turn 0`, presses Enter, and
checks the fake Claude args for `--resume fake-session` and its captured stdin
for the stored prompt. It then checks the durable store for a completed turn
11, the same prompt, the fake reply, `session_mode=orchestrate`, no
orchestration rows, the original failed rows and errors, and a clean `/quit`.

## Gotchas
- A retry is a new turn, not a rewrite of the failed turn. The original row and message stay in history.
- The failed turn's provider must match the durable current provider. The retry uses the effective current model and permission mode. After a provider switch, switch back before retrying.
- Only `failed` and `unverified` turns are eligible. An `interrupted` turn is a deliberate stop and is not offered.
- The prompt is sent without a cross-provider handoff preview because it stays on the same thread and harness.
- A retry on another harness is refused. Switch the thread back to the failed turn's harness first.
- The picker names the durable current harness, effective model, and permission mode, and labels the action as one turn. A provider mismatch is refused. A provider or effective-model change while the picker is open is revalidated before submit.
- A dead orchestration owner is reconciled before the retry guard. A running run with no owner stays blocked because Baton cannot prove that a legacy process is gone.
- A failed turn with no first user message is not listed, and a direct `/retry <turn>` says that no stored prompt exists.
- The probe covers a present native session and asserts the provider receives its id and the row shows its covered turn. With no native session, the row says `no session; prompt only`; the unit test covers that presenter. A session that covers a later turn is refused so an old prompt cannot run inside later context.
