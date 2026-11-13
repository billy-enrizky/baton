use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Terminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::adapters::{AdapterLifecycle, TurnEvent};
use crate::checkpoints::{diff_checkpoint_against_worktree, restore_checkpoint};
use crate::config::{describe_auth, load_config_file, resolve_config, save_config_file};
use crate::handoff::git_branch_label;
use crate::orchestrate::{self, IntegrateReport, OrchestrationProgress, RunReport, ShipReport};
use crate::runtime::{ensure_persisted, EngineCommand, EngineEvent};
use crate::sync;
use crate::store::{BatonStore, MessageRow, NewThread, ThreadRow};
use crate::types::{
    normalize_reasoning_effort, ApprovalDecision, PermissionMode, ProviderId, SessionMode,
    TurnPhase,
};
use crate::usage::{cache_hit_rate, format_token_count, format_usage_line, sum_usage};

pub mod viewport;

mod diff_render;
mod markdown;

use viewport::wrapped_rows;

const COLORS: ColorConstants = ColorConstants {
    accent: Color::Rgb(240, 198, 116),
    secondary: Color::Rgb(196, 181, 253),
    text: Color::Rgb(229, 231, 235),
    muted: Color::Rgb(148, 163, 184),
    border: Color::Rgb(59, 68, 80),
    user_bg: Color::Rgb(48, 54, 61),
    code_bg: Color::Rgb(30, 36, 46),
    success: Color::Rgb(134, 239, 172),
    error: Color::Rgb(252, 165, 165),
    warning: Color::Rgb(251, 191, 36),
    selection_bg: Color::Rgb(62, 80, 120),
};

struct ColorConstants {
    accent: Color,
    secondary: Color,
    text: Color,
    muted: Color,
    border: Color,
    user_bg: Color,
    code_bg: Color,
    success: Color,
    error: Color,
    warning: Color,
    selection_bg: Color,
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
    ("/switch", "switch to a single harness: /switch <provider> [model]"),
    ("/model", "set model for this thread"),
    ("/effort", "set reasoning effort: /effort low|medium|high|xhigh|max|default"),
    ("/mode", "permission mode: default|acceptEdits|bypassPermissions|plan"),
    ("/auth", "billing mode: /auth <provider> <subscription|api>"),
    ("/new", "start a new thread"),
    ("/fork", "branch this conversation into a new thread"),
    ("/worktree", "fork this conversation into an isolated git worktree"),
    ("/threads", "list threads in this directory"),
    ("/open", "open a thread by id prefix"),
    ("/usage", "token usage and cache hit rate"),
    ("/checkpoints", "list checkpoints for this thread"),
    ("/review", "review the latest turn diff with line notes"),
    ("/diff", "diff a checkpoint against the working tree"),
    ("/rewind", "rewind to a previous message"),
    ("/goal", "set a multi-turn objective the agent works toward"),
    ("/find", "search the conversation"),
    ("/multi", "same prompt to multiple providers; --worktrees isolates them"),
    ("/orchestrate", "orchestrate mode or one-off: /orchestrate [on|off] [goal]"),
    ("/runs", "list or reopen orchestration runs"),
    ("/queue", "show or clear queued follow-ups"),
    ("/turns", "list durable turn phases"),
    ("/status", "show thread phase, goal, queue, and adapter states"),
    ("/compact", "compact the conversation to free context"),
    ("/context", "show context usage"),
    ("/init", "create a CLAUDE.md memory file"),
    ("/export", "export the conversation"),
    ("/settings", "configure the current harness's settings"),
    ("/mcp", "manage MCP servers and sync across harnesses"),
    ("/skills", "list skills across harnesses"),
    ("/copy", "copy the latest assistant reply to the clipboard"),
    ("/clear", "clear panels and notices"),
    ("/quit", "exit baton"),
];

const HELP_SUMMARY: &str = "\
Type what you want to do. Baton picks a harness.

Common
  /switch <provider> [model]  use one harness
  /model                      choose a model
  /new                        start a new thread
  /threads                    list threads here
  /status                     what is happening now
  /orchestrate [on|off] [goal]  split work across harnesses
  /runs                       orchestration history
  /review                     line notes on the latest diff
  /rewind                     go back to a previous message
  /quit                       exit

/help all  every command · shift+enter newline · up/down history";

#[derive(Debug, Clone, PartialEq)]
struct ToolLine {
    id: Option<String>,
    label: String,
    status: ToolStatus,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ToolStatus {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone)]
struct ReviewNote {
    id: i64,
    path: String,
    line: usize,
    added: bool,
    body: String,
}

struct ReviewState {
    turn: i64,
    fingerprint: String,
    rows: Vec<diff_render::DiffRowRef>,
    selected: usize,
    notes: Vec<ReviewNote>,
    note_input: Option<String>,
}

struct ApprovalState {
    thread_id: String,
    summary: String,
    respond: tokio::sync::oneshot::Sender<ApprovalDecision>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MultiStatus {
    Queued,
    Running,
    Done,
    Failed,
}

struct MultiChild {
    thread_id: String,
    provider: ProviderId,
    status: MultiStatus,
    reply: String,
    error: Option<String>,
}

struct MultiView {
    prompt: String,
    selected: usize,
    children: Vec<MultiChild>,
}

struct OrchestrationPreview {
    goal: String,
    plan: orchestrate::Plan,
    coordinator: ProviderId,
    coordinator_model: String,
    dirty_source: bool,
}

enum OrchestrationEvent {
    Done(Box<RunReport>),
    Failed(String),
    Preview(Box<OrchestrationPreview>),
    PreviewFailed(String),
    Progress(OrchestrationProgress),
    Integrated(Box<IntegrateReport>),
    IntegrateFailed(String),
    Shipped(Box<ShipReport>),
    ShipFailed(String),
}

enum NativeRewindEvent {
    Done {
        turn: i64,
        text: String,
        provider_retry_skipped: bool,
    },
    Failed(String),
}

struct OrchestrationWorkerView {
    index: usize,
    title: String,
    provider: ProviderId,
    model: String,
    status: String,
    worktree: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct InputRow {
    screen_y: u16,
    screen_x_start: u16,
    char_start: usize,
    line_len: usize,
    scroll: usize,
}

/// The transcript's current rendered window, used to map mouse rows back to
/// absolute transcript lines for selection and copy.
struct TranscriptViewport {
    area: Rect,
    content_top: u16,
    content_start: usize,
    content_rows: usize,
    total: usize,
    plain: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TranscriptPoint {
    line: usize,
    column: usize,
}

enum Overlay {
    None,
    RewindPicker { selected: usize, native: bool },
    CodeConfirm { turn: i64, git_ref: String },
    SwitchPicker { selected: usize },
    ModelPicker { provider: ProviderId, models: Vec<String>, selected: usize, filter: String },
    SettingsPicker { provider: ProviderId, categories: Vec<(String, Vec<String>)>, category_index: usize, value_index: usize },
    McpPicker { selected: usize },
    SkillsPicker { selected: usize, skills: Vec<(String, String)> },
    ModePicker { selected: usize },
    AuthPicker { selected: usize },
    ThreadPicker { selected: usize, threads: Vec<(String, String)> },
    CheckpointPicker { selected: usize, checkpoints: Vec<(i64, String)> },
    DirtyConfirm { goal: String, max_tasks: usize, plan_file: Option<String> },
    OrchestrationPreview { preview: Box<OrchestrationPreview> },
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
    goal_paused: bool,
    custom_commands: Vec<(String, String)>,
    home: std::path::PathBuf,
    thread: ThreadRow,
    adapter_lifecycles: std::collections::HashMap<ProviderId, AdapterLifecycle>,
    history: Vec<MessageRow>,
    input: String,
    input_cursor: usize,
    input_layout: std::cell::RefCell<Vec<InputRow>>,
    running: bool,
    queued_provider: Option<ProviderId>,
    streaming_text: String,
    tool_lines: Vec<ToolLine>,
    overlay: Overlay,
    approvals: Vec<ApprovalState>,
    review: Option<ReviewState>,
    multi: Option<MultiView>,
    notice: Option<String>,
    diff_output: Option<String>,
    diff_cache: std::cell::RefCell<diff_render::DiffCache>,
    markdown_cache: std::cell::RefCell<markdown::MarkdownCache>,
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
    transcript_view: std::cell::RefCell<Option<TranscriptViewport>>,
    transcript_selection: std::cell::Cell<Option<(TranscriptPoint, TranscriptPoint)>>,
    transcript_anchor: std::cell::Cell<Option<TranscriptPoint>>,
    transcript_total: std::cell::Cell<usize>,
    model_request_id: u64,
    banner_cache: std::cell::RefCell<Option<(String, Vec<Line<'static>>)>>,
    orchestration_pending: bool,
    orchestration_cancel: Arc<AtomicBool>,
    orchestration_cancelling: bool,
    orchestration_workers: Vec<OrchestrationWorkerView>,
    orchestration_report: Option<RunReport>,
    orchestration_scroll: usize,
    orchestration_selected: usize,
    orchestration_integrating: bool,
    orchestration_confirm_integrate: bool,
    orchestration_integrate_report: Option<IntegrateReport>,
    orchestration_shipping: bool,
    orchestration_confirm_ship: bool,
    orchestration_ship_report: Option<ShipReport>,
    orchestration_sender: std::sync::mpsc::Sender<OrchestrationEvent>,
    native_rewind_sender: std::sync::mpsc::Sender<NativeRewindEvent>,
    native_rewind_running: bool,
}

pub fn run_tui(
    store: Arc<Mutex<BatonStore>>,
    home: std::path::PathBuf,
    thread: ThreadRow,
    adapter_lifecycles: std::collections::HashMap<ProviderId, AdapterLifecycle>,
    engine_command_sender: tokio::sync::mpsc::UnboundedSender<EngineCommand>,
    engine_event_receiver: std::sync::mpsc::Receiver<EngineEvent>,
    turn_event_receiver: std::sync::mpsc::Receiver<(String, TurnEvent)>,
    initial_prompt: Option<String>,
    permission_override: Option<PermissionMode>,
) -> Result<()> {
    let history = store.lock().unwrap().list_messages(&thread.id)?;
    let queued_messages = store
        .lock()
        .unwrap()
        .list_queued_messages(&thread.id)
        .unwrap_or_default();
    let prompt_history = store.lock().unwrap().list_prompt_history(&thread.cwd).unwrap_or_default();
    let branch_label = git_branch_label(std::path::Path::new(&thread.cwd));
    let permission_mode = permission_override.unwrap_or(thread.permission_mode);
    let goal = thread.goal.clone();
    let goal_paused = thread.goal_paused;
    let reconciled = store
        .lock()
        .unwrap()
        .reconcile_running_turns(Some(thread.cwd.as_str()))
        .unwrap_or_default();
    let recovered_runs = orchestrate::reconcile_abandoned_runs(&store.lock().unwrap())
        .unwrap_or_default();

    let custom_commands = discover_custom_commands(&thread.cwd);
    let (orchestration_sender, orchestration_receiver) = std::sync::mpsc::channel();
    let (native_rewind_sender, native_rewind_receiver) = std::sync::mpsc::channel();
    let mut state = TuiState {
        store: store.clone(),
        prompt_history,
        prompt_history_index: None,
        queued_messages,
        goal,
        goal_paused,
        custom_commands,
        home,
        thread,
        adapter_lifecycles,
        history,
        input: String::new(),
        input_cursor: 0,
        input_layout: std::cell::RefCell::new(Vec::new()),
        running: false,
        queued_provider: None,
        streaming_text: String::new(),
        tool_lines: Vec::new(),
        overlay: Overlay::None,
        approvals: Vec::new(),
        review: None,
        multi: None,
        notice: None,
        diff_output: None,
        diff_cache: std::cell::RefCell::new(diff_render::DiffCache::default()),
        markdown_cache: std::cell::RefCell::new(markdown::MarkdownCache::default()),
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
        transcript_view: std::cell::RefCell::new(None),
        transcript_selection: std::cell::Cell::new(None),
        transcript_anchor: std::cell::Cell::new(None),
        transcript_total: std::cell::Cell::new(0),
        model_request_id: 0,
        banner_cache: std::cell::RefCell::new(None),
        orchestration_pending: false,
        orchestration_cancel: Arc::new(AtomicBool::new(false)),
        orchestration_cancelling: false,
        orchestration_workers: Vec::new(),
        orchestration_report: None,
        orchestration_scroll: 0,
        orchestration_selected: 0,
        orchestration_integrating: false,
        orchestration_confirm_integrate: false,
        orchestration_integrate_report: None,
        orchestration_shipping: false,
        orchestration_confirm_ship: false,
        orchestration_ship_report: None,
        orchestration_sender,
        native_rewind_sender,
        native_rewind_running: false,
    };

    let mut startup_notes = Vec::new();
    if let Some(run) = recovered_runs.last() {
        startup_notes.push(format!(
            "recovered abandoned orchestration {} ({})",
            &run.run_id[..8.min(run.run_id.len())],
            run.status
        ));
    }
    if !reconciled.is_empty() {
        let turns = reconciled
            .iter()
            .map(|row| row.turn.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        startup_notes.push(format!(
            "previous turn(s) {turns} did not finish and were marked unverified"
        ));
    }
    if !startup_notes.is_empty() {
        state.notice = Some(startup_notes.join("\n"));
    }

    if let Some(prompt) = initial_prompt {
        let thread_id = state.thread.id.clone();
        let result = {
            let guard = store.lock().unwrap();
            ensure_persisted(&guard, &mut state.thread)
        };
        if result.is_ok() {
            {
                let guard = store.lock().unwrap();
                let _ = guard.set_thread_permission_mode(&thread_id, state.permission_mode);
                let _ = guard.set_thread_goal(
                    &thread_id,
                    state.goal.as_deref(),
                    state.goal_paused,
                );
            }
            dispatch_message(&mut state, &store, &engine_command_sender, prompt, false);
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
    // Let terminals report Shift+Enter distinctly from Enter. Unsupported
    // terminals ignore this and keep the Alt+Enter fallback.
    let _ = execute!(
        std::io::stdout(),
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    );
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut spinner_frame = 0usize;
    while !state.quit {
        while let Ok((event_thread, event)) = turn_event_receiver.try_recv() {
            apply_turn_event_for_thread(&mut state, event_thread, event);
        }
        while let Ok(event) = engine_event_receiver.try_recv() {
            apply_engine_event(&mut state, &store, event, &engine_command_sender);
        }
        while let Ok(event) = orchestration_receiver.try_recv() {
            apply_orchestration_event(&mut state, event);
        }
        while let Ok(event) = native_rewind_receiver.try_recv() {
            apply_native_rewind_event(&mut state, &store, event);
        }
        if crossterm::event::poll(Duration::from_millis(32))? {
            match crossterm::event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        if std::env::var_os("BATON_DEBUG_KEYS").is_some() {
                            eprintln!("baton key: {:?} {:?}", key.code, key.modifiers);
                        }
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
    let _ = execute!(
        terminal.backend_mut(),
        crossterm::event::PopKeyboardEnhancementFlags
    );
    execute!(
        terminal.backend_mut(),
        crossterm::cursor::Show,
        terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    let _ = engine_command_sender.send(EngineCommand::Quit);
    Ok(())
}

/// Mouse selection inside the composer and the transcript. A click sets an
/// anchor, a drag extends the range, and releasing over the transcript copies
/// the selected lines. The transcript uses absolute rendered-line indices so
/// scrolling does not shift an existing selection.
fn handle_mouse(state: &mut TuiState, mouse: crossterm::event::MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};
    let input_index = input_index_at(state, mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(index) = input_index {
                clear_transcript_selection(state);
                state.selection_anchor = Some(index);
                state.selection = Some((index, index));
                state.input_cursor = index;
            } else if let Some(point) = transcript_point_at(state, mouse.column, mouse.row) {
                state.selection_anchor = None;
                state.selection = None;
                state.transcript_anchor.set(Some(point));
                state.transcript_selection.set(Some((point, point)));
            } else {
                state.selection_anchor = None;
                state.selection = None;
                clear_transcript_selection(state);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let (Some(anchor), Some(index)) = (state.selection_anchor, input_index) {
                state.selection = Some((anchor.min(index), anchor.max(index)));
                state.input_cursor = index;
            } else if let (Some(anchor), Some(point)) = (
                state.transcript_anchor.get(),
                transcript_point_for_drag(state, mouse.column, mouse.row),
            ) {
                state
                    .transcript_selection
                    .set(Some((anchor.min(point), anchor.max(point))));
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if state.transcript_anchor.get().is_some() {
                if state
                    .transcript_selection
                    .get()
                    .map(|(start, end)| start != end)
                    .unwrap_or(false)
                {
                    copy_transcript_selection(state);
                } else {
                    clear_transcript_selection(state);
                }
            }
        }
        _ => {}
    }
}

fn clear_transcript_selection(state: &mut TuiState) {
    state.transcript_selection.set(None);
    state.transcript_anchor.set(None);
}

/// Maps a screen cell to an absolute rendered transcript line. Only the
/// visible content rows count; the scroll indicator and composer are excluded.
fn transcript_line_at(state: &TuiState, column: u16, row: u16) -> Option<usize> {
    let view = state.transcript_view.borrow();
    let view = view.as_ref()?;
    let area = view.area;
    if column < area.x
        || column >= area.x.saturating_add(area.width)
        || row < view.content_top
        || row >= area.y.saturating_add(area.height)
    {
        return None;
    }
    let offset = row.saturating_sub(view.content_top) as usize;
    if offset >= view.content_rows {
        return None;
    }
    Some((view.content_start + offset).min(view.total.saturating_sub(1)))
}

/// Drag rows clamp to the nearest visible line, so dragging past the top or
/// bottom of the transcript extends the selection instead of dropping it.
fn transcript_line_for_drag(state: &TuiState, row: u16) -> Option<usize> {
    let view = state.transcript_view.borrow();
    let view = view.as_ref()?;
    if view.content_rows == 0 || view.total == 0 {
        return None;
    }
    let offset = (row.saturating_sub(view.content_top) as usize).min(view.content_rows - 1);
    Some((view.content_start + offset).min(view.total - 1))
}

fn transcript_point_at(state: &TuiState, column: u16, row: u16) -> Option<TranscriptPoint> {
    let line = transcript_line_at(state, column, row)?;
    let view = state.transcript_view.borrow();
    let view = view.as_ref()?;
    let text = view.plain.get(line)?;
    let max = text.chars().count();
    let column = column.saturating_sub(view.area.x) as usize;
    Some(TranscriptPoint {
        line,
        column: column.min(max),
    })
}

fn transcript_point_for_drag(
    state: &TuiState,
    column: u16,
    row: u16,
) -> Option<TranscriptPoint> {
    let line = transcript_line_for_drag(state, row)?;
    let view = state.transcript_view.borrow();
    let view = view.as_ref()?;
    let text = view.plain.get(line)?;
    let max = text.chars().count();
    let column = column
        .saturating_sub(view.area.x)
        .min(view.area.width.saturating_sub(1)) as usize;
    Some(TranscriptPoint {
        line,
        column: column.min(max),
    })
}

fn transcript_selection_text(state: &TuiState) -> Option<(String, usize, usize)> {
    let (mut start, mut end) = state.transcript_selection.get()?;
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    let view = state.transcript_view.borrow();
    let view = view.as_ref()?;
    if view.plain.is_empty() || view.total == 0 {
        return None;
    }
    let last_line = view.plain.len().saturating_sub(1);
    start.line = start.line.min(last_line);
    end.line = end.line.min(last_line);
    let mut parts = Vec::new();
    for line in start.line..=end.line {
        let chars: Vec<char> = view.plain[line].chars().collect();
        let from = if line == start.line {
            start.column.min(chars.len())
        } else {
            0
        };
        let to = if line == end.line {
            end.column.min(chars.len())
        } else {
            chars.len()
        };
        parts.push(if to > from {
            chars[from..to].iter().collect::<String>()
        } else {
            String::new()
        });
    }
    let text = parts.join("\n");
    if text.is_empty() {
        return None;
    }
    let chars = text.chars().count();
    Some((text, end.line - start.line + 1, chars))
}

fn copy_transcript_selection(state: &mut TuiState) {
    let Some((text, lines, chars)) = transcript_selection_text(state) else {
        return;
    };
    match crate::clipboard::copy(&text) {
        Ok(()) => {
            state.notice = Some(format!(
                "copied {chars} character{} from {lines} transcript line{}",
                if chars == 1 { "" } else { "s" },
                if lines == 1 { "" } else { "s" }
            ))
        }
        Err(error) => state.notice = Some(format!("cannot copy transcript selection: {error}")),
    }
}

fn hide_transcript_view(state: &TuiState) {
    *state.transcript_view.borrow_mut() = None;
    state.transcript_total.set(0);
    state.transcript_selection.set(None);
    state.transcript_anchor.set(None);
}

fn tint_line_range(line: &mut Line<'_>, start: usize, end: usize, background: Color) {
    if end <= start {
        return;
    }
    let mut offset = 0usize;
    let mut spans = Vec::new();
    for span in line.spans.drain(..) {
        let chars: Vec<char> = span.content.chars().collect();
        let length = chars.len();
        let local_start = start.saturating_sub(offset).min(length);
        let local_end = end.saturating_sub(offset).min(length);
        if local_start > 0 {
            spans.push(Span::styled(
                chars[..local_start].iter().collect::<String>(),
                span.style,
            ));
        }
        if local_end > local_start {
            spans.push(Span::styled(
                chars[local_start..local_end].iter().collect::<String>(),
                span.style.bg(background),
            ));
        }
        if local_end < length {
            spans.push(Span::styled(
                chars[local_end..].iter().collect::<String>(),
                span.style,
            ));
        }
        offset = offset.saturating_add(length);
    }
    line.spans = spans;
}

fn line_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
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
        EngineEvent::TurnQueued { thread_id, provider } => {
            if let Some(child) = state
                .multi
                .as_mut()
                .and_then(|multi| multi.children.iter_mut().find(|child| child.thread_id == thread_id))
            {
                child.status = MultiStatus::Queued;
                child.error = None;
            }
            if thread_id != state.thread.id {
                return;
            }
            state.running = true;
            state.queued_provider = Some(provider);
            state.streaming_text.clear();
            state.tool_lines.clear();
            state.notice = Some(format!(
                "queued for {provider}; waiting for that provider's active turn to finish"
            ));
        }
        EngineEvent::TurnStarted { thread_id } => {
            state.approvals.retain(|approval| approval.thread_id != thread_id);
            if let Some(child) = state
                .multi
                .as_mut()
                .and_then(|multi| multi.children.iter_mut().find(|child| child.thread_id == thread_id))
            {
                child.status = MultiStatus::Running;
                child.error = None;
            }
            if thread_id != state.thread.id {
                return;
            }
            state.running = true;
            state.queued_provider = None;
            state.streaming_text.clear();
            state.tool_lines.clear();
            state.notice = None;
            state.diff_output = None;
        }
        EngineEvent::TurnFinished { thread_id, ok, error, phase } => {
            record_worker_followup(state, store, &thread_id, ok, error.as_deref(), phase);
            state.approvals.retain(|approval| approval.thread_id != thread_id);
            if let Some(child) = state
                .multi
                .as_mut()
                .and_then(|multi| multi.children.iter_mut().find(|child| child.thread_id == thread_id))
            {
                child.status = if ok { MultiStatus::Done } else { MultiStatus::Failed };
                child.error = error.clone();
                if ok {
                    let reply = store
                        .lock()
                        .unwrap()
                        .list_messages(&thread_id)
                        .ok()
                        .and_then(|messages| {
                            messages
                                .into_iter()
                                .rev()
                                .find(|message| message.role == "assistant")
                                .map(|message| message.text)
                        });
                    if let Some(reply) = reply {
                        child.reply = reply;
                    }
                }
            }
            if thread_id != state.thread.id {
                return;
            }
            state.running = false;
            state.queued_provider = None;
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
                let detail = error.unwrap_or_else(|| "unknown error".into());
                state.notice = Some(match phase {
                    TurnPhase::Interrupted => "turn interrupted".to_string(),
                    TurnPhase::Unverified => format!("turn unverified: {detail}"),
                    _ => format!("turn failed: {detail}"),
                });
            }
            // Only advance the queue after a clean turn. A failure or an
            // interrupt should not automatically fire the next prompt.
            if ok {
                let next = store
                    .lock()
                    .unwrap()
                    .pop_queued_message(&state.thread.id)
                    .unwrap_or(None);
                if let Some(next) = next {
                    state.queued_messages = store
                        .lock()
                        .unwrap()
                        .list_queued_messages(&state.thread.id)
                        .unwrap_or_default();
                    let remaining = state.queued_messages.len();
                    let preview: String = next.chars().take(60).collect();
                    state.notice = Some(format!("sending queued message ({remaining} remaining): {preview}"));
                    submit(state, store, engine_command_sender, next);
                }
            } else if !state.queued_messages.is_empty() {
                let waiting = state.queued_messages.len();
                let prefix = state.notice.take().unwrap_or_default();
                state.notice = Some(format!("{prefix} · {waiting} queued message(s) kept; press enter to send"));
            }
        }
        EngineEvent::ModelsLoaded {
            thread_id,
            provider,
            request_id,
            models,
            error,
        } => {
            if thread_id != state.thread.id || request_id != state.model_request_id {
                return;
            }
            let picker_matches = matches!(
                &state.overlay,
                Overlay::ModelPicker { provider: current, .. } if *current == provider
            );
            if !picker_matches {
                return;
            }
            if models.is_empty() {
                if let Some(error) = error {
                    state.notice = Some(format!(
                        "live model catalog unavailable for {provider}: {error}"
                    ));
                }
                return;
            }
            let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
            let default_model = resolved.for_provider(provider).model.clone();
            let mut live = models;
            if !default_model.is_empty() && !live.contains(&default_model) {
                live.insert(0, default_model);
            }
            let count = live.len();
            if let Overlay::ModelPicker {
                models: current,
                selected,
                filter,
                ..
            } = &mut state.overlay
            {
                let previous = model_filter_indices(current, filter)
                    .get(*selected)
                    .and_then(|index| current.get(*index))
                    .cloned();
                *current = live;
                let filtered = model_filter_indices(current, filter);
                *selected = previous
                    .and_then(|model| {
                        filtered
                            .iter()
                            .position(|index| current[*index] == model)
                    })
                    .unwrap_or(0);
            }
            state.notice = Some(format!("loaded {count} live model(s) for {provider}"));
        }
        EngineEvent::TurnRejected { thread_id, text, reason } => {
            if let Some(child) = state
                .multi
                .as_mut()
                .and_then(|multi| multi.children.iter_mut().find(|child| child.thread_id == thread_id))
            {
                child.status = MultiStatus::Failed;
                child.error = Some(reason.clone());
            }
            if thread_id == state.thread.id {
                enqueue_message(state, store, text);
                state.notice = Some(format!(
                    "{reason}; queued ({} waiting)",
                    state.queued_messages.len()
                ));
            }
        }
        EngineEvent::SteerRejected { thread_id, text } => {
            if thread_id == state.thread.id {
                enqueue_message(state, store, text);
                state.notice = Some(format!(
                    "message queued for after this turn ({} waiting)",
                    state.queued_messages.len()
                ));
            }
        }
        EngineEvent::Turn(_) => {}
    }
}

fn apply_orchestration_event(state: &mut TuiState, event: OrchestrationEvent) {
    match event {
        OrchestrationEvent::Preview(preview) => {
            state.orchestration_pending = false;
            state.orchestration_cancelling = false;
            state.orchestration_workers.clear();
            state.orchestration_report = None;
            state.orchestration_scroll = 0;
            state.orchestration_selected = 0;
            state.notice = Some(format!(
                "plan ready · {} worker(s) · enter to run · esc to edit",
                preview.plan.tasks.len()
            ));
            state.overlay = Overlay::OrchestrationPreview { preview };
        }
        OrchestrationEvent::PreviewFailed(error) => {
            state.orchestration_pending = false;
            state.orchestration_cancelling = false;
            if error == orchestrate::CANCELLED_ERROR {
                state.notice = Some("planning cancelled".into());
            } else {
                state.notice = Some(format!("planning failed: {error}"));
            }
        }
        OrchestrationEvent::Done(report) => {
            state.orchestration_pending = false;
            state.orchestration_cancelling = false;
            state.orchestration_workers.clear();
            state.orchestration_scroll = 0;
            state.orchestration_confirm_ship = false;
            state.orchestration_ship_report = None;
            let mut summary = format!(
                "run {} · {}\n{}",
                &report.run_id[..8.min(report.run_id.len())],
                report.status,
                report.goal
            );
            for (index, task) in report.tasks.iter().enumerate() {
                summary.push_str(&format!(
                    "\n  [{}] {} · {} · {}",
                    index + 1,
                    task.title,
                    task.provider,
                    task.status
                ));
                if let Some(commit) = &task.commit {
                    summary.push_str(&format!(" · {}", &commit[..8.min(commit.len())]));
                }
            }
            record_orchestration_message(state, summary);
            state.notice = Some(if report.status == "cancelled" {
                format!(
                    "orchestration cancelled: {} worker(s)",
                    report.tasks.len()
                )
            } else {
                format!(
                    "orchestration {}: {} worker(s)",
                    report.status,
                    report.tasks.len()
                )
            });
            state.orchestration_report = Some(*report);
        }
        OrchestrationEvent::Failed(error) => {
            state.orchestration_pending = false;
            state.orchestration_cancelling = false;
            state.orchestration_workers.clear();
            state.orchestration_report = None;
            state.orchestration_scroll = 0;
            state.orchestration_selected = 0;
            state.orchestration_confirm_ship = false;
            state.orchestration_ship_report = None;
            if error == orchestrate::CANCELLED_ERROR {
                record_orchestration_message(state, "run cancelled".to_string());
                state.notice = Some("orchestration cancelled".into());
            } else {
                record_orchestration_message(state, format!("run failed · {error}"));
                state.notice = Some(format!("orchestration failed: {error}"));
            }
        }
        OrchestrationEvent::Progress(progress) => match progress {
            OrchestrationProgress::PlanReady { tasks, .. } => {
                state.orchestration_workers = tasks
                    .iter()
                    .enumerate()
                    .map(|(index, task)| OrchestrationWorkerView {
                        index: index + 1,
                        title: task.title.clone(),
                        provider: ProviderId::parse(&task.provider)
                            .unwrap_or(ProviderId::Codex),
                        model: String::new(),
                        status: "queued".to_string(),
                        worktree: None,
                        error: None,
                    })
                    .collect();
                state.notice = Some(format!(
                    "orchestrating: {} worker(s) planned",
                    state.orchestration_workers.len()
                ));
            }
            OrchestrationProgress::WorkerStarted {
                index,
                title,
                provider,
                model,
                worktree,
            } => {
                let view = orchestration_worker(state, index, &title);
                view.provider = provider;
                view.model = model;
                view.status = "running".to_string();
                view.worktree = worktree.map(|path| path.display().to_string());
            }
            OrchestrationProgress::WorkerFinished {
                index,
                title,
                status,
                commit,
                error,
            } => {
                let view = orchestration_worker(state, index, &title);
                view.status = if status == "completed" {
                    commit
                        .as_ref()
                        .map(|commit| format!("completed · {}", &commit[..8.min(commit.len())]))
                        .unwrap_or_else(|| "completed".to_string())
                } else {
                    status
                };
                view.error = error;
            }
        },
        OrchestrationEvent::Integrated(report) => {
            state.orchestration_integrating = false;
            state.orchestration_confirm_integrate = false;
            let notice = match &report.conflict {
                Some(conflict) => format!(
                    "integration conflict in task {}: {} · {} merged",
                    conflict.index,
                    conflict.files.join(", "),
                    report.merged.len()
                ),
                None => format!(
                    "integration done: {} merged, {} skipped",
                    report.merged.len(),
                    report.skipped.len()
                ),
            };
            record_orchestration_message(state, format!("integration · {notice}"));
            state.notice = Some(notice);
            state.orchestration_integrate_report = Some(*report);
        }
        OrchestrationEvent::IntegrateFailed(error) => {
            state.orchestration_integrating = false;
            record_orchestration_message(state, format!("integration failed · {error}"));
            state.notice = Some(format!("integration failed: {error}"));
        }
        OrchestrationEvent::Shipped(report) => {
            state.orchestration_shipping = false;
            state.orchestration_confirm_ship = false;
            if let Some(run) = state.orchestration_report.as_mut() {
                for shipped in &report.opened {
                    if let Some(task) = run.tasks.get_mut(shipped.index.saturating_sub(1)) {
                        task.pr_url = Some(shipped.pr_url.clone());
                    }
                }
            }
            let notice = format!(
                "ship done: {} PR(s), {} skipped, {} failed",
                report.opened.len(),
                report.skipped.len(),
                report.failed.len()
            );
            let mut message = format!("ship · {notice}");
            for task in &report.opened {
                message.push_str(&format!("\n  [{}] {}", task.index, task.pr_url));
            }
            for task in &report.failed {
                message.push_str(&format!(
                    "\n  failed [{}] {} · {}",
                    task.index, task.title, task.reason
                ));
            }
            record_orchestration_message(state, message);
            state.notice = Some(notice);
            state.orchestration_ship_report = Some(*report);
        }
        OrchestrationEvent::ShipFailed(error) => {
            state.orchestration_shipping = false;
            record_orchestration_message(state, format!("ship failed · {error}"));
            state.notice = Some(format!("ship failed: {error}"));
        }
    }
}

fn orchestration_worker<'a>(
    state: &'a mut TuiState,
    index: usize,
    title: &str,
) -> &'a mut OrchestrationWorkerView {
    if let Some(position) = state
        .orchestration_workers
        .iter()
        .position(|worker| worker.index == index)
    {
        return &mut state.orchestration_workers[position];
    }
    state.orchestration_workers.push(OrchestrationWorkerView {
        index,
        title: title.to_string(),
        provider: ProviderId::Codex,
        model: String::new(),
        status: "queued".to_string(),
        worktree: None,
        error: None,
    });
    let position = state.orchestration_workers.len() - 1;
    &mut state.orchestration_workers[position]
}

fn start_integration(state: &mut TuiState) {
    let Some(report) = state.orchestration_report.as_ref() else {
        return;
    };
    if state.orchestration_integrating {
        return;
    }
    let run_id = report.run_id.clone();
    let cwd = report
        .repo_root
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from(&state.thread.cwd));
    let home = state.home.clone();
    state.orchestration_integrating = true;
    state.orchestration_confirm_integrate = false;
    state.orchestration_integrate_report = None;
    state.notice = Some("integrating worker branches...".into());
    let sender = state.orchestration_sender.clone();
    std::thread::spawn(move || {
        let result = BatonStore::open(&home.join("baton.sqlite")).and_then(|store| {
            orchestrate::integrate(&store, &cwd, &run_id, &[])
        });
        let event = match result {
            Ok(report) => OrchestrationEvent::Integrated(Box::new(report)),
            Err(error) => OrchestrationEvent::IntegrateFailed(error.to_string()),
        };
        let _ = sender.send(event);
    });
}

fn start_shipping(state: &mut TuiState) {
    let Some(report) = state.orchestration_report.as_ref() else {
        return;
    };
    if state.orchestration_shipping {
        return;
    }
    let run_id = report.run_id.clone();
    let cwd = report
        .repo_root
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from(&state.thread.cwd));
    let home = state.home.clone();
    state.orchestration_shipping = true;
    state.orchestration_confirm_ship = false;
    state.orchestration_ship_report = None;
    state.notice = Some("pushing worker branches and opening PRs...".into());
    let sender = state.orchestration_sender.clone();
    std::thread::spawn(move || {
        let result = BatonStore::open(&home.join("baton.sqlite"))
            .and_then(|store| orchestrate::open_prs(&store, &cwd, &run_id, &[]));
        let event = match result {
            Ok(report) => OrchestrationEvent::Shipped(Box::new(report)),
            Err(error) => OrchestrationEvent::ShipFailed(error.to_string()),
        };
        let _ = sender.send(event);
    });
}

const ORCHESTRATE_USAGE: &str =
    "usage: /orchestrate [--plan-only] [--max-tasks N] [--plan-file path] <goal>";

#[derive(Debug, PartialEq, Eq)]
struct OrchestrateArgs {
    goal: String,
    max_tasks: usize,
    plan_file: Option<String>,
    plan_only: bool,
}

fn parse_orchestrate_args(args: &[String]) -> std::result::Result<OrchestrateArgs, String> {
    let mut goal_parts: Vec<String> = Vec::new();
    let mut max_tasks = 4usize;
    let mut plan_file = None;
    let mut plan_only = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--max-tasks" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(format!("--max-tasks needs a number · {ORCHESTRATE_USAGE}"));
                };
                let Ok(value) = value.parse::<usize>() else {
                    return Err(format!("--max-tasks must be a number, got \"{value}\""));
                };
                max_tasks = value.clamp(1, 16);
                index += 2;
            }
            "--plan-file" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(format!("--plan-file needs a path · {ORCHESTRATE_USAGE}"));
                };
                plan_file = Some(value.clone());
                index += 2;
            }
            "--plan-only" | "--preview" => {
                plan_only = true;
                index += 1;
            }
            "--help" | "-h" => return Err(ORCHESTRATE_USAGE.to_string()),
            other => {
                goal_parts.push(other.to_string());
                index += 1;
            }
        }
    }
    let goal = goal_parts.join(" ");
    if goal.trim().is_empty() && plan_file.is_none() {
        return Err(ORCHESTRATE_USAGE.to_string());
    }
    Ok(OrchestrateArgs {
        goal,
        max_tasks,
        plan_file,
        plan_only,
    })
}

