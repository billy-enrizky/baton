use clap::{Parser, Subcommand};

#[derive(Parser, Clone)]
#[command(
    name = "baton",
    version,
    about = "One terminal CLI for every coding agent harness. Same thread, any model, native prompt caching.",
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
    },
    /// Update baton to the latest version
    Update,
    Chat,
    /// Serve the local thread store over MCP stdio for coding agents
    Mcp,
    #[command(visible_alias = "list")]
    Threads {
        /// Emit a JSON array
        #[arg(long)]
        json: bool,
    },
    New {
        title: Vec<String>,
    },
    /// Branch a thread's conversation into a new thread
    Fork {
        title: Vec<String>,
        /// Copy only messages and turns through this completed turn
        #[arg(long)]
        turn: Option<i64>,
        /// Fork the provider's native session when supported (currently Codex)
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
        /// List recent orchestration runs
        #[arg(long, conflicts_with = "run")]
        list: bool,
        /// Show tasks for an orchestration run id
        #[arg(long, conflicts_with = "list")]
        run: Option<String>,
        /// Merge completed worker branches into the current repository
        #[arg(long, value_name = "run-id", conflicts_with_all = ["list", "run"])]
        integrate: Option<String>,
        /// Worker number to integrate (repeatable, 1-based; default all completed)
        #[arg(long = "task", value_name = "N")]
        tasks: Vec<usize>,
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
        /// Also revert the provider's native history when supported (currently Codex)
        #[arg(long)]
        native: bool,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    Import,
    Checkpoints {
        thread_id: String,
    },
    Diff {
        thread_id: String,
        turn: Option<i64>,
    },
    Restore {
        thread_id: String,
        turn: i64,
    },
    Usage {
        thread_id: Option<String>,
    },
    Auth {
        provider: Option<String>,
        mode: Option<String>,
    },
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
