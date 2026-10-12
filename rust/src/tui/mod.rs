use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Terminal;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::adapters::TurnEvent;
use crate::checkpoints::{diff_checkpoint_against_worktree, restore_checkpoint};
use crate::config::{describe_auth, load_config_file, resolve_config, save_config_file};
use crate::handoff::git_branch_label;
use crate::runtime::{ensure_persisted, EngineCommand, EngineEvent};
use crate::store::{BatonStore, MessageRow, NewThread, ThreadRow};
use crate::types::{ApprovalDecision, PermissionMode, ProviderId};
use crate::usage::{cache_hit_rate, format_token_count, format_usage_line, sum_usage};

pub mod viewport;

use viewport::{window_messages, wrapped_rows, ViewportItem};

const COLORS: ColorConstants = ColorConstants {
    accent: Color::Rgb(240, 198, 116),
    secondary: Color::Rgb(196, 181, 253),
    text: Color::Rgb(229, 231, 235),
    muted: Color::Rgb(148, 163, 184),
    border: Color::Rgb(59, 68, 80),
    user_bg: Color::Rgb(48, 54, 61),
    success: Color::Rgb(134, 239, 172),
    error: Color::Rgb(252, 165, 165),
    warning: Color::Rgb(251, 191, 36),
};

struct ColorConstants {
    accent: Color,
    secondary: Color,
    text: Color,
    muted: Color,
    border: Color,
    user_bg: Color,
    success: Color,
    error: Color,
    warning: Color,
}

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const LOGO_ROWS: [&str; 6] = [
    "██████╗  █████╗ █████████╗  ██████╗ ███╗   ██╗",
    "██╔══██╗██╔══██╗╚══██╔══╝ ██╔═══██╗████╗  ██║",
    "██████╔╝███████║   ██║    ██║   ██║██╔██╗ ██║",
    "██╔══██╗██╔══██║   ██║    ██║   ██║██║╚██╗██║",
    "██████╔╝██║  ██║   ██║    ╚██████╔╝██║ ╚████║",
    "╚═════╝ ╚═╝  ╚═╝   ╚═╝     ╚═════╝ ╚═╝  ╚═══╝",
];

const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/switch", "switch harness: /switch <claude|codex> [model]"),
    ("/model", "set model for this thread"),
    ("/mode", "permission mode: default|acceptEdits|bypassPermissions|plan"),
    ("/auth", "billing mode: /auth <provider> <subscription|api>"),
    ("/new", "start a new thread"),
    ("/threads", "list threads in this directory"),
    ("/open", "open a thread by id prefix"),
    ("/usage", "token usage and cache hit rate"),
    ("/checkpoints", "list checkpoints for this thread"),
    ("/diff", "diff a checkpoint against the working tree"),
    ("/rewind", "rewind to a previous message"),
    ("/clear", "clear panels and notices"),
    ("/quit", "exit baton"),
];

#[derive(Debug, Clone, PartialEq)]
struct ToolLine {
    label: String,
    status: ToolStatus,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ToolStatus {
    Running,
    Done,
    Failed,
}

enum Overlay {
    None,
    Approval { summary: String, respond: tokio::sync::oneshot::Sender<ApprovalDecision> },
    RewindPicker { selected: usize },
    CodeConfirm { turn: i64, git_ref: String },
}

pub struct TuiState {
    store: Arc<Mutex<BatonStore>>,
    home: std::path::PathBuf,
    thread: ThreadRow,
    history: Vec<MessageRow>,
    input: String,
    running: bool,
    turn_started: Option<Instant>,
    streaming_text: String,
    tool_lines: Vec<ToolLine>,
    overlay: Overlay,
    notice: Option<String>,
    diff_output: Option<String>,
    permission_mode: PermissionMode,
    scroll_offset: usize,
    esc_primed: bool,
    exit_armed: Option<Instant>,
    branch_label: Option<String>,
    quit: bool,
    suggestion_index: usize,
}

pub fn run_tui(
    store: Arc<Mutex<BatonStore>>,
    home: std::path::PathBuf,
    thread: ThreadRow,
    engine_command_sender: tokio::sync::mpsc::UnboundedSender<EngineCommand>,
    engine_event_receiver: std::sync::mpsc::Receiver<EngineEvent>,
    turn_event_receiver: std::sync::mpsc::Receiver<TurnEvent>,
) -> Result<()> {
    let history = store.lock().unwrap().list_messages(&thread.id)?;
    let branch_label = git_branch_label(std::path::Path::new(&thread.cwd));
    let permission_mode = resolve_config(&home, std::path::Path::new(&thread.cwd))
        .for_provider(thread.current_provider)
        .permission_mode;

    let mut state = TuiState {
        store: store.clone(),
        home,
        thread,
        history,
        input: String::new(),
        running: false,
        turn_started: None,
        streaming_text: String::new(),
        tool_lines: Vec::new(),
        overlay: Overlay::None,
        notice: None,
        diff_output: None,
        permission_mode,
        scroll_offset: 0,
        esc_primed: false,
        exit_armed: None,
        branch_label,
        quit: false,
        suggestion_index: 0,
    };

    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, terminal::EnterAlternateScreen, crossterm::cursor::Hide)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut spinner_frame = 0usize;
    while !state.quit {
        while let Ok(event) = turn_event_receiver.try_recv() {
            apply_turn_event(&mut state, event);
        }
        while let Ok(event) = engine_event_receiver.try_recv() {
            apply_engine_event(&mut state, &store, event);
        }
        if crossterm::event::poll(Duration::from_millis(32))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(&mut state, &store, &engine_command_sender, key);
                }
            }
        }
        spinner_frame = (spinner_frame + 1) % SPINNER_FRAMES.len();
        terminal.draw(|frame| draw(frame, &state, spinner_frame))?;
    }

    terminal::disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        crossterm::cursor::Show,
        terminal::LeaveAlternateScreen
    )?;
    let _ = engine_command_sender.send(EngineCommand::Quit);
    Ok(())
}

