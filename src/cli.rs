use clap::{Parser, Subcommand};

#[derive(Parser, Clone)]
#[command(
    name = "baton",
    version,
    about = "One terminal CLI for every coding agent harness. Same thread, any model, native prompt caching.",
    disable_help_subcommand = true
)]
pub struct Cli {
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
    Chat,
    #[command(visible_alias = "list")]
    Threads,
    New { title: Vec<String> },
    Import,
    Checkpoints { thread_id: String },
    Diff { thread_id: String, turn: Option<i64> },
    Restore { thread_id: String, turn: i64 },
    Usage { thread_id: Option<String> },
    Auth { provider: Option<String>, mode: Option<String> },
    Login { provider: String },
}
