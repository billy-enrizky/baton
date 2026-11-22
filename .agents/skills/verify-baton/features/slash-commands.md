# Slash command choices

## Sub-features
- Typing `/` lists every built-in command.
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

The probe types `/find ` and requires its argument syntax, then types `/mode `, requires all four permission choices and the completion hint, presses Tab to accept `default`, presses Enter, and requires the resulting `permission mode: default` notice. It then types `/model`, requires the model picker header, filter hint, and Enter/Escape affordances, closes it with Escape, and requires a clean `/quit`.

## Gotchas
- The displayed permission banner uses the provider default when the explicit value is `default`. The command notice reports the selected value; the banner reports the effective provider mode.
- `/open` choices come from the current store. A temporary home has no threads, so it may show no choices until a thread is created.
- Turn-number choices for `/review`, `/commands`, and `/fork --turn` come from the current thread history.
- `/memories remove`, `/mcp remove`, `/runs`, and `/copy` choices come from the current store, MCP registry, and conversation.
