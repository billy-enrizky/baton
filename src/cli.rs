use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

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

    /// Internal: act as a trusted child of the process that owns the live host claim
    #[arg(long, global = true, hide = true, value_name = "UUID")]
    pub host_id: Option<String>,
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
        /// Do not run the reviewer after a worker follow-up commit
        #[arg(long = "no-review")]
        no_review: bool,
    },
    /// Run one task in an isolated worktree and return the worker thread
    Spawn {
        prompt: Vec<String>,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Create a linked worktree for a GitHub issue
    Task {
        /// Issue number on the origin repository
        number: u64,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Update baton to the latest version
    Update,
    /// Open the interactive workspace (default)
    Chat,
    /// Claim the local store as the host and idle until stopped
    Serve {
        /// Serve the spawner pipe instead of the local socket
        #[arg(long, conflicts_with = "socket")]
        stdio: bool,
        /// Override the local socket path (default: BATON_HOME/serve.sock)
        #[arg(long, value_name = "PATH")]
        socket: Option<String>,
    },
    /// Connect to a paired host and print its status
    #[command(group(
        clap::ArgGroup::new("mutation")
            .args(["submit", "respond", "answer"])
            .multiple(true)
    ))]
    Attach {
        /// Pairing URL from the host terminal pair command
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
        /// Thread id for thread status or message mode
        #[arg(long, value_name = "ID")]
        thread: Option<String>,
        /// Read the thread's stored messages instead of host status
        #[arg(long, requires = "thread")]
        messages: bool,
        /// Resume after this message id
        #[arg(long, requires = "thread", default_value_t = 0)]
        after: i64,
        /// Maximum messages per page (1 to 200)
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Keep polling until the thread is idle
        #[arg(long, requires = "thread", conflicts_with = "json")]
        follow: bool,
        /// Submit one prompt to the thread over the socket
        #[arg(long, value_name = "TEXT", requires = "thread")]
        submit: Option<String>,
        /// List pending approvals from the host
        #[arg(
            long,
            conflicts_with_all = ["submit", "messages", "follow", "respond", "after"]
        )]
        approvals: bool,
        /// Answer one pending approval request
        #[arg(
            long,
            value_name = "REQUEST",
            requires = "choice",
            conflicts_with_all = ["submit", "messages", "follow", "approvals", "after"]
        )]
        respond: Option<String>,
        /// Choice id for --respond
        #[arg(long, value_name = "CHOICE", requires = "respond")]
        choice: Option<String>,
        /// List pending questions from the host
        #[arg(
            long,
            conflicts_with_all = [
                "submit",
                "messages",
                "follow",
                "approvals",
                "respond",
                "answer",
                "after"
            ]
        )]
        questions: bool,
        /// Answer one pending question request
        #[arg(
            long,
            value_name = "REQUEST",
            requires = "answers",
            conflicts_with_all = [
                "submit",
                "messages",
                "follow",
                "approvals",
                "respond",
                "questions",
                "after"
            ]
        )]
        answer: Option<String>,
        /// JSON object mapping prompt ids to a string or array of strings
        #[arg(long, value_name = "JSON", requires = "answer")]
        answers: Option<String>,
        /// Reuse a key so a retried submit or respond returns the original result
        #[arg(long, value_name = "KEY", requires = "mutation")]
        idempotency_key: Option<String>,
    },
    /// Inspect or change host ownership of the local store
    Host {
        #[command(subcommand)]
        command: HostCommand,
    },
    /// List or revoke pairing grants
    Grant {
        #[command(subcommand)]
        command: GrantCommand,
    },
    /// Inspect append-only audit events
    Audit {
        #[command(subcommand)]
        command: AuditCommand,
    },
    /// Serve the local thread store over MCP stdio or HTTP for coding agents
    Mcp {
        /// Listen for MCP HTTP requests on this host:port
        #[arg(long, value_name = "ADDR", requires = "token_file")]
        http: Option<String>,
        /// File containing the bearer token (mode 0600)
        #[arg(long, value_name = "PATH", requires = "http")]
        token_file: Option<PathBuf>,
        /// Allow a non-loopback HTTP bind on a trusted overlay
        #[arg(long, requires = "http")]
        trusted_overlay: bool,
        /// PEM certificate chain for HTTPS; requires --tls-key
        #[arg(long, value_name = "PATH", requires_all = ["http", "tls_key"], conflicts_with = "trusted_overlay")]
        tls_cert: Option<PathBuf>,
        /// PEM private key for HTTPS; requires --tls-cert
        #[arg(long, value_name = "PATH", requires_all = ["http", "tls_cert"], conflicts_with = "trusted_overlay")]
        tls_key: Option<PathBuf>,
        /// Hide fork and refuse queue clearing
        #[arg(long)]
        read_only: bool,
        /// Allow only these MCP tools; repeat for several (default: all)
        #[arg(long = "allow-tool", value_name = "NAME", conflicts_with = "read_only", action = clap::ArgAction::Append)]
        allow_tool: Vec<String>,
    },
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
        /// Include archived threads
        #[arg(long)]
        all: bool,
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
        /// Maximum number of worker tasks (1-16)
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
        /// Re-run the reviewer over completed worker commits
        #[arg(long, value_name = "run-id", conflicts_with_all = ["list", "run", "archive", "integrate", "open_prs", "plan_only"])]
        review: Option<String>,
        /// Merge completed worker branches into the current repository
        #[arg(long, value_name = "run-id", conflicts_with_all = ["list", "run", "open_prs"])]
        integrate: Option<String>,
        /// Push completed worker branches and open one pull request per worker
        #[arg(long = "open-prs", value_name = "run-id", conflicts_with_all = ["list", "run", "integrate"])]
        open_prs: Option<String>,
        /// Worker number to review, integrate, or ship (repeatable, 1-based; default all eligible)
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
    /// Inspect or settle orchestration runs
    Runs {
        #[command(subcommand)]
        command: RunCommand,
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
        /// Allow a restore in the main checkout or over uncommitted changes
        #[arg(long)]
        force: bool,
    },
    /// Show token usage and cache hit rates
    Usage {
        thread_id: Option<String>,
        /// Group the totals instead of printing one row per thread
        #[arg(long, value_enum, value_name = "GROUP")]
        by: Option<UsageGroupBy>,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
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
    /// Inspect where each provider's API key comes from, without printing it
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
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
    /// List subagent calls recorded for a thread
    Agents {
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Manage cron schedules owned by baton serve
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
}

#[derive(Subcommand, Clone)]
pub enum ScheduleCommand {
    /// Add a cron schedule that posts a turn into a thread
    Add {
        /// Cron expression in UTC: minute hour day month weekday
        #[arg(long)]
        cron: String,
        /// Prompt to post on each run
        #[arg(long)]
        prompt: String,
        /// Thread id or prefix; defaults to the latest thread in this directory
        #[arg(long)]
        thread: Option<String>,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// List schedules
    List {
        /// Emit a JSON array instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Remove a schedule by id or unique prefix
    Remove {
        id: String,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Pause a schedule so its due ticks stop firing
    Pause {
        id: String,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Resume a paused schedule and recompute its next run
    Resume {
        id: String,
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
pub enum GrantCommand {
    /// List grants without their secrets
    List {
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Extend a live grant's expiry without rotating its secret
    Renew {
        /// Grant id
        grant_id: String,
        /// Days from now to extend to; a later stored expiry is kept
        #[arg(long, default_value_t = crate::grants::DEFAULT_GRANT_DAYS, value_parser = clap::value_parser!(i64).range(1..=crate::grants::MAX_GRANT_DAYS))]
        days: i64,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
    /// Revoke a grant and delete its client secret
    Revoke {
        /// Grant id
        grant_id: String,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Clone)]
pub enum AuditCommand {
    /// List audit events, newest first
    List {
        /// Maximum rows to print
        #[arg(long, default_value_t = 50, value_name = "N")]
        limit: usize,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Clone)]
pub enum RunCommand {
    /// Settle a running orchestration with an explicit final status
    Settle {
        /// Run id or an unambiguous prefix
        run_id: String,
        /// Final status to record
        #[arg(long, value_enum, value_name = "STATUS")]
        status: RunStatus,
        /// Also settle a run whose recorded owner process is still alive
        #[arg(long)]
        force: bool,
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Completed,
    Partial,
    Failed,
    Cancelled,
    #[value(name = "needs_review", alias = "needs-review")]
    NeedsReview,
    #[value(name = "needs_input", alias = "needs-input")]
    NeedsInput,
}

impl RunStatus {
    pub fn verdict(self) -> crate::store::RunVerdict {
        match self {
            RunStatus::Completed => crate::store::RunVerdict::Completed,
            RunStatus::Partial => crate::store::RunVerdict::Partial,
            RunStatus::Failed => crate::store::RunVerdict::Failed,
            RunStatus::Cancelled => crate::store::RunVerdict::Cancelled,
            RunStatus::NeedsReview => crate::store::RunVerdict::NeedsReview,
            RunStatus::NeedsInput => crate::store::RunVerdict::NeedsInput,
        }
    }
}

#[derive(Subcommand, Clone)]
pub enum HostCommand {
    /// Replace a live host claim after a terminal confirmation
    Takeover {
        /// Pid named by the current host claim
        #[arg(long)]
        pid: u32,
    },
}

#[derive(Subcommand, Clone)]
pub enum SecretCommand {
    /// Report each provider's credential source and presence
    Check {
        /// Emit a JSON object instead of lines
        #[arg(long)]
        json: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageGroupBy {
    Day,
    Model,
    Provider,
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
    fn exec_accepts_no_review() {
        let cli = Cli::try_parse_from(["baton", "exec", "--no-review", "hi"]).unwrap();
        match cli.command {
            Some(Commands::Exec { no_review, .. }) => assert!(no_review),
            _ => panic!("expected the exec command"),
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
    fn grant_list_renew_and_revoke_parse() {
        let cli = Cli::try_parse_from(["baton", "grant", "list", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Grant {
                command: GrantCommand::List { json: true },
            }) => {}
            _ => panic!("expected grant list"),
        }
        let cli = Cli::try_parse_from([
            "baton",
            "grant",
            "revoke",
            "00000000-0000-4000-8000-000000000000",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Grant {
                command:
                    GrantCommand::Revoke {
                        grant_id,
                        json: true,
                    },
            }) => assert_eq!(grant_id, "00000000-0000-4000-8000-000000000000"),
            _ => panic!("expected grant revoke"),
        }
        let cli = Cli::try_parse_from(["baton", "grant", "renew", "grant-id"]).unwrap();
        match cli.command {
            Some(Commands::Grant {
                command:
                    GrantCommand::Renew {
                        grant_id,
                        days: crate::grants::DEFAULT_GRANT_DAYS,
                        json: false,
                    },
            }) => assert_eq!(grant_id, "grant-id"),
            _ => panic!("expected grant renew with the default window"),
        }
        let cli = Cli::try_parse_from([
            "baton",
            "grant",
            "renew",
            "00000000-0000-4000-8000-000000000000",
            "--days",
            "7",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Grant {
                command:
                    GrantCommand::Renew {
                        grant_id,
                        days: 7,
                        json: true,
                    },
            }) => assert_eq!(grant_id, "00000000-0000-4000-8000-000000000000"),
            _ => panic!("expected grant renew"),
        }
        for days in ["0", "366"] {
            let parsed =
                Cli::try_parse_from(["baton", "grant", "renew", "grant-id", "--days", days]);
            assert!(parsed.is_err(), "--days {days} must be refused");
        }
    }

    #[test]
    fn audit_list_parses_a_limit_and_json() {
        let cli =
            Cli::try_parse_from(["baton", "audit", "list", "--limit", "25", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Audit {
                command:
                    AuditCommand::List {
                        limit: 25,
                        json: true,
                    },
            }) => {}
            _ => panic!("expected audit list"),
        }
    }

    #[test]
    fn mcp_http_requires_a_token_file() {
        let cli = Cli::try_parse_from([
            "baton",
            "mcp",
            "--http",
            "127.0.0.1:8787",
            "--token-file",
            "/tmp/baton-mcp-token",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Mcp {
                http: Some(addr),
                token_file: Some(path),
                trusted_overlay: false,
                read_only: false,
                allow_tool,
                ..
            }) => {
                assert_eq!(addr, "127.0.0.1:8787");
                assert_eq!(path, PathBuf::from("/tmp/baton-mcp-token"));
                assert!(allow_tool.is_empty());
            }
            _ => panic!("expected MCP HTTP"),
        }
        let missing = Cli::try_parse_from(["baton", "mcp", "--http", "127.0.0.1:8787"]);
        assert!(missing.is_err(), "--http needs --token-file");
        let overlay_without_http = Cli::try_parse_from(["baton", "mcp", "--trusted-overlay"]);
        assert!(
            overlay_without_http.is_err(),
            "--trusted-overlay needs --http"
        );
        let read_only = Cli::try_parse_from(["baton", "mcp", "--read-only"]).unwrap();
        match read_only.command {
            Some(Commands::Mcp {
                http: None,
                token_file: None,
                trusted_overlay: false,
                read_only: true,
                ..
            }) => {}
            _ => panic!("expected read-only MCP stdio"),
        }
        let allow = Cli::try_parse_from([
            "baton",
            "mcp",
            "--allow-tool",
            "baton_status",
            "--allow-tool",
            "baton_turns",
        ])
        .unwrap();
        match allow.command {
            Some(Commands::Mcp {
                allow_tool,
                read_only: false,
                ..
            }) => assert_eq!(allow_tool, vec!["baton_status", "baton_turns"]),
            _ => panic!("expected an MCP tool allowlist"),
        }
        let conflict = Cli::try_parse_from([
            "baton",
            "mcp",
            "--read-only",
            "--allow-tool",
            "baton_status",
        ]);
        assert!(
            conflict.is_err(),
            "--read-only and --allow-tool must not be combined"
        );
        let tls = Cli::try_parse_from([
            "baton",
            "mcp",
            "--http",
            "127.0.0.1:8787",
            "--token-file",
            "/tmp/baton-mcp-token",
            "--tls-cert",
            "/tmp/baton-cert.pem",
            "--tls-key",
            "/tmp/baton-key.pem",
        ])
        .unwrap();
        match tls.command {
            Some(Commands::Mcp {
                tls_cert: Some(cert),
                tls_key: Some(key),
                ..
            }) => {
                assert_eq!(cert, PathBuf::from("/tmp/baton-cert.pem"));
                assert_eq!(key, PathBuf::from("/tmp/baton-key.pem"));
            }
            _ => panic!("expected HTTPS MCP"),
        }
        let missing_key = Cli::try_parse_from([
            "baton",
            "mcp",
            "--http",
            "127.0.0.1:8787",
            "--token-file",
            "/tmp/baton-mcp-token",
            "--tls-cert",
            "/tmp/baton-cert.pem",
        ]);
        assert!(missing_key.is_err(), "--tls-cert needs --tls-key");
        let overlay_conflict = Cli::try_parse_from([
            "baton",
            "mcp",
            "--http",
            "0.0.0.0:8787",
            "--token-file",
            "/tmp/baton-mcp-token",
            "--trusted-overlay",
            "--tls-cert",
            "/tmp/baton-cert.pem",
            "--tls-key",
            "/tmp/baton-key.pem",
        ]);
        assert!(
            overlay_conflict.is_err(),
            "--trusted-overlay and TLS must not be combined"
        );
    }

    #[test]
    fn serve_selects_one_transport() {
        let cli = Cli::try_parse_from(["baton", "serve", "--stdio"]).unwrap();
        match cli.command {
            Some(Commands::Serve {
                stdio: true,
                socket: None,
            }) => {}
            _ => panic!("expected stdio serve"),
        }
        let cli = Cli::try_parse_from(["baton", "serve", "--socket", "/tmp/host.sock"]).unwrap();
        match cli.command {
            Some(Commands::Serve {
                stdio: false,
                socket: Some(path),
            }) => assert_eq!(path, "/tmp/host.sock"),
            _ => panic!("expected socket serve"),
        }
        let error = match Cli::try_parse_from([
            "baton",
            "serve",
            "--stdio",
            "--socket",
            "/tmp/host.sock",
        ]) {
            Ok(_) => panic!("the two transports cannot mix"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn attach_accepts_a_pairing_url_and_json() {
        let cli = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Attach {
                url: Some(url),
                json: true,
                thread: None,
                messages: false,
                after: 0,
                limit: 50,
                follow: false,
                submit: None,
                approvals: false,
                respond: None,
                choice: None,
                questions: false,
                answer: None,
                answers: None,
                idempotency_key: None,
            }) => assert!(url.starts_with("baton://local#")),
            _ => panic!("expected attach"),
        }
        let cli = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--thread",
            "thread",
            "--messages",
            "--after",
            "7",
            "--limit",
            "1",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Attach {
                thread: Some(thread),
                messages: true,
                after: 7,
                limit: 1,
                follow: false,
                ..
            }) => assert_eq!(thread, "thread"),
            _ => panic!("expected a message page"),
        }
        let cli = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--thread",
            "thread",
            "--submit",
            "remote hello",
            "--idempotency-key",
            "remote-1",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Attach {
                thread: Some(thread),
                submit: Some(text),
                idempotency_key: Some(key),
                ..
            }) => {
                assert_eq!(thread, "thread");
                assert_eq!(text, "remote hello");
                assert_eq!(key, "remote-1");
            }
            _ => panic!("expected a submit"),
        }
        let cli = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--approvals",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Attach {
                approvals: true,
                respond: None,
                ..
            }) => {}
            _ => panic!("expected approvals"),
        }
        let cli = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--respond",
            "req-1",
            "--choice",
            "accept",
            "--idempotency-key",
            "answer-1",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Attach {
                respond: Some(request),
                choice: Some(choice),
                idempotency_key: Some(key),
                ..
            }) => {
                assert_eq!(request, "req-1");
                assert_eq!(choice, "accept");
                assert_eq!(key, "answer-1");
            }
            _ => panic!("expected a respond"),
        }
        let missing_choice = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--respond",
            "req-1",
        ]);
        assert!(missing_choice.is_err(), "--respond needs --choice");
        let cli = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--questions",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Attach {
                questions: true,
                answer: None,
                ..
            }) => {}
            _ => panic!("expected questions"),
        }
        let cli = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--answer",
            "question-1",
            "--answers",
            r#"{"features":["CI"]}"#,
            "--idempotency-key",
            "question-key",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Attach {
                answer: Some(request),
                answers: Some(answers),
                idempotency_key: Some(key),
                ..
            }) => {
                assert_eq!(request, "question-1");
                assert_eq!(answers, r#"{"features":["CI"]}"#);
                assert_eq!(key, "question-key");
            }
            _ => panic!("expected an answer"),
        }
        let missing_answers = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--answer",
            "question-1",
        ]);
        assert!(missing_answers.is_err(), "--answer needs --answers");
        let key_without_mutation = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--thread",
            "thread",
            "--idempotency-key",
            "remote-1",
        ]);
        assert!(
            key_without_mutation.is_err(),
            "--idempotency-key needs --submit or --respond"
        );
        let conflict = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--thread",
            "thread",
            "--follow",
            "--json",
        ]);
        assert!(conflict.is_err(), "--follow and --json cannot mix");
        let missing_thread = Cli::try_parse_from([
            "baton",
            "attach",
            "--url",
            "baton://local#grant=00000000-0000-4000-8000-000000000000&secret=secret",
            "--messages",
        ]);
        assert!(missing_thread.is_err(), "--messages needs --thread");
    }

    #[test]
    fn runs_settle_accepts_a_status_and_force() {
        let cli = Cli::try_parse_from([
            "baton",
            "runs",
            "settle",
            "3f9c1a2b",
            "--status",
            "needs-review",
            "--force",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Runs {
                command:
                    RunCommand::Settle {
                        run_id,
                        status: RunStatus::NeedsReview,
                        force: true,
                        json: true,
                    },
            }) => assert_eq!(run_id, "3f9c1a2b"),
            _ => panic!("expected runs settle"),
        }
    }

    #[test]
    fn runs_settle_accepts_snake_case_status_names() {
        for (name, status) in [
            ("completed", RunStatus::Completed),
            ("partial", RunStatus::Partial),
            ("failed", RunStatus::Failed),
            ("cancelled", RunStatus::Cancelled),
            ("needs_review", RunStatus::NeedsReview),
            ("needs_input", RunStatus::NeedsInput),
        ] {
            let cli =
                Cli::try_parse_from(["baton", "runs", "settle", "run", "--status", name]).unwrap();
            match cli.command {
                Some(Commands::Runs {
                    command: RunCommand::Settle { status: parsed, .. },
                }) => assert_eq!(parsed, status, "{name}"),
                _ => panic!("expected runs settle"),
            }
        }
    }

    #[test]
    fn runs_settle_rejects_an_unknown_status() {
        let error =
            match Cli::try_parse_from(["baton", "runs", "settle", "run", "--status", "done"]) {
                Ok(_) => panic!("unknown statuses are refused"),
                Err(error) => error,
            };
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
        let error = match Cli::try_parse_from(["baton", "runs", "settle", "run"]) {
            Ok(_) => panic!("a status is required"),
            Err(error) => error,
        };
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
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
