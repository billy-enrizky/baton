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
    },
    /// Update baton to the latest version
    Update,
    Chat,
    #[command(visible_alias = "list")]
    Threads,
    New {
        title: Vec<String>,
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
    },
    /// Show thread phase, goal, queue, and usage
    Status {
        thread_id: Option<String>,
    },
    /// List or clear durable queued follow-ups
    Queue {
        thread_id: Option<String>,
        /// Remove every queued message
        #[arg(long)]
        clear: bool,
    },
}
