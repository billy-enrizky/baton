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
use crate::sync;
use crate::store::{BatonStore, MessageRow, NewThread, ThreadRow};
use crate::types::{ApprovalDecision, PermissionMode, ProviderId};
use crate::usage::{cache_hit_rate, format_token_count, format_usage_line, sum_usage};

pub mod viewport;

mod diff_render;

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
    ("/switch", "switch harness: /switch <provider> [model]"),
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
    ("/goal", "set a multi-turn objective the agent works toward"),
    ("/find", "search the conversation"),
    ("/multi", "send the same prompt to multiple providers"),
    ("/compact", "compact the conversation to free context"),
    ("/context", "show context usage"),
    ("/init", "create a CLAUDE.md memory file"),
    ("/export", "export the conversation"),
    ("/settings", "configure the current harness's settings"),
    ("/mcp", "manage MCP servers and sync across harnesses"),
    ("/skills", "list skills across harnesses"),
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
    SwitchPicker { selected: usize },
    ModelPicker { provider: ProviderId, models: Vec<String>, selected: usize },
    SettingsPicker { provider: ProviderId, categories: Vec<(String, Vec<String>)>, category_index: usize, value_index: usize },
    McpPicker { selected: usize },
    SkillsPicker { selected: usize, skills: Vec<(String, String)> },
    ModePicker { selected: usize },
    AuthPicker { selected: usize },
    ThreadPicker { selected: usize, threads: Vec<(String, String)> },
    CheckpointPicker { selected: usize, checkpoints: Vec<(i64, String)> },
}

const PROVIDERS: [ProviderId; 6] = [
    ProviderId::Claude,
    ProviderId::Codex,
    ProviderId::Cursor,
    ProviderId::Grok,
    ProviderId::OpenCode,
    ProviderId::Antigravity,
];

pub struct TuiState {
    store: Arc<Mutex<BatonStore>>,
    prompt_history: Vec<String>,
    prompt_history_index: Option<usize>,
    queued_messages: Vec<String>,
    goal: Option<String>,
    custom_commands: Vec<(String, String)>,
    home: std::path::PathBuf,
    thread: ThreadRow,
    history: Vec<MessageRow>,
    input: String,
    running: bool,
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
    selection_anchor: Option<usize>,
    selection: Option<(usize, usize)>,
    input_origin: std::cell::Cell<(u16, u16)>,
}

pub fn run_tui(
    store: Arc<Mutex<BatonStore>>,
    home: std::path::PathBuf,
    thread: ThreadRow,
    engine_command_sender: tokio::sync::mpsc::UnboundedSender<EngineCommand>,
    engine_event_receiver: std::sync::mpsc::Receiver<EngineEvent>,
    turn_event_receiver: std::sync::mpsc::Receiver<TurnEvent>,
    initial_prompt: Option<String>,
    initial_permission_mode: PermissionMode,
) -> Result<()> {
    let history = store.lock().unwrap().list_messages(&thread.id)?;
    let prompt_history = store.lock().unwrap().list_prompt_history(&thread.cwd).unwrap_or_default();
    let branch_label = git_branch_label(std::path::Path::new(&thread.cwd));
    let permission_mode = initial_permission_mode;

    let custom_commands = discover_custom_commands(&thread.cwd);
    let mut state = TuiState {
        store: store.clone(),
        prompt_history,
        prompt_history_index: None,
        queued_messages: Vec::new(),
        goal: None,
        custom_commands,
        home,
        thread,
        history,
        input: String::new(),
        running: false,
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
        selection_anchor: None,
        selection: None,
        input_origin: std::cell::Cell::new((0, 0)),
    };

    if let Some(prompt) = initial_prompt {
        state.prompt_history.push(prompt.clone());
        let thread_id = state.thread.id.clone();
        let result = {
            let mut guard = store.lock().unwrap();
            ensure_persisted(&guard, &mut state.thread)
        };
        if result.is_ok() {
            let _ = engine_command_sender.send(EngineCommand::Submit {
                thread_id,
                text: prompt,
                permission_mode: state.permission_mode,
            });
            state.running = true;
        }
    }

    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        crossterm::cursor::Hide,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut spinner_frame = 0usize;
    while !state.quit {
        while let Ok(event) = turn_event_receiver.try_recv() {
            apply_turn_event(&mut state, event);
        }
        while let Ok(event) = engine_event_receiver.try_recv() {
            apply_engine_event(&mut state, &store, event, &engine_command_sender);
        }
        if crossterm::event::poll(Duration::from_millis(32))? {
            match crossterm::event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        handle_key(&mut state, &store, &engine_command_sender, key);
                    }
                }
                Event::Mouse(mouse) => handle_mouse(&mut state, mouse),
                _ => {}
            }
        }
        spinner_frame = (spinner_frame + 1) % SPINNER_FRAMES.len();
        terminal.draw(|frame| draw(frame, &state, spinner_frame))?;
    }

    terminal::disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        crossterm::cursor::Show,
        terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    let _ = engine_command_sender.send(EngineCommand::Quit);
    Ok(())
}

/// Mouse selection inside the composer input: click sets an anchor, drag
/// extends an orange-highlighted range, and clicks outside the input clear it.
fn handle_mouse(state: &mut TuiState, mouse: crossterm::event::MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};
    let (origin_x, origin_y) = state.input_origin.get();
    let char_index = |column: u16, input: &str| input.chars().take(column.saturating_sub(origin_x) as usize).count();
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if mouse.row == origin_y {
                let index = char_index(mouse.column, &state.input);
                state.selection_anchor = Some(index);
                state.selection = Some((index, index));
            } else {
                state.selection_anchor = None;
                state.selection = None;
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(anchor) = state.selection_anchor {
                if mouse.row == origin_y {
                    let index = char_index(mouse.column, &state.input);
                    state.selection = Some((anchor.min(index), anchor.max(index)));
                }
            }
        }
        _ => {}
    }
}