fn record_orchestration_message(state: &mut TuiState, text: String) {
    let result = state.store.lock().unwrap().add_message(
        &state.thread.id,
        0,
        "orchestration",
        None,
        None,
        &text,
    );
    if let Ok(id) = result {
        state.history.push(MessageRow {
            id,
            thread_id: state.thread.id.clone(),
            turn: 0,
            role: "orchestration".to_string(),
            provider: None,
            model: None,
            text,
            created_at: String::new(),
        });
    }
}

fn record_worker_followup(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    thread_id: &str,
    ok: bool,
    error: Option<&str>,
    phase: TurnPhase,
) {
    let outcome = {
        let guard = store.lock().unwrap();
        orchestrate::record_worker_followup(&guard, thread_id, ok, error, phase)
    };
    if thread_id != state.thread.id {
        return;
    }
    match outcome {
        Ok(Some(outcome)) => {
            let run = &outcome.run_id[..8.min(outcome.run_id.len())];
            if let Some(error) = &outcome.error {
                // A failure after a prior commit does not roll the task back.
                if outcome.commit.is_none() {
                    let label = if outcome.status == "cancelled" {
                        "cancellation"
                    } else {
                        "failure"
                    };
                    state.notice = Some(format!("worker {label} recorded in run {run} · {error}"));
                }
            } else if outcome.question.is_some() {
                state.notice = Some(match &outcome.commit {
                    Some(commit) => format!(
                        "worker question recorded in run {run} · commit {}",
                        &commit[..8.min(commit.len())]
                    ),
                    None => format!("worker question recorded in run {run} · no file changes"),
                });
            } else {
                state.notice = Some(match &outcome.commit {
                    Some(commit) => format!(
                        "worker follow-up recorded in run {run} · commit {}",
                        &commit[..8.min(commit.len())]
                    ),
                    None => format!("worker follow-up recorded in run {run} · no file changes"),
                });
            }
        }
        Ok(None) => {}
        Err(error) => state.notice = Some(error.to_string()),
    }
}

fn resolve_plan_path(cwd: &str, plan_file: Option<String>) -> Option<std::path::PathBuf> {
    plan_file.map(|path| {
        let path = std::path::PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            std::path::Path::new(cwd).join(path)
        }
    })
}

fn effective_orchestration_goal(goal: &str, plan_path: Option<&std::path::Path>) -> String {
    if goal.trim().is_empty() {
        plan_path
            .map(|path| format!("plan file {}", path.display()))
            .unwrap_or_else(|| "orchestration".to_string())
    } else {
        goal.to_string()
    }
}

fn request_orchestration_preview(
    state: &mut TuiState,
    goal: String,
    max_tasks: usize,
    plan_file: Option<String>,
) {
    if state.orchestration_pending {
        state.notice = Some("an orchestration is already running".into());
        return;
    }
    if goal.trim().is_empty() && plan_file.is_none() {
        state.notice = Some(ORCHESTRATE_USAGE.into());
        return;
    }
    start_orchestration_preview(state, goal, max_tasks, plan_file);
}

fn start_orchestration_preview(
    state: &mut TuiState,
    goal: String,
    max_tasks: usize,
    plan_file: Option<String>,
) {
    let cwd = state.thread.cwd.clone();
    let home = state.home.clone();
    let provider = state.thread.current_provider;
    let model = state
        .thread
        .current_model
        .clone()
        .unwrap_or_else(|| {
            resolve_config(&home, std::path::Path::new(&cwd))
                .for_provider(provider)
                .model
                .clone()
        });
    let permission_mode = state.permission_mode;
    let plan_path = resolve_plan_path(&cwd, plan_file);
    let effective_goal = effective_orchestration_goal(&goal, plan_path.as_deref());
    let dirty_source = crate::worktree::repo_root(std::path::Path::new(&cwd))
        .ok()
        .and_then(|root| crate::worktree::is_dirty(&root).ok())
        .unwrap_or(false);
    state.orchestration_pending = true;
    state.orchestration_cancelling = false;
    let cancel = Arc::new(AtomicBool::new(false));
    state.orchestration_cancel = cancel.clone();
    state.notice = Some("planning preview...".into());
    let sender = state.orchestration_sender.clone();
    std::thread::spawn(move || {
        let result = orchestrate::plan(
            &effective_goal,
            std::path::Path::new(&cwd),
            provider,
            &model,
            max_tasks,
            plan_path.as_deref(),
            permission_mode,
            Some(cancel.as_ref()),
        );
        let event = match result {
            Ok(plan) => OrchestrationEvent::Preview(Box::new(OrchestrationPreview {
                goal: effective_goal,
                plan,
                coordinator: provider,
                coordinator_model: model,
                dirty_source,
            })),
            Err(error) => OrchestrationEvent::PreviewFailed(error.to_string()),
        };
        let _ = sender.send(event);
    });
}

fn write_preview_plan(
    home: &std::path::Path,
    plan: &orchestrate::Plan,
) -> Result<std::path::PathBuf> {
    let dir = home.join("plans");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("preview-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&path, serde_json::to_string_pretty(plan)?)?;
    Ok(path)
}

fn start_previewed_orchestration(state: &mut TuiState, preview: OrchestrationPreview) {
    let path = match write_preview_plan(&state.home, &preview.plan) {
        Ok(path) => path,
        Err(error) => {
            state.notice = Some(format!("cannot write preview plan: {error}"));
            return;
        }
    };
    state.overlay = Overlay::None;
    start_orchestration(
        state,
        preview.goal,
        preview.plan.tasks.len().max(1),
        Some(path.to_string_lossy().to_string()),
    );
}

fn request_orchestration(
    state: &mut TuiState,
    goal: String,
    max_tasks: usize,
    plan_file: Option<String>,
) {
    if state.orchestration_pending {
        state.notice = Some("an orchestration is already running".into());
        return;
    }
    if goal.trim().is_empty() && plan_file.is_none() {
        state.notice = Some(ORCHESTRATE_USAGE.into());
        return;
    }
    let dirty = crate::worktree::repo_root(std::path::Path::new(&state.thread.cwd))
        .ok()
        .and_then(|root| crate::worktree::is_dirty(&root).ok())
        .unwrap_or(false);
    if dirty {
        state.overlay = Overlay::DirtyConfirm {
            goal,
            max_tasks,
            plan_file,
        };
        state.notice = None;
        return;
    }
    start_orchestration(state, goal, max_tasks, plan_file);
}

fn start_orchestration(
    state: &mut TuiState,
    goal: String,
    max_tasks: usize,
    plan_file: Option<String>,
) {
    if state.orchestration_pending {
        state.notice = Some("an orchestration is already running".into());
        return;
    }
    if goal.trim().is_empty() && plan_file.is_none() {
        state.notice = Some(ORCHESTRATE_USAGE.into());
        return;
    }
    let cwd = state.thread.cwd.clone();
    let home = state.home.clone();
    let provider = state.thread.current_provider;
    let model = state
        .thread
        .current_model
        .clone()
        .unwrap_or_else(|| resolve_config(&home, std::path::Path::new(&cwd)).for_provider(provider).model.clone());
    let permission_mode = state.permission_mode;
    let plan_path = resolve_plan_path(&cwd, plan_file);
    let effective_goal = effective_orchestration_goal(&goal, plan_path.as_deref());
    let result = {
        let guard = state.store.lock().unwrap();
        ensure_persisted(&guard, &mut state.thread)
    };
    if let Err(error) = result {
        state.notice = Some(format!("cannot start orchestration: {error}"));
        return;
    }
    if state.thread.title == "New thread" {
        let title = crate::handoff::derive_title(&effective_goal);
        state.thread.title = title.clone();
        let _ = state
            .store
            .lock()
            .unwrap()
            .set_thread_title(&state.thread.id, &title);
    }
    state.prompt_history.push(effective_goal.clone());
    state.prompt_history_index = None;
    {
        let guard = state.store.lock().unwrap();
        let _ = guard.record_prompt(&state.thread.cwd, &effective_goal);
    }
    let goal_message = format!("goal · {effective_goal}");
    record_orchestration_message(state, goal_message);
    state.orchestration_pending = true;
    state.orchestration_cancelling = false;
    let cancel = Arc::new(AtomicBool::new(false));
    state.orchestration_cancel = cancel.clone();
    state.orchestration_workers.clear();
    state.orchestration_report = None;
    state.orchestration_scroll = 0;
    state.notice = Some(if plan_path.is_some() {
        "orchestrating: starting worker plan...".into()
    } else {
        "orchestrating: coordinator planning...".into()
    });
    let sender = state.orchestration_sender.clone();
    std::thread::spawn(move || {
        let cancel_for_run = cancel.clone();
        let progress_sender = std::sync::Mutex::new(sender.clone());
        let progress = move |event: OrchestrationProgress| {
            if let Ok(sender) = progress_sender.lock() {
                let _ = sender.send(OrchestrationEvent::Progress(event));
            }
        };
        let result = BatonStore::open(&home.join("baton.sqlite")).and_then(|store| {
            orchestrate::run_with_progress(
                &effective_goal,
                std::path::Path::new(&cwd),
                &home,
                provider,
                &model,
                max_tasks,
                plan_path.as_deref(),
                &store,
                permission_mode,
                Some(&progress as &orchestrate::ProgressFn<'_>),
                Some(cancel_for_run.as_ref()),
            )
        });
        let event = match result {
            Ok(report) => OrchestrationEvent::Done(Box::new(report)),
            Err(error) => OrchestrationEvent::Failed(error.to_string()),
        };
        let _ = sender.send(event);
    });
}

fn apply_turn_event(state: &mut TuiState, event: TurnEvent) {
    match event {
        TurnEvent::TextDelta { text } => {
            state.streaming_text.push_str(&text);
        }
        TurnEvent::AssistantText { .. } => {}
        TurnEvent::ToolStarted { id, tool, detail } => {
            let label = match detail {
                Some(detail) => format!("{tool} {detail}"),
                None => tool,
            };
            if let Some(id) = id.as_deref() {
                if let Some(line) = state
                    .tool_lines
                    .iter_mut()
                    .rev()
                    .find(|line| line.status == ToolStatus::Running && line.id.as_deref() == Some(id))
                {
                    line.label = label;
                    return;
                }
            }
            state.tool_lines.push(ToolLine { id, label, status: ToolStatus::Running });
        }
        TurnEvent::ToolCompleted { id, tool, detail, exit_code } => {
            let label = match detail {
                Some(detail) => format!("{tool} {detail}"),
                None => tool,
            };
            let status = if exit_code.unwrap_or(0) != 0 { ToolStatus::Failed } else { ToolStatus::Done };
            let matched = match id.as_deref() {
                Some(id) => state
                    .tool_lines
                    .iter_mut()
                    .rev()
                    .find(|line| line.status == ToolStatus::Running && line.id.as_deref() == Some(id)),
                None => state
                    .tool_lines
                    .iter_mut()
                    .rev()
                    .find(|line| line.status == ToolStatus::Running),
            };
            if let Some(line) = matched {
                line.status = status;
            } else {
                state.tool_lines.push(ToolLine { id, label, status });
            }
        }
        TurnEvent::ProviderTurnId { .. }
        | TurnEvent::ProviderSession { .. }
        | TurnEvent::Usage { .. } => {}
        // Approval requests need their thread id, so the run loop routes them
        // through apply_turn_event_for_thread before calling this function.
        TurnEvent::ApprovalRequest { .. } => {}
        TurnEvent::TurnCompleted { .. }
        | TurnEvent::TurnInterrupted { .. }
        | TurnEvent::TurnFailed { .. } => {}
    }
}

fn apply_turn_event_for_thread(state: &mut TuiState, thread_id: String, event: TurnEvent) {
    if let Some(child) = state
        .multi
        .as_mut()
        .and_then(|multi| multi.children.iter_mut().find(|child| child.thread_id == thread_id))
    {
        match &event {
            TurnEvent::TextDelta { text } => child.reply.push_str(text),
            TurnEvent::TurnFailed { error } => {
                child.status = MultiStatus::Failed;
                child.error = Some(error.clone());
            }
            TurnEvent::TurnInterrupted { reason } => {
                child.status = MultiStatus::Failed;
                child.error = Some(reason.clone().unwrap_or_else(|| "interrupted".into()));
            }
            _ => {}
        }
    }
    match event {
        TurnEvent::ApprovalRequest {
            summary, respond, ..
        } => state.approvals.push(ApprovalState {
            thread_id,
            summary,
            respond,
        }),
        event if thread_id == state.thread.id => apply_turn_event(state, event),
        _ => {}
    }
}

fn load_thread_state(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    thread: ThreadRow,
) {
    state.permission_mode = thread.permission_mode;
    state.goal = thread.goal.clone();
    state.goal_paused = thread.goal_paused;
    state.thread = thread.clone();
    let guard = store.lock().unwrap();
    state.history = guard.list_messages(&thread.id).unwrap_or_default();
    state.queued_messages = guard.list_queued_messages(&thread.id).unwrap_or_default();
    state.branch_label = git_branch_label(std::path::Path::new(&thread.cwd));
    state.diff_output = None;
    state.scroll_offset = 0;
    state.notice = Some(format!("opened {}", thread.title));
}

fn input_index_at(state: &TuiState, column: u16, row: u16) -> Option<usize> {
    let layout = state.input_layout.borrow();
    let line = layout.iter().find(|line| line.screen_y == row)?;
    let column = column.saturating_sub(line.screen_x_start) as usize;
    Some((line.char_start + line.scroll + column).min(line.char_start + line.line_len))
}

fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

fn model_filter_indices(models: &[String], filter: &str) -> Vec<usize> {
    let needle = filter.as_bytes();
    if needle.is_empty() {
        return (0..models.len()).collect();
    }
    models
        .iter()
        .enumerate()
        .filter(|(_, model)| {
            model
                .as_bytes()
                .windows(needle.len())
                .any(|window| window.eq_ignore_ascii_case(needle))
        })
        .map(|(index, _)| index)
        .collect()
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
            let _ = engine_command_sender.send(EngineCommand::Interrupt { thread_id: state.thread.id.clone() });
            state.notice = Some("interrupting...".into());
            return;
        }
        if state.orchestration_pending {
            state.orchestration_cancel.store(true, Ordering::Relaxed);
            state.orchestration_cancelling = true;
            state.notice = Some("cancelling orchestration...".into());
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

    if !state.approvals.is_empty() {
        let decision = match key.code {
            KeyCode::Char('y') => Some(ApprovalDecision::Accept),
            KeyCode::Char('a') => Some(ApprovalDecision::AcceptAlways),
            KeyCode::Char('n') => Some(ApprovalDecision::Deny),
            _ => None,
        };
        if let Some(decision) = decision {
            let approval = state.approvals.pop().expect("approval checked above");
            let label = match decision {
                ApprovalDecision::Accept => "accepted once",
                ApprovalDecision::AcceptAlways => "accepted for the session",
                ApprovalDecision::Deny => "denied",
            };
            if approval.respond.send(decision).is_err() {
                state.notice = Some("approval request expired".into());
            } else if state.approvals.is_empty() {
                state.notice = Some(format!("approval {label}"));
            } else {
                state.notice = Some(format!(
                    "approval {label} · {} request(s) still waiting",
                    state.approvals.len()
                ));
            }
        }
        return;
    }

    if let Some(mut multi) = state.multi.take() {
        let mut open_thread = None;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return,
            KeyCode::Up | KeyCode::Char('k') => {
                multi.selected = multi.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !multi.children.is_empty() {
                    multi.selected = (multi.selected + 1).min(multi.children.len() - 1);
                }
            }
            KeyCode::PageUp => {
                multi.selected = multi.selected.saturating_sub(5);
            }
            KeyCode::PageDown => {
                if !multi.children.is_empty() {
                    multi.selected = (multi.selected + 5).min(multi.children.len() - 1);
                }
            }
            KeyCode::Enter => {
                open_thread = multi
                    .children
                    .get(multi.selected)
                    .map(|child| child.thread_id.clone());
            }
            _ => {}
        }
        if let Some(thread_id) = open_thread {
            let thread = store.lock().unwrap().get_thread(&thread_id).unwrap_or(None);
            if let Some(thread) = thread {
                load_thread_state(state, store, thread);
                state.multi = None;
                return;
            }
            state.notice = Some(format!("parallel thread {thread_id} no longer exists"));
        }
        state.multi = Some(multi);
        return;
    }

    if state.review.is_some() {
        handle_review_key(state, store, engine_command_sender, key);
        return;
    }

    if state.orchestration_report.is_some() {
        handle_orchestration_key(state, store, key);
        return;
    }

    match std::mem::replace(&mut state.overlay, Overlay::None) {
        Overlay::RewindPicker { mut selected, native } => {
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
                        state.overlay = Overlay::RewindPicker { selected, native };
                    } else {
                        state.overlay = Overlay::RewindPicker { selected, native };
                    }
                }
                KeyCode::Down => {
                    if !user_messages.is_empty() {
                        selected = (selected + 1) % user_messages.len();
                        state.overlay = Overlay::RewindPicker { selected, native };
                    } else {
                        state.overlay = Overlay::RewindPicker { selected, native };
                    }
                }
                KeyCode::Enter => {
                    if let Some((turn, text)) = user_messages.get(selected).cloned() {
                        if native {
                            start_native_rewind(state, store, turn, text);
                        } else {
                            apply_rewind(state, store, turn, text);
                        }
                    }
                }
                _ => {
                    state.overlay = Overlay::RewindPicker { selected, native };
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
                    state.model_request_id = state.model_request_id.wrapping_add(1);
                    let _ = engine_command_sender.send(EngineCommand::ListModels {
                        thread_id: state.thread.id.clone(),
                        provider,
                        request_id: state.model_request_id,
                    });
                    state.overlay = Overlay::ModelPicker {
                        provider,
                        models,
                        selected: 0,
                        filter: String::new(),
                    };
                }
            }
            _ => {
                state.overlay = Overlay::SwitchPicker { selected };
            }
        },
        Overlay::ModelPicker {
            provider,
            models,
            mut selected,
            mut filter,
        } => {
            let filtered = model_filter_indices(&models, &filter);
            match key.code {
                KeyCode::Esc => {
                    if filter.is_empty() {
                        return;
                    }
                    filter.clear();
                    selected = 0;
                }
                KeyCode::Left => {
                    state.overlay = Overlay::SwitchPicker { selected: 0 };
                    return;
                }
                KeyCode::Backspace => {
                    filter.pop();
                    selected = 0;
                }
                KeyCode::Char(character)
                    if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    filter.push(character);
                    selected = 0;
                }
                KeyCode::Up => {
                    if !filtered.is_empty() {
                        selected = (selected + filtered.len() - 1) % filtered.len();
                    }
                }
                KeyCode::Down => {
                    if !filtered.is_empty() {
                        selected = (selected + 1) % filtered.len();
                    }
                }
                KeyCode::PageUp => {
                    selected = selected.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    if !filtered.is_empty() {
                        selected = (selected + 10).min(filtered.len() - 1);
                    }
                }
                KeyCode::Enter => {
                    if let Some(index) = filtered.get(selected) {
                        if let Some(model) = models.get(*index).cloned() {
                            perform_switch(state, store, provider, Some(model));
                            return;
                        }
                    }
                }
                _ => {}
            }
            state.overlay = Overlay::ModelPicker {
                provider,
                models,
                selected,
                filter,
            };
        }
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
                    state.apply_permission_mode(store, *mode);
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
                    state.banner_cache.borrow_mut().take();
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
                        state.permission_mode = thread.permission_mode;
                        state.goal = thread.goal.clone();
                        state.goal_paused = thread.goal_paused;
                        state.thread = thread.clone();
                        let guard = store.lock().unwrap();
                        state.history = guard.list_messages(&thread.id).unwrap_or_default();
                        state.queued_messages = guard.list_queued_messages(&thread.id).unwrap_or_default();
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
        Overlay::OrchestrationPreview { preview } => match key.code {
            KeyCode::Esc => {
                state.set_input(preview.goal.clone());
                state.notice = Some("plan cancelled and returned to the composer".into());
            }
            KeyCode::Enter | KeyCode::Char('r') | KeyCode::Char('R') => {
                start_previewed_orchestration(state, *preview);
            }
            _ => {
                state.overlay = Overlay::OrchestrationPreview { preview };
            }
        },
        Overlay::DirtyConfirm {
            goal,
            max_tasks,
            plan_file,
        } => match key.code {
            KeyCode::Enter => start_orchestration(state, goal, max_tasks, plan_file),
            KeyCode::Char('s') | KeyCode::Char('S') => {
                if goal.trim().is_empty() {
                    state.overlay = Overlay::DirtyConfirm {
                        goal,
                        max_tasks,
                        plan_file,
                    };
                    state.notice =
                        Some("a single harness needs a prompt; esc returns it to the composer".into());
                } else {
                    submit(state, store, engine_command_sender, goal);
                }
            }
            KeyCode::Esc => {
                state.set_input(goal);
                state.notice =
                    Some("orchestration cancelled; commit or stash, or run a single harness".into());
            }
            _ => {
                state.overlay = Overlay::DirtyConfirm {
                    goal,
                    max_tasks,
                    plan_file,
                };
            }
        },
        Overlay::None => {
            handle_composer_key(state, store, engine_command_sender, key);
        }
    }
}