fn apply_engine_event(state: &mut TuiState, store: &Arc<Mutex<BatonStore>>, event: EngineEvent) {
    match event {
        EngineEvent::UserEcho { thread_id, turn, text, provider, model } => {
            if thread_id == state.thread.id {
                state.history.push(MessageRow {
                    id: -(turn as i64),
                    thread_id,
                    turn,
                    role: "user".into(),
                    provider: Some(provider.as_str().to_string()),
                    model,
                    text,
                    created_at: String::new(),
                });
                state.scroll_offset = 0;
            }
        }
        EngineEvent::TurnStarted { thread_id } => {
            if thread_id == state.thread.id {
                state.running = true;
                state.turn_started = Some(Instant::now());
                state.streaming_text.clear();
                state.tool_lines.clear();
                state.notice = None;
                state.diff_output = None;
            }
        }
        EngineEvent::TurnFinished { thread_id, ok, error } => {
            if thread_id == state.thread.id {
                state.running = false;
                state.turn_started = None;
                state.streaming_text.clear();
                state.tool_lines.clear();
                {
                    let guard = store.lock().unwrap();
                    state.history = guard.list_messages(&state.thread.id).unwrap_or_default();
                    if let Some(fresh) = guard.get_thread(&state.thread.id).unwrap_or(None) {
                        state.thread = fresh;
                    }
                }
                if !ok {
                    state.notice = Some(format!("turn failed: {}", error.unwrap_or_else(|| "unknown error".into())));
                }
            }
        }
        EngineEvent::Turn(_) => {}
    }
}

fn apply_turn_event(state: &mut TuiState, event: TurnEvent) {
    match event {
        TurnEvent::TextDelta { text } => {
            state.streaming_text.push_str(&text);
        }
        TurnEvent::AssistantText { .. } => {}
        TurnEvent::ToolStarted { tool, detail } => {
            let label = match detail {
                Some(detail) => format!("{tool} {detail}"),
                None => tool,
            };
            state.tool_lines.push(ToolLine { label, status: ToolStatus::Running });
        }
        TurnEvent::ToolCompleted { tool, detail, exit_code } => {
            let label = match detail {
                Some(detail) => format!("{tool} {detail}"),
                None => tool,
            };
            let status = if exit_code.unwrap_or(0) != 0 { ToolStatus::Failed } else { ToolStatus::Done };
            if let Some(line) = state.tool_lines.iter_mut().rev().find(|line| line.status == ToolStatus::Running) {
                line.status = status;
            } else {
                state.tool_lines.push(ToolLine { label, status });
            }
        }
        TurnEvent::ApprovalRequest { summary, respond, .. } => {
            state.overlay = Overlay::Approval { summary, respond };
        }
        TurnEvent::Usage { .. } => {}
        TurnEvent::TurnCompleted { .. } | TurnEvent::TurnFailed { .. } => {}
    }
}

fn suggestions_for(input: &str) -> Vec<&'static (&'static str, &'static str)> {
    if input.starts_with('/') && !input.contains(' ') {
        SLASH_COMMANDS
            .iter()
            .filter(|(command, _)| command.starts_with(input))
            .collect()
    } else {
        Vec::new()
    }
}