fn apply_engine_event(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    event: EngineEvent,
    engine_command_sender: &tokio::sync::mpsc::UnboundedSender<EngineCommand>,
) {
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
                    state.streaming_text.clear();
                state.tool_lines.clear();
                state.notice = None;
                state.diff_output = None;
            }
        }
        EngineEvent::TurnFinished { thread_id, ok, error } => {
            if thread_id == state.thread.id {
                state.running = false;
                state.streaming_text.clear();
                state.tool_lines.clear();
                {
                    let guard = store.lock().unwrap();
                    state.history = guard.list_messages(&state.thread.id).unwrap_or_default();
                    if let Some(fresh) = guard.get_thread(&state.thread.id).unwrap_or(None) {
                        state.thread = fresh;
                    }
                }
                state.scroll_offset = 0;
                if !ok {
                    state.notice = Some(format!("turn failed: {}", error.unwrap_or_else(|| "unknown error".into())));
                }
                if let Some(next) = state.queued_messages.first().cloned() {
                    state.queued_messages.remove(0);
                    let remaining = state.queued_messages.len();
                    let preview: String = next.chars().take(60).collect();
                    state.notice = Some(format!("sending queued message ({remaining} remaining): {preview}"));
                    submit(state, store, engine_command_sender, next);
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
        Overlay::SwitchPicker { mut selected } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                selected = (selected + PROVIDERS.len() - 1) % PROVIDERS.len();
                state.overlay = Overlay::SwitchPicker { selected };
            }
            KeyCode::Down => {
                selected = (selected + 1) % PROVIDERS.len();
                state.overlay = Overlay::SwitchPicker { selected };
            }
            KeyCode::Enter => {
                if let Some(provider) = PROVIDERS.get(selected).copied() {
                    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
                    let default_model = resolved.for_provider(provider).model.clone();
                    let mut models: Vec<String> = crate::models::catalog(provider)
                        .iter()
                        .map(|model| model.to_string())
                        .collect();
                    if !models.contains(&default_model) {
                        models.insert(0, default_model);
                    }
                    state.overlay = Overlay::ModelPicker { provider, models, selected: 0 };
                }
            }
            _ => {
                state.overlay = Overlay::SwitchPicker { selected };
            }
        },
        Overlay::ModelPicker { provider, models, mut selected } => match key.code {
            KeyCode::Esc | KeyCode::Left => {
                if key.code == KeyCode::Left {
                    state.overlay = Overlay::SwitchPicker { selected: 0 };
                }
            }
            KeyCode::Up => {
                if !models.is_empty() {
                    selected = (selected + models.len() - 1) % models.len();
                }
                state.overlay = Overlay::ModelPicker { provider, models, selected };
            }
            KeyCode::Down => {
                if !models.is_empty() {
                    selected = (selected + 1) % models.len();
                }
                state.overlay = Overlay::ModelPicker { provider, models, selected };
            }
            KeyCode::Enter => {
                if let Some(model) = models.get(selected).cloned() {
                    perform_switch(state, store, provider, Some(model));
                }
            }
            _ => {
                state.overlay = Overlay::ModelPicker { provider, models, selected };
            }
        },
        Overlay::SettingsPicker { provider, categories, mut category_index, mut value_index } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Left => {
                state.overlay = Overlay::SwitchPicker { selected: 0 };
            }
            KeyCode::Up => {
                if !categories.is_empty() {
                    category_index = (category_index + categories.len() - 1) % categories.len();
                    value_index = 0;
                }
                state.overlay = Overlay::SettingsPicker { provider, categories, category_index, value_index };
            }
            KeyCode::Down => {
                if !categories.is_empty() {
                    category_index = (category_index + 1) % categories.len();
                    value_index = 0;
                }
                state.overlay = Overlay::SettingsPicker { provider, categories, category_index, value_index };
            }
            KeyCode::Right | KeyCode::Tab => {
                if let Some((_, values)) = categories.get(category_index) {
                    if !values.is_empty() {
                        value_index = (value_index + 1) % values.len();
                    }
                }
                state.overlay = Overlay::SettingsPicker { provider, categories, category_index, value_index };
            }
            KeyCode::Enter => {
                if let Some((label, values)) = categories.get(category_index) {
                    if let Some(value) = values.get(value_index) {
                        state.notice = Some(format!("{provider} {label}: {value}"));
                    }
                }
            }
            _ => {
                state.overlay = Overlay::SettingsPicker { provider, categories, category_index, value_index };
            }
        },
        Overlay::McpPicker { mut selected } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                selected = (selected + 4 - 1) % 4;
                state.overlay = Overlay::McpPicker { selected };
            }
            KeyCode::Down => {
                selected = (selected + 1) % 4;
                state.overlay = Overlay::McpPicker { selected };
            }
            KeyCode::Enter => {
                match selected {
                    0 => {
                        let registry = sync::load_mcp_registry(&state.home);
                        let lines: Vec<String> = if registry.servers.is_empty() {
                            vec!["no MCP servers configured in baton".to_string()]
                        } else {
                            registry.servers.iter().map(|(name, server)| {
                                format!("  {name}: {} {}", server.command, server.args.join(" "))
                            }).collect()
                        };
                        state.notice = Some(format!("MCP servers:\n{}", lines.join("\n")));
                    }
                    1 => {
                        state.notice = Some("usage: /mcp add <name> <command> [args...] (typed command)".into());
                    }
                    2 => {
                        state.notice = Some("usage: /mcp remove <name> (typed command)".into());
                    }
                    3 => {
                        let registry = sync::load_mcp_registry(&state.home);
                        match sync::sync_mcp_to_all(&registry) {
                            Ok(results) => {
                                let lines: Vec<String> = results.iter().map(|(harness, names)| {
                                    format!("  {harness}: {} synced", names.len())
                                }).collect();
                                state.notice = Some(format!("MCP sync complete\n{}", lines.join("\n")));
                            }
                            Err(error) => state.notice = Some(format!("sync failed: {error}")),
                        }
                    }
                    _ => {}
                }
            }
            _ => {
                state.overlay = Overlay::McpPicker { selected };
            }
        },
        Overlay::SkillsPicker { selected, skills } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                if !skills.is_empty() {
                    let new_selected = (selected + skills.len() - 1) % skills.len();
                    state.overlay = Overlay::SkillsPicker { selected: new_selected, skills };
                }
            }
            KeyCode::Down => {
                if !skills.is_empty() {
                    let new_selected = (selected + 1) % skills.len();
                    state.overlay = Overlay::SkillsPicker { selected: new_selected, skills };
                }
            }
            _ => {
                state.overlay = Overlay::SkillsPicker { selected, skills };
            }
        },
        Overlay::ModePicker { mut selected } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                selected = (selected + 3) % 4;
                state.overlay = Overlay::ModePicker { selected };
            }
            KeyCode::Down => {
                selected = (selected + 1) % 4;
                state.overlay = Overlay::ModePicker { selected };
            }
            KeyCode::Enter => {
                let modes = [PermissionMode::Default, PermissionMode::AcceptEdits, PermissionMode::BypassPermissions, PermissionMode::Plan];
                if let Some(mode) = modes.get(selected) {
                    state.permission_mode = *mode;
                    state.notice = Some(format!("permission mode: {mode}"));
                }
            }
            _ => {
                state.overlay = Overlay::ModePicker { selected };
            }
        },
        Overlay::AuthPicker { mut selected } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                selected = (selected + PROVIDERS.len() * 2 - 1) % (PROVIDERS.len() * 2);
                state.overlay = Overlay::AuthPicker { selected };
            }
            KeyCode::Down => {
                selected = (selected + 1) % (PROVIDERS.len() * 2);
                state.overlay = Overlay::AuthPicker { selected };
            }
            KeyCode::Enter => {
                let provider_index = selected / 2;
                let is_api = selected % 2 == 1;
                if let Some(provider) = PROVIDERS.get(provider_index).copied() {
                    let mode = if is_api { crate::config::AuthMode::Api } else { crate::config::AuthMode::Subscription };
                    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
                    let provider_config = resolved.for_provider(provider);
                    if mode == crate::config::AuthMode::Api && provider_config.api_key.is_none() {
                        state.notice = Some(format!(
                            "no API key for {provider}. Set {} in the environment, the harness config, or config.json.",
                            provider_config.api_key_env_name
                        ));
                        return;
                    }
                    let mut file = load_config_file(&state.home).unwrap_or_default();
                    file.providers.entry(provider.as_str().to_string()).or_default().mode = Some(mode);
                    if let Err(error) = save_config_file(&state.home, &file) {
                        state.notice = Some(format!("cannot save config: {error}"));
                        return;
                    }
                    state.notice = Some(format!("{provider} billing set to {}", mode.as_str()));
                }
            }
            _ => {
                state.overlay = Overlay::AuthPicker { selected };
            }
        },
        Overlay::ThreadPicker { selected, threads } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                if !threads.is_empty() {
                    let new_selected = (selected + threads.len() - 1) % threads.len();
                    state.overlay = Overlay::ThreadPicker { selected: new_selected, threads };
                }
            }
            KeyCode::Down => {
                if !threads.is_empty() {
                    let new_selected = (selected + 1) % threads.len();
                    state.overlay = Overlay::ThreadPicker { selected: new_selected, threads };
                }
            }
            KeyCode::Enter => {
                if let Some((thread_id, _)) = threads.get(selected) {
                    let found = {
                        let guard = store.lock().unwrap();
                        guard.list_threads(None).unwrap_or_default().into_iter()
                            .find(|thread| thread.id.starts_with(thread_id.as_str()))
                    };
                    if let Some(thread) = found {
                        state.thread = thread.clone();
                        let guard = store.lock().unwrap();
                        state.history = guard.list_messages(&thread.id).unwrap_or_default();
                        state.branch_label = git_branch_label(std::path::Path::new(&thread.cwd));
                        state.diff_output = None;
                        state.scroll_offset = 0;
                        state.notice = Some(format!("opened {}", thread.title));
                    }
                }
            }
            _ => {
                state.overlay = Overlay::ThreadPicker { selected, threads };
            }
        },
        Overlay::CheckpointPicker { selected, checkpoints } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                if !checkpoints.is_empty() {
                    let new_selected = (selected + checkpoints.len() - 1) % checkpoints.len();
                    state.overlay = Overlay::CheckpointPicker { selected: new_selected, checkpoints };
                }
            }
            KeyCode::Down => {
                if !checkpoints.is_empty() {
                    let new_selected = (selected + 1) % checkpoints.len();
                    state.overlay = Overlay::CheckpointPicker { selected: new_selected, checkpoints };
                }
            }
            KeyCode::Enter => {
                if let Some((turn, _)) = checkpoints.get(selected) {
                    let cwd = std::path::PathBuf::from(&state.thread.cwd);
                    let result = {
                        let guard = store.lock().unwrap();
                        guard.get_checkpoint(&state.thread.id, *turn).unwrap_or(None)
                    };
                    if let Some(checkpoint) = result {
                        match crate::checkpoints::diff_checkpoint_against_worktree(&cwd, &checkpoint.git_ref) {
                            Ok(diff) => {
                                let lines: Vec<&str> = diff.lines().take(200).collect();
                                let mut text = lines.join("\n");
                                if diff.lines().count() > 200 {
                                    text.push_str("\n[...truncated]");
                                }
                                state.diff_output = Some(text);
                            }
                            Err(error) => state.notice = Some(format!("diff failed: {error}")),
                        }
                    }
                }
            }
            _ => {
                state.overlay = Overlay::CheckpointPicker { selected, checkpoints };
            }
        },
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
            state.selection = None;
            state.selection_anchor = None;
            if text.starts_with('/') {
                handle_command(state, store, engine_command_sender, &text);
            } else if state.running {
                state.queued_messages.push(text.clone());
                state.notice = Some(format!(
                    "queued ({} waiting) · sends after current turn · ctrl+c to interrupt instead",
                    state.queued_messages.len()
                ));
            } else {
                submit(state, store, engine_command_sender, text);
            }
        }
        KeyCode::Backspace => {
            state.input.pop();
            state.suggestion_index = 0;
            state.esc_primed = false;
            state.selection = None;
            state.selection_anchor = None;
            state.prompt_history_index = None;
        }
        // Claude Code style recall: with an empty input (or while already
        // recalling), Up walks back through this project's prompt history
        // and Down walks forward. Otherwise the arrows scroll the transcript.
        KeyCode::Up if !state.prompt_history.is_empty() && (state.input.is_empty() || state.prompt_history_index.is_some()) => {
            if !state.prompt_history.is_empty() {
                let next = match state.prompt_history_index {
                    None => state.prompt_history.len() - 1,
                    Some(0) => 0,
                    Some(index) => index - 1,
                };
                state.input = state.prompt_history[next].clone();
                state.prompt_history_index = Some(next);
                state.selection = None;
                state.selection_anchor = None;
                state.esc_primed = false;
            }
        }
        KeyCode::Down if state.prompt_history_index.is_some() => {
            let index = state.prompt_history_index.unwrap() + 1;
            if index >= state.prompt_history.len() {
                state.input.clear();
                state.prompt_history_index = None;
            } else {
                state.input = state.prompt_history[index].clone();
                state.prompt_history_index = Some(index);
            }
            state.selection = None;
            state.selection_anchor = None;
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
        KeyCode::End | KeyCode::Home => {
            state.scroll_offset = 0;
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
                state.prompt_history_index = None;
                state.notice = Some("composer cleared".into());
            }
        }
        KeyCode::Char(char) => {
            if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                state.input.push(char);
                state.suggestion_index = 0;
                state.esc_primed = false;
                state.selection = None;
                state.selection_anchor = None;
                state.prompt_history_index = None;
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
    state.prompt_history.push(text.clone());
    state.prompt_history_index = None;
    let recorded = {
        let guard = store.lock().unwrap();
        guard.record_prompt(&state.thread.cwd, &text)
    };
    if let Err(error) = recorded {
        state.notice = Some(format!("cannot record prompt: {error}"));
    }
    let text = if let Some(goal) = &state.goal {
        format!("[goal: {goal}]\n{text}")
    } else {
        text
    };
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
    engine_command_sender: &tokio::sync::mpsc::UnboundedSender<EngineCommand>,
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
        "/goal" => {
            if let Some(action) = args.first().map(String::as_str) {
                match action {
                    "clear" => {
                        state.goal = None;
                        state.notice = Some("goal cleared".into());
                    }
                    "pause" => {
                        state.notice = Some("goal paused (will not inject into next turns)".into());
                    }
                    "resume" => {
                        state.notice = Some("goal resumed".into());
                    }
                    _ => {
                        state.goal = Some(args.join(" "));
                        state.notice = Some(format!("goal set: {}", args.join(" ")));
                    }
                }
            } else if let Some(goal) = &state.goal {
                state.notice = Some(format!("current goal: {goal}"));
            } else {
                state.notice = Some("usage: /goal <done criteria> · /goal clear".into());
            }
        }
        "/find" => {
            let query = args.join(" ");
            if query.is_empty() {
                state.notice = Some("usage: /find <text>".into());
                return;
            }
            let query_lower = query.to_lowercase();
            let matches: Vec<String> = state
                .history
                .iter()
                .filter(|message| message.text.to_lowercase().contains(&query_lower))
                .map(|message| {
                    let speaker = if message.role == "user" { "you" } else { "agent" };
                    let context = message
                        .text
                        .lines()
                        .find(|line| line.to_lowercase().contains(&query_lower))
                        .unwrap_or("");
                    let start = context.to_lowercase().find(&query_lower).unwrap_or(0);
                    let begin = start.saturating_sub(30);
                    let end = (start + query_lower.len() + 50).min(context.len());
                    format!("  [{speaker}] ...{}...", &context[begin..end])
                })
                .collect();
            state.notice = Some(if matches.is_empty() {
                format!("no matches for \"{query}\"")
            } else {
                format!("found {} matches:\n{}", matches.len(), matches.join("\n"))
            });
        }
        "/multi" => {
            if args.len() < 2 {
                state.notice = Some("usage: /multi <provider> [<provider> ...] <prompt>".into());
                return;
            }
            // Leading arguments that name providers; everything after them is
            // the prompt, so multi-word prompts keep their words.
            let mut valid_providers: Vec<ProviderId> = Vec::new();
            let mut prompt_parts: Vec<String> = Vec::new();
            for arg in &args {
                match ProviderId::parse(arg) {
                    Some(provider) if prompt_parts.is_empty() => {
                        if !valid_providers.contains(&provider) {
                            valid_providers.push(provider);
                        }
                    }
                    _ => prompt_parts.push(arg.clone()),
                }
            }
            let prompt = prompt_parts.join(" ");
            if valid_providers.is_empty() || prompt.trim().is_empty() {
                state.notice = Some("usage: /multi <provider> [<provider> ...] <prompt>".into());
                return;
            }
            state.notice = Some(format!(
                "sending to {} providers in parallel: {}",
                valid_providers.len(),
                valid_providers.iter().map(|provider| provider.to_string()).collect::<Vec<_>>().join(", ")
            ));
            let mut threads = Vec::new();
            for provider in valid_providers.clone() {
                let thread = {
                    let mut guard = store.lock().unwrap();
                    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
                    let model = resolved.for_provider(provider).model.clone();
                    let mut thread = guard.draft_thread(crate::store::NewThread {
                        title: format!("[multi] {prompt}"),
                        cwd: state.thread.cwd.clone(),
                        current_provider: provider,
                        current_model: Some(model),
                    });
                    // Draft threads are not persisted until a turn runs; the
                    // engine looks the thread up immediately, so create it now.
                    let _ = ensure_persisted(&guard, &mut thread);
                    thread
                };
                threads.push(thread);
            }
            for thread in threads {
                let thread_id = thread.id.clone();
                let _ = engine_command_sender.send(EngineCommand::Submit {
                    thread_id,
                    text: prompt.clone(),
                    permission_mode: state.permission_mode,
                });
            }
            state.notice = Some(format!(
                "sent to {} providers in parallel: {} · open their replies with /threads",
                valid_providers.len(),
                valid_providers.iter().map(|provider| provider.to_string()).collect::<Vec<_>>().join(", ")
            ));
        }
        "/compact" => {
            let messages: Vec<(String, String)> = state
                .history
                .iter()
                .map(|message| (message.role.clone(), message.text.clone()))
                .collect();
            if messages.len() < 4 {
                state.notice = Some("not enough conversation to compact".into());
            } else {
                let summary = crate::memory::build_compact_summary(&messages, 8000);
                let thread_id = state.thread.id.clone();
                let next_turn = store.lock().unwrap().next_turn_number(&thread_id).unwrap_or(1);
                {
                    let mut guard = store.lock().unwrap();
                    let _ = guard.rewind_thread(&thread_id, 1);
                    let _ = guard.add_message(&thread_id, next_turn, "user", None, None, "compact");
                    let _ = guard.add_message(&thread_id, next_turn, "assistant", None, None, &summary);
                }
                state.history = store.lock().unwrap().list_messages(&thread_id).unwrap_or_default();
                state.scroll_offset = 0;
                state.notice = Some(format!("compacted {} messages", messages.len()));
            }
        }
        "/context" => {
            let messages: Vec<&crate::store::MessageRow> = state.history.iter().collect();
            let total_chars: usize = messages.iter().map(|message| message.text.len()).sum();
            let streaming_chars = state.streaming_text.len();
            let usage_rows = store.lock().unwrap().list_usage(Some(&state.thread.id)).unwrap_or_default();
            let usage_line = if usage_rows.is_empty() {
                "no token usage recorded".to_string()
            } else {
                format_usage_line(&sum_usage(&usage_rows))
            };
            state.notice = Some(format!(
                "context: {} messages, {:.1}k chars history, {:.1}k chars streaming\ntokens: {usage_line}",
                messages.len(),
                total_chars as f64 / 1000.0,
                streaming_chars as f64 / 1000.0
            ));
        }
        "/init" => {
            match crate::memory::init_memory_file(std::path::Path::new(&state.thread.cwd)) {
                Ok(result) => state.notice = Some(result),
                Err(error) => state.notice = Some(format!("cannot create memory file: {error}")),
            }
        }
        "/export" => {
            let lines: Vec<String> = state
                .history
                .iter()
                .map(|message| {
                    let speaker = if message.role == "user" { "you" } else { message.provider.as_deref().unwrap_or("assistant") };
                    format!("[{speaker}]\n{}", message.text)
                })
                .collect();
            if lines.is_empty() {
                state.notice = Some("nothing to export".into());
            } else {
                let export_path = std::env::temp_dir().join(format!("baton-export-{}.md", state.thread.id[..8].to_string()));
                match std::fs::write(&export_path, lines.join("\n\n---\n\n")) {
                    Ok(()) => state.notice = Some(format!("exported to {}", export_path.display())),
                    Err(error) => state.notice = Some(format!("export failed: {error}")),
                }
            }
        }
        "/settings" => {
            let categories: Vec<(String, Vec<String>)> = sync::settings_for_provider(state.thread.current_provider)
                .into_iter()
                .map(|(label, values)| (label.to_string(), values.into_iter().map(str::to_string).collect()))
                .collect();
            state.overlay = Overlay::SettingsPicker {
                provider: state.thread.current_provider,
                categories,
                category_index: 0,
                value_index: 0,
            };
        }
        "/mcp" => {
            if let Some(action) = args.first().map(String::as_str) {
                handle_mcp_command(state, action, &args[1..]);
            } else {
                state.overlay = Overlay::McpPicker { selected: 0 };
            }
        }
        "/skills" => {
            let skills: Vec<(String, String)> = sync::scan_skills()
                .into_iter()
                .map(|skill| (skill.name, skill.source))
                .collect();
            if skills.is_empty() {
                state.notice = Some("no skills found in ~/.claude/skills or ~/.codex/skills".into());
            } else {
                state.overlay = Overlay::SkillsPicker { selected: 0, skills };
            }
        }
        "/switch" => {
            let target = args.first().map(String::as_str);
            match target.and_then(ProviderId::parse) {
                None => state.overlay = Overlay::SwitchPicker { selected: 0 },
                Some(provider) => perform_switch(state, store, provider, args.get(1).cloned()),
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
                        .unwrap_or_else(|| resolved.for_provider(state.thread.current_provider).model.clone());
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
                    match result {
                        Ok(()) => {
                            state.thread.current_model = Some(model.clone());
                            state.notice = Some(format!("model set to {model}"));
                        }
                        Err(error) => state.notice = Some(format!("cannot set model: {error}")),
                    }
                }
            }
        }
        "/mode" => {
            if let Some(mode) = args.first().map(String::as_str).and_then(PermissionMode::parse) {
                state.permission_mode = mode;
                state.notice = Some(format!("permission mode: {mode}"));
            } else {
                state.overlay = Overlay::ModePicker { selected: 0 };
            }
        }
        "/auth" => {
            let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
            match (args.first().map(String::as_str), args.get(1).map(String::as_str)) {
                (None, _) => {
                    state.overlay = Overlay::AuthPicker { selected: 0 };
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
                    let provider_config = resolved.for_provider(provider);
                    if mode == crate::config::AuthMode::Api && provider_config.api_key.is_none() {
                        state.notice = Some(format!(
                            "no API key for {provider}. Set {} in the environment, the harness config, or config.json.",
                            provider_config.api_key_env_name
                        ));
                        return;
                    }
                    let mut file = load_config_file(&state.home).unwrap_or_default();
                    file.providers
                        .entry(provider.as_str().to_string())
                        .or_default()
                        .mode = Some(mode);
                    if let Err(error) = save_config_file(&state.home, &file) {
                        state.notice = Some(format!("cannot save config: {error}"));
                        return;
                    }
                    state.notice = Some(format!("{provider} billing set to {}", mode.as_str()));
                }
                (Some(provider), None) => {
                    match ProviderId::parse(provider) {
                        Some(parsed) => {
                            state.notice = Some(describe_auth(resolved.for_provider(parsed)));
                        }
                        None => state.notice = Some("unknown provider".into()),
                    }
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
            let threads: Vec<(String, String)> = {
                let guard = store.lock().unwrap();
                guard
                    .list_threads(Some(state.thread.cwd.as_str()))
                    .unwrap_or_default()
                    .into_iter()
                    .take(20)
                    .map(|thread| (thread.id[..8].to_string(), thread.title))
                    .collect()
            };
            if threads.is_empty() {
                state.notice = Some("no threads in this directory".into());
            } else {
                state.overlay = Overlay::ThreadPicker { selected: 0, threads };
            }
        }
        "/open" => {
            if let Some(prefix) = args.first() {
                let found = {
                    let guard = store.lock().unwrap();
                    guard
                        .list_threads(None)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|thread| thread.id.starts_with(prefix.as_str()))
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
            } else {
                let threads: Vec<(String, String)> = {
                    let guard = store.lock().unwrap();
                    guard
                        .list_threads(None)
                        .unwrap_or_default()
                        .into_iter()
                        .take(20)
                        .map(|thread| (thread.id[..8].to_string(), thread.title))
                        .collect()
                };
                if threads.is_empty() {
                    state.notice = Some("no threads".into());
                } else {
                    state.overlay = Overlay::ThreadPicker { selected: 0, threads };
                }
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
            let checkpoints: Vec<(i64, String)> = {
                let guard = store.lock().unwrap();
                guard
                    .list_checkpoints(&state.thread.id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|checkpoint| (checkpoint.turn, checkpoint.git_ref))
                    .collect()
            };
            if checkpoints.is_empty() {
                state.notice = Some("no checkpoints yet".into());
            } else {
                state.overlay = Overlay::CheckpointPicker { selected: checkpoints.len().saturating_sub(1), checkpoints };
            }
        }
        "/diff" => {
            if args.is_empty() {
                let checkpoints: Vec<(i64, String)> = {
                    let guard = store.lock().unwrap();
                    guard
                        .list_checkpoints(&state.thread.id)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|checkpoint| (checkpoint.turn, checkpoint.git_ref))
                        .collect()
                };
                if checkpoints.is_empty() {
                    state.notice = Some("no checkpoints yet".into());
                } else {
                    state.overlay = Overlay::CheckpointPicker { selected: checkpoints.len().saturating_sub(1), checkpoints };
                }
                return;
            }
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
        other => {
            if let Some((_, template)) = state.custom_commands.iter().find(|(name, _)| name == other) {
                let expanded = template
                    .replace("{{args}}", &args.join(" "))
                    .replace("{{cwd}}", &state.thread.cwd);
                submit(state, store, engine_command_sender, expanded);
            } else {
                state.notice = Some(format!("unknown command {other}. Try /help."));
            }
        }
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
        Overlay::SwitchPicker { .. } => PROVIDERS.len() + 4,
        Overlay::ModelPicker { models, .. } => models.len().min(8) + 4,
        Overlay::SettingsPicker { categories, category_index, .. } => {
            categories.get(*category_index).map(|(_, values): &(String, Vec<String>)| values.len()).unwrap_or(0).min(6) + categories.len().min(4) + 4
        },
        Overlay::McpPicker { .. } => 8,
        Overlay::SkillsPicker { skills, .. } => skills.len().min(8) + 4,
        Overlay::ModePicker { .. } => 7,
        Overlay::AuthPicker { .. } => 9,
        Overlay::ThreadPicker { threads, .. } => threads.len().min(8) + 4,
        Overlay::CheckpointPicker { checkpoints, .. } => checkpoints.len().min(8) + 4,
    };
    let chunks = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(overlay_rows as u16),
        Constraint::Length(notice_rows as u16),
        Constraint::Length(composer_rows as u16),
        Constraint::Length(1),
    ])
    .split(area);

    draw_transcript(frame, state, chunks[0], spinner_frame);
    match &state.overlay {
        Overlay::Approval { summary, .. } => draw_approval(frame, chunks[1], summary),
        Overlay::RewindPicker { selected } => draw_rewind_picker(frame, chunks[1], state, *selected),
        Overlay::CodeConfirm { turn, .. } => draw_code_confirm(frame, chunks[1], *turn),
        Overlay::SwitchPicker { selected } => draw_switch_picker(frame, chunks[1], state, *selected),
        Overlay::ModelPicker { provider, models, selected } => {
            draw_model_picker(frame, chunks[1], state, *provider, models, *selected)
        }
        Overlay::SettingsPicker { provider, categories, category_index, value_index } => {
            draw_settings_picker(frame, chunks[1], *provider, categories, *category_index, *value_index)
        }
        Overlay::McpPicker { selected } => draw_mcp_picker(frame, chunks[1], *selected),
        Overlay::SkillsPicker { selected, skills } => {
            draw_skills_picker(frame, chunks[1], skills, *selected)
        }
        Overlay::ModePicker { selected } => draw_mode_picker(frame, chunks[1], state, *selected),
        Overlay::AuthPicker { selected } => draw_auth_picker(frame, chunks[1], state, *selected),
        Overlay::ThreadPicker { selected, threads } => {
            draw_thread_picker(frame, chunks[1], threads, *selected)
        }
        Overlay::CheckpointPicker { selected, checkpoints } => {
            draw_checkpoint_picker(frame, chunks[1], checkpoints, *selected)
        }
        Overlay::None => {}
    }
    if let Some(notice) = &state.notice {
        let paragraph = Paragraph::new(notice.clone()).style(Style::default().fg(COLORS.warning)).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, chunks[2]);
    }
    draw_composer(frame, state, chunks[3], &suggestions);
    draw_status(frame, state, chunks[4]);
}

