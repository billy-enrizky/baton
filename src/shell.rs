//! One-shot shell commands from the composer. Output is captured, bounded, and
//! sanitized before it reaches the terminal. This is not an interactive PTY.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct ShellResult {
    pub command: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub truncated: bool,
}

pub fn default_shell() -> PathBuf {
    #[cfg(windows)]
    {
        return std::env::var_os("COMSPEC")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("cmd.exe"));
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("/bin/sh"))
    }
}

pub fn run(cwd: &Path, command: &str) -> Result<ShellResult> {
    run_with_shell(&default_shell(), cwd, command, DEFAULT_TIMEOUT)
}

pub fn run_with_shell(
    shell: &Path,
    cwd: &Path,
    command: &str,
    timeout: Duration,
) -> Result<ShellResult> {
    let command = command.trim();
    if command.is_empty() {
        bail!("usage: !<shell command>");
    }
    let mut cmd = Command::new(shell);
    #[cfg(windows)]
    cmd.arg("/C").arg(command);
    #[cfg(not(windows))]
    cmd.arg("-lc").arg(command);
    cmd.current_dir(cwd);
    let output = crate::git_workflow::run_command_with_timeout(cmd, "shell command", timeout)?;

    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    let truncated = text.len() > MAX_OUTPUT_BYTES;
    if truncated {
        let end = text.floor_char_boundary(MAX_OUTPUT_BYTES);
        text.truncate(end);
        text.push_str("\n[output truncated]");
    }
    let output_text = crate::sanitize::sanitize_terminal_text(&text).into_owned();
    Ok(ShellResult {
        // The command is echoed in the notice and the overlay header, so it
        // gets the same treatment as the output.
        command: crate::sanitize::sanitize_terminal_text(command).into_owned(),
        output: output_text,
        exit_code: output.status.code(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn runs_and_captures_stdout_stderr_and_exit_code() {
        let cwd = std::env::temp_dir();
        let result = run_with_shell(
            Path::new("/bin/sh"),
            &cwd,
            "printf 'out'; printf 'err' >&2; exit 3",
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(result.output.contains("out"), "{}", result.output);
        assert!(result.output.contains("err"), "{}", result.output);
        assert_eq!(result.exit_code, Some(3));
        assert!(!result.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn command_and_output_are_sanitized_for_echo() {
        let cwd = std::env::temp_dir();
        let result = run_with_shell(
            Path::new("/bin/sh"),
            &cwd,
            "printf 'out' # \u{202e}",
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(!result.command.contains('\u{202e}'), "{}", result.command);
        assert!(result.command.contains("printf 'out'"));
        assert!(!result.output.contains('\u{202e}'), "{}", result.output);
    }

    #[cfg(unix)]
    #[test]
    fn output_is_bounded_and_marked() {
        let cwd = std::env::temp_dir();
        let result = run_with_shell(
            Path::new("/bin/sh"),
            &cwd,
            "yes x | head -c 200000",
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(result.truncated);
        assert!(result.output.len() <= MAX_OUTPUT_BYTES + 32);
        assert!(result.output.ends_with("[output truncated]"));
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_command_times_out_and_is_killed() {
        let cwd = std::env::temp_dir();
        let error = run_with_shell(
            Path::new("/bin/sh"),
            &cwd,
            "sleep 5",
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("timed out"), "{error:#}");
    }

    #[test]
    fn an_empty_command_is_rejected() {
        let cwd = std::env::temp_dir();
        let error =
            run_with_shell(Path::new("/bin/sh"), &cwd, "   ", Duration::from_secs(1)).unwrap_err();
        assert!(format!("{error:#}").contains("usage: !<shell command>"));
    }

    #[cfg(unix)]
    #[test]
    fn control_sequences_are_sanitized() {
        let cwd = std::env::temp_dir();
        let result = run_with_shell(
            Path::new("/bin/sh"),
            &cwd,
            "printf '\\033[31mred'",
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(!result.output.contains('\u{1b}'), "{:?}", result.output);
        assert!(result.output.contains('\u{fffd}'), "{:?}", result.output);
    }
}
