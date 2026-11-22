# Output sanitization

## What it proves

Provider and child-process text cannot drive the host terminal. Every C0 control except newline and tab, DEL, C1, bidi controls, and the invisible space, non-joiner, and word-joiner are replaced before text reaches `baton exec` stdout or the transcript. U+200D survives so emoji sequences render normally.

## Proof

This surface has no PTY proof, and that is deliberate. Baton's own drawing emits legitimate escape sequences for cursor and color control, so a PTY byte scan cannot separate baton's escapes from an injected one. Use the unit proofs instead.

```sh
cargo test --locked --lib sanitize
cargo test --locked --lib transcript_render_never_emits_child_control_sequences
```

`transcript_render_never_emits_child_control_sequences` builds a history row containing an OSC title write, a BEL, and a CSI erase, renders it through `draw`, and asserts the buffer has no ESC or BEL and does contain the replacement character. It fails if the sanitizer is removed from the render path, so it proves the wiring.

## Manual check

```sh
printf 'a\033]0;spoofed\007b' | target/debug/baton exec --stdin
```

Needs a live provider, so it is not a CI gate. The expected output shows `a`, the replacement character, and `b`, with no terminal retitle.

## Limits

- Shell overlay and PTY output are not wired yet. That is D9 in `docs/parity.md`.
- JSON output is intentionally raw and must not be sanitized.
- Ratatui already drops C0 at the cell level, so the transcript is defence in depth; the CLI stdout path is the one without a second line of defence.