fn discover_custom_commands(cwd: &str) -> Vec<(String, String)> {
    let mut commands = Vec::new();
    let dirs = [
        std::path::PathBuf::from(cwd).join(".baton").join("commands"),
        std::path::PathBuf::from(cwd).join(".claude").join("commands"),
    ];
    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                    if let (Some(name), Ok(contents)) = (
                        path.file_stem().and_then(|name| name.to_str()),
                        std::fs::read_to_string(&path),
                    ) {
                        commands.push((format!("/{name}"), contents.trim().to_string()));
                    }
                }
            }
        }
    }
    commands
}

fn handle_mcp_command(state: &mut TuiState, action: &str, args: &[String]) {
    let mut registry = sync::load_mcp_registry(&state.home);
    match action {
        "list" => {
            if registry.servers.is_empty() {
                state.notice = Some("no MCP servers configured. Use /mcp add <name> <command> [args...]".into());
            } else {
                let lines: Vec<String> = registry
                    .servers
                    .iter()
                    .map(|(name, server)| format!("  {name}: {} {}", server.command, server.args.join(" ")))
                    .collect();
                state.notice = Some(format!("MCP servers:\n{}", lines.join("\n")));
            }
        }
        "add" => {
            if args.len() < 2 {
                state.notice = Some("usage: /mcp add <name> <command> [args...]".into());
                return;
            }
            let name = args[0].clone();
            let command = args[1].clone();
            let server_args: Vec<String> = args[2..].to_vec();
            registry.servers.insert(
                name.clone(),
                sync::McpServerConfig {
                    command,
                    args: server_args,
                    env: Default::default(),
                },
            );
            match sync::save_mcp_registry(&state.home, &registry) {
                Ok(()) => state.notice = Some(format!("added MCP server {name}. Run /mcp sync to push to all harnesses")),
                Err(error) => state.notice = Some(format!("cannot save: {error}")),
            }
        }
        "remove" => {
            match args.first() {
                Some(name) => {
                    if registry.servers.remove(name).is_some() {
                        match sync::save_mcp_registry(&state.home, &registry) {
                            Ok(()) => state.notice = Some(format!("removed MCP server {name}")),
                            Err(error) => state.notice = Some(format!("cannot save: {error}")),
                        }
                    } else {
                        state.notice = Some(format!("no MCP server named {name}"));
                    }
                }
                None => state.notice = Some("usage: /mcp remove <name>".into()),
            }
        }
        "sync" => match sync::sync_mcp_to_all(&registry) {
            Ok(results) => {
                let lines: Vec<String> = results
                    .iter()
                    .map(|(harness, names)| format!("  {harness}: {} servers synced", names.len()))
                    .collect();
                state.notice = Some(format!("MCP sync complete\n{}", lines.join("\n")));
            }
            Err(error) => state.notice = Some(format!("sync failed: {error}")),
        }
        other => state.notice = Some(format!("unknown /mcp action {other}. Use list, add, remove, or sync")),
    }
}

