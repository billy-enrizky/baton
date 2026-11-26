# Terminal capabilities

## What it proves

The TUI detects the terminal's color depth and degrades RGB colors to the 256-color cube, the 16-color palette, or monochrome. It also refuses to start on a non-interactive terminal instead of failing inside raw mode.

- Truecolor is used only when `COLORTERM` is `truecolor` or `24bit`, or `TERM` contains `direct`. A `TERM` that advertises `256color` and not `direct` gets the nearest xterm-256 index. Every other terminal gets one of the 16 basic colors.
- The 16-color palette is chosen per role, not by nearest RGB. `BASIC_FALLBACKS` maps each theme color to a basic color that carries its meaning: accent to bright yellow, success to bright green, error to bright red, warning to yellow. Nearest-RGB alone reads the cream accent, the success green, and the error red all as the same gray, which erases the difference between them.
- Diff row tints become `Color::Reset` in 16 colors because the basic palette has no dark green or dark red. The `+`/`-` sign and the green or red text still carry the distinction.
- `NO_COLOR` set to a non-empty value renders with `Color::Reset` foreground, background, and underline color for every cell. Text modifiers such as bold and underline survive.
- Selections and the composer caret switch to reverse video in monochrome mode, so both stay visible without color.
- `TERM=dumb` renders the same way even without `NO_COLOR`.
- The TUI needs an interactive stdin and stdout. A pipe or a closed stdin exits non-zero with `baton needs an interactive terminal for the TUI; use baton exec for scripts and pipes`.
- Terminal width and height keep coming from ratatui's resize handling; this slice does not add a second width source.

## Proof

```sh
cargo test --locked --lib capability
cargo test --locked --test core tui_refuses_a_non_interactive_terminal
uv run --no-project python scripts/color_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
uv run --no-project python .agents/skills/verify-baton/scripts/plan_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

`monochrome_backend_strips_palette_colors` draws an RGB-styled, underlined buffer through the wrapper into a `TestBackend` and asserts `Color::Reset` for foreground, background, and underline color while the underline modifier survives. `color_depth_follows_term_and_colorterm` covers truecolor, 24-bit, `-direct`, `-256color`, plain terms, and `NO_COLOR`. `ansi256_mapping_uses_the_cube` proves the nearest-index mapping, `ansi16_mapping_uses_named_basic_colors` proves the named output for a 16-color terminal, and `ansi16_prefers_the_declared_fallback_over_nearest_rgb` proves the role table wins over distance for the accent while leaving 256-color mode untouched. `ansi16_backend_emits_named_colors` and `ansi16_backend_maps_stray_rgb_to_named_colors` prove the backend never hands the terminal an indexed color in 16-color mode, and `basic_fallbacks_cover_every_palette_role` fails when a theme role is added without a 16-color equivalent or when two roles collapse onto one basic color. `ansi_backend_rewrites_rgb_and_keeps_modifiers` and `truecolor_backend_keeps_rgb` prove the remaining backend paths. `scripts/color_probe.py` runs the real TUI under `TERM=xterm`, `TERM=xterm-256color`, and `COLORTERM=truecolor` and checks the emitted SGR: no RGB in the first two, only indices at or below 15 and bright yellow for the accent in 16 colors, at least one index at or above 16 for 256 colors, and RGB for truecolor. `monochrome_input_selection_and_caret_use_reverse_video` and `transcript_selection_uses_reverse_video_in_monochrome` prove the non-color affordances. `no_color_values_decide_monochrome` covers `NO_COLOR` present, empty, absent, and `TERM=dumb`. `tui_refuses_a_non_interactive_terminal` runs the real binary with a null stdin and piped stdout and checks the message.

The plan probe runs with `NO_COLOR=1`, scans the raw transcript for `38;2;`, `38;5;`, `48;2;`, and `48;5;` SGR sequences, and requires at least one reverse-video `7m` sequence. It fails if any palette color reaches the terminal or if the caret and selection affordance disappears while still exercising the plan overlay.

## Manual check

```sh
NO_COLOR=1 baton
TERM=dumb baton
printf '' | baton
```

The first two draw without color and behave normally. The third exits with the interactive-terminal message.

## Limits

- Color is rewritten at the backend, so the theme constants stay RGB and one mapping table serves every depth.
- The 256-color approximation applies only when `TERM` advertises `256color` and no `direct`. A terminal that supports truecolor but advertises neither gets the 16-color palette. That is deliberate: the capability is advertised, not guessed.
- `TERM` with neither `256color` nor `direct` gets the 16-color palette, and colors outside the role table still fall back to nearest RGB.
- Crossterm 0.29 writes every palette color as `38;5;n`, including the named ones, so a true 16-color terminal still receives the extended form with `n` at or below 15. Baton cannot emit the plain `30`-`37`/`90`-`97` form without bypassing the backend that also handles the alternate screen and cursor.
- Mapping runs per unique theme color and is cached in the backend; it does not rescan the palette for every cell.
- Windows console color behavior is covered by the same environment rules, but no Windows CI runner exercises it.
- `baton exec` and the other CLI subcommands are the supported non-interactive path and are unaffected by this gate.
