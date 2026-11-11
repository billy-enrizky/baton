//! Opens a thread in its provider's native CLI without losing context.
//!
//! Baton already stores the native session id per provider. This module is
//! intentionally just the argv table: no protocol work, no session migration.

use anyhow::Result;

use crate::types::{ProviderId, TurnPhase};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl ResumeCommand {
    pub fn shell_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_./:=@%+,".contains(character))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Refuses to launch a native CLI while baton still considers a turn open.
/// `--force` exists for deliberate recovery from a dead process.
pub fn ensure_no_open_turn(phase: Option<TurnPhase>, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    if let Some(phase) = phase.filter(|phase| {
        !phase.is_terminal()
    }) {
        anyhow::bail!(
            "thread has an open {} turn; wait for it, use `baton status`, or pass --force to launch the native CLI anyway",
            phase.as_str()
        );
    }
    Ok(())
}

/// Builds the native CLI command for resuming a provider session.
///
/// Codex restores the saved cwd, so `-c tui.resume_cwd=current` is required
/// when the caller changes directories. Do not replay permission flags: the
/// native session may have changed them since baton recorded the thread.
pub fn build(provider: ProviderId, session_id: &str, fork: bool) -> Result<ResumeCommand> {
    let (program, args) = match provider {
        ProviderId::Claude => {
            let mut args = vec!["--resume".to_string(), session_id.to_string()];
            if fork {
                args.push("--fork-session".to_string());
            }
            ("claude", args)
        }
        ProviderId::Codex => {
            let subcommand = if fork { "fork" } else { "resume" };
            (
                "codex",
                vec![
                    subcommand.to_string(),
                    "-c".to_string(),
                    "tui.resume_cwd=current".to_string(),
                    session_id.to_string(),
                ],
            )
        }
        ProviderId::OpenCode => {
            let mut args = vec!["--session".to_string(), session_id.to_string()];
            if fork {
                args.push("--fork".to_string());
            }
            ("opencode", args)
        }
        ProviderId::Grok => {
            let mut args = vec!["--resume".to_string(), session_id.to_string()];
            if fork {
                args.push("--fork-session".to_string());
            }
            ("grok", args)
        }
        ProviderId::Cursor => {
            if fork {
                anyhow::bail!("cursor-agent does not support forking a session");
            }
            ("cursor-agent", vec!["--resume".to_string(), session_id.to_string()])
        }
        ProviderId::Antigravity => {
            if fork {
                anyhow::bail!("antigravity does not support forking a conversation");
            }
            ("antigravity", vec!["--conversation".to_string(), session_id.to_string()])
        }
    };
    Ok(ResumeCommand {
        program: program.to_string(),
        args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_guard_blocks_open_turns_unless_forced() {
        assert!(ensure_no_open_turn(None, false).is_ok());
        assert!(ensure_no_open_turn(Some(TurnPhase::Completed), false).is_ok());
        assert!(ensure_no_open_turn(Some(TurnPhase::Failed), false).is_ok());
        let error = ensure_no_open_turn(Some(TurnPhase::Running), false).unwrap_err();
        assert!(error.to_string().contains("open running turn"));
        assert!(ensure_no_open_turn(Some(TurnPhase::Running), true).is_ok());
    }

    #[test]
    fn claude_resume_and_fork() {
        let resume = build(ProviderId::Claude, "s-1", false).unwrap();
        assert_eq!(resume.program, "claude");
        assert_eq!(resume.args, vec!["--resume", "s-1"]);
        let fork = build(ProviderId::Claude, "s-1", true).unwrap();
        assert_eq!(fork.args, vec!["--resume", "s-1", "--fork-session"]);
    }

    #[test]
    fn codex_resume_pins_the_current_working_directory() {
        let resume = build(ProviderId::Codex, "s-2", false).unwrap();
        assert_eq!(
            resume.args,
            vec!["resume", "-c", "tui.resume_cwd=current", "s-2"]
        );
        let fork = build(ProviderId::Codex, "s-2", true).unwrap();
        assert_eq!(
            fork.args,
            vec!["fork", "-c", "tui.resume_cwd=current", "s-2"]
        );
    }

    #[test]
    fn opencode_resume_and_fork() {
        let resume = build(ProviderId::OpenCode, "s-3", false).unwrap();
        assert_eq!(resume.args, vec!["--session", "s-3"]);
        let fork = build(ProviderId::OpenCode, "s-3", true).unwrap();
        assert_eq!(fork.args, vec!["--session", "s-3", "--fork"]);
    }

    #[test]
    fn providers_without_fork_refuse_instead_of_guessing() {
        assert!(build(ProviderId::Cursor, "s-4", true).is_err());
        assert!(build(ProviderId::Antigravity, "s-5", true).is_err());
    }

    #[test]
    fn shell_line_quotes_spaces_and_quotes() {
        let command = ResumeCommand {
            program: "codex".into(),
            args: vec!["resume".into(), "two words".into(), "it's".into()],
        };
        assert_eq!(command.shell_line(), "codex resume 'two words' 'it'\\''s'");
    }
}
