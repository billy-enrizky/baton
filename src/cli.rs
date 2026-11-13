use clap::{Parser, Subcommand};

const QUICK_START: &str = "\
Start here:
  baton                              open the interactive workspace
  baton \"fix the failing test\"        start with a prompt
  baton exec \"summarize this repo\"    one non-interactive turn
  baton orchestrate \"split this task\" workers in isolated worktrees
  baton spawn \"fix the failing test\"   one isolated worker in a worktree
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
    #[command(visible_alias = "list")]
    Threads {
        /// Emit a JSON array
        #[arg(long)]
        json: bool,
    },
    /// Start a new thread with a title
    New {
        title: Vec<String>,
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
    /// List provider processes tracked by baton
    Ps {
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
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
}