fn handle_key(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    engine_command_sender: &tokio::sync::mpsc::UnboundedSender<EngineCommand>,
    key: KeyEvent,
) {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if state.running {
            let _ = engine_command_sender.send(EngineCommand::Interrupt);
            state.notice = Some("interrupting...".into());
            return;
        }
        let now = Instant::now();
        if let Some(armed) = state.exit_armed {
            if now.duration_since(armed) < Duration::from_secs(1) {
                state.quit = true;
                return;
            }
        }
        state.exit_armed = Some(now);
        state.notice = Some("press ctrl+c again to exit".into());
        return;
    }

    match std::mem::replace(&mut state.overlay, Overlay::None) {
        Overlay::Approval { summary, respond } => match key.code {
            KeyCode::Char('y') => {
                let _ = respond.send(ApprovalDecision::Accept);
            }
            KeyCode::Char('a') => {
                let _ = respond.send(ApprovalDecision::AcceptAlways);
            }
            KeyCode::Char('n') => {
                let _ = respond.send(ApprovalDecision::Deny);
            }
            _ => {
                state.overlay = Overlay::Approval { summary, respond };
            }
        },
        Overlay::RewindPicker { mut selected } => {
            let user_messages: Vec<(i64, String)> = state
                .history
                .iter()
                .filter(|message| message.role == "user")
                .map(|message| (message.turn, message.text.clone()))
                .collect();
            match key.code {
                KeyCode::Esc => {}
                KeyCode::Up => {
                    if !user_messages.is_empty() {
                        selected = (selected + user_messages.len() - 1) % user_messages.len();
                        state.overlay = Overlay::RewindPicker { selected };
                    } else {
                        state.overlay = Overlay::RewindPicker { selected };
                    }
                }
                KeyCode::Down => {
                    if !user_messages.is_empty() {
                        selected = (selected + 1) % user_messages.len();
                        state.overlay = Overlay::RewindPicker { selected };
                    } else {
                        state.overlay = Overlay::RewindPicker { selected };
                    }
                }
                KeyCode::Enter => {
                    if let Some((turn, text)) = user_messages.get(selected).cloned() {
                        apply_rewind(state, store, turn, text);
                    }
                }
                _ => {
                    state.overlay = Overlay::RewindPicker { selected };
                }
            }
        }
        Overlay::CodeConfirm { turn, git_ref } => match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let cwd = std::path::PathBuf::from(&state.thread.cwd);
                match restore_checkpoint(&cwd, &git_ref) {
                    Ok(()) => state.notice = Some(format!("restored code to turn {}", turn - 1)),
                    Err(error) => state.notice = Some(format!("restore failed: {error}")),
                }
            }
            _ => {}
        },
        Overlay::None => {
            handle_composer_key(state, store, engine_command_sender, key);
        }
    }
}

fn apply_rewind(state: &mut TuiState, store: &Arc<Mutex<BatonStore>>, turn: i64, text: String) {
    let result = {
        let guard = store.lock().unwrap();
        guard.rewind_thread(&state.thread.id, turn)
    };
    match result {
        Ok(()) => {
            {
                let guard = store.lock().unwrap();
                state.history = guard.list_messages(&state.thread.id).unwrap_or_default();
                if let Some(checkpoint) = guard.get_checkpoint(&state.thread.id, turn - 1).unwrap_or(None) {
                    state.overlay = Overlay::CodeConfirm {
                        turn,
                        git_ref: checkpoint.git_ref,
                    };
                }
            }
            state.scroll_offset = 0;
            state.input = text;
            state.notice = Some(format!("rewound to before turn {turn}; message restored to composer"));
        }
        Err(error) => {
            state.notice = Some(format!("rewind failed: {error}"));
        }
    }
}

