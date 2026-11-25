# Terminal capabilities

## What it proves

The TUI degrades to a monochrome drawing when the terminal cannot or should not show palette colors, and it refuses to start on a non-interactive terminal instead of failing inside raw mode.

- `NO_COLOR` set to a non-empty value renders with `Color::Reset` foreground, background, and underline color for every cell. Text modifiers such as bold and underline survive.
- Selections and the composer caret switch to reverse video in monochrome mode, so both stay visible without color.
- `TERM=dumb` renders the same way even without `NO_COLOR`.
- The TUI needs an interactive stdin and stdout. A pipe or a closed stdin exits non-zero with `baton needs an interactive terminal for the TUI; use baton exec for scripts and pipes`.
- Terminal width and height keep coming from ratatui's resize handling; this slice does not add a second width source.

## Proof

```sh
cargo test --locked --lib capability
cargo test --locked --test core tui_refuses_a_non_interactive_terminal
uv run --no-project python .agents/skills/verify-baton/scripts/plan_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

`monochrome_backend_strips_palette_colors` draws an RGB-styled, underlined buffer through the wrapper into a `TestBackend` and asserts `Color::Reset` for foreground, background, and underline color while the underline modifier survives; `monochrome_backend_keeps_colors_when_disabled` asserts the same buffer keeps its color when the flag is off. `monochrome_input_selection_and_caret_use_reverse_video` and `transcript_selection_uses_reverse_video_in_monochrome` prove the non-color affordances. `no_color_values_decide_monochrome` covers `NO_COLOR` present, empty, absent, and `TERM=dumb`. `tui_refuses_a_non_interactive_terminal` runs the real binary with a null stdin and piped stdout and checks the message.

The plan probe runs with `NO_COLOR=1`, scans the raw transcript for `38;2;`, `38;5;`, `48;2;`, and `48;5;` SGR sequences, and requires at least one reverse-video `7m` sequence. It fails if any palette color reaches the terminal or if the caret and selection affordance disappears while still exercising the plan overlay.

## Manual check

```sh
NO_COLOR=1 baton
TERM=dumb baton
printf '' | baton
```

The first two draw without color and behave normally. The third exits with the interactive-terminal message.

## Limits

- Color is stripped at the backend, so the theme constants still exist; this is deliberate and keeps the colored path unchanged.
- Windows console color behavior is covered by the same `NO_COLOR` environment rule, but no Windows CI runner exercises it.
- `baton exec` and the other CLI subcommands are the supported non-interactive path and are unaffected by this gate.
