# Shell command

## Sub-features
- A composer line that starts with `!` runs one command in the thread working directory. The line is not sent to the harness.
- stdout and stderr are captured together, sanitized for terminal control sequences, and capped at 64 KB. The result is marked truncated when the cap is reached.
- The command runs on a background thread with a 60-second timeout. The TUI stays responsive and shows `shell command running` in the composer hint.
- The transcript area shows `shell · !<command> · exit N` and the output. `/clear` closes it.
- One command runs at a time; a second `!` line is refused with a notice.
- A prompt snippet whose body starts with `!` runs through the same shell path after `{{args}}` and `{{cwd}}` expand. The snippet is not submitted to the harness.

## How to get to it
Type `!printf 'hello\n'` and press Enter.

## Proof
- `shell::tests::runs_and_captures_stdout_stderr_and_exit_code`
- `shell::tests::output_is_bounded_and_marked`
- `shell::tests::a_hung_command_times_out_and_is_killed`
- `shell::tests::control_sequences_are_sanitized`
- `tui::tests::shell_command_runs_in_the_background_and_renders`
- `tui::tests::shell_command_needs_a_command`
- `tui::tests::a_shell_snippet_runs_the_shell_path`
- PTY: `.agents/skills/verify-baton/scripts/tui_probe.py --command '!printf shell-probe' --expect 'shell · !printf shell-probe' --expect shell-probe`

## Gotchas
- This is not an interactive terminal. Commands that need a TTY, such as editors or pagers, are not supported yet.
- The child runs in its own process group and is killed by the shared timeout helper. Baton records no provider process row for it.
- The output is not added to the provider context. Copy what you need into the next prompt.