fn start_native_rewind(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    turn: i64,
    text: String,
) {
    if state.native_rewind_running {
        state.notice = Some("a native rewind is already running".into());
        return;
    }
    if state.running {
        state.notice = Some("interrupt the current turn before a native rewind".into());
        return;
    }
    if !matches!(
        state.thread.current_provider,
        ProviderId::Codex | ProviderId::OpenCode
    ) {
        state.notice = Some(
            "native rewind is Codex and OpenCode-only; use /rewind without --native".into(),
        );
        return;
    }
    let persisted = {
        let guard = store.lock().unwrap();
        ensure_persisted(&guard, &mut state.thread)
    };
    if let Err(error) = persisted {
        state.notice = Some(format!("cannot prepare native rewind: {error}"));
        return;
    }
    state.native_rewind_running = true;
    state.notice = Some(format!(
        "rewinding {} session natively...",
        state.thread.current_provider
    ));
    let home = state.home.clone();
    let source = state.thread.clone();
    let sender = state.native_rewind_sender.clone();
    std::thread::spawn(move || {
        let result = (|| -> Result<bool> {
            let store = BatonStore::open(&home.join("baton.sqlite"))?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let recorder_store =
                Arc::new(Mutex::new(BatonStore::open(&home.join("baton.sqlite"))?));
            let recorder =
                crate::process::ProcessRecorder::new(recorder_store, source.cwd.clone());
            let outcome = runtime.block_on(crate::provider_runtime::rewind_to_turn(
                &store,
                &home,
                &source,
                turn,
                recorder,
            ))?;
            Ok(outcome.provider_retry_skipped)
        })();
        let event = match result {
            Ok(provider_retry_skipped) => NativeRewindEvent::Done {
                turn,
                text,
                provider_retry_skipped,
            },
            Err(error) => NativeRewindEvent::Failed(error.to_string()),
        };
        let _ = sender.send(event);
    });
}

fn apply_native_rewind_event(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    event: NativeRewindEvent,
) {
    match event {
        NativeRewindEvent::Done {
            turn,
            text,
            provider_retry_skipped,
        } => {
            state.native_rewind_running = false;
            let checkpoint = {
                let guard = store.lock().unwrap();
                state.history = guard.list_messages(&state.thread.id).unwrap_or_default();
                state.queued_messages = guard
                    .list_queued_messages(&state.thread.id)
                    .unwrap_or_default();
                guard
                    .get_checkpoint(&state.thread.id, turn - 1)
                    .unwrap_or(None)
            };
            state.scroll_offset = 0;
            state.set_input(text);
            state.overlay = match checkpoint {
                Some(checkpoint) => Overlay::CodeConfirm {
                    turn,
                    git_ref: checkpoint.git_ref,
                },
                None => Overlay::None,
            };
            state.notice = Some(format!(
                "rewound to before turn {turn} natively{}{}",
                if provider_retry_skipped {
                    " (completed pending store rewind)"
                } else {
                    ""
                },
                if state.overlay_is_code_confirm() {
                    "; choose whether to restore code"
                } else {
                    ""
                }
            ));
        }
        NativeRewindEvent::Failed(error) => {
            state.native_rewind_running = false;
            state.notice = Some(format!("native rewind failed: {error}"));
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
            state.queued_messages.clear();
            state.set_input(text);
            state.notice = Some(format!("rewound to before turn {turn}; message restored to composer"));
        }
        Err(error) => {
            state.notice = Some(format!("rewind failed: {error}"));
        }
    }
}

fn enqueue_message(state: &mut TuiState, store: &Arc<Mutex<BatonStore>>, text: String) {
    let _ = store
        .lock()
        .unwrap()
        .enqueue_message(&state.thread.id, &text);
    state.queued_messages.push(text);
}

fn effective_permission_mode(
    selected: PermissionMode,
    provider_default: PermissionMode,
) -> PermissionMode {
    if selected == PermissionMode::Default {
        provider_default
    } else {
        selected
    }
}

fn dispatch_message(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    engine_command_sender: &tokio::sync::mpsc::UnboundedSender<EngineCommand>,
    text: String,
    allow_commands: bool,
) {
    if allow_commands && text.starts_with('/') {
        handle_command(state, store, engine_command_sender, &text);
    } else if state.thread.session_mode == SessionMode::Orchestrate {
        if state.running {
            state.notice = Some(
                "a single turn is still running; finish it before starting an orchestration"
                    .into(),
            );
            return;
        }
        if state.orchestration_pending {
            state.notice = Some("an orchestration is already running".into());
            return;
        }
        if orchestrate::is_conversational_greeting(&text) {
            submit(state, store, engine_command_sender, text);
            return;
        }
        if crate::worktree::repo_root(std::path::Path::new(&state.thread.cwd)).is_err() {
            state.notice = Some(
                "orchestrate needs a git repository; this prompt runs on a single harness".into(),
            );
            submit(state, store, engine_command_sender, text);
            return;
        }
        let text = if let Some(goal) = &state.goal {
            if state.goal_paused {
                text
            } else {
                format!("[goal: {goal}]\n{text}")
            }
        } else {
            text
        };
        request_orchestration(state, text, 4, None);
    } else if state.running {
        let _ = engine_command_sender.send(EngineCommand::Steer {
            thread_id: state.thread.id.clone(),
            text,
        });
        state.notice = Some("steering the current turn...".into());
    } else {
        submit(state, store, engine_command_sender, text);
    }
}

fn escape_review_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn format_review_notes(turn: i64, notes: &[ReviewNote]) -> String {
    let mut text = format!(
        "Review the following line-anchored notes from turn {turn}:\n\n"
    );
    for note in notes {
        text.push_str(&format!(
            "File: {}\nLine: {}\nChange: {}\nUser comment: \"{}\"\n\n",
            note.path,
            note.line,
            if note.added { "added" } else { "removed" },
            escape_review_text(&note.body)
        ));
    }
    text.trim_end().to_string()
}

fn previous_file_row(rows: &[diff_render::DiffRowRef], selected: usize) -> Option<usize> {
    let current_path = rows.get(selected)?.path.as_str();
    let change = rows[..selected]
        .iter()
        .rposition(|row| row.path != current_path)?;
    let path = rows[change].path.as_str();
    let mut start = change;
    while start > 0 && rows[start - 1].path == path {
        start -= 1;
    }
    Some(start)
}

fn next_file_row(rows: &[diff_render::DiffRowRef], selected: usize) -> Option<usize> {
    let current_path = rows.get(selected)?.path.as_str();
    rows.iter()
        .enumerate()
        .skip(selected + 1)
        .find(|(_, row)| row.path != current_path)
        .map(|(index, _)| index)
}

fn handle_review_key(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    engine_command_sender: &tokio::sync::mpsc::UnboundedSender<EngineCommand>,
    key: KeyEvent,
) {
    let Some(mut review) = state.review.take() else {
        return;
    };

    if let Some(input) = review.note_input.as_mut() {
        match key.code {
            KeyCode::Esc => review.note_input = None,
            KeyCode::Enter => {
                let body = input.trim().to_string();
                if !body.is_empty() && !review.rows.is_empty() {
                    let row = &review.rows[review.selected.min(review.rows.len() - 1)];
                    let saved = store.lock().unwrap().add_review_note(
                        &state.thread.id,
                        review.turn,
                        &row.path,
                        row.line_number,
                        row.added,
                        &body,
                        &review.fingerprint,
                    );
                    match saved {
                        Ok(id) => review.notes.push(ReviewNote {
                            id,
                            path: row.path.clone(),
                            line: row.line_number,
                            added: row.added,
                            body,
                        }),
                        Err(error) => {
                            state.notice = Some(format!("cannot save review note: {error}"));
                        }
                    }
                }
                review.note_input = None;
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(character) => {
                if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                    input.push(character);
                }
            }
            _ => {}
        }
        state.review = Some(review);
        return;
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return,
        KeyCode::Up | KeyCode::Char('k') => {
            review.selected = review.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if !review.rows.is_empty() {
                review.selected = (review.selected + 1).min(review.rows.len() - 1);
            }
        }
        KeyCode::PageUp => {
            review.selected = review.selected.saturating_sub(10);
        }
        KeyCode::PageDown => {
            if !review.rows.is_empty() {
                review.selected = (review.selected + 10).min(review.rows.len() - 1);
            }
        }
        KeyCode::Char('n') => {
            if let Some(row) = review.rows.get(review.selected) {
                if row.added || row.removed {
                    review.note_input = Some(String::new());
                } else {
                    state.notice = Some("notes anchor added or removed lines only".into());
                }
            }
        }
        KeyCode::Char('[') => {
            if let Some(index) = previous_file_row(&review.rows, review.selected) {
                review.selected = index;
            }
        }
        KeyCode::Char(']') => {
            if let Some(index) = next_file_row(&review.rows, review.selected) {
                review.selected = index;
            }
        }
        KeyCode::Char('d') => {
            if let Some(row) = review.rows.get(review.selected) {
                let ids: Vec<i64> = review
                    .notes
                    .iter()
                    .filter(|note| {
                        note.path == row.path
                            && note.line == row.line_number
                            && note.added == row.added
                    })
                    .map(|note| note.id)
                    .collect();
                if !ids.is_empty() {
                    match store
                        .lock()
                        .unwrap()
                        .delete_review_notes(&state.thread.id, &ids)
                    {
                        Ok(()) => review.notes.retain(|note| !ids.contains(&note.id)),
                        Err(error) => {
                            state.notice = Some(format!("cannot delete review note: {error}"));
                        }
                    }
                }
            }
        }
        KeyCode::Char('s') => {
            if review.notes.is_empty() {
                state.notice = Some("no review notes to send yet; press n on a changed line".into());
                state.review = Some(review);
                return;
            }
            let count = review.notes.len();
            let ids: Vec<i64> = review.notes.iter().map(|note| note.id).collect();
            let text = format_review_notes(review.turn, &review.notes);
            let steering = state.running;
            dispatch_message(state, store, engine_command_sender, text, false);
            let marked = store
                .lock()
                .unwrap()
                .mark_review_notes_sent(&state.thread.id, &ids);
            state.notice = Some(match marked {
                Ok(()) if steering => {
                    format!("steering {count} review note(s) into the current turn")
                }
                Ok(()) => format!("sent {count} review note(s)"),
                Err(error) => format!(
                    "sent {count} review note(s), but could not mark them sent: {error}"
                ),
            });
            return;
        }
        _ => {}
    }

    state.review = Some(review);
}