fn handle_composer_key(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    engine_command_sender: &tokio::sync::mpsc::UnboundedSender<EngineCommand>,
    key: KeyEvent,
) {
    let suggestions = suggestions_for(&state.input);
    let clamped = state.suggestion_index.min(suggestions.len().saturating_sub(1));

    if !suggestions.is_empty() {
        match key.code {
            KeyCode::Up => {
                state.suggestion_index = if clamped == 0 {
                    suggestions.len() - 1
                } else {
                    clamped - 1
                };
                return;
            }
            KeyCode::Down => {
                state.suggestion_index = (clamped + 1) % suggestions.len();
                return;
            }
            KeyCode::Tab => {
                if let Some((command, _)) = suggestions.get(clamped) {
                    state.input = format!("{command} ");
                    state.suggestion_index = 0;
                }
                return;
            }
            KeyCode::Enter => {
                let typed = state.input.trim().to_string();
                if let Some((command, _)) = suggestions.iter().find(|(command, _)| *command == typed) {
                    let command = command.to_string();
                    state.input.clear();
                    handle_command(state, store, engine_command_sender, &command);
                } else if let Some((command, _)) = suggestions.get(clamped) {
                    state.input = format!("{command} ");
                    state.suggestion_index = 0;
                }
                return;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter => {
            let text = state.input.trim().to_string();
            if text.is_empty() {
                return;
            }
            state.input.clear();
            state.suggestion_index = 0;
            state.esc_primed = false;
            if text.starts_with('/') {
                handle_command(state, store, engine_command_sender, &text);
            } else {
                submit(state, store, engine_command_sender, text);
            }
        }
        KeyCode::Backspace => {
            state.input.pop();
            state.suggestion_index = 0;
            state.esc_primed = false;
        }
        KeyCode::Up => {
            state.scroll_offset += 1;
        }
        KeyCode::Down => {
            state.scroll_offset = state.scroll_offset.saturating_sub(1);
        }
        KeyCode::PageUp => {
            state.scroll_offset += 10;
        }
        KeyCode::PageDown => {
            state.scroll_offset = state.scroll_offset.saturating_sub(10);
        }
        KeyCode::Esc => {
            if state.running {
                let _ = engine_command_sender.send(EngineCommand::Interrupt);
                state.notice = Some("interrupting...".into());
            } else if state.input.is_empty() {
                if state.esc_primed {
                    state.esc_primed = false;
                    state.overlay = Overlay::RewindPicker { selected: 0 };
                } else {
                    state.esc_primed = true;
                }
            } else {
                state.input.clear();
                state.notice = Some("composer cleared".into());
            }
        }
        KeyCode::Char(char) => {
            if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                state.input.push(char);
                state.suggestion_index = 0;
                state.esc_primed = false;
            }
        }
        _ => {}
    }
}

fn submit(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    engine_command_sender: &tokio::sync::mpsc::UnboundedSender<EngineCommand>,
    text: String,
) {
    let result = {
        let guard = store.lock().unwrap();
        ensure_persisted(&guard, &mut state.thread)
    };
    if let Err(error) = result {
        state.notice = Some(format!("cannot start turn: {error}"));
        return;
    }
    let thread_id = state.thread.id.clone();
    let permission_mode = state.permission_mode;
    let _ = engine_command_sender.send(EngineCommand::Submit { thread_id, text, permission_mode });
}

fn handle_command(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    _engine_command_sender: &tokio::sync::mpsc::UnboundedSender<EngineCommand>,
    raw: &str,
) {
    let mut parts = raw.split_whitespace();
    let command = parts.next().unwrap_or("");
    let args: Vec<String> = parts.map(str::to_string).collect();
    match command {
        "/quit" | "/exit" => state.quit = true,
        "/help" => {
            state.notice = Some(
                SLASH_COMMANDS
                    .iter()
                    .map(|(name, description)| format!("{name:<14} {description}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        "/clear" => {
            state.diff_output = None;
            state.notice = None;
        }
        "/switch" => {
            let target = args.first().map(String::as_str);
            let provider = match target {
                Some("claude") => Some(ProviderId::Claude),
                Some("codex") => Some(ProviderId::Codex),
                _ => None,
            };
            match provider {
                None => state.notice = Some("usage: /switch <claude|codex> [model]".into()),
                Some(provider) => {
                    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
                    let default_model = match provider {
                        ProviderId::Claude => resolved.claude.model.clone(),
                        ProviderId::Codex => resolved.codex.model.clone(),
                    };
                    let model = args.get(1).cloned().unwrap_or(default_model);
                    let display_model = model.clone();
                    let result = {
                        let guard = store.lock().unwrap();
                        ensure_persisted(&guard, &mut state.thread)
                            .and_then(|()| guard.set_thread_provider(&state.thread.id, provider, if args.len() > 1 { Some(model) } else { None }))
                    };
                    match result {
                        Ok(()) => {
                            state.permission_mode = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd))
                                .for_provider(provider)
                                .permission_mode;
                            let existing = store
                                .lock()
                                .unwrap()
                                .get_provider_session(&state.thread.id, provider)
                                .unwrap_or(None);
                            state.notice = Some(match existing {
                                Some(session) => format!(
                                    "switched to {provider}/{display_model}, resuming native session {}",
                                    &session.native_session_id[..8.min(session.native_session_id.len())]
                                ),
                                None => format!(
                                    "switched to {provider}/{display_model}. First turn carries a context handoff."
                                ),
                            });
                        }
                        Err(error) => state.notice = Some(format!("switch failed: {error}")),
                    }
                }
            }
        }
        "/model" => {
            match args.first() {
                None => {
                    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
                    let effective = state
                        .thread
                        .current_model
                        .clone()
                        .unwrap_or_else(|| match state.thread.current_provider {
                            ProviderId::Claude => resolved.claude.model.clone(),
                            ProviderId::Codex => resolved.codex.model.clone(),
                        });
                    let source = if state.thread.current_model.is_some() { "thread" } else { "harness config" };
                    state.notice = Some(format!(
                        "current model: {}/{} ({source})",
                        state.thread.current_provider, effective
                    ));
                }
                Some(model) => {
                    let result = {
                        let guard = store.lock().unwrap();
                        ensure_persisted(&guard, &mut state.thread)
                            .and_then(|()| guard.set_thread_model(&state.thread.id, model))
                    };
                    if let Err(error) = result {
                        state.notice = Some(format!("cannot set model: {error}"));
                    }
                }
            }
        }
        "/mode" => {
            match args.first().map(String::as_str).and_then(PermissionMode::parse) {
                Some(mode) => {
                    state.permission_mode = mode;
                    state.notice = Some(format!("permission mode: {mode}"));
                }
                None => {
                    state.notice = Some(format!(
                        "usage: /mode <default|acceptEdits|bypassPermissions|plan> (current: {})",
                        state.permission_mode
                    ));
                }
            }
        }
        "/auth" => {
            let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
            match (args.first().map(String::as_str), args.get(1).map(String::as_str)) {
                (None, _) => {
                    state.notice = Some(format!(
                        "claude: {} | codex: {}",
                        describe_auth(&resolved.claude),
                        describe_auth(&resolved.codex)
                    ));
                }
                (Some(provider), Some(mode)) => {
                    let provider = match ProviderId::parse(provider) {
                        Some(provider) => provider,
                        None => {
                            state.notice = Some("usage: /auth <claude|codex> <subscription|api>".into());
                            return;
                        }
                    };
                    let mode = match crate::config::AuthMode::parse(mode) {
                        Some(mode) => mode,
                        None => {
                            state.notice = Some("usage: /auth <claude|codex> <subscription|api>".into());
                            return;
                        }
                    };
                    let provider_config = match provider {
                        ProviderId::Claude => &resolved.claude,
                        ProviderId::Codex => &resolved.codex,
                    };
                    if mode == crate::config::AuthMode::Api && provider_config.api_key.is_none() {
                        state.notice = Some(format!(
                            "no API key for {provider}. Set {} in the environment, the harness config, or config.json.",
                            provider_config.api_key_env_name
                        ));
                        return;
                    }
                    let mut file = load_config_file(&state.home).unwrap_or_default();
                    {
                        let entry = match provider {
                            ProviderId::Claude => file.claude.get_or_insert_with(Default::default),
                            ProviderId::Codex => file.codex.get_or_insert_with(Default::default),
                        };
                        entry.mode = Some(mode);
                    }
                    if let Err(error) = save_config_file(&state.home, &file) {
                        state.notice = Some(format!("cannot save config: {error}"));
                        return;
                    }
                    state.notice = Some(format!("{provider} billing set to {}", mode.as_str()));
                }
                (Some(provider), None) => {
                    let label = match ProviderId::parse(provider) {
                        Some(ProviderId::Claude) => describe_auth(&resolved.claude),
                        Some(ProviderId::Codex) => describe_auth(&resolved.codex),
                        None => "unknown provider".into(),
                    };
                    state.notice = Some(label);
                }
            }
        }
        "/new" => {
            let title = if args.is_empty() { "New thread".to_string() } else { args.join(" ") };
            state.thread = {
                let guard = store.lock().unwrap();
                guard.draft_thread(NewThread {
                    title,
                    cwd: state.thread.cwd.clone(),
                    current_provider: state.thread.current_provider,
                    current_model: None,
                })
            };
            state.history.clear();
            state.branch_label = None;
            state.diff_output = None;
            state.scroll_offset = 0;
            state.notice = Some("new thread, unsaved until you send a message".into());
        }
        "/threads" => {
            let lines = {
                let guard = store.lock().unwrap();
                let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
                guard
                    .list_threads(Some(state.thread.cwd.as_str()))
                    .unwrap_or_default()
                    .into_iter()
                    .take(20)
                    .map(|thread| {
                        let model = thread.current_model.clone().unwrap_or_else(|| match thread.current_provider {
                            ProviderId::Claude => resolved.claude.model.clone(),
                            ProviderId::Codex => resolved.codex.model.clone(),
                        });
                        format!("{}  {}/{}  {}", &thread.id[..8], thread.current_provider, model, thread.title)
                    })
                    .collect::<Vec<_>>()
            };
            state.notice = Some(if lines.is_empty() {
                "no threads in this directory".into()
            } else {
                lines.join("\n")
            });
        }
        "/open" => {
            let prefix = match args.first() {
                Some(prefix) => prefix.clone(),
                None => {
                    state.notice = Some("usage: /open <threadId>".into());
                    return;
                }
            };
            let found = {
                let guard = store.lock().unwrap();
                guard
                    .list_threads(None)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|thread| thread.id.starts_with(&prefix))
            };
            match found {
                Some(thread) => {
                    state.thread = thread.clone();
                    let guard = store.lock().unwrap();
                    state.history = guard.list_messages(&thread.id).unwrap_or_default();
                    state.branch_label = git_branch_label(std::path::Path::new(&thread.cwd));
                    state.diff_output = None;
                    state.scroll_offset = 0;
                    state.notice = Some(format!("opened {}", thread.title));
                }
                None => state.notice = Some(format!("no thread matches \"{prefix}\"")),
            }
        }
        "/usage" => {
            let lines = {
                let guard = store.lock().unwrap();
                let rows = guard.list_usage(Some(&state.thread.id)).unwrap_or_default();
                if rows.is_empty() {
                    None
                } else {
                    Some(format_usage_line(&sum_usage(&rows)))
                }
            };
            state.notice = Some(lines.unwrap_or_else(|| "no usage yet".into()));
        }
        "/checkpoints" => {
            let lines = {
                let guard = store.lock().unwrap();
                guard
                    .list_checkpoints(&state.thread.id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|checkpoint| format!("turn {:>3}  {}", checkpoint.turn, checkpoint.git_ref))
                    .collect::<Vec<_>>()
            };
            state.notice = Some(if lines.is_empty() {
                "no checkpoints yet".into()
            } else {
                lines.join("\n")
            });
        }
        "/diff" => {
            let result = (|| -> anyhow::Result<String> {
                let guard = store.lock().unwrap();
                let checkpoints = guard.list_checkpoints(&state.thread.id)?;
                if checkpoints.is_empty() {
                    anyhow::bail!("no checkpoints yet");
                }
                let turn = args
                    .first()
                    .and_then(|turn| turn.parse::<i64>().ok())
                    .or_else(|| checkpoints.last().map(|checkpoint| checkpoint.turn));
                let checkpoint = turn
                    .and_then(|turn| checkpoints.into_iter().find(|checkpoint| checkpoint.turn == turn))
                    .ok_or_else(|| anyhow::anyhow!("checkpoint not found"))?;
                drop(guard);
                let diff = diff_checkpoint_against_worktree(std::path::Path::new(&state.thread.cwd), &checkpoint.git_ref)?;
                Ok(diff)
            })();
            match result {
                Ok(diff) => {
                    let lines: Vec<&str> = diff.lines().take(200).collect();
                    let mut text = lines.join("\n");
                    if diff.lines().count() > 200 {
                        text.push_str("\n[...truncated]");
                    }
                    state.diff_output = Some(text);
                }
                Err(error) => state.notice = Some(format!("{error}")),
            }
        }
        "/rewind" => {
            let has_user_messages = state.history.iter().any(|message| message.role == "user");
            if has_user_messages {
                state.overlay = Overlay::RewindPicker { selected: 0 };
            } else {
                state.notice = Some("nothing to rewind to yet".into());
            }
        }
        other => state.notice = Some(format!("unknown command {other}. Try /help.")),
    }
}

fn draw(frame: &mut ratatui::Frame, state: &TuiState, spinner_frame: usize) {
    let area = frame.area();
    let suggestions = suggestions_for(&state.input);
    let composer_rows = 4 + if suggestions.is_empty() { 0 } else { suggestions.len().min(6) + 2 };
    let notice_rows = state
        .notice
        .as_ref()
        .map(|notice| wrapped_rows(notice, area.width as usize))
        .unwrap_or(0);
    let overlay_rows = match &state.overlay {
        Overlay::None => 0,
        Overlay::Approval { .. } => 4,
        Overlay::RewindPicker { .. } => state
            .history
            .iter()
            .filter(|message| message.role == "user")
            .count()
            .min(8)
            + 4,
        Overlay::CodeConfirm { .. } => 4,
    };
    let live_tool_rows = state.tool_lines.len().min(5);
    let streaming_cap = ((area.height as usize).saturating_sub(composer_rows + 6) * 3 / 5).max(3);
    let streaming_rows = if state.streaming_text.is_empty() {
        0
    } else {
        wrapped_rows(&state.streaming_text, area.width as usize).min(streaming_cap)
    };
    let thinking = state.running && state.tool_lines.is_empty() && state.streaming_text.is_empty();
    let live_rows = if thinking {
        1
    } else {
        live_tool_rows + streaming_rows + if streaming_rows > 0 { 1 } else { 0 }
    };

    let chunks = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(live_rows as u16),
        Constraint::Length(overlay_rows as u16),
        Constraint::Length(notice_rows as u16),
        Constraint::Length(composer_rows as u16),
        Constraint::Length(1),
    ])
    .split(area);

    draw_transcript(frame, state, chunks[0]);
    if live_rows > 0 {
        draw_live(frame, state, chunks[1], spinner_frame, thinking);
    }
    match &state.overlay {
        Overlay::Approval { summary, .. } => draw_approval(frame, chunks[2], summary),
        Overlay::RewindPicker { selected } => draw_rewind_picker(frame, chunks[2], state, *selected),
        Overlay::CodeConfirm { turn, .. } => draw_code_confirm(frame, chunks[2], *turn),
        Overlay::None => {}
    }
    if let Some(notice) = &state.notice {
        let paragraph = Paragraph::new(notice.clone()).style(Style::default().fg(COLORS.warning)).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, chunks[3]);
    }
    draw_composer(frame, state, chunks[4], &suggestions);
    draw_status(frame, state, chunks[5], spinner_frame);
}

