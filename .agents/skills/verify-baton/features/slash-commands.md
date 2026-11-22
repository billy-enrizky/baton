# Slash command choices

## Sub-features
- Typing `/` lists every command in the completion registry, including `/help` and the `/exit` alias.
- Typing a command followed by a space lists valid argument choices, or its argument syntax when the command takes free-form text.
- Partial command and argument text filters the same list.
- Tab or Enter completes the selected choice; Enter on a complete choice runs it.
- Free-form commands still accept typed text.

## How to get to it (user POV)
Open the TUI and type `/mode `, `/model`, `/switch `, `/effort `, `/auth `, `/memories `, `/mcp `, `/orchestrate `, or `/runs `.

## Driving it with the PTY harness
```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe types `/find ` and requires its argument syntax, then types `/mode `, requires all four permission choices and the completion hint, presses Tab to accept `default`, presses Enter, and requires the resulting `permission mode: default` notice. It then types `/model`, requires the model picker header, filter hint, and Enter/Escape affordances, and closes it with Escape. It opens `/pin`, toggles a pinned row, closes that picker, creates and pins a second thread, then runs a real `/pin move <second-id> down`. It requires the moved notice, refreshed picker, and swapped row order, closes the picker, runs the same move again, requires the last-position boundary notice, and requires a clean `/quit`.

## Gotchas
- The displayed permission banner uses the provider default when the explicit value is `default`. The command notice reports the selected value; the banner reports the effective provider mode.
- `/open` choices come from the current store. A temporary home has no threads, so it may show no choices until a thread is created.
- Turn-number choices for `/review`, `/commands`, and `/fork --turn` come from the current thread history.
- `/memories remove`, `/mcp remove`, `/runs`, and `/copy` choices come from the current store, MCP registry, and conversation.
- `/pin move ` offers both directions only for pinned threads in the current project. The probe enters the full ID from the picker row because the move notice, refreshed order, and boundary result are part of the proof.