fn handle_orchestration_key(
    state: &mut TuiState,
    store: &Arc<Mutex<BatonStore>>,
    key: KeyEvent,
) {
    let Some(worker_count) = state
        .orchestration_report
        .as_ref()
        .map(|report| report.tasks.len())
    else {
        return;
    };
    if state.orchestration_integrating || state.orchestration_shipping {
        return;
    }
    if state.orchestration_confirm_integrate {
        match key.code {
            KeyCode::Enter => start_integration(state),
            KeyCode::Esc => {
                state.orchestration_confirm_integrate = false;
                state.notice = Some("integration cancelled".into());
            }
            _ => {}
        }
        return;
    }
    if state.orchestration_confirm_ship {
        match key.code {
            KeyCode::Enter => start_shipping(state),
            KeyCode::Esc => {
                state.orchestration_confirm_ship = false;
                state.notice = Some("ship cancelled".into());
            }
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            state.orchestration_report = None;
            state.orchestration_scroll = 0;
            state.orchestration_selected = 0;
            state.orchestration_confirm_integrate = false;
            state.orchestration_confirm_ship = false;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.orchestration_selected = state.orchestration_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if worker_count > 0 {
                state.orchestration_selected =
                    (state.orchestration_selected + 1).min(worker_count - 1);
            }
        }
        KeyCode::PageUp => {
            state.orchestration_scroll = state.orchestration_scroll.saturating_sub(10);
        }
        KeyCode::PageDown => {
            state.orchestration_scroll = state.orchestration_scroll.saturating_add(10);
        }
        KeyCode::Char('i') => {
            let integrable = state
                .orchestration_report
                .as_ref()
                .map(|report| {
                    report
                        .tasks
                        .iter()
                        .filter(|task| task.status == "completed" && task.commit.is_some())
                        .count()
                })
                .unwrap_or(0);
            if integrable == 0 {
                state.notice = Some("no completed worker commits to integrate".into());
            } else {
                state.orchestration_confirm_integrate = true;
                state.notice = Some(format!(
                    "merge {integrable} worker branch(es) into the source checkout?"
                ));
            }
        }
        KeyCode::Char('a') => {
            let Some(run_id) = state
                .orchestration_report
                .as_ref()
                .map(|report| report.run_id.clone())
            else {
                return;
            };
            let result = {
                let guard = store.lock().unwrap();
                orchestrate::archive(&guard, &run_id)
            };
            match result {
                Ok(run_id) => {
                    let short = &run_id[..8.min(run_id.len())];
                    record_orchestration_message(
                        state,
                        format!("run {short} archived; worktrees remain"),
                    );
                    state.notice = Some(format!(
                        "archived run {short}; worktrees remain, use baton worktrees --clean --run {short}"
                    ));
                    state.orchestration_report = None;
                    state.orchestration_scroll = 0;
                    state.orchestration_selected = 0;
                    state.orchestration_confirm_integrate = false;
                    state.orchestration_confirm_ship = false;
                }
                Err(error) => state.notice = Some(format!("cannot archive run: {error}")),
            }
        }
        KeyCode::Char('p') => {
            let shippable = state
                .orchestration_report
                .as_ref()
                .map(|report| {
                    report
                        .tasks
                        .iter()
                        .filter(|task| {
                            task.status == "completed"
                                && task.commit.is_some()
                                && task.pr_url.is_none()
                        })
                        .count()
                })
                .unwrap_or(0);
            if shippable == 0 {
                state.notice = Some("no completed worker commits to ship".into());
            } else {
                state.orchestration_confirm_ship = true;
                state.notice = Some(format!(
                    "push {shippable} worker branch(es) and open pull requests?"
                ));
            }
        }
        KeyCode::Enter => {
            let thread_id = state
                .orchestration_report
                .as_ref()
                .and_then(|report| report.tasks.get(state.orchestration_selected))
                .and_then(|task| task.thread_id.clone());
            match thread_id {
                Some(thread_id) => {
                    let thread = store
                        .lock()
                        .unwrap()
                        .get_thread(&thread_id)
                        .unwrap_or(None);
                    match thread {
                        Some(thread) => {
                            state.orchestration_report = None;
                            state.orchestration_scroll = 0;
                            state.orchestration_selected = 0;
                            load_thread_state(state, store, thread);
                        }
                        None => {
                            state.notice =
                                Some(format!("worker thread {thread_id} no longer exists"));
                        }
                    }
                }
                None => {
                    state.notice = Some("worker has no thread (it never started)".into());
                }
            }
        }
        _ => {}
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
    let force_newline = (key.code == KeyCode::Enter
        && key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT))
        || (key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL));
    if force_newline {
        state.insert_input_char('\n');
        state.suggestion_index = 0;
        state.esc_primed = false;
        state.prompt_history_index = None;
        return;
    }

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
                    state.set_input(format!("{command} "));
                    state.suggestion_index = 0;
                }
                return;
            }
            KeyCode::Enter => {
                let typed = state.input.trim().to_string();
                if let Some((command, _)) = suggestions.iter().find(|(command, _)| *command == typed) {
                    let command = command.to_string();
                    state.set_input(String::new());
                    handle_command(state, store, engine_command_sender, &command);
                } else if let Some((command, _)) = suggestions.get(clamped) {
                    state.set_input(format!("{command} "));
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
            state.set_input(String::new());
            state.suggestion_index = 0;
            state.esc_primed = false;
            dispatch_message(state, store, engine_command_sender, text, true);
        }
        KeyCode::Backspace => {
            state.backspace_input();
            state.suggestion_index = 0;
            state.esc_primed = false;
            state.selection = None;
            state.selection_anchor = None;
            state.prompt_history_index = None;
        }
        KeyCode::Delete => {
            state.delete_input();
            state.suggestion_index = 0;
            state.esc_primed = false;
            state.prompt_history_index = None;
        }
        KeyCode::Left => {
            state.input_cursor = state.input_cursor.saturating_sub(1);
            state.selection = None;
            state.selection_anchor = None;
        }
        KeyCode::Right => {
            let length = state.input.chars().count();
            state.input_cursor = (state.input_cursor + 1).min(length);
            state.selection = None;
            state.selection_anchor = None;
        }
        KeyCode::Home => {
            let (start, _) = state.cursor_line_bounds();
            state.input_cursor = start;
            state.selection = None;
            state.selection_anchor = None;
        }
        KeyCode::End => {
            let (_, end) = state.cursor_line_bounds();
            state.input_cursor = end;
            state.selection = None;
            state.selection_anchor = None;
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
                state.set_input(state.prompt_history[next].clone());
                state.prompt_history_index = Some(next);
                state.esc_primed = false;
            }
        }
        KeyCode::Down if state.prompt_history_index.is_some() => {
            let index = state.prompt_history_index.unwrap() + 1;
            if index >= state.prompt_history.len() {
                state.set_input(String::new());
                state.prompt_history_index = None;
            } else {
                state.set_input(state.prompt_history[index].clone());
                state.prompt_history_index = Some(index);
            }
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
                let _ = engine_command_sender.send(EngineCommand::Interrupt { thread_id: state.thread.id.clone() });
                state.notice = Some("interrupting...".into());
            } else if state.input.is_empty() {
                if state.esc_primed {
                    state.esc_primed = false;
                    state.overlay = Overlay::RewindPicker { selected: 0, native: false };
                } else {
                    state.esc_primed = true;
                }
            } else {
                state.set_input(String::new());
                state.prompt_history_index = None;
                state.notice = Some("composer cleared".into());
            }
        }
        KeyCode::Char(character) => {
            if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                state.insert_input_char(character);
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

const MULTI_USAGE: &str =
    "usage: /multi [--worktrees] <provider> [<provider> ...] <prompt>";

#[derive(Debug, PartialEq, Eq)]
struct MultiArgs {
    worktrees: bool,
    providers: Vec<ProviderId>,
    prompt: String,
}

fn parse_multi_args(args: &[String]) -> std::result::Result<MultiArgs, String> {
    let mut start = 0usize;
    let worktrees = matches!(args.first().map(String::as_str), Some("--worktrees") | Some("--wt"));
    if worktrees {
        start = 1;
    }
    let mut providers: Vec<ProviderId> = Vec::new();
    let mut prompt_parts: Vec<String> = Vec::new();
    for arg in &args[start..] {
        match ProviderId::parse(arg) {
            Some(provider) if prompt_parts.is_empty() => {
                if !providers.contains(&provider) {
                    providers.push(provider);
                }
            }
            _ => prompt_parts.push(arg.clone()),
        }
    }
    let prompt = prompt_parts.join(" ");
    if providers.is_empty() || prompt.trim().is_empty() {
        return Err(MULTI_USAGE.to_string());
    }
    Ok(MultiArgs {
        worktrees,
        providers,
        prompt,
    })
}

fn multi_worktree_plan(providers: &[ProviderId], prompt: &str) -> orchestrate::Plan {
    orchestrate::Plan {
        summary: Some("multi fan-out across isolated worktrees".to_string()),
        tasks: providers
            .iter()
            .map(|provider| orchestrate::PlanTask {
                title: format!("multi {provider}"),
                provider: provider.as_str().to_string(),
                model: None,
                effort: None,
                prompt: prompt.to_string(),
            })
            .collect(),
    }
}

fn write_multi_worktree_plan(
    providers: &[ProviderId],
    prompt: &str,
) -> Result<String> {
    let path = std::env::temp_dir().join(format!(
        "baton-multi-{}.json",
        uuid::Uuid::new_v4()
    ));
    let plan = multi_worktree_plan(providers, prompt);
    std::fs::write(&path, serde_json::to_string_pretty(&plan)?)?;
    Ok(path.to_string_lossy().to_string())
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
        if state.goal_paused {
            text
        } else {
            format!("[goal: {goal}]\n{text}")
        }
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
    state.persist_thread_state(store);
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
            let all = args.first().map(String::as_str) == Some("all");
            state.notice = Some(if all {
                let mut help = SLASH_COMMANDS
                    .iter()
                    .map(|(name, description)| format!("{name:<14} {description}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                help.push_str(
                    "\n\nshift+enter or alt+enter: newline · left/right/home/end: edit · up/down: history or scroll · drag transcript to copy",
                );
                help
            } else {
                HELP_SUMMARY.to_string()
            });
        }
        "/clear" => {
            state.diff_output = None;
            state.orchestration_report = None;
            state.orchestration_scroll = 0;
            state.orchestration_selected = 0;
            state.notice = None;
        }
        "/goal" => {
            if let Some(action) = args.first().map(String::as_str) {
                match action {
                    "clear" => {
                        state.set_goal_state(store, None, false);
                        state.notice = Some("goal cleared".into());
                    }
                    "pause" => {
                        if state.goal.is_none() {
                            state.notice = Some("no goal to pause; set one with /goal <criteria>".into());
                        } else {
                            let goal = state.goal.clone();
                            state.set_goal_state(store, goal, true);
                            state.notice = Some("goal paused (not injected into new turns)".into());
                        }
                    }
                    "resume" => {
                        if state.goal.is_none() {
                            state.notice = Some("no goal to resume; set one with /goal <criteria>".into());
                        } else {
                            let goal = state.goal.clone();
                            state.set_goal_state(store, goal, false);
                            state.notice = Some("goal resumed".into());
                        }
                    }
                    _ => {
                        let goal = args.join(" ");
                        state.set_goal_state(store, Some(goal.clone()), false);
                        state.notice = Some(format!("goal set: {goal}"));
                    }
                }
            } else if let Some(goal) = &state.goal {
                let suffix = if state.goal_paused { " (paused)" } else { "" };
                state.notice = Some(format!("current goal{suffix}: {goal}"));
            } else {
                state.notice = Some("usage: /goal <done criteria> · /goal pause|resume|clear".into());
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
        "/copy" => {
            let index = args
                .first()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(1)
                .max(1);
            let reply = state
                .history
                .iter()
                .rev()
                .filter(|message| message.role == "assistant")
                .nth(index - 1);
            match reply {
                Some(reply) => match crate::clipboard::copy(&reply.text) {
                    Ok(()) => {
                        state.notice = Some(format!(
                            "copied assistant reply {} ({} chars)",
                            index,
                            reply.text.chars().count()
                        ));
                    }
                    Err(error) => state.notice = Some(format!("cannot copy reply: {error}")),
                },
                None => {
                    state.notice = Some(format!("no assistant reply at position {index}"));
                }
            }
        }
        "/multi" => {
            let parsed = match parse_multi_args(&args) {
                Ok(parsed) => parsed,
                Err(message) => {
                    state.notice = Some(message);
                    return;
                }
            };
            if parsed.worktrees {
                if let Err(error) = crate::worktree::repo_root(std::path::Path::new(&state.thread.cwd)) {
                    state.notice = Some(format!(
                        "/multi --worktrees needs a git repository: {error}"
                    ));
                    return;
                }
                let plan_file = match write_multi_worktree_plan(&parsed.providers, &parsed.prompt) {
                    Ok(path) => path,
                    Err(error) => {
                        state.notice = Some(format!("cannot write multi plan: {error}"));
                        return;
                    }
                };
                request_orchestration(
                    state,
                    parsed.prompt,
                    parsed.providers.len(),
                    Some(plan_file),
                );
                return;
            }
            let valid_providers = parsed.providers;
            let prompt = parsed.prompt;
            state.notice = Some(format!(
                "sending to {} providers in parallel: {}",
                valid_providers.len(),
                valid_providers.iter().map(|provider| provider.to_string()).collect::<Vec<_>>().join(", ")
            ));
            let mut threads = Vec::new();
            for provider in valid_providers.clone() {
                let thread = {
                    let guard = store.lock().unwrap();
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
                    let _ = guard.set_thread_permission_mode(&thread.id, state.permission_mode);
                    let _ = guard.set_thread_goal(
                        &thread.id,
                        state.goal.as_deref(),
                        state.goal_paused,
                    );
                    thread
                };
                threads.push(thread);
            }
            let children = threads
                .iter()
                .map(|thread| MultiChild {
                    thread_id: thread.id.clone(),
                    provider: thread.current_provider,
                    status: MultiStatus::Running,
                    reply: String::new(),
                    error: None,
                })
                .collect();
            for thread in &threads {
                let _ = engine_command_sender.send(EngineCommand::Submit {
                    thread_id: thread.id.clone(),
                    text: prompt.clone(),
                    permission_mode: state.permission_mode,
                });
            }
            state.multi = Some(MultiView {
                prompt: prompt.clone(),
                selected: 0,
                children,
            });
            state.notice = Some(format!(
                "sent to {} providers in parallel: {} · esc closes the merged view",
                valid_providers.len(),
                valid_providers.iter().map(|provider| provider.to_string()).collect::<Vec<_>>().join(", ")
            ));
        }
        "/orchestrate" => {
            match args.first().map(String::as_str) {
                Some("on") => {
                    state.set_session_mode(store, SessionMode::Orchestrate);
                    state.notice =
                        Some("orchestrate mode: prompts spawn worker harnesses".into());
                    return;
                }
                Some("off") => {
                    state.set_session_mode(store, SessionMode::Single);
                    state.notice =
                        Some("single harness mode: prompts run on the current provider".into());
                    return;
                }
                _ => {}
            }
            match parse_orchestrate_args(&args) {
                Ok(parsed) => {
                    if parsed.plan_only {
                        request_orchestration_preview(
                            state,
                            parsed.goal,
                            parsed.max_tasks,
                            parsed.plan_file,
                        );
                    } else if parsed.plan_file.is_none()
                        && orchestrate::is_conversational_greeting(&parsed.goal)
                    {
                        submit(state, store, engine_command_sender, parsed.goal);
                    } else {
                        request_orchestration(
                            state,
                            parsed.goal,
                            parsed.max_tasks,
                            parsed.plan_file,
                        );
                    }
                }
                Err(message) => state.notice = Some(message),
            }
        }
        "/runs" => {
            let include_archived = args.first().map(|arg| arg == "--all").unwrap_or(false);
            let run_id = args.first().filter(|arg| *arg != "--all").cloned();
            if let Some(run_id) = run_id {
                let result = {
                    let guard = store.lock().unwrap();
                    orchestrate::report_from_rows(
                        &guard,
                        std::path::Path::new(&state.thread.cwd),
                        &run_id,
                    )
                };
                match result {
                    Ok(report) => {
                        state.orchestration_report = Some(report);
                        state.orchestration_scroll = 0;
                        state.orchestration_selected = 0;
                        state.orchestration_integrate_report = None;
                        state.orchestration_confirm_ship = false;
                        state.orchestration_ship_report = None;
                        state.notice = Some(format!(
                            "opened run {}",
                            &run_id[..8.min(run_id.len())]
                        ));
                    }
                    Err(error) => state.notice = Some(format!("cannot open run: {error}")),
                }
            } else {
                let runs = store
                    .lock()
                    .unwrap()
                    .list_orchestration_runs(10)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|run| include_archived || run.archived_at.is_none())
                    .collect::<Vec<_>>();
                if runs.is_empty() {
                    state.notice = Some(if include_archived {
                        "no orchestration runs yet".to_string()
                    } else {
                        "no active orchestration runs; /runs --all includes archived runs".to_string()
                    });
                } else {
                    let lines = runs
                        .iter()
                        .map(|run| {
                            format!(
                                "{}  {:<9}  {}{}",
                                &run.id[..8.min(run.id.len())],
                                run.status,
                                run.summary.as_deref().unwrap_or(&run.goal),
                                if run.archived_at.is_some() { " (archived)" } else { "" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    state.notice = Some(format!(
                        "recent orchestration runs:\n{lines}\n\n/runs <id> opens a report · /runs --all includes archived"
                    ));
                }
            }
        }
        "/status" => {
            let turns = store
                .lock()
                .unwrap()
                .list_turn_records(&state.thread.id)
                .unwrap_or_default();
            let turn_count = turns.len();
            let latest = turns.into_iter().last();
            let mut text = format!(
                "thread: {} {}\nprovider: {}/{}\neffort: {}\nsession: {}\nmode: {}",
                &state.thread.id[..8.min(state.thread.id.len())],
                state.thread.title,
                state.thread.current_provider,
                state.thread.current_model.as_deref().unwrap_or("-"),
                state.thread.reasoning_effort.as_deref().unwrap_or("default"),
                state.thread.session_mode,
                state.permission_mode
            );
            match &state.goal {
                Some(goal) => {
                    let suffix = if state.goal_paused { " (paused)" } else { "" };
                    text.push_str(&format!("\ngoal: {goal}{suffix}"));
                }
                None => text.push_str("\ngoal: none"),
            }
            let open_turn = latest
                .as_ref()
                .map(|turn| {
                    !turn.phase.is_terminal()
                })
                .unwrap_or(false);
            let latest_error = latest.as_ref().and_then(|turn| turn.error.clone());
            text.push_str(&format!(
                "\nturns: {}\nturn: {}\nqueue: {} waiting",
                turn_count,
                latest
                    .map(|turn| format!("{} {}", turn.turn, turn.phase.as_str()))
                    .unwrap_or_else(|| "none".into()),
                state.queued_messages.len()
            ));
            if let Some(error) = latest_error {
                let preview: String = error.chars().take(160).collect();
                text.push_str(&format!("\nerror: {preview}"));
            }
            if open_turn {
                text.push_str("\nnote: open turn may be stale; /turns shows history");
            }
            if !state.adapter_lifecycles.is_empty() {
                let mut adapters: Vec<(String, String)> = state
                    .adapter_lifecycles
                    .iter()
                    .map(|(provider, lifecycle)| (provider.to_string(), lifecycle.describe()))
                    .collect();
                adapters.sort();
                text.push_str(&format!(
                    "\nadapters: {}",
                    adapters
                        .iter()
                        .map(|(provider, state)| format!("{provider}={state}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            state.notice = Some(text);
        }
        "/turns" => {
            let turns = store
                .lock()
                .unwrap()
                .list_turn_records(&state.thread.id)
                .unwrap_or_default();
            if turns.is_empty() {
                state.notice = Some("no turn records yet".into());
            } else {
                let mut text = format!("turns: {}", turns.len());
                let mut has_open = false;
                for turn in turns.iter().rev().take(12) {
                    if !turn.phase.is_terminal() {
                        has_open = true;
                    }
                    let ended = turn.ended_at.as_deref().unwrap_or("open");
                    let error = turn
                        .error
                        .as_deref()
                        .map(|error| format!(" · {error}"))
                        .unwrap_or_default();
                    text.push_str(&format!(
                        "\n  {} {} · ended {}{}",
                        turn.turn,
                        turn.phase.as_str(),
                        ended,
                        error
                    ));
                }
                if has_open {
                    text.push_str("\n\nopen turn may be stale; /status shows the guard details");
                }
                state.notice = Some(text);
            }
        }
        "/queue" => {
            if args.first().map(String::as_str) == Some("clear") {
                state.queued_messages.clear();
                match store.lock().unwrap().clear_queued_messages(&state.thread.id) {
                    Ok(()) => state.notice = Some("queue cleared".into()),
                    Err(error) => state.notice = Some(format!("cannot clear queue: {error}")),
                }
            } else if args.is_empty() {
                if state.queued_messages.is_empty() {
                    state.notice = Some("queue: empty".into());
                } else {
                    let mut text = format!("queue: {} waiting", state.queued_messages.len());
                    for (index, message) in state.queued_messages.iter().enumerate() {
                        let preview: String = message.lines().next().unwrap_or("").chars().take(70).collect();
                        text.push_str(&format!("\n  {}. {preview}", index + 1));
                    }
                    text.push_str("\n\n/queue clear to remove them");
                    state.notice = Some(text);
                }
            } else {
                state.notice = Some("usage: /queue · /queue clear".into());
            }
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
                    let guard = store.lock().unwrap();
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
        "/effort" => {
            match args.first().map(String::as_str) {
                None => {
                    state.notice = Some(format!(
                        "reasoning effort: {}",
                        state
                            .thread
                            .reasoning_effort
                            .as_deref()
                            .unwrap_or("harness default")
                    ));
                }
                Some(value) if matches!(value, "off" | "default" | "none") => {
                    let result = {
                        let guard = store.lock().unwrap();
                        ensure_persisted(&guard, &mut state.thread)
                            .and_then(|()| guard.set_thread_effort(&state.thread.id, None))
                    };
                    match result {
                        Ok(()) => {
                            state.thread.reasoning_effort = None;
                            state.banner_cache.borrow_mut().take();
                            state.notice = Some("reasoning effort cleared".into());
                        }
                        Err(error) => {
                            state.notice = Some(format!("cannot clear effort: {error}"))
                        }
                    }
                }
                Some(value) => match normalize_reasoning_effort(value) {
                    Some(effort) if state.thread.current_provider.supports_effort() => {
                        let result = {
                            let guard = store.lock().unwrap();
                            ensure_persisted(&guard, &mut state.thread).and_then(|()| {
                                guard.set_thread_effort(&state.thread.id, Some(&effort))
                            })
                        };
                        match result {
                            Ok(()) => {
                                state.thread.reasoning_effort = Some(effort.clone());
                                state.banner_cache.borrow_mut().take();
                                state.notice = Some(format!("reasoning effort set to {effort}"));
                            }
                            Err(error) => {
                                state.notice = Some(format!("cannot set effort: {error}"))
                            }
                        }
                    }
                    Some(_) => {
                        state.notice = Some(format!(
                            "{} does not support reasoning effort; use codex or claude",
                            state.thread.current_provider
                        ));
                    }
                    None => {
                        state.notice = Some(
                            "usage: /effort low|medium|high|xhigh|max|default".into(),
                        );
                    }
                },
            }
        }
        "/mode" => {
            if let Some(mode) = args.first().map(String::as_str).and_then(PermissionMode::parse) {
                state.apply_permission_mode(store, mode);
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
                    state.banner_cache.borrow_mut().take();
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
            state.thread.session_mode = crate::config::get_default_session_mode(&state.home);
            state.thread.permission_mode = state.permission_mode;
            state.history.clear();
            state.queued_messages.clear();
            state.thread.reasoning_effort = None;
            state.goal = None;
            state.goal_paused = false;
            state.thread.goal = None;
            state.thread.goal_paused = false;
            state.branch_label = None;
            state.diff_output = None;
            state.scroll_offset = 0;
            state.notice = Some("new thread, unsaved until you send a message".into());
        }
        "/fork" => {
            let mut title_args = args.clone();
            let through_turn = if title_args.first().map(String::as_str) == Some("--turn") {
                let Some(turn) = title_args.get(1).and_then(|value| value.parse::<i64>().ok()) else {
                    state.notice = Some("usage: /fork [--turn N] [title]".into());
                    return;
                };
                title_args.drain(..2);
                Some(turn)
            } else {
                None
            };
            if through_turn.is_none() {
                let latest_phase = store
                    .lock()
                    .unwrap()
                    .list_turn_records(&state.thread.id)
                    .unwrap_or_default()
                    .into_iter()
                    .last()
                    .map(|turn| turn.phase);
                if latest_phase
                    .map(|phase| !phase.is_terminal())
                    .unwrap_or(false)
                {
                    state.notice = Some("cannot fork while a turn is open".into());
                    return;
                }
            }
            let title = if title_args.is_empty() {
                match through_turn {
                    Some(turn) => format!("{} (fork at turn {turn})", state.thread.title),
                    None => format!("{} (fork)", state.thread.title),
                }
            } else {
                title_args.join(" ")
            };
            let source_title = state.thread.title.clone();
            let result = store
                .lock()
                .unwrap()
                .fork_thread_through_turn(&state.thread.id, &title, through_turn);
            match result {
                Ok(thread) => {
                    load_thread_state(state, store, thread);
                    state.notice = Some(match through_turn {
                        Some(turn) => format!(
                            "forked {source_title} through turn {turn}; both threads share this working tree"
                        ),
                        None => format!(
                            "forked {source_title}; both threads share this working tree"
                        ),
                    });
                }
                Err(error) => state.notice = Some(format!("cannot fork: {error}")),
            }
        }
        "/worktree" => {
            if state.running {
                state.notice = Some("wait for the current turn to finish before creating a worktree".into());
                return;
            }
            let source_id = state.thread.id.clone();
            let source_title = state.thread.title.clone();
            let source_cwd = state.thread.cwd.clone();
            let source_provider = state.thread.current_provider;
            let source_session_mode = state.thread.session_mode;
            let repo_root = match crate::worktree::repo_root(std::path::Path::new(&source_cwd)) {
                Ok(root) => root,
                Err(error) => {
                    state.notice = Some(format!("/worktree needs a git repository: {error}"));
                    return;
                }
            };
            if crate::worktree::is_dirty(&repo_root).unwrap_or(false) {
                state.notice = Some(
                    "source checkout has uncommitted changes; commit or stash before /worktree, or use /fork to stay in this checkout".into(),
                );
                return;
            }
            let persisted = {
                let guard = store.lock().unwrap();
                ensure_persisted(&guard, &mut state.thread)
            };
            if let Err(error) = persisted {
                state.notice = Some(format!("cannot prepare worktree fork: {error}"));
                return;
            }
            let title = if args.is_empty() {
                format!("{source_title} (worktree)")
            } else {
                args.join(" ")
            };
            let run_id = uuid::Uuid::new_v4().to_string();
            let short_thread = source_id[..8.min(source_id.len())].to_string();
            let worktree = match crate::worktree::create(
                &state.home,
                &repo_root,
                &run_id,
                &format!("thread-{short_thread}"),
            ) {
                Ok(worktree) => worktree,
                Err(error) => {
                    state.notice = Some(format!("cannot create worktree: {error}"));
                    return;
                }
            };
            let fork = {
                let guard = store.lock().unwrap();
                guard.fork_thread(&source_id, &title)
            };
            let fork = match fork {
                Ok(fork) => fork,
                Err(error) => {
                    let _ = crate::worktree::remove(&worktree);
                    state.notice = Some(format!("cannot fork into worktree: {error}"));
                    return;
                }
            };
            let worktree_path = worktree.path.to_string_lossy().to_string();
            let switched = {
                let guard = store.lock().unwrap();
                guard
                    .set_thread_cwd(&fork.id, &worktree_path)
                    .and_then(|()| guard.set_thread_session_mode(&fork.id, SessionMode::Single))
            };
            if let Err(error) = switched {
                let _ = crate::worktree::remove(&worktree);
                state.notice = Some(format!("cannot attach worktree to fork: {error}"));
                return;
            }
            let resolved = resolve_config(&state.home, std::path::Path::new(&source_cwd));
            let model = state
                .thread
                .current_model
                .clone()
                .unwrap_or_else(|| resolved.for_provider(source_provider).model.clone());
            let plan = orchestrate::Plan {
                summary: Some(format!("thread worktree from {short_thread}")),
                tasks: vec![orchestrate::PlanTask {
                    title: "thread worktree".to_string(),
                    provider: source_provider.as_str().to_string(),
                    model: None,
                    effort: None,
                    prompt: title.clone(),
                }],
            };
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let task_id = uuid::Uuid::new_v4().to_string();
            let record = {
                let guard = store.lock().unwrap();
                serde_json::to_string(&plan)
                    .map_err(anyhow::Error::from)
                    .and_then(|plan_json| {
                        guard.create_orchestration_run(
                            &run_id,
                            &title,
                            source_provider,
                            &model,
                            Some("thread worktree"),
                            &plan_json,
                            false,
                        )
                    })
                    .and_then(|()| {
                        guard.add_orchestration_task(
                            &task_id,
                            &run_id,
                            1,
                            "thread worktree",
                            source_provider,
                            &model,
                            &title,
                            Some(worktree_path.as_str()),
                            Some(&worktree.branch),
                            None,
                            Some(fork.id.as_str()),
                            "completed",
                            None,
                            None,
                            &now,
                            Some(&now),
                            None,
                            None,
                            None,
                        )
                    })
                    .and_then(|()| guard.finish_orchestration_run(&run_id, "completed"))
            };
            if let Err(error) = record {
                let _ = store.lock().unwrap().set_thread_cwd(&fork.id, &source_cwd);
                let _ = store
                    .lock()
                    .unwrap()
                    .set_thread_session_mode(&fork.id, source_session_mode);
                let _ = crate::worktree::remove(&worktree);
                state.notice = Some(format!("cannot record worktree run: {error}"));
                return;
            }
            let short_run = run_id[..8.min(run_id.len())].to_string();
            let short_fork = fork.id[..8.min(fork.id.len())].to_string();
            {
                let guard = store.lock().unwrap();
                let _ = guard.add_message(
                    &source_id,
                    0,
                    "orchestration",
                    None,
                    None,
                    &format!(
                        "worktree fork {short_fork} · run {short_run} · branch {}",
                        worktree.branch
                    ),
                );
                let _ = guard.add_message(
                    &fork.id,
                    0,
                    "orchestration",
                    None,
                    None,
                    &format!(
                        "worktree · run {short_run} · next successful turn commits here"
                    ),
                );
            }
            let mut fork = fork;
            fork.cwd = worktree_path;
            fork.session_mode = SessionMode::Single;
            load_thread_state(state, store, fork);
            state.prompt_history = store
                .lock()
                .unwrap()
                .list_prompt_history(&state.thread.cwd)
                .unwrap_or_default();
            state.prompt_history_index = None;
            state.custom_commands = discover_custom_commands(&state.thread.cwd);
            state.overlay = Overlay::None;
            state.multi = None;
            state.review = None;
            state.orchestration_report = None;
            state.input.clear();
            state.input_cursor = 0;
            state.notice = Some(format!(
                "worktree fork {short_fork} · run {short_run} · next turn commits here"
            ));
        }
        "/threads" => {
            let threads: Vec<(String, String)> = {
                let guard = store.lock().unwrap();
                guard
                    .list_threads_for_path(std::path::Path::new(&state.thread.cwd))
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
                        state.queued_messages = guard.list_queued_messages(&thread.id).unwrap_or_default();
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
        "/review" => {
            let requested = args.first().and_then(|turn| turn.parse::<i64>().ok());
            let message = match requested {
                Some(turn) => state
                    .history
                    .iter()
                    .rev()
                    .find(|message| message.role == "diff" && message.turn == turn),
                None => state.history.iter().rev().find(|message| message.role == "diff"),
            };
            match message {
                Some(message) => {
                    let rows = diff_render::review_rows(&message.text);
                    let changed_count = rows.iter().filter(|row| row.added || row.removed).count();
                    if changed_count == 0 {
                        state.notice = Some("this turn has no changed lines to review".into());
                    } else {
                        let fingerprint = diff_render::fingerprint(&message.text);
                        let saved = store.lock().unwrap().list_unsent_review_notes(
                            &state.thread.id,
                            message.turn,
                            &fingerprint,
                        );
                        let (notes, load_error) = match saved {
                            Ok(rows) => (
                                rows.into_iter()
                                    .map(|note| ReviewNote {
                                        id: note.id,
                                        path: note.path,
                                        line: note.line,
                                        added: note.added,
                                        body: note.body,
                                    })
                                    .collect(),
                                None,
                            ),
                            Err(error) => (Vec::new(), Some(error)),
                        };
                        let selected = rows
                            .iter()
                            .position(|row| row.added || row.removed)
                            .unwrap_or(0);
                        state.review = Some(ReviewState {
                            turn: message.turn,
                            fingerprint,
                            rows,
                            selected,
                            notes,
                            note_input: None,
                        });
                        state.notice = load_error
                            .map(|error| format!("cannot restore review notes: {error}"));
                    }
                }
                None => {
                    state.notice = Some(
                        "no turn diff yet; run a turn or use /review <turn>".into(),
                    );
                }
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
            let native = args.iter().any(|arg| arg == "--native");
            let has_user_messages = state.history.iter().any(|message| message.role == "user");
            if has_user_messages {
                state.overlay = Overlay::RewindPicker {
                    selected: 0,
                    native,
                };
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

fn draw_review(frame: &mut ratatui::Frame, area: Rect, state: &TuiState, review: &ReviewState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLORS.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::styled(
        format!(
            "review turn {} · {} changed lines · ↑↓ move · [ ] file · n note · d delete · s send · esc close",
            review.turn,
            review.rows.iter().filter(|row| row.added || row.removed).count()
        ),
        Style::default().fg(COLORS.muted),
    ));

    let footer_rows = 1u16;
    let available = inner.height.saturating_sub(1 + footer_rows) as usize;
    let start = if available == 0 || review.rows.len() <= available {
        0
    } else {
        review
            .selected
            .saturating_sub(available / 2)
            .min(review.rows.len() - available)
    };
    let end = (start + available).min(review.rows.len());

    for index in start..end {
        let row = &review.rows[index];
        let selected = index == review.selected;
        let noted = (row.added || row.removed)
            && review.notes.iter().any(|note| {
                note.path == row.path && note.line == row.line_number && note.added == row.added
            });
        let path_label = format!("{}:{}", row.path, row.line_number);
        let max_path = (inner.width as usize).saturating_sub(12).max(12);
        let path_label = if path_label.chars().count() > max_path {
            let tail: String = path_label
                .chars()
                .rev()
                .take(max_path.saturating_sub(1))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!("…{tail}")
        } else {
            path_label
        };
        let marker = if selected { "›" } else { " " };
        let note_marker = if noted { "•" } else { " " };
        let (sign, row_color) = if row.added {
            ("+", COLORS.success)
        } else if row.removed {
            ("-", COLORS.error)
        } else {
            (" ", COLORS.muted)
        };
        let used = path_label.chars().count() + 4;
        let text_width = (inner.width as usize).saturating_sub(used).max(1);
        let text: String = row.text.chars().take(text_width).collect();
        let mut style = Style::default().fg(row_color);
        if selected {
            style = style.bg(COLORS.user_bg).add_modifier(Modifier::BOLD);
        }
        let prefix_style = if selected {
            Style::default().fg(COLORS.accent).bg(COLORS.user_bg)
        } else {
            Style::default().fg(COLORS.muted)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}{note_marker} "), prefix_style),
            Span::styled(path_label, prefix_style),
            Span::styled(format!(" {sign} "), style),
            Span::styled(text, style),
        ]));
    }

    if let Some(input) = review.note_input.as_ref() {
        let row = review.rows.get(review.selected);
        let target = row
            .map(|row| format!("{}:{}", row.path, row.line_number))
            .unwrap_or_else(|| "no line".into());
        lines.push(Line::from(vec![
            Span::styled(format!("note {target} › "), Style::default().fg(COLORS.accent)),
            Span::styled(input.clone(), Style::default().fg(COLORS.text)),
            Span::styled("█", Style::default().fg(COLORS.accent)),
        ]));
    } else {
        lines.push(Line::styled(
            format!(
                "notes: {} · {}",
                review.notes.len(),
                if review.notes.is_empty() {
                    "press n on a changed line to add one"
                } else {
                    "press s to send them to the agent"
                }
            ),
            Style::default().fg(COLORS.muted),
        ));
    }

    let _ = state;
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_multi(frame: &mut ratatui::Frame, area: Rect, multi: &MultiView) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLORS.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;
    let prompt: String = multi.prompt.chars().take(width.saturating_sub(14)).collect();
    let mut lines: Vec<Line> = vec![
        Line::styled(
            format!("multi · {} provider(s) · {prompt}", multi.children.len()),
            Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            "↑↓ select · enter opens the thread · esc closes this view",
            Style::default().fg(COLORS.muted),
        ),
    ];
    for (index, child) in multi.children.iter().enumerate() {
        let selected = index == multi.selected;
        let (marker, status_style) = match child.status {
            MultiStatus::Queued => ("·", Style::default().fg(COLORS.muted)),
            MultiStatus::Running => ("◐", Style::default().fg(COLORS.accent)),
            MultiStatus::Done => ("✓", Style::default().fg(COLORS.success)),
            MultiStatus::Failed => ("✗", Style::default().fg(COLORS.error)),
        };
        let preview = child
            .error
            .as_deref()
            .or_else(|| child.reply.lines().find(|line| !line.trim().is_empty()))
            .unwrap_or(match child.status {
                MultiStatus::Queued => "queued behind another turn",
                MultiStatus::Running => "waiting...",
                _ => "no reply",
            });
        let provider = child.provider.as_str();
        let used = provider.chars().count() + 8;
        let preview: String = preview.chars().take(width.saturating_sub(used)).collect();
        let pointer = if selected { "›" } else { " " };
        let text_style = if selected {
            Style::default().fg(COLORS.text).bg(COLORS.user_bg)
        } else {
            Style::default().fg(COLORS.muted)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{pointer} {marker} "), status_style),
            Span::styled(format!("{provider:<7} "), text_style),
            Span::styled(preview, text_style),
        ]));
    }
    let visible = inner.height.saturating_sub(2) as usize;
    if lines.len() > visible {
        lines.truncate(visible.max(1));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        out.pop();
        out.push('…');
    }
    out
}

fn draw_orchestration_report(
    frame: &mut ratatui::Frame,
    area: Rect,
    state: &TuiState,
    report: &RunReport,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLORS.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;
    let short_run = &report.run_id[..8.min(report.run_id.len())];
    let status_color = match report.status.as_str() {
        "completed" => COLORS.success,
        "failed" => COLORS.error,
        _ => COLORS.warning,
    };
    let header_lines = vec![
        Line::from(vec![
            Span::styled(
                format!("orchestration {short_run} "),
                Style::default().fg(COLORS.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                report.status.clone(),
                Style::default().fg(status_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {} worker(s)", report.tasks.len()),
                Style::default().fg(COLORS.muted),
            ),
            if report.archived_at.is_some() {
                Span::styled(" · archived", Style::default().fg(COLORS.warning))
            } else {
                Span::raw("")
            },
        ]),
        if state.orchestration_confirm_integrate {
            Line::styled(
                format!(
                    "merge worker branches into {}? enter confirms · esc cancels",
                    report
                        .repo_root
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "the source checkout".to_string())
                ),
                Style::default().fg(COLORS.warning).add_modifier(Modifier::BOLD),
            )
        } else if state.orchestration_confirm_ship {
            Line::styled(
                format!(
                    "push worker branches and open PRs from {}? enter confirms · esc cancels",
                    report
                        .repo_root
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "the source checkout".to_string())
                ),
                Style::default()
                    .fg(COLORS.warning)
                    .add_modifier(Modifier::BOLD),
            )
        } else if state.orchestration_integrating {
            Line::styled(
                "integrating worker branches...",
                Style::default().fg(COLORS.secondary).add_modifier(Modifier::BOLD),
            )
        } else if state.orchestration_shipping {
            Line::styled(
                "pushing worker branches and opening PRs...",
                Style::default()
                    .fg(COLORS.secondary)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Line::styled(
                "↑↓ select · enter open · i merge · p PRs · a archive · PgUp/PgDn scroll · esc closes",
                Style::default().fg(COLORS.muted),
            )
        },
    ];

    let mut lines: Vec<Line> = Vec::new();
    let mut task_starts: Vec<usize> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("goal: ", Style::default().fg(COLORS.muted)),
        Span::styled(
            truncate_chars(&report.goal, width.saturating_sub(6)),
            Style::default().fg(COLORS.text),
        ),
    ]));
    lines.push(Line::styled(
        truncate_chars(
            &format!(
                "coordinator: {}/{}",
                report.coordinator_provider, report.coordinator_model
            ),
            width,
        ),
        Style::default().fg(COLORS.muted),
    ));
    let source = report
        .repo_root
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "not a git repository".to_string());
    let dirty = if report.dirty_source {
        " (dirty; workers received a source snapshot)"
    } else {
        ""
    };
    lines.push(Line::styled(
        truncate_chars(&format!("source: {source}{dirty}"), width),
        Style::default().fg(COLORS.muted),
    ));
    if let Some(summary) = &report.summary {
        lines.push(Line::styled(
            truncate_chars(&format!("plan: {summary}"), width),
            Style::default().fg(COLORS.secondary),
        ));
    }
    for (index, task) in report.tasks.iter().enumerate() {
        task_starts.push(lines.len());
        let (marker, status_color) = match task.status.as_str() {
            "completed" => ("✓", COLORS.success),
            "needs_input" => ("?", COLORS.warning),
            "cancelled" => ("⊘", COLORS.warning),
            "failed" => ("✗", COLORS.error),
            "running" => ("◐", COLORS.accent),
            _ => ("·", COLORS.muted),
        };
        let selected = index == state.orchestration_selected;
        let row_style = if selected {
            Style::default().fg(COLORS.text).bg(COLORS.user_bg)
        } else {
            Style::default().fg(COLORS.muted)
        };
        let pointer = if selected { "›" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{pointer} {marker} "),
                Style::default().fg(status_color).bg(if selected {
                    COLORS.user_bg
                } else {
                    Color::Reset
                }),
            ),
            Span::styled(
                truncate_chars(
                    &format!(
                        "[{}] {} · {}/{}{} · {}",
                        index + 1,
                        task.title,
                        task.provider,
                        task.model,
                        task.effort
                            .as_deref()
                            .map(|effort| format!("@{effort}"))
                            .unwrap_or_default(),
                        task.status
                    ),
                    width.saturating_sub(4),
                ),
                row_style.add_modifier(Modifier::BOLD),
            ),
        ]));
        if let Some(worktree) = &task.worktree {
            lines.push(Line::styled(
                truncate_chars(
                    &format!(
                        "    worktree: {} ({})",
                        worktree.path.display(),
                        worktree.branch
                    ),
                    width,
                ),
                Style::default().fg(COLORS.muted),
            ));
        }
        if let Some(commit) = &task.commit {
            lines.push(Line::styled(
                format!("    commit: {}", &commit[..8.min(commit.len())]),
                Style::default().fg(COLORS.success),
            ));
        }
        if let Some(pr_url) = &task.pr_url {
            lines.push(Line::styled(
                truncate_chars(&format!("    pr: {pr_url}"), width),
                Style::default().fg(COLORS.accent),
            ));
        }
        if let Some(thread_id) = &task.thread_id {
            lines.push(Line::styled(
                format!("    thread: {}", &thread_id[..8.min(thread_id.len())]),
                Style::default().fg(COLORS.muted),
            ));
        }
        if let Some(question) = task.text.as_deref().and_then(orchestrate::worker_question) {
            lines.push(Line::styled(
                truncate_chars(&format!("    question: {question}"), width),
                Style::default().fg(COLORS.warning),
            ));
        } else if let Some(text) = &task.text {
            for line in text.lines().filter(|line| !line.trim().is_empty()).take(2) {
                lines.push(Line::styled(
                    truncate_chars(&format!("    {line}"), width),
                    Style::default().fg(COLORS.text),
                ));
            }
        }
        if let Some(error) = &task.error {
            lines.push(Line::styled(
                truncate_chars(&format!("    error: {error}"), width),
                Style::default().fg(COLORS.error),
            ));
        }
        lines.push(Line::raw(""));
    }
    if report.tasks.is_empty() {
        lines.push(Line::styled(
            "no workers were planned",
            Style::default().fg(COLORS.muted),
        ));
    }
    if let Some(integrate) = &state.orchestration_integrate_report {
        lines.push(Line::from(vec![
            Span::styled("integration: ", Style::default().fg(COLORS.muted)),
            Span::styled(
                if integrate.conflict.is_some() { "conflict" } else { "done" },
                Style::default()
                    .fg(if integrate.conflict.is_some() {
                        COLORS.error
                    } else {
                        COLORS.success
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        for task in &integrate.merged {
            lines.push(Line::styled(
                truncate_chars(
                    &format!(
                        "  merged [{}] {} -> {}",
                        task.index,
                        task.title,
                        &task.merge_commit[..8.min(task.merge_commit.len())]
                    ),
                    width,
                ),
                Style::default().fg(COLORS.success),
            ));
        }
        for task in &integrate.skipped {
            lines.push(Line::styled(
                truncate_chars(
                    &format!("  skipped [{}] {} · {}", task.index, task.title, task.reason),
                    width,
                ),
                Style::default().fg(COLORS.muted),
            ));
        }
        if let Some(conflict) = &integrate.conflict {
            lines.push(Line::styled(
                truncate_chars(
                    &format!(
                        "  conflict [{}] {} · {}",
                        conflict.index,
                        conflict.title,
                        conflict.files.join(", ")
                    ),
                    width,
                ),
                Style::default().fg(COLORS.error),
            ));
        }
    }

    if let Some(ship) = &state.orchestration_ship_report {
        lines.push(Line::from(vec![
            Span::styled("ship: ", Style::default().fg(COLORS.muted)),
            Span::styled(
                if !ship.failed.is_empty() {
                    if ship.opened.is_empty() {
                        "failed"
                    } else {
                        "partial"
                    }
                } else if ship.opened.is_empty() {
                    "skipped"
                } else {
                    "done"
                },
                Style::default()
                    .fg(if ship.failed.is_empty() {
                        COLORS.success
                    } else {
                        COLORS.error
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        for task in &ship.opened {
            lines.push(Line::styled(
                truncate_chars(
                    &format!(
                        "  opened [{}] {} -> {}",
                        task.index, task.title, task.pr_url
                    ),
                    width,
                ),
                Style::default().fg(COLORS.success),
            ));
        }
        for task in &ship.skipped {
            lines.push(Line::styled(
                truncate_chars(
                    &format!(
                        "  skipped [{}] {} · {}",
                        task.index, task.title, task.reason
                    ),
                    width,
                ),
                Style::default().fg(COLORS.muted),
            ));
        }
        for task in &ship.failed {
            lines.push(Line::styled(
                truncate_chars(
                    &format!("  failed [{}] {} · {}", task.index, task.title, task.reason),
                    width,
                ),
                Style::default().fg(COLORS.error),
            ));
        }
    }

    let header_height = header_lines.len() as u16;
    let header_area = Rect {
        height: header_height.min(inner.height),
        ..inner
    };
    frame.render_widget(Paragraph::new(header_lines), header_area);

    let body_height = inner.height.saturating_sub(header_height);
    if body_height == 0 {
        return;
    }
    let body_area = Rect {
        y: inner.y + header_height,
        height: body_height,
        ..inner
    };
    let visible = body_height as usize;
    let max_scroll = lines.len().saturating_sub(visible);
    let mut offset = state.orchestration_scroll.min(max_scroll);
    if let Some(start) = task_starts.get(state.orchestration_selected) {
        if *start < offset {
            offset = *start;
        } else if *start >= offset + visible {
            offset = start.saturating_sub(visible.saturating_sub(1));
        }
    }
    frame.render_widget(
        Paragraph::new(lines).scroll((offset.min(u16::MAX as usize) as u16, 0)),
        body_area,
    );
}

fn draw_floating_approval(frame: &mut ratatui::Frame, area: Rect, state: &TuiState) {
    let Some(approval) = state.approvals.last() else {
        return;
    };
    if area.height < 4 {
        return;
    }
    let approval_area = Rect {
        y: area.y + area.height.saturating_sub(4),
        height: 4,
        ..area
    };
    draw_approval(frame, approval_area, state, approval);
}

fn draw(frame: &mut ratatui::Frame, state: &TuiState, spinner_frame: usize) {
    let area = frame.area();
    if let Some(report) = &state.orchestration_report {
        hide_transcript_view(state);
        draw_orchestration_report(frame, area, state, report);
        draw_floating_approval(frame, area, state);
        return;
    }
    if let Some(multi) = &state.multi {
        draw_multi(frame, area, multi);
        draw_floating_approval(frame, area, state);
        return;
    }
    if let Some(review) = &state.review {
        hide_transcript_view(state);
        draw_review(frame, area, state, review);
        draw_floating_approval(frame, area, state);
        return;
    }
    let suggestions = suggestions_for(&state.input);
    let composer_rows = 4u16
        + state.visible_input_lines().saturating_sub(1) as u16
        + if suggestions.is_empty() {
            0
        } else {
            (suggestions.len().min(6) + 2) as u16
        };
    let notice_rows = state
        .notice
        .as_ref()
        .map(|notice| wrapped_rows(notice, area.width as usize))
        .unwrap_or(0);
    let overlay_rows = if !state.approvals.is_empty() {
        4
    } else {
        match &state.overlay {
            Overlay::None => 0,
            Overlay::RewindPicker { .. } => {
                state
                    .history
                    .iter()
                    .filter(|message| message.role == "user")
                    .count()
                    .min(8)
                    + 4
            }
            Overlay::CodeConfirm { .. } => 4,
            Overlay::SwitchPicker { .. } => PROVIDERS.len() + 4,
            Overlay::ModelPicker { models, filter, .. } => {
                model_filter_indices(models, filter).len().min(8) + 4
            }
            Overlay::SettingsPicker {
                categories,
                category_index,
                ..
            } => {
                categories
                    .get(*category_index)
                    .map(|(_, values): &(String, Vec<String>)| values.len())
                    .unwrap_or(0)
                    .min(6)
                    + categories.len().min(4)
                    + 4
            }
            Overlay::McpPicker { .. } => 8,
            Overlay::SkillsPicker { skills, .. } => skills.len().min(8) + 4,
            Overlay::ModePicker { .. } => 7,
            Overlay::AuthPicker { .. } => 9,
            Overlay::ThreadPicker { threads, .. } => threads.len().min(8) + 4,
            Overlay::CheckpointPicker { checkpoints, .. } => checkpoints.len().min(8) + 4,
            Overlay::DirtyConfirm { .. } => 5,
            Overlay::OrchestrationPreview { preview } => {
                preview.plan.tasks.len().min(8) * 2 + 8
            }
        }
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
    if let Some(approval) = state.approvals.last() {
        draw_approval(frame, chunks[1], state, approval);
    } else {
        match &state.overlay {
            Overlay::RewindPicker { selected, native } => {
                draw_rewind_picker(frame, chunks[1], state, *selected, *native)
            }
            Overlay::CodeConfirm { turn, .. } => draw_code_confirm(frame, chunks[1], *turn),
            Overlay::SwitchPicker { selected } => {
                draw_switch_picker(frame, chunks[1], state, *selected)
            }
            Overlay::ModelPicker {
                provider,
                models,
                selected,
                filter,
            } => draw_model_picker(
                frame,
                chunks[1],
                state,
                *provider,
                models,
                *selected,
                filter,
            ),
            Overlay::SettingsPicker {
                provider,
                categories,
                category_index,
                value_index,
            } => draw_settings_picker(
                frame,
                chunks[1],
                *provider,
                categories,
                *category_index,
                *value_index,
            ),
            Overlay::McpPicker { selected } => draw_mcp_picker(frame, chunks[1], *selected),
            Overlay::SkillsPicker { selected, skills } => {
                draw_skills_picker(frame, chunks[1], skills, *selected)
            }
            Overlay::ModePicker { selected } => {
                draw_mode_picker(frame, chunks[1], state, *selected)
            }
            Overlay::AuthPicker { selected } => {
                draw_auth_picker(frame, chunks[1], state, *selected)
            }
            Overlay::ThreadPicker { selected, threads } => {
                draw_thread_picker(frame, chunks[1], threads, *selected)
            }
            Overlay::CheckpointPicker {
                selected,
                checkpoints,
            } => draw_checkpoint_picker(frame, chunks[1], checkpoints, *selected),
            Overlay::DirtyConfirm { .. } => draw_dirty_confirm(frame, chunks[1]),
            Overlay::OrchestrationPreview { preview } => {
                draw_orchestration_preview(frame, chunks[1], state, preview)
            }
            Overlay::None => {}
        }
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
    if let Some(hint) = crate::doctor::missing_provider_hint(provider) {
        state.notice = Some(hint);
        return;
    }
    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
    let default_model = resolved.for_provider(provider).model.clone();
    let display_model = model.clone().unwrap_or(default_model);
    let result = {
        let guard = store.lock().unwrap();
        let result = ensure_persisted(&guard, &mut state.thread)
            .and_then(|()| guard.set_thread_provider(&state.thread.id, provider, model.clone()));
        if result.is_ok() && !provider.supports_effort() {
            guard.set_thread_effort(&state.thread.id, None)
        } else {
            result
        }
    };
    match result {
        Ok(()) => {
            state.thread.current_provider = provider;
            state.thread.current_model = model.clone();
            if !provider.supports_effort() {
                state.thread.reasoning_effort = None;
            }
            state.banner_cache.borrow_mut().take();
            state.set_session_mode(store, SessionMode::Single);
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
                    "single harness: {provider}/{display_model}, resuming native session {}",
                    &session.native_session_id[..8.min(session.native_session_id.len())]
                ),
                None => format!(
                    "single harness: {provider}/{display_model}. First turn carries a context handoff."
                ),
            });
        }
        Err(error) => state.notice = Some(format!("switch failed: {error}")),
    }
}

fn provider_label_with(provider: ProviderId, installed: bool) -> String {
    if installed {
        provider.to_string()
    } else {
        format!("{provider} (not installed)")
    }
}

fn provider_label(provider: ProviderId) -> String {
    provider_label_with(
        provider,
        crate::doctor::find_executable(provider.binary_name()).is_some(),
    )
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
                "❯ {marker} {} · {} · {}",
                provider_label(*provider),
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
    filter: &str,
) {
    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
    let selected_model = if state.thread.current_provider == provider {
        state
            .thread
            .current_model
            .clone()
            .unwrap_or_else(|| resolved.for_provider(provider).model.clone())
    } else {
        resolved.for_provider(provider).model.clone()
    };
    let filtered = model_filter_indices(models, filter);
    let visible = filtered.len().min(8);
    let start = if visible == 0 || filtered.len() <= visible {
        0
    } else {
        selected
            .saturating_sub(visible / 2)
            .min(filtered.len() - visible)
    };
    let end = (start + visible).min(filtered.len());
    let filter_label = if filter.is_empty() {
        "type to filter".to_string()
    } else {
        format!("filter \"{filter}\"")
    };
    let position = if filtered.is_empty() {
        0
    } else {
        selected.min(filtered.len() - 1) + 1
    };
    let mut lines: Vec<Line> = vec![Line::styled(
        format!(
            "{provider} models · {position}/{} · {filter_label} · enter · esc",
            filtered.len()
        ),
        Style::default().fg(COLORS.muted),
    )];
    for (offset, model_index) in filtered[start..end].iter().enumerate() {
        let index = start + offset;
        let model = &models[*model_index];
        let marker = if *model == selected_model { "●" } else { " " };
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
            lines.push(Line::styled(
                format!("❯ {marker} {} {mode}", provider_label(*provider)),
                style,
            ));
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
        hide_transcript_view(state);
        let rendered = state
            .diff_cache
            .borrow_mut()
            .render(diff, area.width.saturating_sub(2) as usize);
        let lines: Vec<Line> = std::iter::once(Line::styled(
            "diff · /clear to close",
            Style::default().fg(COLORS.muted),
        ))
        .chain(rendered.iter().cloned())
        .collect();
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }

    let banner = state.banner_lines();
    if state.history.is_empty()
        && !state.running
        && state.streaming_text.is_empty()
        && !state.orchestration_pending
        && area.height >= 12
    {
        hide_transcript_view(state);
        let banner_height = (banner.len() as u16).min(area.height);
        let banner_area = Rect {
            height: banner_height,
            ..area
        };
        frame.render_widget(Paragraph::new(banner.clone()), banner_area);
        let remaining = Rect {
            y: area.y + banner_height,
            height: area.height.saturating_sub(banner_height),
            ..area
        };
        let logo_height = LOGO_ROWS.len() as u16 + 2;
        if remaining.height >= logo_height {
            let top = remaining.height.saturating_sub(logo_height) / 2;
            let logo_area = Rect {
                y: remaining.y + top,
                height: logo_height,
                ..remaining
            };
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
        }
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

    // Build all terminal lines (each element = exactly 1 rendered row).
    // The banner scrolls with the conversation, like Codex and Claude Code.
    let mut all_lines: Vec<Line> = banner;
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
        if message.role == "orchestration" {
            let style = if message.text.starts_with("goal") {
                Style::default()
                    .fg(COLORS.secondary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(COLORS.muted)
            };
            for line in wrap(&message.text) {
                all_lines.push(Line::styled(line, style));
            }
            all_lines.push(Line::from(vec![Span::raw(" ")]));
            continue;
        }
        if message.role == "diff" {
            let rendered = state
                .diff_cache
                .borrow_mut()
                .render(&message.text, width.saturating_sub(2));
            all_lines.extend(rendered.iter().cloned());
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
            let rendered = state
                .markdown_cache
                .borrow_mut()
                .render(&message.text, width.saturating_sub(2));
            all_lines.extend(rendered.iter().cloned());
        }
        all_lines.push(Line::from(vec![Span::raw(" ")]));
    }

    // Live turn activity flows right after the newest message, in the
    // transcript itself rather than a separate panel above the composer.
    let mut live_lines = 0usize;
    let spinner = SPINNER_FRAMES[spinner_frame];
    if state.running && state.tool_lines.is_empty() && state.streaming_text.is_empty() {
        if let Some(provider) = state.queued_provider {
            all_lines.push(Line::from(vec![
                Span::styled("·", Style::default().fg(COLORS.muted)),
                Span::raw(" "),
                Span::styled(format!("Queued for {provider}"), Style::default().fg(COLORS.muted)),
            ]));
        } else {
            all_lines.push(Line::from(vec![
                Span::styled(spinner, Style::default().fg(COLORS.accent)),
                Span::raw(" "),
                Span::styled("Thinking", Style::default().fg(COLORS.accent)),
            ]));
        }
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
        let markdown_lines = markdown::render(&state.streaming_text, width.saturating_sub(2));
        live_lines += markdown_lines.len();
        all_lines.extend(markdown_lines);
    }
    if state.orchestration_pending {
        all_lines.push(Line::from(vec![
            Span::styled(spinner, Style::default().fg(COLORS.secondary)),
            Span::raw(" "),
            Span::styled(
                if state.orchestration_cancelling {
                    "Cancelling orchestration"
                } else {
                    "Orchestrating"
                },
                Style::default().fg(if state.orchestration_cancelling {
                    COLORS.warning
                } else {
                    COLORS.secondary
                }),
            ),
        ]));
        live_lines += 1;
        for worker in &state.orchestration_workers {
            let (marker, color) = if worker.status.starts_with("completed") {
                ("✓", COLORS.success)
            } else if worker.status == "needs_input" {
                ("?", COLORS.warning)
            } else if worker.status == "cancelled" {
                ("⊘", COLORS.warning)
            } else if worker.status == "failed" {
                ("✗", COLORS.error)
            } else if worker.status == "running" {
                (spinner, COLORS.accent)
            } else {
                ("·", COLORS.muted)
            };
            let model = if worker.model.is_empty() {
                worker.provider.to_string()
            } else {
                format!("{}/{}", worker.provider, worker.model)
            };
            let error = worker
                .error
                .as_deref()
                .map(|error| format!(" · {error}"))
                .unwrap_or_default();
            all_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(marker, Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(
                    format!(
                        "[{}] {} · {} · {}{}",
                        worker.index, worker.title, model, worker.status, error
                    ),
                    Style::default().fg(color),
                ),
            ]));
            live_lines += 1;
        }
    }
    if live_lines > 0 {
        all_lines.push(Line::from(vec![Span::raw(" ")]));
    }

    let total = all_lines.len();
    let viewport = area.height as usize;
    let previous_total = state.transcript_total.replace(total);
    if previous_total != total {
        state.transcript_selection.set(None);
        state.transcript_anchor.set(None);
    }
    if let Some((mut selection_start, mut selection_end)) = state.transcript_selection.get() {
        if selection_start > selection_end {
            std::mem::swap(&mut selection_start, &mut selection_end);
        }
        if total > 0 {
            let last_line = total - 1;
            selection_start.line = selection_start.line.min(last_line);
            selection_end.line = selection_end.line.min(last_line);
            for (line_index, line) in all_lines
                .iter_mut()
                .enumerate()
                .take(selection_end.line.saturating_add(1))
                .skip(selection_start.line)
            {
                let line_length = line
                    .spans
                    .iter()
                    .map(|span| span.content.chars().count())
                    .sum::<usize>();
                let from = if line_index == selection_start.line {
                    selection_start.column.min(line_length)
                } else {
                    0
                };
                let to = if line_index == selection_end.line {
                    selection_end.column.min(line_length)
                } else {
                    line_length
                };
                tint_line_range(line, from, to, COLORS.selection_bg);
            }
        }
    }
    let plain: Vec<String> = all_lines.iter().map(line_plain_text).collect();

    if total <= viewport {
        *state.transcript_view.borrow_mut() = Some(TranscriptViewport {
            area,
            content_top: area.y,
            content_start: 0,
            content_rows: total,
            total,
            plain,
        });
        frame.render_widget(Paragraph::new(all_lines), area);
        return;
    }

    // Content is taller than viewport - compute which lines to show.
    let scroll_offset = state.scroll_offset.min(total.saturating_sub(1));
    let (start, visible) = if scroll_offset == 0 {
        // Bottom-sticky: show the LAST viewport lines.
        let start = total.saturating_sub(viewport);
        (start, all_lines[start..].to_vec())
    } else {
        // Scrolled up by scroll_offset lines from the bottom.
        let end = total.saturating_sub(scroll_offset);
        let start = end.saturating_sub(viewport);
        (start, all_lines[start..end].to_vec())
    };

    // Add scroll indicator as the first line if scrolled.
    let mut render_lines = Vec::new();
    let (content_start, content_rows) = if scroll_offset > 0 {
        render_lines.push(Line::styled(
            format!("↑ {scroll_offset} lines above · ↓ or End for bottom"),
            Style::default().fg(COLORS.muted),
        ));
        let content = visible[1.min(visible.len())..].to_vec();
        let rows = content.len();
        render_lines.extend(content);
        (start + 1, rows)
    } else {
        let rows = visible.len();
        render_lines = visible;
        (start, rows)
    };
    *state.transcript_view.borrow_mut() = Some(TranscriptViewport {
        area,
        content_top: if scroll_offset > 0 { area.y.saturating_add(1) } else { area.y },
        content_start,
        content_rows,
        total,
        plain,
    });
    frame.render_widget(Paragraph::new(render_lines), area);
}

fn draw_approval(
    frame: &mut ratatui::Frame,
    area: Rect,
    state: &TuiState,
    approval: &ApprovalState,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLORS.warning))
        .border_style(Style::default().fg(COLORS.border));
    let target = if approval.thread_id == state.thread.id {
        String::new()
    } else {
        format!(" for thread {} ", &approval.thread_id[..approval.thread_id.len().min(8)])
    };
    let lines = vec![
        Line::styled(
            format!("approval needed{target}: {}", approval.summary),
            Style::default().fg(COLORS.warning),
        ),
        Line::styled("[y] allow once · [a] allow for session · [n] deny", Style::default().fg(COLORS.muted)),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(block),
        area,
    );
}

fn draw_rewind_picker(
    frame: &mut ratatui::Frame,
    area: Rect,
    state: &TuiState,
    selected: usize,
    native: bool,
) {
    let user_messages: Vec<(&i64, &String)> = state
        .history
        .iter()
        .filter(|message| message.role == "user")
        .map(|message| (&message.turn, &message.text))
        .collect();
    let newest_first: Vec<&(&i64, &String) > = user_messages.iter().rev().collect();
    let mut lines: Vec<Line> = vec![Line::styled(
        if native {
            "rewind to message · native history · enter select · esc cancel"
        } else {
            "rewind to message · enter select · esc cancel"
        },
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

fn draw_orchestration_preview(
    frame: &mut ratatui::Frame,
    area: Rect,
    state: &TuiState,
    preview: &OrchestrationPreview,
) {
    let resolved = resolve_config(&state.home, std::path::Path::new(&state.thread.cwd));
    let mut lines = vec![
        Line::styled(
            "plan preview · enter run · esc edit",
            Style::default()
                .fg(COLORS.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(format!("goal: {}", preview.goal), Style::default().fg(COLORS.text)),
        Line::styled(
            format!(
                "coordinator: {}/{}",
                preview.coordinator, preview.coordinator_model
            ),
            Style::default().fg(COLORS.muted),
        ),
    ];
    if preview.dirty_source {
        lines.push(Line::styled(
            "warning: source is dirty; workers receive a snapshot",
            Style::default().fg(COLORS.warning),
        ));
    }
    if let Some(summary) = &preview.plan.summary {
        lines.push(Line::styled(
            format!("summary: {summary}"),
            Style::default().fg(COLORS.muted),
        ));
    }
    lines.push(Line::styled(
        format!("workers: {}", preview.plan.tasks.len()),
        Style::default().fg(COLORS.text),
    ));
    for (index, task) in preview.plan.tasks.iter().enumerate().take(8) {
        let provider = ProviderId::parse(task.provider.trim()).unwrap_or(ProviderId::Codex);
        let effective_model = task
            .model
            .clone()
            .unwrap_or_else(|| resolved.for_provider(provider).model.clone());
        lines.push(Line::styled(
            format!(
                "  [{}] {} · {}/{}{}",
                index + 1,
                task.title,
                provider,
                effective_model,
                task.effort
                    .as_deref()
                    .map(|effort| format!("@{effort}"))
                    .unwrap_or_default()
            ),
            Style::default().fg(COLORS.text),
        ));
        if let Some(first) = task.prompt.lines().next() {
            let prompt: String = first.chars().take(120).collect();
            lines.push(Line::styled(
                format!("      {prompt}"),
                Style::default().fg(COLORS.muted),
            ));
        }
    }
    if preview.plan.tasks.len() > 8 {
        lines.push(Line::styled(
            format!("  ... and {} more", preview.plan.tasks.len() - 8),
            Style::default().fg(COLORS.muted),
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLORS.border));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

fn draw_dirty_confirm(frame: &mut ratatui::Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLORS.warning));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = vec![
        Line::styled(
            "source checkout has uncommitted changes",
            Style::default()
                .fg(COLORS.warning)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            "workers receive a snapshot; commits keep only worker-touched files",
            Style::default().fg(COLORS.muted),
        ),
        Line::styled(
            "enter: orchestrate with snapshot · s: run single harness · esc: cancel and edit",
            Style::default().fg(COLORS.muted),
        ),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
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
    let input_width = area.width.saturating_sub(2).max(10) as usize;
    let chars: Vec<char> = state.input.chars().collect();
    let line_count = state.input.split('\n').count().max(1);
    let (cursor_line, cursor_column) = state.cursor_line_column();
    let visible_count = line_count.min(8);
    let first_line = if line_count > visible_count {
        cursor_line
            .saturating_sub(visible_count.saturating_sub(1))
            .min(line_count - visible_count)
    } else {
        0
    };
    let mut layout: Vec<InputRow> = Vec::new();
    let mut line_char_start = 0usize;
    for line_index in 0..line_count {
        let line_end = chars[line_char_start..]
            .iter()
            .position(|character| *character == '\n')
            .map(|offset| line_char_start + offset)
            .unwrap_or(chars.len());
        if line_index >= first_line && line_index < first_line + visible_count {
            let line_len = line_end - line_char_start;
            let scroll = if line_index == cursor_line {
                cursor_column.saturating_sub(input_width.saturating_sub(1))
            } else {
                0
            };
            let start = line_char_start + scroll.min(line_len);
            let end = (start + input_width).min(line_end);
            let mut spans: Vec<Span> = Vec::new();
            if state.input.is_empty() && line_index == 0 {
                let placeholder = if state.thread.session_mode == SessionMode::Orchestrate {
                    "Ask anything; baton will split the work across harnesses"
                } else {
                    "Ask anything..."
                };
                spans.push(Span::styled(
                    placeholder,
                    Style::default().fg(COLORS.muted),
                ));
            } else {
                for absolute in start..end {
                    let mut style = Style::default().fg(COLORS.text);
                    if let Some((selection_start, selection_end)) = state.selection {
                        if selection_end > selection_start
                            && absolute >= selection_start
                            && absolute < selection_end
                        {
                            style = style.fg(Color::Black).bg(COLORS.accent);
                        }
                    }
                    if absolute == state.input_cursor && state.selection.is_none() {
                        style = style.fg(Color::Black).bg(COLORS.accent);
                    }
                    spans.push(Span::styled(chars[absolute].to_string(), style));
                }
                if state.input_cursor == line_end
                    && line_index == cursor_line
                    && state.selection.is_none()
                {
                    spans.push(Span::styled(
                        " ",
                        Style::default().fg(Color::Black).bg(COLORS.accent),
                    ));
                }
            }
            if spans.is_empty() {
                spans.push(Span::raw(""));
            }
            rows.push(Line::from(spans));
            layout.push(InputRow {
                screen_y: area.y + 1 + (input_row_index + (line_index - first_line)) as u16,
                screen_x_start: area.x + 1,
                char_start: line_char_start,
                line_len,
                scroll,
            });
        }
        if line_end >= chars.len() {
            break;
        }
        line_char_start = line_end + 1;
    }
    if let Some(first) = layout.first() {
        state.input_origin.set((first.screen_x_start, first.screen_y));
    }
    state.input_layout.replace(layout);
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
            format!(
                " · {effective_model} · {} · mode {}",
                describe_auth(provider_config),
                effective_permission_mode(state.permission_mode, provider_config.permission_mode)
            ),
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
        let (mode_label, mode_color) = match state.thread.session_mode {
            SessionMode::Orchestrate => ("orchestrate".to_string(), COLORS.secondary),
            SessionMode::Single => (state.thread.current_provider.to_string(), COLORS.muted),
        };
        Line::from(vec![
            Span::styled(
                mode_label,
                Style::default()
                    .fg(mode_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {location} · {}", state.thread.title),
                Style::default().fg(COLORS.muted),
            ),
        ])
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
    fn persist_thread_state(&self, store: &Arc<Mutex<BatonStore>>) {
        let guard = store.lock().unwrap();
        if guard.get_thread(&self.thread.id).ok().flatten().is_none() {
            return;
        }
        let _ = guard.set_thread_permission_mode(&self.thread.id, self.permission_mode);
        let _ = guard.set_thread_goal(
            &self.thread.id,
            self.goal.as_deref(),
            self.goal_paused,
        );
    }

    fn apply_permission_mode(&mut self, store: &Arc<Mutex<BatonStore>>, mode: PermissionMode) {
        self.permission_mode = mode;
        self.thread.permission_mode = mode;
        self.persist_thread_state(store);
    }

    fn banner_lines(&self) -> Vec<Line<'static>> {
        let provider = self.thread.current_provider;
        let key = format!(
            "{}|{:?}|{}|{}|{}|{:?}",
            self.thread.cwd,
            self.thread.session_mode,
            provider,
            self.thread.current_model.as_deref().unwrap_or(""),
            self.thread.reasoning_effort.as_deref().unwrap_or(""),
            self.permission_mode,
        );
        if let Some((cached_key, lines)) = &*self.banner_cache.borrow() {
            if cached_key == &key {
                return lines.clone();
            }
        }
        let resolved = resolve_config(&self.home, std::path::Path::new(&self.thread.cwd));
        let model = self
            .thread
            .current_model
            .clone()
            .unwrap_or_else(|| resolved.for_provider(provider).model.clone());
        let provider_config = resolved.for_provider(provider);
        let home = std::env::var("HOME").unwrap_or_default();
        let cwd = if !home.is_empty() && self.thread.cwd.starts_with(&home) {
            self.thread.cwd.replacen(&home, "~", 1)
        } else {
            self.thread.cwd.clone()
        };
        let mode = match self.thread.session_mode {
            SessionMode::Orchestrate => "orchestrate",
            SessionMode::Single => "single",
        };
        let permission_mode =
            effective_permission_mode(self.permission_mode, provider_config.permission_mode);
        let (permission, permission_color) = match permission_mode {
            PermissionMode::Default => ("default", COLORS.muted),
            PermissionMode::AcceptEdits => ("auto-accept edits", COLORS.secondary),
            PermissionMode::BypassPermissions => ("YOLO mode", COLORS.warning),
            PermissionMode::Plan => ("plan mode", COLORS.secondary),
        };
        let auth = describe_auth(&provider_config);
        let auth_color = if auth.contains("MISSING") {
            COLORS.warning
        } else {
            COLORS.muted
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    ">_ baton",
                    Style::default()
                        .fg(COLORS.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" v{}", env!("CARGO_PKG_VERSION")),
                    Style::default().fg(COLORS.muted),
                ),
                Span::styled(
                    "  one thread · every harness · pass the baton",
                    Style::default().fg(COLORS.muted),
                ),
            ]),
            Line::styled(format!("   {cwd}"), Style::default().fg(COLORS.muted)),
            Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    mode,
                    Style::default()
                        .fg(COLORS.secondary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if self.thread.session_mode == SessionMode::Orchestrate {
                        " · coordinator "
                    } else {
                        " · "
                    },
                    Style::default().fg(COLORS.muted),
                ),
                Span::styled(
                    format!(
                        "{provider}/{model}{}",
                        self.thread
                            .reasoning_effort
                            .as_deref()
                            .map(|effort| format!("@{effort}"))
                            .unwrap_or_default()
                    ),
                    Style::default().fg(COLORS.text),
                ),
            ]),
            Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    format!("permissions: {permission}"),
                    Style::default().fg(permission_color),
                ),
                Span::styled(
                    format!(" · billing: {auth}"),
                    Style::default().fg(auth_color),
                ),
            ]),
        ];
        if let Some(hint) = crate::doctor::missing_provider_hint(provider) {
            lines.push(Line::styled(
                format!("   {hint}"),
                Style::default().fg(COLORS.warning),
            ));
        }
        if provider == ProviderId::Claude
            && provider_config.mode == crate::config::AuthMode::Subscription
            && std::env::var("ANTHROPIC_API_KEY")
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
        {
            lines.push(Line::styled(
                "   warning: ANTHROPIC_API_KEY is set while billing is subscription; auth may be ambiguous",
                Style::default().fg(COLORS.warning),
            ));
        }
        lines.push(Line::raw(""));
        *self.banner_cache.borrow_mut() = Some((key, lines.clone()));
        lines
    }

    fn overlay_is_code_confirm(&self) -> bool {
        matches!(self.overlay, Overlay::CodeConfirm { .. })
    }

    fn set_session_mode(&mut self, store: &Arc<Mutex<BatonStore>>, mode: SessionMode) {
        self.thread.session_mode = mode;
        let guard = store.lock().unwrap();
        let _ = guard.set_thread_session_mode(&self.thread.id, mode);
    }

    fn set_goal_state(
        &mut self,
        store: &Arc<Mutex<BatonStore>>,
        goal: Option<String>,
        paused: bool,
    ) {
        self.goal = goal;
        self.goal_paused = paused;
        self.thread.goal = self.goal.clone();
        self.thread.goal_paused = paused;
        self.persist_thread_state(store);
    }

    fn set_input(&mut self, text: String) {
        self.input_cursor = text.chars().count();
        self.input = text;
        self.selection = None;
        self.selection_anchor = None;
    }

    fn insert_input_char(&mut self, character: char) {
        let byte = char_to_byte(&self.input, self.input_cursor);
        self.input.insert(byte, character);
        self.input_cursor += 1;
    }

    fn backspace_input(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let start = char_to_byte(&self.input, self.input_cursor - 1);
        let end = char_to_byte(&self.input, self.input_cursor);
        self.input.replace_range(start..end, "");
        self.input_cursor -= 1;
    }

    fn delete_input(&mut self) {
        let chars = self.input.chars().count();
        if self.input_cursor >= chars {
            return;
        }
        let start = char_to_byte(&self.input, self.input_cursor);
        let end = char_to_byte(&self.input, self.input_cursor + 1);
        self.input.replace_range(start..end, "");
    }

    fn cursor_line_bounds(&self) -> (usize, usize) {
        let chars: Vec<char> = self.input.chars().collect();
        let cursor = self.input_cursor.min(chars.len());
        let start = chars[..cursor]
            .iter()
            .rposition(|character| *character == '\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let end = chars[cursor..]
            .iter()
            .position(|character| *character == '\n')
            .map(|index| cursor + index)
            .unwrap_or(chars.len());
        (start, end)
    }

    fn cursor_line_column(&self) -> (usize, usize) {
        let chars: Vec<char> = self.input.chars().collect();
        let cursor = self.input_cursor.min(chars.len());
        let line = chars[..cursor].iter().filter(|character| **character == '\n').count();
        let column = chars[..cursor]
            .iter()
            .rposition(|character| *character == '\n')
            .map(|index| cursor - index - 1)
            .unwrap_or(cursor);
        (line, column)
    }

    fn visible_input_lines(&self) -> usize {
        self.input.split('\n').count().clamp(1, 8)
    }

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
        let queued_messages = store
            .lock()
            .unwrap()
            .list_queued_messages(&thread.id)
            .unwrap_or_default();
        TuiState {
            store: store.clone(),
            prompt_history: Vec::new(),
            prompt_history_index: None,
            queued_messages,
            goal: None,
            goal_paused: false,
            custom_commands: Vec::new(),
            home: home.to_path_buf(),
            thread,
            adapter_lifecycles: std::collections::HashMap::new(),
            history,
            input: String::new(),
            input_cursor: 0,
            input_layout: std::cell::RefCell::new(Vec::new()),
            running: false,
            queued_provider: None,
            streaming_text: String::new(),
            tool_lines: Vec::new(),
            overlay: Overlay::None,
            approvals: Vec::new(),
            review: None,
            multi: None,
            notice: None,
            diff_output: None,
            diff_cache: std::cell::RefCell::new(diff_render::DiffCache::default()),
            markdown_cache: std::cell::RefCell::new(markdown::MarkdownCache::default()),
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
            transcript_view: std::cell::RefCell::new(None),
            transcript_selection: std::cell::Cell::new(None),
            transcript_anchor: std::cell::Cell::new(None),
            transcript_total: std::cell::Cell::new(0),
            model_request_id: 0,
            banner_cache: std::cell::RefCell::new(None),
            orchestration_pending: false,
            orchestration_cancel: Arc::new(AtomicBool::new(false)),
            orchestration_cancelling: false,
            orchestration_workers: Vec::new(),
            orchestration_report: None,
            orchestration_scroll: 0,
            orchestration_selected: 0,
            orchestration_integrating: false,
            orchestration_confirm_integrate: false,
            orchestration_integrate_report: None,
            orchestration_shipping: false,
            orchestration_confirm_ship: false,
            orchestration_ship_report: None,
            orchestration_sender: std::sync::mpsc::channel().0,
            native_rewind_sender: std::sync::mpsc::channel().0,
            native_rewind_running: false,
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
        assert_eq!(suggestions.len(), 33);

        let buffer = render_state(&state, 100, 30);
        let text = buffer_lines(&buffer).join("\n");

        assert!(text.contains("11/33"), "position indicator must reflect the selection");
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
    fn approvals_stack_over_pickers_without_losing_them() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.overlay = Overlay::SwitchPicker { selected: 2 };
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let (first_sender, mut first_receiver) = tokio::sync::oneshot::channel();
        let (second_sender, mut second_receiver) = tokio::sync::oneshot::channel();
        let thread_id = state.thread.id.clone();
        apply_turn_event_for_thread(
            &mut state,
            thread_id.clone(),
            TurnEvent::ApprovalRequest {
                id: "first".into(),
                kind: "permissions".into(),
                summary: "run first".into(),
                respond: first_sender,
            },
        );
        apply_turn_event_for_thread(
            &mut state,
            thread_id,
            TurnEvent::ApprovalRequest {
                id: "second".into(),
                kind: "permissions".into(),
                summary: "run second".into(),
                respond: second_sender,
            },
        );
        assert_eq!(state.approvals.len(), 2);

        let key = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char('n')));
        assert_eq!(second_receiver.try_recv().unwrap(), ApprovalDecision::Deny);
        assert_eq!(state.approvals.len(), 1, "the older approval stays queued");
        assert!(
            matches!(state.overlay, Overlay::SwitchPicker { selected: 2 }),
            "answering a request must not destroy the picker underneath"
        );

        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char('y')));
        assert_eq!(first_receiver.try_recv().unwrap(), ApprovalDecision::Accept);
        assert!(state.approvals.is_empty());
        assert!(matches!(state.overlay, Overlay::SwitchPicker { selected: 2 }));
        assert!(state.notice.as_deref().unwrap_or("").contains("accepted once"));
    }

    #[test]
    fn child_approvals_are_cleared_by_the_child_turn_only() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let current_id = state.thread.id.clone();
        let child_id = "child-thread".to_string();
        let (current_sender, _current_receiver) = tokio::sync::oneshot::channel();
        let (child_sender, _child_receiver) = tokio::sync::oneshot::channel();
        apply_turn_event_for_thread(
            &mut state,
            current_id.clone(),
            TurnEvent::ApprovalRequest {
                id: "current".into(),
                kind: "permissions".into(),
                summary: "current request".into(),
                respond: current_sender,
            },
        );
        apply_turn_event_for_thread(
            &mut state,
            child_id.clone(),
            TurnEvent::ApprovalRequest {
                id: "child".into(),
                kind: "permissions".into(),
                summary: "child request".into(),
                respond: child_sender,
            },
        );
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::TurnFinished {
                thread_id: child_id,
                ok: true,
                error: None,
                phase: TurnPhase::Completed,
            },
            &engine_command_sender,
        );
        assert_eq!(state.approvals.len(), 1);
        assert_eq!(state.approvals[0].thread_id, current_id);
        let current_id = state.thread.id.clone();
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::TurnFinished {
                thread_id: current_id,
                ok: true,
                error: None,
                phase: TurnPhase::Completed,
            },
            &engine_command_sender,
        );
        assert!(state.approvals.is_empty());
    }

    #[test]
    fn an_approval_can_be_answered_while_review_is_open() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.history = vec![review_diff_message(&state.thread.id)];
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(&mut state, &store, &engine_command_sender, "/review");
        assert!(state.review.is_some());
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        let thread_id = state.thread.id.clone();
        apply_turn_event_for_thread(
            &mut state,
            thread_id,
            TurnEvent::ApprovalRequest {
                id: "review-approval".into(),
                kind: "permissions".into(),
                summary: "edit file".into(),
                respond: sender,
            },
        );
        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: KeyModifiers::empty(),
                kind: KeyEventKind::Press,
                state: crossterm::event::KeyEventState::empty(),
            },
        );
        assert_eq!(receiver.try_recv().unwrap(), ApprovalDecision::AcceptAlways);
        assert!(state.approvals.is_empty());
        assert!(state.review.is_some(), "review resumes after the approval");
    }

    #[test]
    fn model_picker_window_follows_a_large_live_catalog() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.overlay = Overlay::ModelPicker {
            provider: ProviderId::Codex,
            models: (0..20).map(|index| format!("model-{index:02}")).collect(),
            selected: 15,
            filter: String::new(),
        };
        let text = buffer_lines(&render_state(&state, 100, 30)).join("\n");
        assert!(text.contains("16/20"), "position header: {text}");
        assert!(text.contains("model-15"), "selected model: {text}");
        assert!(!text.contains("model-00"), "first row should be outside the window");
    }

    #[test]
    fn model_picker_can_filter_a_live_catalog() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.overlay = Overlay::ModelPicker {
            provider: ProviderId::Codex,
            models: vec!["alpha-model".into(), "beta-model".into(), "gamma-model".into()],
            selected: 0,
            filter: String::new(),
        };
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let key = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char('b')));
        let text = buffer_lines(&render_state(&state, 80, 30)).join("\n");
        assert!(text.contains("beta-model"), "filtered row: {text}");
        assert!(!text.contains("alpha-model"), "non-match hidden: {text}");
        assert!(text.contains("filter \"b\""), "filter visible: {text}");
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Enter));
        assert!(matches!(state.overlay, Overlay::None));
        assert_eq!(state.thread.current_provider, ProviderId::Codex);
        assert_eq!(state.thread.current_model.as_deref(), Some("beta-model"));
    }

    #[test]
    fn live_model_catalog_updates_the_open_picker_without_losing_selection() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.overlay = Overlay::ModelPicker {
            provider: ProviderId::Codex,
            models: vec!["selected-model".into()],
            selected: 0,
            filter: String::new(),
        };
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let thread_id = state.thread.id.clone();
        let request_id = state.model_request_id;
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::ModelsLoaded {
                thread_id,
                provider: ProviderId::Codex,
                request_id,
                models: vec!["selected-model".into(), "live-model".into()],
                error: None,
            },
            &engine_command_sender,
        );
        match &state.overlay {
            Overlay::ModelPicker {
                models, selected, ..
            } => {
                assert!(models.iter().any(|model| model == "live-model"));
                assert_eq!(models.get(*selected).map(String::as_str), Some("selected-model"));
            }
            _ => panic!("model picker closed"),
        }
        assert!(
            state.notice.as_deref().unwrap_or("").contains("live model"),
            "notice: {:?}",
            state.notice
        );
    }

    #[test]
    fn stale_model_catalog_response_is_ignored() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.overlay = Overlay::ModelPicker {
            provider: ProviderId::Codex,
            models: vec!["current-model".into()],
            selected: 0,
            filter: String::new(),
        };
        state.model_request_id = 2;
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let thread_id = state.thread.id.clone();
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::ModelsLoaded {
                thread_id: thread_id.clone(),
                provider: ProviderId::Codex,
                request_id: 1,
                models: vec!["stale-model".into()],
                error: None,
            },
            &engine_command_sender,
        );
        match &state.overlay {
            Overlay::ModelPicker { models, .. } => {
                assert_eq!(models, &vec!["current-model".to_string()]);
            }
            _ => panic!("model picker closed"),
        }
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::ModelsLoaded {
                thread_id,
                provider: ProviderId::Codex,
                request_id: 2,
                models: vec!["fresh-model".into()],
                error: None,
            },
            &engine_command_sender,
        );
        match &state.overlay {
            Overlay::ModelPicker { models, .. } => {
                assert!(models.iter().any(|model| model == "fresh-model"));
                assert!(!models.iter().any(|model| model == "stale-model"));
            }
            _ => panic!("model picker closed"),
        }
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
    fn transcript_mouse_selection_maps_visible_rows_and_copies() {
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
        let history: Vec<MessageRow> = (1..=30)
            .map(|index| history_message(&thread_id, index))
            .collect();
        let mut state = test_state(home.path(), &store, history);
        let _ = render_state(&state, 100, 30);
        let (area, content_start, content_rows, second_line_len) = {
            let view = state.transcript_view.borrow();
            let view = view.as_ref().expect("transcript viewport recorded");
            assert!(view.total > view.content_rows, "history must overflow the viewport");
            assert!(view.content_rows >= 2);
            let second_len = view
                .plain
                .get(view.content_start + 1)
                .map(|line| line.chars().count())
                .unwrap_or(0);
            (view.area, view.content_start, view.content_rows, second_len)
        };
        let mouse = |kind: crossterm::event::MouseEventKind, column: u16, row: u16| {
            crossterm::event::MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::empty(),
            }
        };
        handle_mouse(
            &mut state,
            mouse(
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                area.x,
                area.y,
            ),
        );
        handle_mouse(
            &mut state,
            mouse(
                crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                area.x + area.width.saturating_sub(1),
                area.y + 1,
            ),
        );
        let (start, end) = state
            .transcript_selection
            .get()
            .expect("drag creates a character range");
        assert_eq!(start, TranscriptPoint { line: content_start, column: 0 });
        assert_eq!(
            end,
            TranscriptPoint {
                line: content_start + 1,
                column: (area.width.saturating_sub(1) as usize).min(second_line_len)
            }
        );

        handle_mouse(
            &mut state,
            mouse(
                crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                area.x + area.width.saturating_sub(1),
                area.y + area.height + 10,
            ),
        );
        let (_, end) = state
            .transcript_selection
            .get()
            .expect("dragging past the bottom keeps a range");
        assert_eq!(
            end.line,
            content_start + content_rows - 1,
            "dragging past the bottom clamps to the last visible line"
        );

        let buffer = render_state(&state, 100, 30);
        let highlighted = (0..100).any(|x| buffer.get(x, area.y).bg == COLORS.selection_bg);
        assert!(highlighted, "selected transcript row must carry the selection background");
    }

    #[test]
    fn transcript_selection_copies_character_ranges() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let state = test_state(home.path(), &store, Vec::new());
        *state.transcript_view.borrow_mut() = Some(TranscriptViewport {
            area: Rect {
                x: 0,
                y: 0,
                width: 20,
                height: 2,
            },
            content_top: 0,
            content_start: 0,
            content_rows: 2,
            total: 2,
            plain: vec!["alpha beta".to_string(), "gamma delta".to_string()],
        });
        state.transcript_selection.set(Some((
            TranscriptPoint {
                line: 0,
                column: 2,
            },
            TranscriptPoint {
                line: 1,
                column: 5,
            },
        )));
        let (text, lines, chars) = transcript_selection_text(&state).expect("selected text");
        assert_eq!(text, "pha beta\ngamma");
        assert_eq!(lines, 2);
        assert_eq!(chars, 14);

        state.transcript_selection.set(Some((
            TranscriptPoint {
                line: 1,
                column: 5,
            },
            TranscriptPoint {
                line: 0,
                column: 2,
            },
        )));
        let (reversed, _, _) = transcript_selection_text(&state).expect("reversed selection");
        assert_eq!(reversed, text, "a reverse drag copies the same range");
    }

    #[test]
    fn transcript_selection_tints_only_the_selected_characters() {
        let mut line = Line::from(vec![Span::raw("abcdef")]);
        tint_line_range(&mut line, 2, 4, COLORS.selection_bg);
        assert_eq!(line_plain_text(&line), "abcdef");
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content.as_ref(), "ab");
        assert_eq!(line.spans[1].content.as_ref(), "cd");
        assert_eq!(line.spans[1].style.bg, Some(COLORS.selection_bg));
        assert_eq!(line.spans[2].content.as_ref(), "ef");
        assert_ne!(line.spans[2].style.bg, Some(COLORS.selection_bg));
    }

    #[test]
    fn transcript_scroll_indicator_is_not_selectable() {
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
        let history: Vec<MessageRow> = (1..=30)
            .map(|index| history_message(&thread_id, index))
            .collect();
        let mut state = test_state(home.path(), &store, history);
        state.scroll_offset = 8;
        let _ = render_state(&state, 100, 30);
        let (area, content_start) = {
            let view = state.transcript_view.borrow();
            let view = view.as_ref().expect("transcript viewport recorded");
            (view.area, view.content_start)
        };
        assert!(transcript_line_at(&state, area.x, area.y).is_none());
        assert_eq!(
            transcript_line_at(&state, area.x, area.y + 1),
            Some(content_start)
        );
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
    fn effort_command_persists_and_clears() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _engine_command_receiver) = tokio::sync::mpsc::unbounded_channel();

        handle_command(&mut state, &store, &engine_command_sender, "/effort High");
        assert_eq!(state.thread.reasoning_effort.as_deref(), Some("high"));
        let persisted = store.lock().unwrap().get_thread(&state.thread.id).unwrap().unwrap();
        assert_eq!(persisted.reasoning_effort.as_deref(), Some("high"));
        let text = buffer_lines(&render_state(&state, 100, 30)).join("\n");
        assert!(text.contains("@high"), "banner must show the effort immediately: {text}");

        handle_command(&mut state, &store, &engine_command_sender, "/effort default");
        assert_eq!(state.thread.reasoning_effort, None);
        let persisted = store.lock().unwrap().get_thread(&state.thread.id).unwrap().unwrap();
        assert_eq!(persisted.reasoning_effort, None);
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

        state.tool_lines.push(ToolLine { id: None, label: "shell cargo test".into(), status: ToolStatus::Running });
        state.streaming_text = "partial answer".into();
        let lines = buffer_lines(&render_state(&state, 100, 30));
        let user_row = lines.iter().position(|line| line.contains("message 01")).expect("user message rendered");
        let tool_row = lines.iter().position(|line| line.contains("cargo test")).expect("tool line in transcript");
        let stream_row = lines.iter().position(|line| line.contains("partial answer")).expect("streaming text in transcript");
        assert!(user_row < tool_row && tool_row < stream_row, "tools and streaming follow the user message in order");
        assert!(!lines.iter().any(|line| line.contains("Working [")), "the status bar no longer shows a Working spinner");
    }

    #[test]
    fn queued_turn_renders_queued_status_instead_of_thinking() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let thread_id = state.thread.id.clone();
        let history = vec![history_message(&thread_id, 1)];
        state.history = history;
        let (command_sender, _command_receiver) = tokio::sync::mpsc::unbounded_channel();

        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::TurnQueued {
                thread_id: thread_id.clone(),
                provider: ProviderId::Codex,
            },
            &command_sender,
        );
        assert_eq!(state.queued_provider, Some(ProviderId::Codex));
        assert!(state.running);
        assert!(state
            .notice
            .as_deref()
            .unwrap_or("")
            .contains("queued for codex"));
        let lines = buffer_lines(&render_state(&state, 100, 30));
        assert!(lines.iter().any(|line| line.contains("Queued for codex")));
        assert!(!lines.iter().any(|line| line.contains("Thinking")));

        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::TurnStarted { thread_id },
            &command_sender,
        );
        assert_eq!(state.queued_provider, None);
    }

    #[test]
    fn assistant_messages_render_basic_markdown() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.history = vec![MessageRow {
            id: 1,
            thread_id: state.thread.id.clone(),
            turn: 1,
            role: "assistant".into(),
            provider: Some("codex".into()),
            model: Some("m".into()),
            text: "# Title\n\n- **one** with `code`\n\n```rust\nlet x = 1;\n```".into(),
            created_at: String::new(),
        }];
        let buffer = render_state(&state, 80, 30);
        let lines = buffer_lines(&buffer);
        let text = lines.join("\n");
        assert!(text.contains("Title"), "heading stripped: {text}");
        assert!(!text.contains('#' ), "heading marker removed: {text}");
        assert!(text.contains("• one with code"), "inline markers stripped: {text}");
        assert!(text.contains("let x = 1;"), "code body visible: {text}");
        assert!(!text.contains("```"), "fence markers removed: {text}");
        let code_row = lines
            .iter()
            .position(|line| line.contains("let x = 1;"))
            .expect("code row");
        assert!(
            (0..buffer.area.width).any(|x| buffer.get(x, code_row as u16).bg == COLORS.code_bg),
            "code row carries a tinted background"
        );
    }

    #[test]
    fn live_tool_detail_updates_the_existing_call_id_row() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        apply_turn_event(
            &mut state,
            TurnEvent::ToolStarted {
                id: Some("call-1".into()),
                tool: "read".into(),
                detail: None,
            },
        );
        apply_turn_event(
            &mut state,
            TurnEvent::ToolStarted {
                id: Some("call-1".into()),
                tool: "read".into(),
                detail: Some("/work".into()),
            },
        );
        assert_eq!(state.tool_lines.len(), 1);
        assert_eq!(state.tool_lines[0].label, "read /work");
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
    fn worker_failure_without_a_commit_marks_the_task_failed() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let worker_id = state.thread.id.clone();
        {
            let guard = store.lock().unwrap();
            guard
                .create_orchestration_run(
                    "run-failure",
                    "goal",
                    ProviderId::Codex,
                    "model",
                    None,
                    "{}",
                    false,
                )
                .unwrap();
            guard
                .add_orchestration_task(
                    "task-failure",
                    "run-failure",
                    1,
                    "worker task",
                    ProviderId::Codex,
                    "model",
                    "prompt",
                    Some("/tmp/worktree"),
                    Some("baton/run-failure/worker-task"),
                    None,
                    Some(&worker_id),
                    "completed",
                    None,
                    None,
                    "now",
                    Some("now"),
                    None,
                    None,
                    None,
                )
                .unwrap();
            guard
                .finish_orchestration_run("run-failure", "completed")
                .unwrap();
        }
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::TurnFinished {
                thread_id: worker_id.clone(),
                ok: false,
                error: Some("boom".to_string()),
                phase: TurnPhase::Failed,
            },
            &engine_command_sender,
        );
        let task = store
            .lock()
            .unwrap()
            .orchestration_task_for_thread(&worker_id)
            .unwrap()
            .unwrap();
        assert_eq!(task.status, "failed");
        assert_eq!(task.error.as_deref(), Some("boom"));
        assert_eq!(
            store.lock().unwrap().list_orchestration_runs(10).unwrap()[0].status,
            "failed"
        );

        let worker2 = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "worker with commit".to_string(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        {
            let guard = store.lock().unwrap();
            guard
                .create_orchestration_run(
                    "run-success",
                    "goal",
                    ProviderId::Codex,
                    "model",
                    None,
                    "{}",
                    false,
                )
                .unwrap();
            guard
                .add_orchestration_task(
                    "task-success",
                    "run-success",
                    1,
                    "committed worker",
                    ProviderId::Codex,
                    "model",
                    "prompt",
                    Some("/tmp/worktree"),
                    Some("baton/run-success/worker-task"),
                    Some("old-commit"),
                    Some(&worker2.id),
                    "completed",
                    Some("old answer"),
                    None,
                    "now",
                    Some("now"),
                    None,
                    None,
                    None,
                )
                .unwrap();
            guard
                .mark_orchestration_task_merged("task-success", "merged")
                .unwrap();
            guard
                .mark_orchestration_task_pr("task-success", "https://example.test/pr/2")
                .unwrap();
            guard
                .finish_orchestration_run("run-success", "completed")
                .unwrap();
        }
        record_worker_followup(
            &mut state,
            &store,
            &worker2.id,
            false,
            Some("follow-up failed"),
            TurnPhase::Failed,
        );
        let task = store
            .lock()
            .unwrap()
            .orchestration_task_for_thread(&worker2.id)
            .unwrap()
            .unwrap();
        assert_eq!(task.status, "completed", "a prior commit is still valid");
        assert_eq!(task.commit_sha.as_deref(), Some("old-commit"));
        assert!(task.error.is_none());
        assert!(task.merged_at.is_some());
        assert!(task.pr_url.is_some());
    }

    #[test]
    fn worker_followup_commits_and_updates_the_orchestration_task() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str], cwd: &std::path::Path| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&output.stderr));
        };
        git(&["init", "-q"], repo.path());
        git(&["config", "user.email", "test@test"], repo.path());
        git(&["config", "user.name", "test"], repo.path());
        std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
        git(&["add", "."], repo.path());
        git(&["commit", "-q", "-m", "init"], repo.path());
        let repo_root = crate::worktree::repo_root(repo.path()).unwrap();

        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let worktree = crate::worktree::create(
            home.path(),
            &repo_root,
            "run-followup",
            "worker task",
        )
        .unwrap();
        let worker = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "worker".to_string(),
                cwd: worktree.path.to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: Some("model".to_string()),
            })
            .unwrap();
        {
            let guard = store.lock().unwrap();
            guard
                .create_orchestration_run(
                    "run-followup",
                    "goal",
                    ProviderId::Codex,
                    "model",
                    None,
                    "{}",
                    false,
                )
                .unwrap();
            guard
                .add_orchestration_task(
                    "task-followup",
                    "run-followup",
                    1,
                    "worker task",
                    ProviderId::Codex,
                    "model",
                    "prompt",
                    Some(&worktree.path.to_string_lossy()),
                    Some(&worktree.branch),
                    Some("old-commit"),
                    Some(&worker.id),
                    "needs_input",
                    Some("BATON_QUESTION: old answer"),
                    None,
                    "now",
                    Some("now"),
                    None,
                    None,
                    None,
                )
                .unwrap();
            guard
                .mark_orchestration_task_merged("task-followup", "merged")
                .unwrap();
            guard
                .mark_orchestration_task_pr("task-followup", "https://example.test/pr/1")
                .unwrap();
            guard
                .finish_orchestration_run("run-followup", "needs_input")
                .unwrap();
            guard
                .add_message(
                    &worker.id,
                    1,
                    "assistant",
                    Some("codex"),
                    None,
                    "updated answer",
                )
                .unwrap();
        }
        std::fs::write(worktree.path.join("followup.txt"), "new work\n").unwrap();

        let mut state = test_state(home.path(), &store, Vec::new());
        state.thread = worker.clone();
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::TurnFinished {
                thread_id: worker.id.clone(),
                ok: true,
                error: None,
                phase: TurnPhase::Completed,
            },
            &engine_command_sender,
        );

        let task = store
            .lock()
            .unwrap()
            .orchestration_task_for_thread(&worker.id)
            .unwrap()
            .unwrap();
        assert_eq!(task.status, "completed");
        assert_eq!(task.text.as_deref(), Some("updated answer"));
        assert_eq!(task.commit_sha.as_deref().map(str::len), Some(40));
        assert!(task.merged_at.is_none(), "new work clears the merge marker");
        assert!(task.pr_url.is_none(), "new work must be shipped again");
        assert_eq!(
            store
                .lock()
                .unwrap()
                .find_orchestration_runs("run-followup")
                .unwrap()
                .into_iter()
                .find(|run| run.id == "run-followup")
                .unwrap()
                .status,
            "completed",
            "answering the question resolves the run"
        );
        assert!(
            state.notice.as_deref().unwrap_or("").contains("worker follow-up recorded"),
            "notice: {:?}",
            state.notice
        );
        assert_eq!(
            store.lock().unwrap().list_orchestration_runs(10).unwrap()[0].status,
            "completed"
        );

        {
            let guard = store.lock().unwrap();
            guard
                .mark_orchestration_task_merged("task-followup", "merged")
                .unwrap();
            guard
                .mark_orchestration_task_pr("task-followup", "https://example.test/pr/1")
                .unwrap();
            guard
                .add_message(
                    &worker.id,
                    2,
                    "assistant",
                    Some("codex"),
                    None,
                    "BATON_QUESTION: choose the follow-up path?",
                )
                .unwrap();
        }
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::TurnFinished {
                thread_id: worker.id.clone(),
                ok: true,
                error: None,
                phase: TurnPhase::Completed,
            },
            &engine_command_sender,
        );
        let task = store
            .lock()
            .unwrap()
            .orchestration_task_for_thread(&worker.id)
            .unwrap()
            .unwrap();
        assert_eq!(task.status, "needs_input", "a second question keeps the gate");
        assert_eq!(
            task.text.as_deref(),
            Some("BATON_QUESTION: choose the follow-up path?")
        );
        assert!(
            task.merged_at.is_some(),
            "a question without new work preserves the merge marker"
        );
        assert!(
            task.pr_url.is_some(),
            "a question without new work preserves the PR marker"
        );
        assert_eq!(
            store.lock().unwrap().list_orchestration_runs(10).unwrap()[0].status,
            "needs_input"
        );
        assert!(
            state.notice.as_deref().unwrap_or("").contains("worker question recorded"),
            "notice: {:?}",
            state.notice
        );
    }

    #[test]
    fn multi_args_parse_worktree_flag_and_dedupe_providers() {
        let parsed = parse_multi_args(&[
            "--worktrees".to_string(),
            "codex".to_string(),
            "opencode".to_string(),
            "codex".to_string(),
            "compare".to_string(),
            "notes".to_string(),
        ])
        .unwrap();
        assert!(parsed.worktrees);
        assert_eq!(parsed.providers, vec![ProviderId::Codex, ProviderId::OpenCode]);
        assert_eq!(parsed.prompt, "compare notes");

        let live = parse_multi_args(&[
            "codex".to_string(),
            "claude".to_string(),
            "compare".to_string(),
        ])
        .unwrap();
        assert!(!live.worktrees);
        assert_eq!(live.providers, vec![ProviderId::Codex, ProviderId::Claude]);

        assert!(parse_multi_args(&[]).is_err());
        assert!(parse_multi_args(&["--worktrees".to_string()]).is_err());
        assert!(parse_multi_args(&["codex".to_string()]).is_err());
    }

    #[test]
    fn multi_worktree_plan_has_one_task_per_provider() {
        let plan = multi_worktree_plan(
            &[ProviderId::Codex, ProviderId::Claude],
            "compare the API",
        );
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[0].provider, "codex");
        assert_eq!(plan.tasks[0].prompt, "compare the API");
        assert_eq!(plan.tasks[1].provider, "claude");
        assert_eq!(
            plan.summary.as_deref(),
            Some("multi fan-out across isolated worktrees")
        );
    }

    #[test]
    fn multi_worktree_plan_file_round_trips() {
        let path = write_multi_worktree_plan(
            &[ProviderId::Codex, ProviderId::OpenCode],
            "compare the API",
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let plan = crate::orchestrate::parse_plan(&text, 4).unwrap();
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[0].provider, "codex");
        assert_eq!(plan.tasks[1].provider, "opencode");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn multi_worktrees_requires_git_and_uses_the_dirty_guard() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(
            &mut state,
            &store,
            &engine_command_sender,
            "/multi --worktrees codex claude compare notes",
        );
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("needs a git repository"),
            "notice: {:?}",
            state.notice
        );
        assert!(!state.orchestration_pending);
        assert!(state.multi.is_none());

        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(output.status.success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(repo.path().join("tracked.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::write(repo.path().join("tracked.txt"), "dirty\n").unwrap();
        state.thread.cwd = repo.path().to_string_lossy().to_string();
        handle_command(
            &mut state,
            &store,
            &engine_command_sender,
            "/multi --worktrees codex claude compare notes",
        );
        assert!(
            matches!(state.overlay, Overlay::DirtyConfirm { .. }),
            "dirty source must ask before multi worktrees"
        );
        assert!(!state.orchestration_pending);
        assert!(state.multi.is_none());
    }

    #[test]
    fn multi_command_persists_parallel_threads() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, mut engine_command_receiver) = tokio::sync::mpsc::unbounded_channel();
        state.apply_permission_mode(&store, PermissionMode::Plan);

        handle_command(&mut state, &store, &engine_command_sender, "/multi codex opencode compare notes");

        let mut submitted = Vec::new();
        while let Ok(command) = engine_command_receiver.try_recv() {
            if let EngineCommand::Submit { thread_id, text, .. } = command {
                submitted.push((thread_id, text));
            }
        }
        assert_eq!(submitted.len(), 2, "one submit per provider");
        assert!(submitted.iter().all(|(_, text)| text == "compare notes"));
        let multi = state.multi.as_ref().expect("multi view opens");
        assert_eq!(multi.children.len(), 2);
        assert!(multi.children.iter().all(|child| child.status == MultiStatus::Running));
        for (thread_id, _) in &submitted {
            let thread = store.lock().unwrap().get_thread(thread_id).unwrap();
            assert!(thread.is_some(), "parallel thread {thread_id} must be persisted before submit");
            assert_eq!(
                thread.unwrap().permission_mode,
                PermissionMode::Plan,
                "parallel threads inherit the active mode"
            );
        }
    }

    #[test]
    fn copy_command_selects_the_requested_assistant_reply() {
        std::env::set_var("BATON_CLIPBOARD_COMMAND", "cat");
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let thread_id = state.thread.id.clone();
        state.history = vec![
            MessageRow {
                id: 1,
                thread_id: thread_id.clone(),
                turn: 1,
                role: "assistant".into(),
                provider: None,
                model: None,
                text: "older reply".into(),
                created_at: String::new(),
            },
            MessageRow {
                id: 2,
                thread_id,
                turn: 2,
                role: "assistant".into(),
                provider: None,
                model: None,
                text: "newer reply".into(),
                created_at: String::new(),
            },
        ];
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(&mut state, &store, &engine_command_sender, "/copy 2");
        assert!(state.notice.as_deref().unwrap_or("").contains("copied assistant reply 2"));
        handle_command(&mut state, &store, &engine_command_sender, "/copy 3");
        assert!(state.notice.as_deref().unwrap_or("").contains("no assistant reply at position 3"));
        std::env::remove_var("BATON_CLIPBOARD_COMMAND");
    }

    #[test]
    fn multi_view_tracks_children_and_opens_the_selected_thread() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let child = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "parallel child".into(),
                cwd: state.thread.cwd.clone(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        state.multi = Some(MultiView {
            prompt: "compare notes".into(),
            selected: 0,
            children: vec![MultiChild {
                thread_id: child.id.clone(),
                provider: ProviderId::Codex,
                status: MultiStatus::Running,
                reply: String::new(),
                error: None,
            }],
        });
        apply_turn_event_for_thread(
            &mut state,
            child.id.clone(),
            TurnEvent::TextDelta {
                text: "partial".into(),
            },
        );
        assert_eq!(state.multi.as_ref().unwrap().children[0].reply, "partial");
        store
            .lock()
            .unwrap()
            .add_message(&child.id, 1, "assistant", Some("codex"), None, "final answer")
            .unwrap();
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::TurnFinished {
                thread_id: child.id.clone(),
                ok: true,
                error: None,
                phase: TurnPhase::Completed,
            },
            &engine_command_sender,
        );
        let multi = state.multi.as_ref().unwrap();
        assert_eq!(multi.children[0].status, MultiStatus::Done);
        assert_eq!(multi.children[0].reply, "final answer");
        let rendered = buffer_lines(&render_state(&state, 100, 30)).join("\n");
        assert!(rendered.contains("multi"), "merged view: {rendered}");
        assert!(rendered.contains("codex"), "provider row: {rendered}");
        assert!(rendered.contains("final answer"), "reply preview: {rendered}");

        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::empty(),
                kind: KeyEventKind::Press,
                state: crossterm::event::KeyEventState::empty(),
            },
        );
        assert!(state.multi.is_none());
        assert_eq!(state.thread.id, child.id);
        assert!(state.history.iter().any(|message| message.text == "final answer"));
    }

    #[test]
    fn shift_enter_inserts_a_newline_instead_of_submitting() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, mut engine_command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let key = |code: KeyCode, modifiers: KeyModifiers| KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };

        state.set_input("first".into());
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Enter, KeyModifiers::SHIFT));
        state.set_input(format!("{}second", state.input));
        assert_eq!(state.input, "first\nsecond");
        assert!(
            engine_command_receiver.try_recv().is_err(),
            "shift+enter must not submit"
        );

        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(state.input, "", "plain enter still submits");
        let command = engine_command_receiver.try_recv().expect("submit command");
        match command {
            EngineCommand::Submit { text, .. } => assert_eq!(text, "first\nsecond"),
            _ => panic!("expected a submit command"),
        }
    }

    #[test]
    fn composer_cursor_moves_and_edits_mid_string() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };

        state.set_input("abc".into());
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Left));
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char('X')));
        assert_eq!(state.input, "abXc");
        assert_eq!(state.input_cursor, 3);

        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Home));
        assert_eq!(state.input_cursor, 0);
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Delete));
        assert_eq!(state.input, "bXc");
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::End));
        assert_eq!(state.input_cursor, 3);
    }

    #[test]
    fn multiline_composer_renders_every_line_and_maps_mouse_rows() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.set_input("line one\nline two".into());

        let buffer = render_state(&state, 100, 30);
        let text = buffer_lines(&buffer).join("\n");
        assert!(text.contains("line one"));
        assert!(text.contains("line two"));

        let layout = state.input_layout.borrow().clone();
        assert_eq!(layout.len(), 2, "both logical lines register a mouse row");
        let second = &layout[1];
        let mouse = |kind: crossterm::event::MouseEventKind, column: u16, row: u16| {
            crossterm::event::MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::empty(),
            }
        };
        handle_mouse(
            &mut state,
            mouse(
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                second.screen_x_start,
                second.screen_y,
            ),
        );
        handle_mouse(
            &mut state,
            mouse(
                crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                second.screen_x_start + 4,
                second.screen_y,
            ),
        );
        assert_eq!(
            state.selection,
            Some((9, 13)),
            "the second line maps to the absolute char range"
        );
    }

    fn review_diff_message(thread_id: &str) -> MessageRow {
        MessageRow {
            id: 90,
            thread_id: thread_id.to_string(),
            turn: 1,
            role: "diff".to_string(),
            provider: None,
            model: None,
            text: "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n context\n-old line\n+new line\n+another line\n".to_string(),
            created_at: String::new(),
        }
    }

    #[test]
    fn review_mode_sends_line_anchored_notes() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.history = vec![review_diff_message(&state.thread.id)];
        let (engine_command_sender, mut engine_command_receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(&mut state, &store, &engine_command_sender, "/review");
        let review = state.review.as_ref().expect("review opens");
        assert_eq!(review.rows.len(), 4, "review keeps context and changed rows");
        assert_eq!(review.rows.iter().filter(|row| row.added || row.removed).count(), 3);
        assert_eq!(review.rows[review.selected].text, "old line", "selection starts on the first change");

        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Down));
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char('n')));
        for character in "fix this".chars() {
            handle_key(
                &mut state,
                &store,
                &engine_command_sender,
                key(KeyCode::Char(character)),
            );
        }
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Enter));
        let review = state.review.as_ref().expect("review stays open");
        assert_eq!(review.notes.len(), 1);
        assert_eq!(review.notes[0].path, "src/lib.rs");
        assert_eq!(review.notes[0].line, 2);
        assert!(review.notes[0].added);
        assert_eq!(review.notes[0].body, "fix this");

        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char('s')));
        assert!(state.review.is_none(), "sending closes review");
        match engine_command_receiver.try_recv().expect("submit") {
            EngineCommand::Submit { text, .. } => {
                assert!(text.contains("File: src/lib.rs"));
                assert!(text.contains("Line: 2"));
                assert!(text.contains("Change: added"));
                assert!(text.contains("User comment: \"fix this\""));
            }
            _ => panic!("expected submit"),
        }
        let fingerprint = diff_render::fingerprint(&review_diff_message(&state.thread.id).text);
        assert!(
            store
                .lock()
                .unwrap()
                .list_unsent_review_notes(&state.thread.id, 1, &fingerprint)
                .unwrap()
                .is_empty(),
            "sending marks notes delivered so reopen cannot resend them"
        );
    }

    #[test]
    fn review_notes_stay_anchored_to_changed_lines() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.history = vec![review_diff_message(&state.thread.id)];
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(&mut state, &store, &engine_command_sender, "/review");
        let key = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Up));
        assert!(!state.review.as_ref().unwrap().rows[state.review.as_ref().unwrap().selected].added);
        assert!(!state.review.as_ref().unwrap().rows[state.review.as_ref().unwrap().selected].removed);
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char('n')));
        assert!(state.review.as_ref().unwrap().note_input.is_none());
        assert!(state.notice.as_deref().unwrap_or("").contains("added or removed"));
    }

    #[test]
    fn review_file_navigation_jumps_between_paths() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.history = vec![MessageRow {
            id: 91,
            thread_id: state.thread.id.clone(),
            turn: 1,
            role: "diff".into(),
            provider: None,
            model: None,
            text: "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old a\n+new a\ndiff --git a/src/b.rs b/src/b.rs\n--- a/src/b.rs\n+++ b/src/b.rs\n@@ -1 +1 @@\n-old b\n+new b\n".into(),
            created_at: String::new(),
        }];
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(&mut state, &store, &engine_command_sender, "/review");
        let review = state.review.as_ref().unwrap();
        assert_eq!(review.rows[review.selected].path, "src/a.rs");
        let key = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char(']')));
        assert_eq!(state.review.as_ref().unwrap().rows[state.review.as_ref().unwrap().selected].path, "src/b.rs");
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char('[')));
        assert_eq!(state.review.as_ref().unwrap().rows[state.review.as_ref().unwrap().selected].path, "src/a.rs");
    }

    #[test]
    fn review_notes_restore_on_reopen_and_delete_persists() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let diff = review_diff_message(&state.thread.id);
        state.history = vec![diff.clone()];
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };

        handle_command(&mut state, &store, &engine_command_sender, "/review");
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char('n')));
        for character in "persist me".chars() {
            handle_key(
                &mut state,
                &store,
                &engine_command_sender,
                key(KeyCode::Char(character)),
            );
        }
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Enter));
        let fingerprint = diff_render::fingerprint(&diff.text);
        assert_eq!(
            store
                .lock()
                .unwrap()
                .list_unsent_review_notes(&state.thread.id, 1, &fingerprint)
                .unwrap()
                .len(),
            1,
            "adding a note persists it before sending"
        );

        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Esc));
        handle_command(&mut state, &store, &engine_command_sender, "/review");
        let review = state.review.as_ref().expect("review reopens");
        assert_eq!(review.notes.len(), 1);
        assert_eq!(review.notes[0].body, "persist me");

        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Char('d')));
        assert!(state.review.as_ref().unwrap().notes.is_empty());
        assert!(
            store
                .lock()
                .unwrap()
                .list_unsent_review_notes(&state.thread.id, 1, &fingerprint)
                .unwrap()
                .is_empty(),
            "deleting a note removes its persisted row"
        );

        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Esc));
        handle_command(&mut state, &store, &engine_command_sender, "/review");
        assert!(
            state.review.as_ref().unwrap().notes.is_empty(),
            "deleted notes do not come back on the next reopen"
        );
    }

    #[test]
    fn review_notes_steer_when_a_turn_is_running() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.history = vec![review_diff_message(&state.thread.id)];
        let (engine_command_sender, mut engine_command_receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(&mut state, &store, &engine_command_sender, "/review");
        state.running = true;
        let thread_id = state.thread.id.clone();
        let review = state.review.as_mut().unwrap();
        let id = store
            .lock()
            .unwrap()
            .add_review_note(
                &thread_id,
                review.turn,
                "src/lib.rs",
                3,
                true,
                "tighten this",
                &review.fingerprint,
            )
            .unwrap();
        review.notes.push(ReviewNote {
            id,
            path: "src/lib.rs".into(),
            line: 3,
            added: true,
            body: "tighten this".into(),
        });
        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            KeyEvent {
                code: KeyCode::Char('s'),
                modifiers: KeyModifiers::empty(),
                kind: KeyEventKind::Press,
                state: crossterm::event::KeyEventState::empty(),
            },
        );
        match engine_command_receiver.try_recv().expect("steer") {
            EngineCommand::Steer { text, .. } => assert!(text.contains("Line: 3")),
            _ => panic!("expected steer"),
        }
    }

    #[test]
    fn review_overlay_renders_rows_and_note_hint() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.history = vec![review_diff_message(&state.thread.id)];
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(&mut state, &store, &engine_command_sender, "/review");
        let text = buffer_lines(&render_state(&state, 100, 30)).join("\n");
        assert!(text.contains("review turn 1"), "header: {text}");
        assert!(text.contains("src/lib.rs:2"), "anchored row: {text}");
        assert!(text.contains("new line"));
        assert!(text.contains("press n on a changed line"));
    }

    #[test]
    fn a_message_while_running_steers_instead_of_queueing() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, mut engine_command_receiver) = tokio::sync::mpsc::unbounded_channel();
        state.running = true;
        state.set_input("change direction".into());
        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::empty(),
                kind: KeyEventKind::Press,
                state: crossterm::event::KeyEventState::empty(),
            },
        );
        match engine_command_receiver.try_recv().expect("steer command") {
            EngineCommand::Steer { thread_id, text } => {
                assert_eq!(thread_id, state.thread.id);
                assert_eq!(text, "change direction");
            }
            _ => panic!("expected a steer command"),
        }
        assert!(state.queued_messages.is_empty(), "a steer is not a queued turn");
    }

    #[test]
    fn rejected_steering_falls_back_to_the_queue() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let thread_id = state.thread.id.clone();
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::SteerRejected {
                thread_id,
                text: "follow up".into(),
            },
            &engine_command_sender,
        );
        assert_eq!(state.queued_messages, vec!["follow up".to_string()]);
        assert_eq!(
            store.lock().unwrap().list_queued_messages(&state.thread.id).unwrap(),
            vec!["follow up".to_string()],
            "a rejected steer is durable"
        );
        assert!(state.notice.as_deref().unwrap_or("").contains("queued"));
    }

    #[test]
    fn a_second_submit_for_the_same_thread_falls_back_to_the_queue() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let thread_id = state.thread.id.clone();
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::TurnRejected {
                thread_id,
                text: "second".into(),
                reason: "a turn is already running for this thread".into(),
            },
            &engine_command_sender,
        );
        assert_eq!(state.queued_messages, vec!["second".to_string()]);
        assert_eq!(
            store.lock().unwrap().list_queued_messages(&state.thread.id).unwrap(),
            vec!["second".to_string()]
        );
        assert!(state.notice.as_deref().unwrap_or("").contains("already running"));
    }

    #[test]
    fn status_command_summarizes_phase_goal_and_queue() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        state.apply_permission_mode(&store, PermissionMode::Plan);
        state.set_goal_state(&store, Some("ship".into()), true);
        enqueue_message(&mut state, &store, "later".into());
        let lifecycle = AdapterLifecycle::new();
        lifecycle.starting("thread-1");
        lifecycle.turn_running("thread-1");
        state.adapter_lifecycles.insert(ProviderId::Codex, lifecycle);
        {
            let guard = store.lock().unwrap();
            guard.start_turn_record(&state.thread.id, 1, ProviderId::Codex, Some("m")).unwrap();
            guard.finish_turn_record(&state.thread.id, 1, TurnPhase::Failed, Some("boom")).unwrap();
        }
        handle_command(&mut state, &store, &engine_command_sender, "/status");
        let notice = state.notice.clone().unwrap_or_default();
        assert!(notice.contains("provider: claude"));
        assert!(notice.contains("mode: plan"));
        assert!(notice.contains("goal: ship (paused)"));
        assert!(notice.contains("turns: 1"));
        assert!(notice.contains("turn: 1 failed"));
        assert!(notice.contains("error: boom"));
        assert!(notice.contains("queue: 1 waiting"));
        assert!(
            notice.contains("adapters: codex=turn-running(thread-1)"),
            "adapter lifecycle missing from status: {notice}"
        );
    }

    #[test]
    fn status_command_warns_about_open_turns() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        store
            .lock()
            .unwrap()
            .start_turn_record(&state.thread.id, 1, ProviderId::Codex, Some("m"))
            .unwrap();
        handle_command(&mut state, &store, &engine_command_sender, "/status");
        let notice = state.notice.clone().unwrap_or_default();
        assert!(notice.contains("1 running"));
        assert!(notice.contains("may be stale"));
    }

    #[test]
    fn turns_command_lists_durable_phases() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        {
            let guard = store.lock().unwrap();
            guard.start_turn_record(&state.thread.id, 1, ProviderId::Codex, Some("m")).unwrap();
            guard.finish_turn_record(&state.thread.id, 1, TurnPhase::Completed, None).unwrap();
            guard.start_turn_record(&state.thread.id, 2, ProviderId::Codex, Some("m")).unwrap();
        }
        handle_command(&mut state, &store, &engine_command_sender, "/turns");
        let notice = state.notice.clone().unwrap_or_default();
        assert!(notice.contains("turns: 2"));
        assert!(notice.contains("2 running"));
        assert!(notice.contains("1 completed"));
        assert!(notice.contains("may be stale"));
    }

    #[test]
    fn queue_command_lists_and_clears_persisted_messages() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        enqueue_message(&mut state, &store, "first".into());
        enqueue_message(&mut state, &store, "second line\nrest".into());

        handle_command(&mut state, &store, &engine_command_sender, "/queue");
        let notice = state.notice.clone().unwrap_or_default();
        assert!(notice.contains("queue: 2 waiting"));
        assert!(notice.contains("1. first"));
        assert!(notice.contains("2. second line"));
        assert!(!notice.contains("rest"), "previews stay on one line");

        handle_command(&mut state, &store, &engine_command_sender, "/queue clear");
        assert!(state.queued_messages.is_empty());
        assert!(store
            .lock()
            .unwrap()
            .list_queued_messages(&state.thread.id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_successful_turn_pops_the_persisted_queue() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, mut engine_command_receiver) = tokio::sync::mpsc::unbounded_channel();
        enqueue_message(&mut state, &store, "follow up".into());
        assert_eq!(
            store.lock().unwrap().list_queued_messages(&state.thread.id).unwrap(),
            vec!["follow up".to_string()]
        );

        let thread_id = state.thread.id.clone();
        apply_engine_event(
            &mut state,
            &store,
            EngineEvent::TurnFinished {
                thread_id,
                ok: true,
                error: None,
                phase: TurnPhase::Completed,
            },
            &engine_command_sender,
        );

        assert!(state.queued_messages.is_empty());
        assert!(store
            .lock()
            .unwrap()
            .list_queued_messages(&state.thread.id)
            .unwrap()
            .is_empty());
        match engine_command_receiver.try_recv().expect("queued message is sent") {
            EngineCommand::Submit { text, .. } => assert_eq!(text, "follow up"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn permission_mode_and_goal_persist_and_paused_goals_are_not_injected() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, mut engine_command_receiver) = tokio::sync::mpsc::unbounded_channel();

        handle_command(&mut state, &store, &engine_command_sender, "/mode plan");
        let saved = store.lock().unwrap().get_thread(&state.thread.id).unwrap().unwrap();
        assert_eq!(saved.permission_mode, PermissionMode::Plan, "mode persists on the thread");

        handle_command(&mut state, &store, &engine_command_sender, "/goal ship the fix");
        let saved = store.lock().unwrap().get_thread(&state.thread.id).unwrap().unwrap();
        assert_eq!(saved.goal.as_deref(), Some("ship the fix"));
        assert!(!saved.goal_paused);

        handle_command(&mut state, &store, &engine_command_sender, "/goal pause");
        let saved = store.lock().unwrap().get_thread(&state.thread.id).unwrap().unwrap();
        assert!(saved.goal_paused, "pause is durable, not just a notice");
        submit(&mut state, &store, &engine_command_sender, "while paused".into());
        match engine_command_receiver.try_recv().expect("submit") {
            EngineCommand::Submit { text, permission_mode, .. } => {
                assert_eq!(text, "while paused", "a paused goal must not be injected");
                assert_eq!(permission_mode, PermissionMode::Plan);
            }
            _ => panic!("expected submit"),
        }

        handle_command(&mut state, &store, &engine_command_sender, "/goal resume");
        submit(&mut state, &store, &engine_command_sender, "after resume".into());
        match engine_command_receiver.try_recv().expect("submit") {
            EngineCommand::Submit { text, .. } => {
                assert_eq!(text, "[goal: ship the fix]\nafter resume");
            }
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn opening_a_thread_restores_its_mode_and_goal() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let other = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "other".into(),
                cwd: state.thread.cwd.clone(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        {
            let guard = store.lock().unwrap();
            guard.set_thread_permission_mode(&other.id, PermissionMode::BypassPermissions).unwrap();
            guard
                .set_thread_goal(&other.id, Some("other goal"), true)
                .unwrap();
            guard
                .set_thread_session_mode(&other.id, SessionMode::Orchestrate)
                .unwrap();
        }
        state.overlay = Overlay::ThreadPicker {
            selected: 0,
            threads: vec![(other.id.clone(), other.title.clone())],
        };
        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::empty(),
                kind: KeyEventKind::Press,
                state: crossterm::event::KeyEventState::empty(),
            },
        );
        assert_eq!(state.thread.id, other.id);
        assert_eq!(state.permission_mode, PermissionMode::BypassPermissions);
        assert_eq!(state.goal.as_deref(), Some("other goal"));
        assert!(state.goal_paused);
        assert_eq!(state.thread.session_mode, SessionMode::Orchestrate);
    }

    #[test]
    fn worktree_command_forks_into_an_isolated_checked_out_thread() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(output.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&output.stderr));
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);

        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let source_id = state.thread.id.clone();
        state.thread.cwd = repo.path().to_string_lossy().to_string();
        state.thread.session_mode = SessionMode::Orchestrate;
        {
            let guard = store.lock().unwrap();
            guard
                .set_thread_cwd(&source_id, &state.thread.cwd)
                .unwrap();
            guard
                .set_thread_session_mode(&source_id, SessionMode::Orchestrate)
                .unwrap();
        }
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(
            &mut state,
            &store,
            &engine_command_sender,
            "/worktree",
        );

        assert_ne!(state.thread.id, source_id);
        assert_eq!(state.thread.title, "test (worktree)");
        assert_eq!(state.thread.session_mode, SessionMode::Single);
        assert!(
            state.thread.cwd.contains("/worktrees/"),
            "cwd: {}",
            state.thread.cwd
        );
        assert!(std::path::Path::new(&state.thread.cwd).is_dir());
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("worktree fork"),
            "notice: {:?}",
            state.notice
        );

        let runs = store.lock().unwrap().list_orchestration_runs(10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "completed");
        let tasks = store
            .lock()
            .unwrap()
            .list_orchestration_tasks(&runs[0].id)
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].thread_id.as_deref(), Some(state.thread.id.as_str()));
        assert_eq!(tasks[0].worktree_path.as_deref(), Some(state.thread.cwd.as_str()));
        assert_eq!(tasks[0].status, "completed");
        assert!(tasks[0].commit_sha.is_none());

        let source_messages = store.lock().unwrap().list_messages(&source_id).unwrap();
        assert!(
            source_messages
                .iter()
                .any(|message| message.text.contains("worktree fork")),
            "source thread records the fork"
        );
        let fork_messages = store
            .lock()
            .unwrap()
            .list_messages(&state.thread.id)
            .unwrap();
        assert!(
            fork_messages
                .iter()
                .any(|message| message.text.contains("next successful turn commits here")),
            "fork thread records the worktree run"
        );
    }

    #[test]
    fn worktree_command_refuses_dirty_and_non_git_sources() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let source_id = state.thread.id.clone();
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(&mut state, &store, &engine_command_sender, "/worktree");
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("needs a git repository"),
            "notice: {:?}",
            state.notice
        );
        assert_eq!(state.thread.id, source_id);

        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(output.status.success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(repo.path().join("tracked.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::write(repo.path().join("tracked.txt"), "dirty\n").unwrap();
        state.thread.cwd = repo.path().to_string_lossy().to_string();
        store
            .lock()
            .unwrap()
            .set_thread_cwd(&source_id, &state.thread.cwd)
            .unwrap();
        handle_command(&mut state, &store, &engine_command_sender, "/worktree");
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("uncommitted changes"),
            "notice: {:?}",
            state.notice
        );
        assert_eq!(state.thread.id, source_id);
        let worktrees = home.path().join("worktrees");
        assert!(!worktrees.exists() || std::fs::read_dir(worktrees).unwrap().next().is_none());
    }

    #[test]
    fn missing_provider_label_is_explicit() {
        assert_eq!(
            provider_label_with(ProviderId::Cursor, false),
            "cursor (not installed)"
        );
        assert_eq!(provider_label_with(ProviderId::Cursor, true), "cursor");
    }

    #[test]
    fn help_shows_common_paths_before_the_full_list() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();

        handle_command(&mut state, &store, &engine_command_sender, "/help");
        let text = state.notice.as_deref().unwrap_or("");
        assert!(text.starts_with("Type what you want to do."), "{text}");
        assert!(text.contains("Common"));
        assert!(text.contains("/orchestrate"));
        assert!(text.contains("/help all"));
        assert!(!text.contains("/compact"), "the default help stays short");

        handle_command(&mut state, &store, &engine_command_sender, "/help all");
        let text = state.notice.as_deref().unwrap_or("");
        for (name, _) in SLASH_COMMANDS {
            assert!(text.contains(name), "missing {name} from /help all");
        }
    }

    #[test]
    fn rewind_native_command_opens_the_native_picker() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread_id = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "rewind".to_string(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap()
            .id;
        store
            .lock()
            .unwrap()
            .add_message(&thread_id, 1, "user", None, None, "first prompt")
            .unwrap();
        let history = store.lock().unwrap().list_messages(&thread_id).unwrap();
        let mut state = test_state(home.path(), &store, history);
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(&mut state, &store, &engine_command_sender, "/rewind --native");
        assert!(matches!(
            state.overlay,
            Overlay::RewindPicker { native: true, .. }
        ));
        let text = buffer_lines(&render_state(&state, 100, 30)).join("\n");
        assert!(text.contains("native history"), "{text}");
    }

    #[test]
    fn native_rewind_result_restores_the_composer_and_code_prompt() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let thread_id = state.thread.id.clone();
        store
            .lock()
            .unwrap()
            .add_message(&thread_id, 1, "user", None, None, "kept")
            .unwrap();
        store
            .lock()
            .unwrap()
            .add_checkpoint(&thread_id, 1, "refs/baton/checkpoints/test/1")
            .unwrap();
        state.native_rewind_running = true;
        apply_native_rewind_event(
            &mut state,
            &store,
            NativeRewindEvent::Done {
                turn: 2,
                text: "retry prompt".to_string(),
                provider_retry_skipped: true,
            },
        );
        assert!(!state.native_rewind_running);
        assert_eq!(state.input, "retry prompt");
        assert!(state.history.iter().any(|message| message.text == "kept"));
        assert!(matches!(
            state.overlay,
            Overlay::CodeConfirm { turn: 2, .. }
        ));
        let notice = state.notice.as_deref().unwrap_or("");
        assert!(notice.contains("natively"), "{notice}");
        assert!(notice.contains("pending store rewind"), "{notice}");
        assert!(notice.contains("restore code"), "{notice}");

        state.native_rewind_running = true;
        state.overlay = Overlay::None;
        apply_native_rewind_event(
            &mut state,
            &store,
            NativeRewindEvent::Failed("boom".to_string()),
        );
        assert!(!state.native_rewind_running);
        assert!(
            state.notice.as_deref().unwrap_or("").contains("native rewind failed: boom"),
            "notice: {:?}",
            state.notice
        );
    }

    #[test]
    fn native_rewind_refuses_unsupported_threads() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.thread.current_provider = ProviderId::Cursor;
        start_native_rewind(&mut state, &store, 1, "prompt".to_string());
        assert!(!state.native_rewind_running);
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("Codex and OpenCode-only"),
            "notice: {:?}",
            state.notice
        );
    }

    #[test]
    fn fork_command_branches_the_conversation_and_opens_the_new_thread() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let source_id = state.thread.id.clone();
        {
            let guard = store.lock().unwrap();
            guard.set_thread_permission_mode(&source_id, PermissionMode::Plan).unwrap();
            guard.set_thread_goal(&source_id, Some("fork goal"), false).unwrap();
            guard
                .add_message(&source_id, 1, "user", Some("claude"), None, "hello")
                .unwrap();
            guard
                .add_message(&source_id, 1, "assistant", Some("claude"), None, "hi")
                .unwrap();
        }
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();

        handle_command(
            &mut state,
            &store,
            &engine_command_sender,
            "/fork branched",
        );

        assert_ne!(state.thread.id, source_id);
        assert_eq!(state.thread.title, "branched");
        assert_eq!(state.permission_mode, PermissionMode::Plan);
        assert_eq!(state.goal.as_deref(), Some("fork goal"));
        assert_eq!(state.history.len(), 2);
        assert!(state
            .notice
            .as_deref()
            .unwrap_or("")
            .contains("forked"));
        assert!(store.lock().unwrap().get_thread(&source_id).unwrap().is_some());
    }

    #[test]
    fn fork_command_can_stop_at_a_completed_turn() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let source_id = state.thread.id.clone();
        {
            let guard = store.lock().unwrap();
            for turn in 1..=2 {
                guard
                    .add_message(
                        &source_id,
                        turn,
                        "user",
                        Some("claude"),
                        None,
                        &format!("user {turn}"),
                    )
                    .unwrap();
                guard
                    .add_message(
                        &source_id,
                        turn,
                        "assistant",
                        Some("claude"),
                        None,
                        &format!("assistant {turn}"),
                    )
                    .unwrap();
                guard
                    .start_turn_record(&source_id, turn, ProviderId::Claude, None)
                    .unwrap();
                guard
                    .finish_turn_record(&source_id, turn, TurnPhase::Completed, None)
                    .unwrap();
            }
        }
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();

        handle_command(
            &mut state,
            &store,
            &engine_command_sender,
            "/fork --turn 1 branched one",
        );

        assert_ne!(state.thread.id, source_id);
        assert_eq!(state.thread.title, "branched one");
        assert_eq!(state.history.len(), 2);
        assert!(state.history.iter().all(|message| message.turn == 1));
        assert!(state
            .notice
            .as_deref()
            .unwrap_or("")
            .contains("through turn 1"));
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

    fn sample_report(thread_id: Option<String>) -> RunReport {
        RunReport {
            run_id: "abcdef1234567890".to_string(),
            goal: "split the work".to_string(),
            coordinator_provider: ProviderId::Codex,
            coordinator_model: "gpt-test".to_string(),
            summary: Some("two independent workers".to_string()),
            plan: crate::orchestrate::Plan {
                summary: Some("two independent workers".to_string()),
                tasks: vec![
                    crate::orchestrate::PlanTask {
                        title: "worker one".to_string(),
                        provider: "codex".to_string(),
                        model: None,
                        effort: None,
                        prompt: "do one".to_string(),
                    },
                    crate::orchestrate::PlanTask {
                        title: "worker two".to_string(),
                        provider: "claude".to_string(),
                        model: None,
                        effort: None,
                        prompt: "do two".to_string(),
                    },
                ],
            },
            repo_root: Some(std::path::PathBuf::from("/tmp/repo")),
            dirty_source: false,
            status: "completed".to_string(),
            archived_at: None,
            tasks: vec![
                crate::orchestrate::WorkerResult {
                    id: "task-1".to_string(),
                    title: "worker one".to_string(),
                    provider: ProviderId::Codex,
                    model: "gpt-test".to_string(),
                    effort: None,
                    prompt: "do one".to_string(),
                    worktree: Some(crate::worktree::WorktreeInfo {
                        repo_root: std::path::PathBuf::from("/tmp/repo"),
                        path: std::path::PathBuf::from("/tmp/worktrees/run/worker-one"),
                        branch: "baton/run/worker-one".to_string(),
                        base_ref: "HEAD".to_string(),
                        shared_paths: Vec::new(),
                    }),
                    commit: Some("0123456789abcdef".to_string()),
                    snapshot: None,
                    thread_id,
                    status: "completed".to_string(),
                    text: Some("one is done".to_string()),
                    error: None,
                    pr_url: None,
                    merged_at: None,
                    created_at: String::new(),
                    finished_at: String::new(),
                },
                crate::orchestrate::WorkerResult {
                    id: "task-2".to_string(),
                    title: "worker two".to_string(),
                    provider: ProviderId::Claude,
                    model: "claude-test".to_string(),
                    effort: None,
                    prompt: "do two".to_string(),
                    worktree: None,
                    commit: None,
                    snapshot: None,
                    thread_id: None,
                    status: "failed".to_string(),
                    text: None,
                    error: Some("boom".to_string()),
                    pr_url: None,
                    merged_at: None,
                    created_at: String::new(),
                    finished_at: String::new(),
                },
            ],
        }
    }

    fn sample_integrate_report() -> IntegrateReport {
        IntegrateReport {
            run_id: "abcdef1234567890".to_string(),
            repo_root: std::path::PathBuf::from("/tmp/repo"),
            merged: vec![crate::orchestrate::IntegratedTask {
                index: 1,
                title: "worker one".to_string(),
                branch: "baton/run/worker-one".to_string(),
                commit: "0123456789abcdef".to_string(),
                merge_commit: "fedcba9876543210".to_string(),
            }],
            skipped: Vec::new(),
            conflict: None,
        }
    }

    fn sample_ship_report() -> ShipReport {
        ShipReport {
            run_id: "abcdef1234567890".to_string(),
            repo_root: std::path::PathBuf::from("/tmp/repo"),
            remote: "origin".to_string(),
            base: "main".to_string(),
            opened: vec![crate::orchestrate::ShippedTask {
                index: 1,
                title: "worker one".to_string(),
                branch: "baton/run/worker-one".to_string(),
                commit: "0123456789abcdef".to_string(),
                pr_url: "https://github.com/example/repo/pull/1".to_string(),
            }],
            skipped: Vec::new(),
            failed: Vec::new(),
        }
    }

    #[test]
    fn runs_command_hides_archived_runs_unless_all_is_requested() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        {
            let guard = store.lock().unwrap();
            for (id, goal) in [("run-active", "active goal"), ("run-archived", "archived goal")] {
                guard
                    .create_orchestration_run(id, goal, ProviderId::Codex, "model-1", None, "{}", false)
                    .unwrap();
                guard.finish_orchestration_run(id, "completed").unwrap();
            }
            guard.archive_orchestration_run("run-archived").unwrap();
        }

        handle_command(&mut state, &store, &engine_command_sender, "/runs");
        let notice = state.notice.as_deref().unwrap_or("");
        assert!(notice.contains("active goal"), "{notice}");
        assert!(!notice.contains("archived goal"), "{notice}");
        assert!(notice.contains("/runs --all"), "{notice}");

        handle_command(&mut state, &store, &engine_command_sender, "/runs --all");
        let notice = state.notice.as_deref().unwrap_or("");
        assert!(notice.contains("active goal"), "{notice}");
        assert!(notice.contains("archived goal (archived)"), "{notice}");
    }

    #[test]
    fn orchestration_report_a_archives_and_closes_the_run() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        {
            let guard = store.lock().unwrap();
            guard
                .create_orchestration_run(
                    "abcdef1234567890",
                    "goal",
                    ProviderId::Codex,
                    "model",
                    None,
                    "{}",
                    false,
                )
                .unwrap();
            guard
                .finish_orchestration_run("abcdef1234567890", "completed")
                .unwrap();
        }
        state.orchestration_report = Some(sample_report(None));
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            key(KeyCode::Char('a')),
        );
        assert!(state.orchestration_report.is_none());
        assert!(
            state.notice.as_deref().unwrap_or("").contains("archived run abcdef12"),
            "notice: {:?}",
            state.notice
        );
        let runs = store.lock().unwrap().list_orchestration_runs(10).unwrap();
        assert!(runs[0].archived_at.is_some());
    }

    #[test]
    fn runs_command_lists_and_reopens_stored_runs() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let now = "2026-11-12T00:00:00.000Z";
        {
            let guard = store.lock().unwrap();
            guard
                .create_orchestration_run(
                    "run-12345678",
                    "fix the bug",
                    ProviderId::Codex,
                    "model-1",
                    Some("one worker"),
                    r#"{"tasks":[]}"#,
                    false,
                )
                .unwrap();
            guard
                .add_orchestration_task(
                    "task-1",
                    "run-12345678",
                    1,
                    "answer",
                    ProviderId::Codex,
                    "model-1",
                    "fix it",
                    None,
                    None,
                    None,
                    None,
                    "completed",
                    Some("fixed"),
                    None,
                    now,
                    Some(now),
                    None,
                    None,
                    None,
                )
                .unwrap();
            guard
                .finish_orchestration_run("run-12345678", "completed")
                .unwrap();
        }

        handle_command(&mut state, &store, &engine_command_sender, "/runs");
        assert!(
            state.notice.as_deref().unwrap_or("").contains("run-1234"),
            "notice: {:?}",
            state.notice
        );

        handle_command(
            &mut state,
            &store,
            &engine_command_sender,
            "/runs run-1234",
        );
        let report = state.orchestration_report.as_ref().expect("report opened");
        assert_eq!(report.run_id, "run-12345678");
        assert_eq!(report.status, "completed");
        assert_eq!(report.tasks.len(), 1);
        assert_eq!(report.tasks[0].title, "answer");
        assert_eq!(report.tasks[0].text.as_deref(), Some("fixed"));
    }

    #[test]
    fn dirty_confirm_overlay_renders_the_choices() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.overlay = Overlay::DirtyConfirm {
            goal: "fix the bug".to_string(),
            max_tasks: 4,
            plan_file: None,
        };
        let buffer = render_state(&state, 120, 30);
        let text = buffer_lines(&buffer).join("\n");
        assert!(text.contains("source checkout has uncommitted changes"));
        assert!(text.contains("workers receive a snapshot"));
        assert!(text.contains("run single harness"));
    }

    #[test]
    fn effective_permission_uses_the_provider_default_until_overridden() {
        assert_eq!(
            effective_permission_mode(
                PermissionMode::Default,
                PermissionMode::BypassPermissions
            ),
            PermissionMode::BypassPermissions
        );
        assert_eq!(
            effective_permission_mode(PermissionMode::Plan, PermissionMode::BypassPermissions),
            PermissionMode::Plan
        );
    }

    #[test]
    fn banner_shows_version_cwd_mode_and_permissions() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.thread.session_mode = SessionMode::Orchestrate;
        state.permission_mode = PermissionMode::BypassPermissions;
        let buffer = render_state(&state, 120, 30);
        let text = buffer_lines(&buffer).join("\n");
        assert!(text.contains(">_ baton v"), "{text}");
        assert!(text.contains("permissions: YOLO mode"), "{text}");
        assert!(text.contains("orchestrate"), "{text}");
        assert!(text.contains("coordinator"), "{text}");
        assert!(text.contains(&state.thread.cwd), "{text}");
    }

    #[test]
    fn banner_shows_single_harness_and_billing() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.thread.session_mode = SessionMode::Single;
        let buffer = render_state(&state, 120, 30);
        let text = buffer_lines(&buffer).join("\n");
        assert!(text.contains("single"), "{text}");
        assert!(text.contains("codex") || text.contains("claude"), "{text}");
        assert!(text.contains("billing:"), "{text}");
    }

    #[test]
    fn orchestrate_mode_shows_badge_and_placeholder() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.thread.session_mode = SessionMode::Orchestrate;
        let buffer = render_state(&state, 120, 30);
        let text = buffer_lines(&buffer).join("\n");
        assert!(text.contains("orchestrate"), "{text}");
        assert!(text.contains("baton will split the work"), "{text}");

        state.thread.session_mode = SessionMode::Single;
        let buffer = render_state(&state, 120, 30);
        let text = buffer_lines(&buffer).join("\n");
        assert!(!text.contains("baton will split the work"), "{text}");
        assert!(text.contains("Ask anything..."), "{text}");
    }

    #[test]
    fn orchestrate_mode_routes_git_prompts_away_from_single_turns() {
        let repo = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(init.success());
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.thread.cwd = repo.path().to_string_lossy().to_string();
        state.thread.session_mode = SessionMode::Orchestrate;
        state.orchestration_pending = true;
        let (engine_command_sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        dispatch_message(
            &mut state,
            &store,
            &engine_command_sender,
            "hello".to_string(),
            false,
        );
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("already running"),
            "notice: {:?}",
            state.notice
        );
        assert!(
            receiver.try_recv().is_err(),
            "a git repository prompt must not fall through to the single-harness engine"
        );
    }

    #[test]
    fn dirty_source_prompts_before_orchestration() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(output.status.success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@test"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(repo.path().join("tracked.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::write(repo.path().join("tracked.txt"), "dirty\n").unwrap();

        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.thread.cwd = repo.path().to_string_lossy().to_string();
        state.thread.session_mode = SessionMode::Orchestrate;
        let (engine_command_sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        dispatch_message(
            &mut state,
            &store,
            &engine_command_sender,
            "fix the bug".to_string(),
            false,
        );
        assert!(!state.orchestration_pending);
        assert!(
            matches!(state.overlay, Overlay::DirtyConfirm { .. }),
            "dirty source must ask before orchestration"
        );
        assert!(receiver.try_recv().is_err(), "no turn may start yet");

        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            KeyEvent {
                code: KeyCode::Char('s'),
                modifiers: KeyModifiers::empty(),
                kind: KeyEventKind::Press,
                state: crossterm::event::KeyEventState::empty(),
            },
        );
        assert!(matches!(state.overlay, Overlay::None));
        assert!(
            matches!(receiver.try_recv(), Ok(EngineCommand::Submit { .. })),
            "s must run a single harness"
        );

        dispatch_message(
            &mut state,
            &store,
            &engine_command_sender,
            "fix the bug".to_string(),
            false,
        );
        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::empty(),
                kind: KeyEventKind::Press,
                state: crossterm::event::KeyEventState::empty(),
            },
        );
        assert!(matches!(state.overlay, Overlay::None));
        assert_eq!(state.input, "fix the bug");
    }

    #[test]
    fn greeting_detection_is_narrow() {
        assert!(orchestrate::is_conversational_greeting("hi"));
        assert!(orchestrate::is_conversational_greeting(" hello! "));
        assert!(orchestrate::is_conversational_greeting("thanks."));
        assert!(!orchestrate::is_conversational_greeting("hi, fix the tests"));
        assert!(!orchestrate::is_conversational_greeting("fix the bug"));
    }

    #[test]
    fn orchestrate_command_routes_a_greeting_to_one_turn() {
        let repo = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(init.success());
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.thread.cwd = repo.path().to_string_lossy().to_string();
        state.thread.session_mode = SessionMode::Orchestrate;
        let (engine_command_sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(&mut state, &store, &engine_command_sender, "/orchestrate hi");
        assert!(!state.orchestration_pending);
        assert!(
            matches!(
                receiver.try_recv(),
                Ok(EngineCommand::Submit { ref text, .. }) if text == "hi"
            ),
            "a greeting should use one turn"
        );
    }

    #[test]
    fn greeting_in_orchestrate_mode_uses_a_single_turn() {
        let repo = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(init.success());
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.thread.cwd = repo.path().to_string_lossy().to_string();
        state.thread.session_mode = SessionMode::Orchestrate;
        let (engine_command_sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        dispatch_message(
            &mut state,
            &store,
            &engine_command_sender,
            "hi".to_string(),
            false,
        );
        assert!(!state.orchestration_pending);
        assert!(
            matches!(receiver.try_recv(), Ok(EngineCommand::Submit { .. })),
            "a greeting should run a normal single turn"
        );
    }

    #[test]
    fn orchestrate_mode_falls_back_to_single_outside_git() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.thread.session_mode = SessionMode::Orchestrate;
        let (engine_command_sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        dispatch_message(
            &mut state,
            &store,
            &engine_command_sender,
            "fix the bug".to_string(),
            false,
        );
        assert!(!state.orchestration_pending);
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("needs a git repository"),
            "notice: {:?}",
            state.notice
        );
        assert!(
            matches!(receiver.try_recv(), Ok(EngineCommand::Submit { .. })),
            "the prompt falls back to the single-harness engine"
        );
    }

    #[test]
    fn ctrl_c_cancels_orchestration_without_exiting() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.orchestration_pending = true;
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                state: crossterm::event::KeyEventState::empty(),
            },
        );
        assert!(!state.quit);
        assert!(state.exit_armed.is_none());
        assert!(state.orchestration_cancelling);
        assert!(state.orchestration_cancel.load(Ordering::Relaxed));
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("cancelling orchestration"),
            "notice: {:?}",
            state.notice
        );
    }

    #[test]
    fn orchestrate_mode_toggles_and_is_persisted() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        assert_eq!(state.thread.session_mode, SessionMode::Single);

        handle_command(
            &mut state,
            &store,
            &engine_command_sender,
            "/orchestrate on",
        );
        assert_eq!(state.thread.session_mode, SessionMode::Orchestrate);
        assert_eq!(
            store
                .lock()
                .unwrap()
                .get_thread(&state.thread.id)
                .unwrap()
                .unwrap()
                .session_mode,
            SessionMode::Orchestrate
        );

        handle_command(
            &mut state,
            &store,
            &engine_command_sender,
            "/orchestrate off",
        );
        assert_eq!(state.thread.session_mode, SessionMode::Single);
    }

    #[test]
    fn new_thread_defaults_to_orchestrate_mode() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_command(&mut state, &store, &engine_command_sender, "/new");
        assert_eq!(state.thread.session_mode, SessionMode::Orchestrate);
    }

    #[test]
    fn switching_harness_returns_to_single_mode() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        state.set_session_mode(&store, SessionMode::Orchestrate);
        handle_command(
            &mut state,
            &store,
            &engine_command_sender,
            "/switch claude",
        );
        assert_eq!(state.thread.session_mode, SessionMode::Single);
        assert_eq!(
            store
                .lock()
                .unwrap()
                .get_thread(&state.thread.id)
                .unwrap()
                .unwrap()
                .session_mode,
            SessionMode::Single
        );
    }

    #[test]
    fn orchestrate_args_parse_goal_and_flags() {
        let parsed = parse_orchestrate_args(&[
            "--max-tasks".to_string(),
            "2".to_string(),
            "split".to_string(),
            "the".to_string(),
            "work".to_string(),
        ])
        .unwrap();
        assert_eq!(parsed.goal, "split the work");
        assert_eq!(parsed.max_tasks, 2);
        assert_eq!(parsed.plan_file, None);
        assert!(!parsed.plan_only);

        let parsed = parse_orchestrate_args(&[
            "--plan-file".to_string(),
            "plan.json".to_string(),
            "--max-tasks".to_string(),
            "99".to_string(),
            "goal".to_string(),
        ])
        .unwrap();
        assert_eq!(parsed.max_tasks, 16, "task count is capped");
        assert_eq!(parsed.plan_file.as_deref(), Some("plan.json"));
        assert!(!parsed.plan_only);

        let preview = parse_orchestrate_args(&[
            "--plan-only".to_string(),
            "preview".to_string(),
            "this".to_string(),
        ])
        .unwrap();
        assert!(preview.plan_only);
        assert_eq!(preview.goal, "preview this");

        assert!(parse_orchestrate_args(&[]).is_err());
        assert!(parse_orchestrate_args(&[
            "--max-tasks".to_string(),
            "many".to_string(),
            "goal".to_string()
        ])
        .is_err());
        assert!(
            parse_orchestrate_args(&["--plan-file".to_string(), "plan.json".to_string()]).is_ok(),
            "a plan file alone is a valid run"
        );
    }

    #[test]
    fn orchestration_preview_event_renders_and_escape_restores_the_goal() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let plan = sample_report(None).plan;
        state.orchestration_pending = true;
        apply_orchestration_event(
            &mut state,
            OrchestrationEvent::Preview(Box::new(OrchestrationPreview {
                goal: "split the work".to_string(),
                plan,
                coordinator: ProviderId::Codex,
                coordinator_model: "gpt-test".to_string(),
                dirty_source: true,
            })),
        );
        assert!(!state.orchestration_pending);
        assert!(matches!(
            state.overlay,
            Overlay::OrchestrationPreview { .. }
        ));
        let text = buffer_lines(&render_state(&state, 120, 40)).join("\n");
        assert!(text.contains("plan preview"), "{text}");
        assert!(text.contains("worker one"), "{text}");
        assert!(text.contains("gpt-test"), "{text}");
        assert!(text.contains("source is dirty"), "{text}");

        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::empty(),
                kind: KeyEventKind::Press,
                state: crossterm::event::KeyEventState::empty(),
            },
        );
        assert!(matches!(state.overlay, Overlay::None));
        assert_eq!(state.input, "split the work");
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("plan cancelled"),
            "notice: {:?}",
            state.notice
        );
    }

    #[test]
    fn preview_plan_file_round_trips_the_resolved_plan() {
        let home = tempfile::tempdir().unwrap();
        let plan = sample_report(None).plan;
        let path = write_preview_plan(home.path(), &plan).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        let parsed = crate::orchestrate::parse_plan(&text, 4).unwrap();
        assert_eq!(parsed.summary, plan.summary);
        assert_eq!(parsed.tasks.len(), plan.tasks.len());
        assert_eq!(parsed.tasks[0].title, plan.tasks[0].title);
    }

    #[test]
    fn orchestration_events_update_state() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.orchestration_pending = true;
        apply_orchestration_event(
            &mut state,
            OrchestrationEvent::Done(Box::new(sample_report(None))),
        );
        assert!(!state.orchestration_pending);
        assert_eq!(state.orchestration_report.as_ref().unwrap().tasks.len(), 2);
        assert!(state.notice.as_deref().unwrap_or("").contains("completed"));

        state.orchestration_pending = true;
        apply_orchestration_event(&mut state, OrchestrationEvent::Failed("boom".to_string()));
        assert!(!state.orchestration_pending);
        assert!(state.orchestration_report.is_none());
        assert!(state.notice.as_deref().unwrap_or("").contains("boom"));
    }

    #[test]
    fn orchestration_report_keys_scroll_select_and_close() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.orchestration_report = Some(sample_report(None));
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };

        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Down));
        assert_eq!(state.orchestration_selected, 1);
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Down));
        assert_eq!(state.orchestration_selected, 1, "selection stops at the last worker");
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::PageDown));
        assert_eq!(state.orchestration_scroll, 10);
        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Esc));
        assert!(state.orchestration_report.is_none(), "esc closes the report");
        assert_eq!(state.orchestration_selected, 0);
        assert_eq!(state.orchestration_scroll, 0);
    }

    #[test]
    fn orchestration_report_enter_opens_worker_thread() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let worker = store
            .lock()
            .unwrap()
            .create_thread(NewThread {
                title: "worker one".to_string(),
                cwd: home.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: Some("gpt-test".to_string()),
            })
            .unwrap();
        state.orchestration_report = Some(sample_report(Some(worker.id.clone())));
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let enter = KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        handle_key(&mut state, &store, &engine_command_sender, enter);
        assert!(state.orchestration_report.is_none());
        assert_eq!(state.thread.id, worker.id, "enter switches to the worker thread");
        assert_eq!(state.thread.current_provider, ProviderId::Codex);
    }

    #[test]
    fn orchestration_progress_renders_worker_states() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.orchestration_pending = true;
        apply_orchestration_event(
            &mut state,
            OrchestrationEvent::Progress(OrchestrationProgress::PlanReady {
                summary: Some("two workers".to_string()),
                tasks: vec![
                    crate::orchestrate::PlanTask {
                        title: "first task".to_string(),
                        provider: "codex".to_string(),
                        model: None,
                        effort: None,
                        prompt: "one".to_string(),
                    },
                    crate::orchestrate::PlanTask {
                        title: "second task".to_string(),
                        provider: "claude".to_string(),
                        model: None,
                        effort: None,
                        prompt: "two".to_string(),
                    },
                ],
            }),
        );
        assert_eq!(state.orchestration_workers.len(), 2);
        assert_eq!(state.orchestration_workers[0].status, "queued");
        apply_orchestration_event(
            &mut state,
            OrchestrationEvent::Progress(OrchestrationProgress::WorkerStarted {
                index: 1,
                title: "first task".to_string(),
                provider: ProviderId::Codex,
                model: "gpt-test".to_string(),
                worktree: Some(std::path::PathBuf::from("/tmp/worktree")),
            }),
        );
        apply_orchestration_event(
            &mut state,
            OrchestrationEvent::Progress(OrchestrationProgress::WorkerFinished {
                index: 2,
                title: "second task".to_string(),
                status: "failed".to_string(),
                commit: None,
                error: Some("boom".to_string()),
            }),
        );
        assert_eq!(state.orchestration_workers[0].status, "running");
        assert_eq!(state.orchestration_workers[1].status, "failed");
        let buffer = render_state(&state, 120, 30);
        let text = buffer_lines(&buffer).join("\n");
        assert!(text.contains("Orchestrating"), "{text}");
        assert!(text.contains("[1] first task · codex/gpt-test · running"), "{text}");
        assert!(text.contains("[2] second task"), "{text}");
        assert!(text.contains("failed · boom"), "{text}");
    }

    #[test]
    fn orchestration_report_i_confirms_before_integrating() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.orchestration_report = Some(sample_report(None));
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };

        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            key(KeyCode::Char('i')),
        );
        assert!(state.orchestration_confirm_integrate);
        assert!(
            state.notice.as_deref().unwrap_or("").contains("merge 1 worker"),
            "notice: {:?}",
            state.notice
        );
        let buffer = render_state(&state, 100, 30);
        assert!(buffer_lines(&buffer).join("\n").contains("enter confirms"));

        handle_key(&mut state, &store, &engine_command_sender, key(KeyCode::Esc));
        assert!(!state.orchestration_confirm_integrate);
        assert!(state.orchestration_report.is_some(), "esc only cancels the merge");
    }

    #[test]
    fn orchestration_integration_event_updates_state_and_renders() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.orchestration_report = Some(sample_report(None));
        state.orchestration_integrating = true;
        apply_orchestration_event(
            &mut state,
            OrchestrationEvent::Integrated(Box::new(sample_integrate_report())),
        );
        assert!(!state.orchestration_integrating);
        assert!(!state.orchestration_confirm_integrate);
        assert!(state.orchestration_integrate_report.is_some());
        assert!(
            state.notice.as_deref().unwrap_or("").contains("integration done"),
            "notice: {:?}",
            state.notice
        );
        let buffer = render_state(&state, 100, 40);
        let text = buffer_lines(&buffer).join("\n");
        assert!(text.contains("integration: done"), "{text}");
        assert!(text.contains("merged [1] worker one -> fedcba98"), "{text}");

        state.orchestration_integrating = true;
        apply_orchestration_event(
            &mut state,
            OrchestrationEvent::IntegrateFailed("boom".to_string()),
        );
        assert!(!state.orchestration_integrating);
        assert!(state.notice.as_deref().unwrap_or("").contains("boom"));
    }

    #[test]
    fn orchestration_report_p_confirms_before_shipping() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.orchestration_report = Some(sample_report(None));
        let (engine_command_sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };

        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            key(KeyCode::Char('p')),
        );
        assert!(state.orchestration_confirm_ship);
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("open pull requests"),
            "notice: {:?}",
            state.notice
        );
        let buffer = render_state(&state, 120, 30);
        let text = buffer_lines(&buffer).join("\n");
        assert!(text.contains("enter confirms"), "{text}");
        assert!(text.contains("open PRs"), "{text}");

        handle_key(
            &mut state,
            &store,
            &engine_command_sender,
            key(KeyCode::Esc),
        );
        assert!(!state.orchestration_confirm_ship);
        assert!(
            state.orchestration_report.is_some(),
            "esc only cancels the ship prompt"
        );
        let buffer = render_state(&state, 120, 30);
        assert!(buffer_lines(&buffer).join("\n").contains("p PRs"));
    }

    #[test]
    fn orchestration_ship_event_updates_pr_links_and_report() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.orchestration_report = Some(sample_report(None));
        state.orchestration_shipping = true;
        apply_orchestration_event(
            &mut state,
            OrchestrationEvent::Shipped(Box::new(sample_ship_report())),
        );
        assert!(!state.orchestration_shipping);
        assert!(!state.orchestration_confirm_ship);
        assert!(state.orchestration_ship_report.is_some());
        assert_eq!(
            state.orchestration_report.as_ref().unwrap().tasks[0]
                .pr_url
                .as_deref(),
            Some("https://github.com/example/repo/pull/1")
        );
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("ship done: 1 PR"),
            "notice: {:?}",
            state.notice
        );
        let buffer = render_state(&state, 120, 40);
        let text = buffer_lines(&buffer).join("\n");
        assert!(
            text.contains("pr: https://github.com/example/repo/pull/1"),
            "{text}"
        );
        assert!(text.contains("ship: done"), "{text}");

        state.orchestration_shipping = true;
        apply_orchestration_event(
            &mut state,
            OrchestrationEvent::ShipFailed("no git remote".to_string()),
        );
        assert!(!state.orchestration_shipping);
        assert!(
            state
                .notice
                .as_deref()
                .unwrap_or("")
                .contains("no git remote"),
            "notice: {:?}",
            state.notice
        );
    }

    #[test]
    fn orchestration_report_renders_a_worker_question() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        let mut report = sample_report(None);
        report.status = "needs_input".to_string();
        report.tasks[0].status = "needs_input".to_string();
        report.tasks[0].text = Some("BATON_QUESTION: approve the worker change?".to_string());
        state.orchestration_report = Some(report);
        let text = buffer_lines(&render_state(&state, 100, 30)).join("\n");
        assert!(text.contains("needs_input"), "{text}");
        assert!(text.contains("question: approve the worker change?"), "{text}");
    }

    #[test]
    fn orchestration_report_renders_workers_and_hints() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let mut state = test_state(home.path(), &store, Vec::new());
        state.orchestration_report = Some(sample_report(None));
        let buffer = render_state(&state, 100, 30);
        let text = buffer_lines(&buffer).join("\n");
        assert!(text.contains("orchestration abcdef12 completed"), "{text}");
        assert!(text.contains("worker one"), "{text}");
        assert!(text.contains("worker two"), "{text}");
        assert!(text.contains("commit: 01234567"), "{text}");
        assert!(text.contains("esc closes"), "{text}");
        assert!(text.contains("boom"), "{text}");
    }
}