fn perform_switch(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    provider: ProviderId,
    model: Option<String>,
) {
    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
    let default_model = resolved.for_provider(provider).model.clone();
    let display_model = model.clone().unwrap_or(default_model);
    let result = {
        let guard = store.lock().unwrap();
        ensure_persisted(&guard, &mut state.thread)
            .and_then(|()| guard.set_thread_provider(&state.thread.id, provider, model.clone()))
    };
    match result {
        Ok(()) => {
            state.thread.current_provider = provider;
            state.thread.current_model = model.clone();
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
                None => format!("switched to {provider}/{display_model}. First turn carries a context handoff."),
            });
        }
        Err(error) => state.notice = Some(format!("switch failed: {error}")),
    }
}

fn draw_switch_picker(frame: &mut ratatui::Frame, area: Rect, state: &TuiState, selected: usize) {
    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
    let mut lines: Vec<Line> = vec![Line::styled(
        "switch harness · enter select · esc cancel",
        Style::default().fg(COLORS.muted),
    )];
    for (index, provider) in PROVIDERS.iter().enumerate() {
        let provider_config = resolved.for_provider(*provider);
        let style = if index == selected {
            Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLORS.muted)
        };
        let marker = if *provider == state.thread.current_provider { "●" } else { " " };
        lines.push(Line::styled(
            format!(
                "❯ {marker} {provider} · {} · {}",
                provider_config.model,
                describe_auth(provider_config)
            ),
            style,
        ));
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(COLORS.border));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_model_picker(
    frame: &mut ratatui::Frame,
    area: Rect,
    state: &TuiState,
    provider: ProviderId,
    models: &[String],
    selected: usize,
) {
    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
    let default_model = resolved.for_provider(provider).model.clone();
    let mut lines: Vec<Line> = vec![Line::styled(
        format!("{provider} models · enter select · esc cancel"),
        Style::default().fg(COLORS.muted),
    )];
    for (index, model) in models.iter().take(8).enumerate() {
        let marker = if *model == default_model { "●" } else { " " };
        let style = if index == selected {
            Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLORS.muted)
        };
        lines.push(Line::styled(format!("❯ {marker} {model}"), style));
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(COLORS.border));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_settings_picker(
    frame: &mut ratatui::Frame,
    area: Rect,
    provider: ProviderId,
    categories: &[(String, Vec<String>)],
    category_index: usize,
    value_index: usize,
) {
    let mut lines: Vec<Line> = vec![Line::styled(
        format!("{provider} settings · ←back · ↑↓ category · →/tab value · enter apply"),
        Style::default().fg(COLORS.muted),
    )];
    for (index, (label, values)) in categories.iter().enumerate() {
        let style = if index == category_index {
            Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLORS.muted)
        };
        let current_value = values.get(value_index).map(|value| value.as_str()).unwrap_or("?");
        lines.push(Line::styled(
            format!("❯ {label}: {current_value}"),
            style,
        ));
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(COLORS.border));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_mcp_picker(frame: &mut ratatui::Frame, area: Rect, selected: usize) {
    let actions = ["list servers", "add server (typed)", "remove server (typed)", "sync to all harnesses"];
    let mut lines: Vec<Line> = vec![Line::styled(
        "MCP servers · enter select · esc close",
        Style::default().fg(COLORS.muted),
    )];
    for (index, action) in actions.iter().enumerate() {
        let style = if index == selected {
            Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLORS.muted)
        };
        lines.push(Line::styled(format!("❯ {action}"), style));
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(COLORS.border));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_skills_picker(frame: &mut ratatui::Frame, area: Rect, skills: &[(String, String)], selected: usize) {
    let mut lines: Vec<Line> = vec![Line::styled(
        format!("skills ({}) · esc close", skills.len()),
        Style::default().fg(COLORS.muted),
    )];
    for (index, (name, source)) in skills.iter().take(8).enumerate() {
        let style = if index == selected {
            Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLORS.muted)
        };
        lines.push(Line::styled(format!("❯ {name} ({source})"), style));
    }
    if skills.len() > 8 {
        lines.push(Line::styled(format!("... and {} more", skills.len() - 8), Style::default().fg(COLORS.muted)));
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(COLORS.border));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_mode_picker(frame: &mut ratatui::Frame, area: Rect, state: &TuiState, selected: usize) {
    let modes = ["default", "acceptEdits", "bypassPermissions", "plan"];
    let mut lines: Vec<Line> = vec![Line::styled(
        "permission mode · enter select · esc close",
        Style::default().fg(COLORS.muted),
    )];
    for (index, mode) in modes.iter().enumerate() {
        let is_current = state.permission_mode.as_str() == *mode;
        let marker = if is_current { "●" } else { " " };
        let style = if index == selected {
            Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLORS.muted)
        };
        lines.push(Line::styled(format!("❯ {marker} {mode}"), style));
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(COLORS.border));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_auth_picker(frame: &mut ratatui::Frame, area: Rect, state: &TuiState, selected: usize) {
    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
    let mut lines: Vec<Line> = vec![Line::styled(
        "billing mode · enter select · esc close",
        Style::default().fg(COLORS.muted),
    )];
    for (index, provider) in PROVIDERS.iter().enumerate() {
        let config = resolved.for_provider(*provider);
        for (mode_index, mode) in [0usize, 1usize].iter().zip(["subscription", "api"]) {
            let flat_index = index * 2 + mode_index;
            let is_api = *mode_index == 1;
            let is_current = (config.mode == crate::config::AuthMode::Api) == is_api;
            let marker = if is_current { "●" } else { " " };
            let style = if flat_index == selected {
                Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(COLORS.muted)
            };
            lines.push(Line::styled(format!("❯ {marker} {provider} {mode}"), style));
        }
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(COLORS.border));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_thread_picker(frame: &mut ratatui::Frame, area: Rect, threads: &[(String, String)], selected: usize) {
    let mut lines: Vec<Line> = vec![Line::styled(
        "threads · enter open · esc close",
        Style::default().fg(COLORS.muted),
    )];
    for (index, (thread_id, title)) in threads.iter().take(8).enumerate() {
        let style = if index == selected {
            Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLORS.muted)
        };
        lines.push(Line::styled(format!("❯ {thread_id}  {title}"), style));
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(COLORS.border));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_checkpoint_picker(frame: &mut ratatui::Frame, area: Rect, checkpoints: &[(i64, String)], selected: usize) {
    let mut lines: Vec<Line> = vec![Line::styled(
        "checkpoints · enter diff · esc close",
        Style::default().fg(COLORS.muted),
    )];
    for (index, (turn, _)) in checkpoints.iter().take(8).enumerate() {
        let style = if index == selected {
            Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLORS.muted)
        };
        lines.push(Line::styled(format!("❯ turn {turn}"), style));
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(COLORS.border));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_transcript(frame: &mut ratatui::Frame, state: &TuiState, area: Rect, spinner_frame: usize) {
    if let Some(diff) = &state.diff_output {
        let lines: Vec<Line> = std::iter::once(Line::styled("diff · /clear to close", Style::default().fg(COLORS.muted)))
            .chain(diff_render::render_diff_lines(diff, area.width.saturating_sub(2) as usize))
            .collect();
        frame.render_widget(Paragraph::new(lines), area);
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

    // Pre-wrap all text at the actual terminal width so line counts are exact
    let width = area.width as usize;
    let wrap = |text: &str| -> Vec<String> {
        text.split('\n')
            .flat_map(|line| {
                if line.is_empty() {
                    vec![String::new()]
                } else {
                    let mut wrapped = Vec::new();
                    let mut current = String::new();
                    for ch in line.chars() {
                        if current.chars().count() >= width.saturating_sub(2) {
                            wrapped.push(current.clone());
                            current.clear();
                        }
                        current.push(ch);
                    }
                    if !current.is_empty() || wrapped.is_empty() {
                        wrapped.push(current);
                    }
                    wrapped
                }
            })
            .collect()
    };

    // Build all terminal lines (each element = exactly 1 rendered row)
    let mut all_lines: Vec<Line> = Vec::new();
    for message in &state.history {
        if message.role == "tool" {
            let style = if message.text.starts_with("✗") {
                COLORS.error
            } else if message.text.starts_with("✓") {
                COLORS.success
            } else {
                COLORS.muted
            };
            for line in wrap(&message.text) {
                all_lines.push(Line::styled(line, Style::default().fg(style)));
            }
            continue;
        }
        if message.role == "diff" {
            all_lines.extend(diff_render::render_diff_lines(
                &message.text,
                width.saturating_sub(2),
            ));
            all_lines.push(Line::from(vec![Span::raw(" ")]));
            continue;
        }
        if message.role == "user" {
            for line in wrap(&message.text) {
                all_lines.push(Line::styled(line, Style::default().fg(COLORS.text).bg(COLORS.user_bg)));
            }
        } else {
            all_lines.push(Line::styled(
                format!(
                    "{} · {}",
                    message.provider.as_deref().unwrap_or("?"),
                    message.model.as_deref().unwrap_or("?")
                ),
                Style::default().fg(COLORS.muted),
            ));
            for line in wrap(&message.text) {
                all_lines.push(Line::styled(line, Style::default().fg(COLORS.text)));
            }
        }
        all_lines.push(Line::from(vec![Span::raw(" ")]));
    }

    // Live turn activity flows right after the newest message, in the
    // transcript itself rather than a separate panel above the composer.
    let mut live_lines = 0usize;
    let spinner = SPINNER_FRAMES[spinner_frame];
    if state.running && state.tool_lines.is_empty() && state.streaming_text.is_empty() {
        all_lines.push(Line::from(vec![
            Span::styled(spinner, Style::default().fg(COLORS.accent)),
            Span::raw(" "),
            Span::styled("Thinking", Style::default().fg(COLORS.accent)),
        ]));
        live_lines += 1;
    }
    for tool_line in &state.tool_lines {
        let line = match tool_line.status {
            ToolStatus::Running => Line::from(vec![
                Span::styled(spinner, Style::default().fg(COLORS.accent)),
                Span::raw(" "),
                Span::styled(tool_line.label.clone(), Style::default().fg(COLORS.accent)),
            ]),
            ToolStatus::Done => Line::styled(format!("✓ {}", tool_line.label), Style::default().fg(COLORS.success)),
            ToolStatus::Failed => Line::styled(format!("✗ {}", tool_line.label), Style::default().fg(COLORS.error)),
        };
        all_lines.push(line);
        live_lines += 1;
    }
    if !state.streaming_text.is_empty() {
        for line in wrap(&state.streaming_text) {
            all_lines.push(Line::styled(line, Style::default().fg(COLORS.text)));
            live_lines += 1;
        }
    }
    if live_lines > 0 {
        all_lines.push(Line::from(vec![Span::raw(" ")]));
    }

    let total = all_lines.len();
    let viewport = area.height as usize;

    if total <= viewport {
        // Everything fits - render all, no scrolling
        frame.render_widget(Paragraph::new(all_lines), area);
        return;
    }

    // Content is taller than viewport - compute which lines to show
    let scroll_offset = state.scroll_offset.min(total.saturating_sub(1));
    let visible: Vec<Line> = if scroll_offset == 0 {
        // Bottom-sticky: show the LAST viewport lines
        all_lines[total.saturating_sub(viewport)..].to_vec()
    } else {
        // Scrolled up by scroll_offset lines from the bottom
        let end = total.saturating_sub(scroll_offset);
        let start = end.saturating_sub(viewport);
        all_lines[start..end].to_vec()
    };

    // Add scroll indicator as the first line if scrolled
    let mut render_lines = Vec::new();
    if scroll_offset > 0 {
        render_lines.push(Line::styled(
            format!("↑ {} lines above · ↓ or End for bottom", scroll_offset),
            Style::default().fg(COLORS.muted),
        ));
        // One less line for content since we used one for the indicator
        let content = visible[1.min(visible.len())..].to_vec();
        render_lines.extend(content);
    } else {
        render_lines = visible;
    }

    frame.render_widget(Paragraph::new(render_lines), area);
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
        let visible_count = 6.min(suggestions.len());

        // Scroll the window to keep the selection visible
        let start = if clamped < visible_count {
            0
        } else {
            clamped - visible_count + 1
        }
        .min(suggestions.len().saturating_sub(visible_count));

        let end = (start + visible_count).min(suggestions.len());

        for (index, (command, description)) in suggestions[start..end].iter().enumerate() {
            let actual_index = start + index;
            let style = if actual_index == clamped {
                Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(COLORS.muted)
            };
            rows.push(Line::styled(format!("❯ {command:<14} {description}"), style));
        }
        let position = format!("{}/{}", clamped + 1, suggestions.len());
        rows.push(Line::styled(
            format!("↑↓ select ({position}) · tab/enter complete · esc dismiss"),
            Style::default().fg(COLORS.muted),
        ));
        rows.push(Line::from(""));
    }
    let input_row_index = rows.len();
    let display: Vec<Span> = if state.input.is_empty() {
        vec![Span::styled("Ask anything...", Style::default().fg(COLORS.muted))]
    } else {
        match state.selection {
            Some((start, end)) if end > start => {
                let chars: Vec<char> = state.input.chars().collect();
                let (start, end) = (start.min(chars.len()), end.min(chars.len()));
                if end > start {
                    let selected = chars[start..end].iter().collect::<String>();
                    vec![
                        Span::styled(chars[..start].iter().collect::<String>(), Style::default().fg(COLORS.text)),
                        Span::styled(selected, Style::default().fg(Color::Black).bg(COLORS.accent)),
                        Span::styled(chars[end..].iter().collect::<String>(), Style::default().fg(COLORS.text)),
                    ]
                } else {
                    vec![Span::styled(state.input.clone(), Style::default().fg(COLORS.text))]
                }
            }
            _ => vec![Span::styled(state.input.clone(), Style::default().fg(COLORS.text))],
        }
    };
    // Screen position of the input text, for mapping mouse columns to chars.
    state.input_origin.set((area.x + 1, area.y + 1 + input_row_index as u16));
    rows.push(Line::from(display));
    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
    let provider_config = resolved.for_provider(state.thread.current_provider);
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
        Paragraph::new(rows).block(block),
        composer_area,
    );
}

