# Output sanitization

## What it proves

Provider and child-process text cannot drive the host terminal. Every C0 control except newline and tab, DEL, C1, bidi controls, and the invisible space, non-joiner, and word-joiner are replaced before text reaches `baton exec` stdout or the transcript. U+200D survives so emoji sequences render normally.

## Proof

This surface has no PTY proof, and that is deliberate. Baton's own drawing emits legitimate escape sequences for cursor and color control, so a PTY byte scan cannot separate baton's escapes from an injected one. Use the unit proofs instead.

```sh
cargo test --locked --lib sanitize
cargo test --locked --lib transcript_render_never_emits_child_control_sequences
cargo test --locked --lib shell_overlay_never_emits_child_control_sequences
cargo test --locked --lib shell_notice_sanitizes_the_command
```

`transcript_render_never_emits_child_control_sequences` builds a history row containing an OSC title write, a BEL, and a CSI erase, renders it through `draw`, and asserts the buffer has no ESC or BEL and does contain the replacement character. It fails if the sanitizer is removed from the render path, so it proves the wiring. `shell_overlay_never_emits_child_control_sequences` runs a real `!` command that prints an OSC title, a BEL, and a right-to-left override, then renders the captured result through the overlay: no control or bidi character reaches the buffer and the replacement character does. `shell_notice_sanitizes_the_command` hands the notice a `ShellResult` built without the capture path and still requires a clean notice, so the notice does not depend on its caller. `command_and_output_are_sanitized_for_echo` proves the shell boundary sanitizes the command it stores as well as the output.

## Manual check

Use the unit tests above. A live-provider transcript may echo control lookalike text, but the host terminal never receives the child sequence.

## Limits

- The one-shot `!` shell command sanitizes its captured output and the stored command in `src/shell.rs`, and the overlay and the notice sanitize again; an interactive PTY surface is not implemented.
- JSON output is intentionally raw and must not be sanitized.
- Ratatui already drops C0 at the cell level, so the transcript is defence in depth; the CLI stdout path is the one without a second line of defence.
