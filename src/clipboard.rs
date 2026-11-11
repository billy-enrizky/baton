//! Small clipboard bridge with no GUI dependency. Supports an explicit
//! command override for tests and unusual environments.

use anyhow::Result;
use std::io::Write;
use std::process::{Command, Stdio};

fn run_command(command: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("{command} exited with {status}");
    }
    Ok(())
}

pub fn copy(text: &str) -> Result<()> {
    if let Ok(command) = std::env::var("BATON_CLIPBOARD_COMMAND") {
        let command = command.trim();
        if !command.is_empty() {
            return run_command(command, &[], text);
        }
    }
    let candidates: &[(&str, &[&str])] = match std::env::consts::OS {
        "macos" => &[("pbcopy", &[])],
        "windows" => &[("clip", &[])],
        _ => &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ],
    };
    let mut errors = Vec::new();
    for (command, args) in candidates {
        match run_command(command, args, text) {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(format!("{command}: {error}")),
        }
    }
    anyhow::bail!(
        "no clipboard command worked ({})",
        errors.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_receives_the_text_on_stdin() {
        run_command("cat", &[], "clipboard probe").unwrap();
    }
}
