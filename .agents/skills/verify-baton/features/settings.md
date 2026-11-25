# Settings

## Sub-features
- Global default session mode: `orchestrate` or `single`, persisted in `config.json` and applied to new sessions and `/new`.
- Global terminal bell: `off` or `on`, persisted in `config.json` and applied to the current TUI immediately.
- Available harness settings: permission, approval, sandbox, and key-name values shown read-only. The row does not claim to mirror every provider-owned current value.
- The selected category's full option list, with the pending value marked.

## Problem statement

The old `/settings` picker showed harness values as if Enter applied them, but Enter only printed a notice. It also did not expose the two preferences Baton owns: the default session mode and the terminal bell.

## Goals and non-goals

Goals: make Baton-owned preferences writable, show their current value, list the selected category's complete choices, update the live bell, preserve unrelated config keys, and refuse to overwrite a malformed config.

Non-goals: writing harness-owned configuration from a generic picker, editing keymap or managed policy files, profile management, and synchronized edits from another process.

## Approach

The picker has global rows first and read-only harness rows after them. A global row applies to Baton's `config.json`; a harness row only reports an available option list. `write_global_setting` loads the current config, changes one field, and saves it. A strict load distinguishes a missing file from a malformed one, so a bad file cannot become an empty config. Unknown nested fields survive a save. The write goes to a mode-0600 temporary file, syncs, renames over `config.json`, and syncs the directory; a new Baton home is mode 0700.

## Alternatives considered

- A separate `baton config` command for globals was rejected for this slice because it separates the setting from the live TUI state and needs a second discoverable surface.
- Writing every harness row in place was rejected because each adapter has its own file format, precedence, and preservation rules. A generic writer can corrupt provider-owned configuration.
- A single global config that replaces the harness picker was rejected because terminal users still need to inspect which values the current harness understands.

## Risks and mitigations

- A concurrent external writer can race the save. Baton preserves unknown keys and replaces the file atomically, but a file lease is not implemented yet.
- Harness rows can look more authoritative than they are. They are labelled read-only, and this guide does not call them current provider state.
- The bell rings on turn end, an approval request, and a provider question; a two-second limiter mutes a burst.
- A malformed config blocks the write and leaves the file untouched, with an error notice. Unknown top-level types that cannot satisfy the typed schema also fail closed.

## How to get to it (user POV)
Start `baton` and type `/settings`. Up and Down select a category. Left and Right, Tab, or BackTab select a value. Enter writes a global value and keeps the picker open. Enter on a harness row reports that the row is read-only. Escape closes the picker.

## Driving it with the PTY harness
```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The slash probe opens `/settings`, requires the global and Claude rows, the selected category's marked option list, `enter apply`, and `esc`. It selects `single`, applies it, requires the new-session notice, checks the bell's option list, turns it on, requires the bell notice, opens a harness category to check its read-only option list, and checks the persisted `config.json`.

## Gotchas
- The default session mode applies to new sessions and `/new`. It does not rewrite the current thread. `/fork` keeps the source mode and `/worktree` starts a single-harness thread.
- Harness rows are intentionally read-only until each harness has a writer that preserves that harness's own configuration. Their value column is a set of available choices, not the provider's live state.
- The bell changes the current TUI immediately. It rings on turn end, an approval request, and a provider question, with a two-second limiter.
- A malformed existing config refuses the write and stays byte-for-byte unchanged. Fix the JSON, then reopen `/settings`.
- The read-only row itself does not show a live provider value. The options line shows the available choices with the cursor, and the notice calls the result an option, not the harness's current setting.