fn draw_status(frame: &mut ratatui::Frame, state: &TuiState, area: Rect) {
    let left = if state.esc_primed {
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

    let usage_rows = state.store_rows();
    let totals = usage_rows.map(|rows| sum_usage(&rows));
    let (cache_badge, cache_color, total_input) = match totals {
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
            (badge, color, totals.input_tokens + totals.cache_read_tokens + totals.cache_creation_tokens)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn render_state(state: &TuiState, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, state, 0)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_lines(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.get(x, y).symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    fn test_state(home: &std::path::Path, store: &Arc<Mutex<BatonStore>>, history: Vec<MessageRow>) -> TuiState {
        let thread = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "test".to_string(),
                cwd: home.to_string_lossy().to_string(),
                current_provider: ProviderId::Claude,
                current_model: None,
            })
            .unwrap();
        TuiState {
            store: store.clone(),
            prompt_history: Vec::new(),
            prompt_history_index: None,
            queued_messages: Vec::new(),
            goal: None,
            custom_commands: Vec::new(),
            home: home.to_path_buf(),
            thread,
            history,
            input: String::new(),
            running: false,
                streaming_text: String::new(),
            tool_lines: Vec::new(),
            overlay: Overlay::None,
            notice: None,
            diff_output: None,
            permission_mode: PermissionMode::Default,
            scroll_offset: 0,
            esc_primed: false,
            exit_armed: None,
            branch_label: None,
            quit: false,
            suggestion_index: 0,
            selection_anchor: None,
            selection: None,
            input_origin: std::cell::Cell::new((0, 0)),
        }
    }

    fn history_message(thread_id: &str, index: usize) -> MessageRow {
        MessageRow {
            id: index as i64,
            thread_id: thread_id.to_string(),
            turn: index as i64,
            role: "user".to_string(),
            provider: None,
            model: None,
            text: format!("message {index:02}"),
            created_at: String::new(),
        }
    }

    #[test]
    fn suggestion_window_follows_selection() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.input = "/".to_string();
        state.suggestion_index = 10;

        let suggestions = suggestions_for(&state.input);
        assert_eq!(suggestions.len(), 23);

        let buffer = render_state(&state, 100, 30);
        let text = buffer_lines(&buffer).join("\n");

        assert!(text.contains("11/23"), "position indicator must reflect the selection");
        let selected = suggestions[10].0;
        assert!(text.contains(selected), "selected command {selected} must be visible");
        assert!(!text.contains("/switch"), "commands above the window must be hidden");
        assert!(!text.contains("/clear"), "commands below the window must be hidden");

        let row = buffer_lines(&buffer)
            .iter()
            .position(|line| line.contains(selected))
            .expect("selected row rendered");
        let styled = (0..buffer.area.width).any(|x| {
            let cell = buffer.get(x, row as u16);
            cell.fg == COLORS.accent && cell.modifier.contains(Modifier::BOLD)
        });
        assert!(styled, "selected row must be highlighted");
    }

    #[test]
    fn transcript_scroll_shows_older_lines() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread_id = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "test".to_string(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Claude,
                current_model: None,
            })
            .unwrap()
            .id;
        let history: Vec<MessageRow> = (1..=30).map(|index| history_message(&thread_id, index)).collect();
        let mut state = test_state(home.path(), &store, history);

        let bottom = buffer_lines(&render_state(&state, 100, 30)).join("\n");
        assert!(bottom.contains("message 30"), "bottom view shows the newest message");
        assert!(!bottom.contains("message 01"), "oldest message is off-screen");

        state.scroll_offset = 8;
        let scrolled = buffer_lines(&render_state(&state, 100, 30)).join("\n");
        assert!(scrolled.contains("8 lines above"), "scroll indicator must render");
        assert!(!scrolled.contains("message 30"), "scrolled view must move away from the bottom");
        assert!(scrolled.contains("message 15"), "scrolled view must show older content");
    }

    #[test]
    fn switch_updates_composer_immediately() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        assert_eq!(state.thread.current_provider, ProviderId::Claude);

        perform_switch(&mut state, &store, ProviderId::Codex, Some("rits/zai-org/glm-5-3".to_string()));

        assert_eq!(state.thread.current_provider, ProviderId::Codex, "in-memory thread must reflect the switch");
        assert_eq!(state.thread.current_model.as_deref(), Some("rits/zai-org/glm-5-3"));
        let persisted = store.lock().unwrap().get_thread(&state.thread.id).unwrap().unwrap();
        assert_eq!(persisted.current_provider, ProviderId::Codex);

        let text = buffer_lines(&render_state(&state, 100, 30)).join("\n");
        let composer_row = text
            .lines()
            .find(|line| line.contains("codex") || line.contains("claude"))
            .expect("composer provider row rendered");
        assert!(composer_row.contains("codex"), "composer must show the new provider: {composer_row}");
        assert!(composer_row.contains("rits/zai-org/glm-5-3"), "composer must show the new model: {composer_row}");
    }

    #[test]
    fn model_command_updates_composer_immediately() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _engine_command_receiver) = tokio::sync::mpsc::unbounded_channel();

        handle_command(&mut state, &store, &engine_command_sender, "/model claude-opus-5-5");

        assert_eq!(state.thread.current_model.as_deref(), Some("claude-opus-5-5"));
        let persisted = store.lock().unwrap().get_thread(&state.thread.id).unwrap().unwrap();
        assert_eq!(persisted.current_model.as_deref(), Some("claude-opus-5-5"));
        let text = buffer_lines(&render_state(&state, 100, 30)).join("\n");
        assert!(text.contains("claude-opus-5-5"), "composer must show the new model immediately");
    }

    #[test]
    fn input_selection_renders_orange_highlight() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.input = "hello world".to_string();
        state.selection = Some((0, 5));

        let buffer = render_state(&state, 100, 30);
        let (origin_x, origin_y) = state.input_origin.get();
        assert!(origin_y > 0, "input origin must be recorded during draw");
        let selected_cells_orange = (origin_x..origin_x + 5)
            .filter(|&x| buffer.get(x, origin_y).bg == COLORS.accent)
            .count();
        assert_eq!(selected_cells_orange, 5, "selected 'hello' must carry the orange background");
        assert_ne!(buffer.get(origin_x + 6, origin_y).bg, COLORS.accent, "unselected text must not be highlighted");

        state.selection = Some((6, 11));
        let buffer = render_state(&state, 100, 30);
        let world_orange = (origin_x + 6..origin_x + 11)
            .filter(|&x| buffer.get(x, origin_y).bg == COLORS.accent)
            .count();
        assert_eq!(world_orange, 5, "a later range highlights only that range");
    }

    #[test]
    fn mouse_drag_builds_selection_from_input_origin() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.input = "hello world".to_string();
        render_state(&state, 100, 30);
        let (origin_x, origin_y) = state.input_origin.get();

        let mouse = |kind: crossterm::event::MouseEventKind, column: u16| crossterm::event::MouseEvent {
            kind,
            column,
            row: origin_y,
            modifiers: KeyModifiers::empty(),
        };
        handle_mouse(&mut state, mouse(crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), origin_x));
        handle_mouse(&mut state, mouse(crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left), origin_x + 5));
        assert_eq!(state.selection, Some((0, 5)), "drag from the first column selects 'hello'");

        let buffer = render_state(&state, 100, 30);
        let orange = (origin_x..origin_x + 5).filter(|&x| buffer.get(x, origin_y).bg == COLORS.accent).count();
        assert_eq!(orange, 5, "the dragged range renders with the orange background");
    }

    #[test]
    fn live_activity_follows_last_message_in_transcript() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread_id = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "test".to_string(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap()
            .id;
        let history = vec![history_message(&thread_id, 1)];
        let mut state = test_state(home.path(), &store, history);

        state.running = true;
        let lines = buffer_lines(&render_state(&state, 100, 30));
        let user_row = lines.iter().position(|line| line.contains("message 01")).expect("user message rendered");
        let thinking_row = lines.iter().position(|line| line.contains("Thinking")).expect("thinking spinner in transcript");
        assert!(thinking_row > user_row, "thinking renders after the user message, not above the composer");

        state.tool_lines.push(ToolLine { label: "shell cargo test".into(), status: ToolStatus::Running });
        state.streaming_text = "partial answer".into();
        let lines = buffer_lines(&render_state(&state, 100, 30));
        let user_row = lines.iter().position(|line| line.contains("message 01")).expect("user message rendered");
        let tool_row = lines.iter().position(|line| line.contains("cargo test")).expect("tool line in transcript");
        let stream_row = lines.iter().position(|line| line.contains("partial answer")).expect("streaming text in transcript");
        assert!(user_row < tool_row && tool_row < stream_row, "tools and streaming follow the user message in order");
        assert!(!lines.iter().any(|line| line.contains("Working [")), "the status bar no longer shows a Working spinner");
    }

    #[test]
    fn persisted_tool_history_renders_in_conversation() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread_id = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "test".to_string(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap()
            .id;
        {
            let guard = store.lock().unwrap();
            guard.add_message(&thread_id, 1, "user", None, None, "run the tests").unwrap();
            guard.add_message(&thread_id, 1, "tool", Some("codex"), None, "✓ shell cargo test").unwrap();
            guard.add_message(&thread_id, 1, "tool", Some("codex"), None, "✗ shell npm build").unwrap();
            guard.add_message(&thread_id, 1, "assistant", Some("codex"), None, "tests pass").unwrap();
        }
        let history = store.lock().unwrap().list_messages(&thread_id).unwrap();
        let state = test_state(home.path(), &store, history);

        let buffer = render_state(&state, 100, 30);
        let lines = buffer_lines(&buffer);
        let user_row = lines.iter().position(|line| line.contains("run the tests")).expect("user message");
        let ok_row = lines.iter().position(|line| line.contains("✓ shell cargo test")).expect("succeeded tool");
        let fail_row = lines.iter().position(|line| line.contains("✗ shell npm build")).expect("failed tool");
        let reply_row = lines.iter().position(|line| line.contains("tests pass")).expect("assistant reply");
        assert!(user_row < ok_row && ok_row < fail_row && fail_row < reply_row, "tool history stays in conversation order");

        let ok_line: Vec<(u16, u16)> = (0..buffer.area.width)
            .filter(|&x| buffer.get(x, ok_row as u16).symbol() == "✓")
            .map(|x| (x, ok_row as u16))
            .collect();
        assert!(!ok_line.is_empty());
        assert_eq!(buffer.get(ok_line[0].0, ok_line[0].1).fg, COLORS.success, "succeeded tool renders green");
        let fail_cell = (0..buffer.area.width).find(|&x| buffer.get(x, fail_row as u16).symbol() == "✗").expect("marker");
        assert_eq!(buffer.get(fail_cell, fail_row as u16).fg, COLORS.error, "failed tool renders red");
    }

    #[test]
    fn diff_messages_render_with_diff_colors() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread_id = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "test".to_string(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap()
            .id;
        {
            let guard = store.lock().unwrap();
            guard.add_message(&thread_id, 1, "user", None, None, "edit the file").unwrap();
            guard.add_message(&thread_id, 1, "assistant", Some("codex"), None, "edited").unwrap();
            guard
                .add_message(&thread_id, 1, "diff", None, None, "diff --git a/notes.txt b/notes.txt\nindex 123..456 100644\n--- a/notes.txt\n+++ b/notes.txt\n@@ -1 +1 @@\n-old line\n+new line\n")
                .unwrap();
        }
        let history = store.lock().unwrap().list_messages(&thread_id).unwrap();
        let state = test_state(home.path(), &store, history);

        let buffer = render_state(&state, 100, 30);
        let lines = buffer_lines(&buffer);
        let find_row = |needle: &str| lines.iter().position(|line| line.contains(needle)).unwrap_or_else(|| panic!("{needle} rendered"));
        let header_row = find_row("• Edited notes.txt (+1 -1)");
        let added_row = find_row("+new line");
        let removed_row = find_row("-old line");
        let reply_row = find_row("edited");
        assert!(reply_row < header_row && header_row < added_row, "the diff block renders after the reply, in conversation order");
        let added_cell = (0..buffer.area.width).find(|&x| buffer.get(x, added_row as u16).symbol() == "+").unwrap();
        let removed_cell = (0..buffer.area.width).find(|&x| buffer.get(x, removed_row as u16).symbol() == "-").unwrap();
        assert_eq!(buffer.get(added_cell, added_row as u16).fg, COLORS.success, "added lines render green");
        assert_eq!(buffer.get(removed_cell, removed_row as u16).fg, COLORS.error, "removed lines render red");
        assert_eq!(buffer.get(0, added_row as u16).bg, ratatui::style::Color::Rgb(33, 58, 43), "added rows carry the tinted background");
        assert_eq!(buffer.get(0, removed_row as u16).bg, ratatui::style::Color::Rgb(74, 34, 29), "removed rows carry the tinted background");
    }

    #[test]
    fn multi_command_persists_parallel_threads() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, mut engine_command_receiver) = tokio::sync::mpsc::unbounded_channel();

        handle_command(&mut state, &store, &engine_command_sender, "/multi codex opencode compare notes");

        let mut submitted = Vec::new();
        while let Ok(command) = engine_command_receiver.try_recv() {
            if let EngineCommand::Submit { thread_id, text, .. } = command {
                submitted.push((thread_id, text));
            }
        }
        assert_eq!(submitted.len(), 2, "one submit per provider");
        assert!(submitted.iter().all(|(_, text)| text == "compare notes"));
        for (thread_id, _) in &submitted {
            let thread = store.lock().unwrap().get_thread(thread_id).unwrap();
            assert!(thread.is_some(), "parallel thread {thread_id} must be persisted before submit");
        }
    }

    #[test]
    fn up_arrow_recalls_project_prompt_history() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();

        submit(&mut state, &store, &engine_command_sender, "first prompt".to_string());
        submit(&mut state, &store, &engine_command_sender, "second prompt".to_string());
        assert_eq!(
            store.lock().unwrap().list_prompt_history(&state.thread.cwd).unwrap(),
            vec!["first prompt", "second prompt"],
            "submits persist to the project history"
        );

        let key = |code: KeyCode| KeyEvent { code, modifiers: KeyModifiers::empty(), kind: KeyEventKind::Press, state: crossterm::event::KeyEventState::empty() };
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Up));
        assert_eq!(state.input, "second prompt", "Up on an empty composer recalls the newest prompt");
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Up));
        assert_eq!(state.input, "first prompt", "Up again walks back");
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Down));
        assert_eq!(state.input, "second prompt", "Down walks forward");
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Down));
        assert_eq!(state.input, "", "Down past the newest returns to an empty composer");

        // editing exits recall mode; Up then scrolls instead
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Up));
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char('x')));
        let scrolled = state.scroll_offset;
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Up));
        assert_eq!(state.scroll_offset, scrolled + 1, "Up with edited input scrolls the transcript");
    }
}
