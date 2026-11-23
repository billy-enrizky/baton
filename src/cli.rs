use clap::{Parser, Subcommand};

use crate::types::ProviderId;

const QUICK_START: &str = "\
Start here:
  baton                              open the interactive workspace
  baton \"fix the failing test\"        start with a prompt
  baton exec \"summarize this repo\"    one non-interactive turn
  baton orchestrate \"split this task\" workers in isolated worktrees
  baton spawn \"fix the failing test\"   one isolated worker in a worktree
  baton roles                        inspect orchestrator and subagent roles
  baton doctor                       check installed harnesses and config

Inside the workspace, type what you want to do, or /help for common paths.";

#[derive(Parser, Clone)]
#[command(
    name = "baton",
    version,
    about = "One terminal CLI for every coding agent harness. Same thread, any model, native prompt caching.",
    after_help = QUICK_START,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Initial prompt (starts the TUI with this submitted)
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,

    #[arg(short = 'C', long, global = true)]
    pub cwd: Option<String>,

    #[arg(short = 't', long, global = true)]
    pub thread: Option<String>,

    #[arg(short = 'p', long, global = true)]
    pub provider: Option<String>,

    #[arg(short = 'm', long, global = true)]
    pub model: Option<String>,

    /// Reasoning effort for exec, new, or interactive threads (low, medium, high, xhigh, max; default clears)
    #[arg(long, global = true, value_name = "LEVEL")]
    pub effort: Option<String>,

    /// Session mode: single or orchestrate
    #[arg(long, global = true, value_name = "MODE")]
    pub mode: Option<String>,

    /// Skip all permission checks (bypass permissions)
    #[arg(long, global = true, alias = "dangerously-skip-permissions")]
    pub yolo: bool,

    /// Auto-accept file edits, ask for shell commands
    #[arg(long, global = true)]
    pub full_auto: bool,

    /// Plan mode (read-only, proposes plans)
    #[arg(long, global = true)]
    pub plan: bool,

    #[arg(long, global = true)]
    pub claude_home: Option<String>,

    #[arg(long, global = true)]
    pub codex_home: Option<String>,

    #[arg(long, global = true, default_value_t = 50)]
    pub limit: usize,

    #[arg(long, global = true)]
    pub dry_run: bool,
}

