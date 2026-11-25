# Command palette

## Sub-features
- Ctrl+K opens a palette with built-in actions, recent threads, and recent files from the current project.
- Typing filters labels and details case-insensitively. Backspace edits, Up/Down wrap, and PageUp/PageDown jump ten rows.
- Enter runs the action through the same slash handler, opens the selected thread, or inserts the file path with a trailing space.
- Esc closes the palette. Ctrl+K is ignored while another picker is open, so it cannot replace work in progress.
- The list windows at ten rows and paints one cursor.

## How to get to it
Press Ctrl+K from the composer.

## Proof
- `ctrl_k_palette_filters_and_runs_an_action`
- `ctrl_k_palette_opens_a_thread`
- `ctrl_k_palette_inserts_a_recent_file`
- `ctrl_k_palette_respects_an_open_picker_and_escape_closes`
- `every_remaining_picker_paints_one_cursor`

## Gotchas
- Actions call `handle_command`, so they behave exactly like the slash form, including opening another picker.
- The palette is a snapshot built on open. A thread created or a file picked after that appears on the next open.