fn draw_transcript(frame: &mut ratatui::Frame, state: &TuiState, area: Rect) {
    if let Some(diff) = &state.diff_output {
        let lines: Vec<Line> = std::iter::once(Line::styled("diff · /clear to close", Style::default().fg(COLORS.muted)))
            .chain(diff.lines().take(area.height.saturating_sub(1) as usize).map(|line| {
                let style = if line.starts_with('+') {
                    Style::default().fg(COLORS.success)
                } else if line.starts_with('-') {
                    Style::default().fg(COLORS.error)
                } else {
                    Style::default()
                };
                Line::styled(line.to_string(), style)
            }))
            .collect();
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
        return;
    }

    if state.history.is_empty() && !state.running && state.streaming_text.is_empty() && area.height >= 12 {
        let logo_height = LOGO_ROWS.len() as u16 + 2;
        let top = area.height.saturating_sub(logo_height) / 2;
        let logo_area = Rect { y: area.y + top, height: logo_height, ..area };
        let lines: Vec<Line> = LOGO_ROWS
            .iter()
            .map(|row| Line::styled(*row, Style::default().fg(COLORS.accent)))
            .chain([Line::styled(
                "one thread · every harness · pass the baton",
                Style::default().fg(COLORS.muted),
            )])
            .collect();
        frame.render_widget(
            Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center),
            logo_area,
        );
        return;
    }

    let items: Vec<ViewportItem> = state
        .history
        .iter()
        .map(|message| ViewportItem {
            id: message.id,
            role: message.role.clone(),
            text: message.text.clone(),
            has_label: message.role == "assistant",
            has_usage: false,
        })
        .collect();
    let scroll_offset = state.scroll_offset.min(items.len().saturating_sub(1));
    let windowed = window_messages(&items, area.height.saturating_sub(1) as usize, area.width as usize, scroll_offset);

    let mut lines: Vec<Line> = Vec::new();
    if scroll_offset > 0 {
        lines.push(Line::styled(
            format!(
                "↑ {} older messages · PageUp/PageDown scroll",
                windowed.hidden_above
            ),
            Style::default().fg(COLORS.muted),
        ));
    }
    for item in windowed.visible {
        let message = state.history.iter().find(|message| message.id == item.id);
        if item.role == "user" {
            lines.push(Line::styled(item.text.clone(), Style::default().fg(COLORS.text).bg(COLORS.user_bg)));
        } else {
            if let Some(message) = message {
                lines.push(Line::styled(
                    format!("{} · {}", message.provider.as_deref().unwrap_or("?"), message.model.as_deref().unwrap_or("?")),
                    Style::default().fg(COLORS.muted),
                ));
            }
            lines.push(Line::styled(item.text.clone(), Style::default().fg(COLORS.text)));
        }
        lines.push(Line::from(vec![Span::raw(" ")]));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_live(frame: &mut ratatui::Frame, state: &TuiState, area: Rect, spinner_frame: usize, thinking: bool) {
    let spinner = SPINNER_FRAMES[spinner_frame];
    if thinking {
        let line = Line::from(vec![
            Span::styled(spinner, Style::default().fg(COLORS.accent)),
            Span::raw(" "),
            Span::styled("Thinking", Style::default().fg(COLORS.accent)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    let mut lines: Vec<Line> = Vec::new();
    for tool_line in state.tool_lines.iter().rev().take(area.height as usize) {
        let line = match tool_line.status {
            ToolStatus::Running => Line::from(vec![
                Span::styled(spinner, Style::default().fg(COLORS.accent)),
                Span::raw(" "),
                Span::styled(tool_line.label.clone(), Style::default().fg(COLORS.accent)),
            ]),
            ToolStatus::Done => Line::styled(format!("✓ {}", tool_line.label), Style::default().fg(COLORS.success)),
            ToolStatus::Failed => Line::styled(format!("✗ {}", tool_line.label), Style::default().fg(COLORS.error)),
        };
        lines.push(line);
    }
    if !state.streaming_text.is_empty() {
        lines.push(Line::styled(state.streaming_text.clone(), Style::default().fg(COLORS.text)));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_approval(frame: &mut ratatui::Frame, area: Rect, summary: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLORS.warning))
        .border_style(Style::default().fg(COLORS.border));
    let lines = vec![
        Line::styled(format!("approval needed: {summary}"), Style::default().fg(COLORS.warning)),
        Line::styled("[y] allow once · [a] allow for session · [n] deny", Style::default().fg(COLORS.muted)),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(block),
        area,
    );
}

fn draw_rewind_picker(frame: &mut ratatui::Frame, area: Rect, state: &TuiState, selected: usize) {
    let user_messages: Vec<(&i64, &String)> = state
        .history
        .iter()
        .filter(|message| message.role == "user")
        .map(|message| (&message.turn, &message.text))
        .collect();
    let newest_first: Vec<&(&i64, &String) > = user_messages.iter().rev().collect();
    let mut lines: Vec<Line> = vec![Line::styled(
        "rewind to message · enter select · esc cancel",
        Style::default().fg(COLORS.muted),
    )];
    for (index, (turn, text)) in newest_first.iter().take(8).enumerate() {
        let summary: String = text.lines().next().unwrap_or("").chars().take(60).collect();
        let style = if index == selected {
            Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLORS.muted)
        };
        lines.push(Line::styled(format!("❯ turn {turn}: {summary}"), style));
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(COLORS.border));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_code_confirm(frame: &mut ratatui::Frame, area: Rect, turn: i64) {
    let lines = vec![
        Line::styled(
            format!("restore working tree to the end of turn {}?", turn - 1),
            Style::default().fg(COLORS.warning),
        ),
        Line::styled("[y] restore code · [n] conversation only", Style::default().fg(COLORS.muted)),
    ];
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(COLORS.warning));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_composer(frame: &mut ratatui::Frame, state: &TuiState, area: Rect, suggestions: &[&(&str, &str)]) {
    let mut rows: Vec<Line> = Vec::new();
    if !suggestions.is_empty() {
        let clamped = state.suggestion_index.min(suggestions.len() - 1);
        for (index, (command, description)) in suggestions.iter().take(6).enumerate() {
            let style = if index == clamped {
                Style::default().fg(COLORS.accent)
            } else {
                Style::default().fg(COLORS.muted)
            };
            rows.push(Line::styled(format!("❯ {command:<12} {description}"), style));
        }
        rows.push(Line::styled(
            "↑↓ select (wraps) · tab/enter complete",
            Style::default().fg(COLORS.muted),
        ));
        rows.push(Line::from(""));
    }
    let display = if state.input.is_empty() {
        Line::styled("Ask anything...", Style::default().fg(COLORS.muted))
    } else {
        Line::styled(state.input.clone(), Style::default().fg(COLORS.text))
    };
    rows.push(display);
    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
    let provider_config = match state.thread.current_provider {
        ProviderId::Claude => &resolved.claude,
        ProviderId::Codex => &resolved.codex,
    };
    let effective_model = state
        .thread
        .current_model
        .clone()
        .unwrap_or_else(|| provider_config.model.clone());
    rows.push(Line::from(vec![
        Span::styled(
            state.thread.current_provider.to_string(),
            Style::default().fg(COLORS.secondary),
        ),
        Span::styled(
            format!(" · {effective_model} · {} · mode {}", describe_auth(provider_config), state.permission_mode),
            Style::default().fg(COLORS.muted),
        ),
    ]));
    let border_color = if state.running { COLORS.border } else { COLORS.accent };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let composer_area = Rect {
        height: (rows.len() as u16 + 2).min(area.height),
        ..area
    };
    frame.render_widget(
        Paragraph::new(rows).wrap(Wrap { trim: false }).block(block),
        composer_area,
    );
}

fn draw_status(frame: &mut ratatui::Frame, state: &TuiState, area: Rect, spinner_frame: usize) {
    let left = if state.running {
        let elapsed = state
            .turn_started
            .map(|started| started.elapsed().as_secs())
            .unwrap_or(0);
        Line::from(vec![
            Span::styled(SPINNER_FRAMES[spinner_frame], Style::default().fg(COLORS.accent)),
            Span::styled(
                format!(" Working [{elapsed}s] · esc to interrupt"),
                Style::default().fg(COLORS.accent),
            ),
        ])
    } else if state.esc_primed {
        Line::styled("esc again to edit previous message", Style::default().fg(COLORS.muted))
    } else {
        let home = std::env::var("HOME").unwrap_or_default();
        let location = state.thread.cwd.replace(&home, "~");
        let location = match &state.branch_label {
            Some(branch) => format!("{location}:{branch}"),
            None => location,
        };
        Line::styled(format!("{location} · {}", state.thread.title), Style::default().fg(COLORS.muted))
    };

    let usage_rows = {
        let store = state.store_rows();
        store
            .map(|rows| sum_usage(&rows))
    };
    let (cache_badge, cache_color, total_input) = match usage_rows {
        Some(totals) => {
            let rate = cache_hit_rate(&totals.total_usage());
            let badge = match rate {
                None => "cache n/a".to_string(),
                Some(rate) => format!("cache {:.0}%", rate * 100.0),
            };
            let color = match rate {
                None => COLORS.muted,
                Some(rate) if rate >= 0.7 => COLORS.success,
                Some(rate) if rate >= 0.4 => COLORS.warning,
                Some(_) => COLORS.error,
            };
            (
                badge,
                color,
                totals.input_tokens + totals.cache_read_tokens + totals.cache_creation_tokens,
            )
        }
        None => ("cache n/a".to_string(), COLORS.muted, 0),
    };

    let right = Line::from(vec![
        Span::styled(cache_badge, Style::default().fg(cache_color)),
        Span::styled(format!(" · {} in", format_token_count(total_input)), Style::default().fg(COLORS.muted)),
    ]);

    let chunks = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area);
    frame.render_widget(Paragraph::new(left), chunks[0]);
    frame.render_widget(
        Paragraph::new(right).alignment(ratatui::layout::Alignment::Right),
        chunks[1],
    );
}

impl TuiState {
    fn store_rows(&self) -> Option<Vec<crate::store::UsageRow>> {
        self.store.lock().unwrap().list_usage(Some(&self.thread.id)).ok()
    }
}