#[derive(Subcommand, Clone)]
pub enum Commands {
    /// Run a prompt non-interactively and print the result
    Exec {
        prompt: Vec<String>,
        /// Emit a JSON result object instead of plain text
        #[arg(long)]
        json: bool,
        /// Emit one JSON event per line, then the final result object
        #[arg(long, conflicts_with = "json")]
        stream_json: bool,
    },
    /// Run one task in an isolated worktree and return the worker thread
    Spawn {
        prompt: Vec<String>,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Update baton to the latest version
    Update,
    /// Open the interactive workspace (default)
    Chat,
    /// Serve the local thread store over MCP stdio for coding agents
    Mcp,
    /// Inspect every turn's tool calls, diff, and usage
    Activity {
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    #[command(visible_alias = "list")]
    Threads {
        /// Emit a JSON array
        #[arg(long)]
        json: bool,
    },
    /// List threads that need attention across all projects
    Inbox {
        /// Emit a JSON array
        #[arg(long)]
        json: bool,
    },
    /// Print a shell completion script
    Completions {
        /// Shell to generate for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Start a new thread with a title
    New {
        title: Vec<String>,
    },
    /// List, add, remove, or clear governed project and global memories
    Memories {
        #[command(subcommand)]
        action: Option<MemoriesCommand>,
        /// Emit JSON
        #[arg(long, global = true)]
        json: bool,
    },
    /// Branch a thread's conversation into a new thread
    Fork {
        title: Vec<String>,
        /// Copy only messages and turns through this completed turn
        #[arg(long)]
        turn: Option<i64>,
        /// Fork the provider's native session when supported (Codex and OpenCode)
        #[arg(long)]
        native: bool,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Split a goal into worker tasks and run each harness in its own worktree
    Orchestrate {
        /// Goal for the coordinator to split
        goal: Vec<String>,
        /// Maximum number of worker tasks
        #[arg(long, default_value_t = 4)]
        max_tasks: usize,
        /// Read a plan JSON file instead of asking the coordinator
        #[arg(long = "plan-file")]
        plan_file: Option<String>,
        /// Print the resolved plan without creating a run or starting workers
        #[arg(long = "plan-only", conflicts_with_all = ["list", "run", "archive", "integrate", "open_prs"])]
        plan_only: bool,
        /// List recent orchestration runs
        #[arg(long, conflicts_with = "run")]
        list: bool,
        /// Include archived runs with --list
        #[arg(long, requires = "list")]
        all: bool,
        /// Show tasks for an orchestration run id
        #[arg(long, conflicts_with = "list")]
        run: Option<String>,
        /// Hide a completed run from the default run lists without deleting it
        #[arg(long, value_name = "run-id", conflicts_with_all = ["list", "run", "integrate", "open_prs"])]
        archive: Option<String>,
        /// Merge completed worker branches into the current repository
        #[arg(long, value_name = "run-id", conflicts_with_all = ["list", "run", "open_prs"])]
        integrate: Option<String>,
        /// Push completed worker branches and open one pull request per worker
        #[arg(long = "open-prs", value_name = "run-id", conflicts_with_all = ["list", "run", "integrate"])]
        open_prs: Option<String>,
        /// Worker number to integrate or ship (repeatable, 1-based; default all eligible)
        #[arg(long = "task", value_name = "N")]
        tasks: Vec<usize>,
        /// Print worker progress lines to stderr
        #[arg(long)]
        progress: bool,
        /// Emit one JSON progress event per line, then the final report
        #[arg(long, conflicts_with_all = ["json", "progress"])]
        stream_json: bool,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// List or clean orchestration worktrees
    Worktrees {
        /// Remove clean worktrees whose branch is merged into the current branch
        #[arg(long)]
        clean: bool,
        /// Also remove dirty or unmerged worktrees
        #[arg(long, requires = "clean")]
        force: bool,
        /// Only worktrees whose run id starts with this value
        #[arg(long)]
        run: Option<String>,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Rewind a thread to before a turn
    Rewind {
        thread_id: Option<String>,
        /// Remove this turn and every later turn
        #[arg(long)]
        turn: i64,
        /// Also revert the provider's native history when supported (Codex and OpenCode)
        #[arg(long)]
        native: bool,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Import recent native provider sessions as threads
    Import,
    /// List checkpoints for a thread
    Checkpoints {
        thread_id: String,
    },
    /// Show a checkpoint diff against the working tree
    Diff {
        thread_id: String,
        turn: Option<i64>,
    },
    /// Restore a checkpoint into the working tree
    Restore {
        thread_id: String,
        turn: i64,
    },
    /// Show token usage and cache hit rates
    Usage {
        thread_id: Option<String>,
    },
    /// Inspect orchestrator, planner, implementor, and reviewer roles
    Roles {
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Show or change a provider billing mode
    Auth {
        provider: Option<String>,
        mode: Option<String>,
    },
    /// Sign in to a provider
    Login {
        provider: String,
    },
    /// Set the default provider (used by exec and new threads)
    Default {
        provider: String,
    },
    /// Open this thread in the provider's native CLI
    Resume {
        /// Fork the native session instead of resuming it in place
        #[arg(long)]
        fork: bool,
        /// Print the command instead of running it
        #[arg(long)]
        print: bool,
        /// Resume even if the thread has an open turn
        #[arg(long)]
        force: bool,
    },
    /// Show thread phase, goal, queue, and usage
    Status {
        thread_id: Option<String>,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Search thread titles and conversation text
    Search {
        query: Vec<String>,
        /// Search every project instead of the current directory
        #[arg(long)]
        all: bool,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// List project files for terminal quick-open
    Files {
        /// Optional path query
        query: Vec<String>,
        /// Include gitignored files in the ranked results
        #[arg(long)]
        ignored: bool,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Open a project file in VISUAL or EDITOR
    Open {
        /// Path relative to the -C directory, or absolute
        path: String,
        /// Print the editor command instead of running it
        #[arg(long)]
        print: bool,
    },
    /// Search project file contents
    Grep {
        /// Search text
        query: Vec<String>,
        /// Match case exactly
        #[arg(long)]
        case_sensitive: bool,
        /// Match whole words only
        #[arg(long)]
        word: bool,
        /// Treat the query as a regular expression
        #[arg(long)]
        regex: bool,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// List durable turn phases for a thread
    Turns {
        thread_id: Option<String>,
        /// Emit a JSON array instead of lines
        #[arg(long)]
        json: bool,
    },
    /// List or clear durable queued follow-ups
    Queue {
        thread_id: Option<String>,
        /// Remove every queued message
        #[arg(long)]
        clear: bool,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Check provider binaries, configs, database, and tracked processes
    Doctor {
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Measure startup phases so a performance change has a before and after
    Bench {
        /// What to measure; only `startup` exists
        #[arg(default_value = "startup")]
        target: String,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
        /// Run the measured phases this many times and report every sample
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
    /// List provider processes tracked by baton
    Ps {
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
}

/// The interactive login command for a provider. Antigravity has no CLI
/// login; it uses Google sign-in or an API key.
pub fn login_command(provider: ProviderId) -> Option<Vec<&'static str>> {
    match provider {
        ProviderId::Codex => Some(vec!["codex", "login"]),
        ProviderId::Claude => Some(vec!["claude", "auth", "login"]),
        ProviderId::Cursor => Some(vec!["cursor-agent", "login"]),
        ProviderId::Grok => Some(vec!["grok", "login"]),
        ProviderId::OpenCode => Some(vec!["opencode", "auth", "login"]),
        ProviderId::Antigravity => None,
    }
}

#[derive(Subcommand, Clone)]
pub enum MemoriesCommand {
    /// Add a memory; project scope unless --global is set
    Add {
        #[arg(long)]
        global: bool,
        text: Vec<String>,
    },
    /// Remove a memory by id
    Remove { id: i64 },
    /// Clear all project memories, or all global memories with --global --yes
    Clear {
        #[arg(long)]
        global: bool,
        #[arg(long)]
        yes: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_conflicts_json_and_stream_json() {
        let error = match Cli::try_parse_from(["baton", "exec", "--json", "--stream-json", "hi"]) {
            Ok(_) => panic!("the two machine formats cannot mix"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn help_includes_a_quick_start() {
        let error = match Cli::try_parse_from(["baton", "--help"]) {
            Ok(_) => panic!("--help should return a display error"),
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(text.contains("Start here:"), "{text}");
        assert!(text.contains("baton orchestrate"), "{text}");
        assert!(text.contains("baton doctor"), "{text}");
    }

    #[test]
    fn orchestrate_stream_json_conflicts_with_json_and_progress() {
        for extra in ["--json", "--progress"] {
            let error = match Cli::try_parse_from([
                "baton",
                "orchestrate",
                "--stream-json",
                extra,
                "do work",
            ]) {
                Ok(_) => panic!("--stream-json cannot mix with {extra}"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }

    #[test]
    fn spawn_accepts_a_task() {
        let cli = Cli::try_parse_from(["baton", "spawn", "fix", "the", "test"]).unwrap();
        match cli.command {
            Some(Commands::Spawn { prompt, json }) => {
                assert_eq!(prompt, vec!["fix", "the", "test"]);
                assert!(!json);
            }
            _ => panic!("expected the spawn command"),
        }
    }

    #[test]
    fn login_uses_the_installed_cursor_binary() {
        assert_eq!(
            login_command(ProviderId::Cursor),
            Some(vec!["cursor-agent", "login"])
        );
        assert_eq!(login_command(ProviderId::Antigravity), None);
    }

    #[test]
    fn activity_uses_the_global_thread_and_json_flags() {
        let cli = Cli::try_parse_from(["baton", "-t", "thread-1", "activity", "--json"]).unwrap();
        assert_eq!(cli.thread.as_deref(), Some("thread-1"));
        match cli.command {
            Some(Commands::Activity { json: true }) => {}
            _ => panic!("expected activity"),
        }
    }

    #[test]
    fn files_accepts_query_ignored_and_json() {
        let cli = Cli::try_parse_from(["baton", "files", "parser", "--ignored", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Files {
                query,
                ignored: true,
                json: true,
            }) => assert_eq!(query, vec!["parser"]),
            _ => panic!("expected files"),
        }
    }

    #[test]
    fn grep_accepts_flags_and_json() {
        let cli = Cli::try_parse_from([
            "baton",
            "grep",
            "alpha",
            "--case-sensitive",
            "--word",
            "--regex",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Grep {
                query,
                case_sensitive: true,
                word: true,
                regex: true,
                json: true,
            }) => assert_eq!(query, vec!["alpha"]),
            _ => panic!("expected grep"),
        }
    }

    #[test]
    fn open_accepts_path_and_print() {
        let cli = Cli::try_parse_from(["baton", "open", "src/main.rs", "--print"]).unwrap();
        match cli.command {
            Some(Commands::Open { path, print: true }) => {
                assert_eq!(path, "src/main.rs");
            }
            _ => panic!("expected open"),
        }
    }

    #[test]
    fn inbox_accepts_json() {
        let cli = Cli::try_parse_from(["baton", "inbox", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Inbox { json: true }) => {}
            _ => panic!("expected inbox"),
        }
    }

    #[test]
    fn completions_accept_every_supported_shell() {
        use clap::CommandFactory;

        for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
            let cli = Cli::try_parse_from(["baton", "completions", shell]).unwrap();
            assert!(
                matches!(cli.command, Some(Commands::Completions { .. })),
                "expected completions for {shell}"
            );
        }
        let mut command = Cli::command();
        let mut output = Vec::new();
        clap_complete::generate(
            clap_complete::Shell::Bash,
            &mut command,
            "baton",
            &mut output,
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("baton"), "completion script is not empty");
    }

    #[test]
    fn memories_use_the_global_cwd_and_json_flag() {
        let cli = Cli::try_parse_from(["baton", "-C", "/tmp/project", "memories", "--json"]).unwrap();
        assert_eq!(cli.cwd.as_deref(), Some("/tmp/project"));
        match cli.command {
            Some(Commands::Memories { action: None, json: true }) => {}
            _ => panic!("expected memories list"),
        }
        let cli = Cli::try_parse_from(["baton", "memories", "add", "--global", "Use", "tabs", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Memories { action: Some(MemoriesCommand::Add { global: true, text }), json: true }) => {
                assert_eq!(text, vec!["Use", "tabs"]);
            }
            _ => panic!("expected memories add"),
        }
    }

    #[test]
    fn exec_accepts_stream_json_alone() {
        let cli = Cli::try_parse_from(["baton", "exec", "--stream-json", "hi"]).unwrap();
        match cli.command {
            Some(Commands::Exec { json, stream_json, .. }) => {
                assert!(stream_json);
                assert!(!json);
            }
            _ => panic!("expected the exec command"),
        }
    }

    #[test]
    fn roles_accepts_json() {
        let cli = Cli::try_parse_from(["baton", "roles", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Roles { json: true }) => {}
            _ => panic!("expected roles --json"),
        }
    }
}
