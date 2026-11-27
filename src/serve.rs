//! Newline-delimited JSON protocol for `baton serve` and `--stdio`.
//!
//! Stdout carries protocol responses only. Diagnostics stay on stderr.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
#[cfg(unix)]
use std::os::unix::net::UnixListener as StdUnixListener;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener as TokioUnixListener, UnixStream};

use crate::store::{BatonStore, HostClaimRow};

pub const PROTOCOL_VERSION: u64 = 1;

/// Stops this host generation's provider children, settles work whose owner
/// stopped, and releases the claim. The server loop result is returned after
/// cleanup, so a loop error cannot leave the store claimed by an exiting
/// process.
pub fn finish_host(store: &BatonStore, host_id: &str, loop_result: Result<()>) -> Result<()> {
    match crate::process::terminate_host_children(store, host_id) {
        Ok(report) => {
            if !report.reaped.is_empty() {
                eprintln!(
                    "baton: stopped {} provider child(ren) for this host",
                    report.reaped.len()
                );
            }
            for error in &report.errors {
                eprintln!("baton: child shutdown: {error}");
            }
        }
        Err(error) => eprintln!("baton: child shutdown failed: {error}"),
    }
    // Children stop first so a provider cannot overwrite a settled turn.
    // A deliberate host stop then cancels the work this host owns with the
    // signed host reason, and reports the worktrees it leaves in place.
    match store.get_host_claim() {
        Ok(Some(claim)) if claim.host_id == host_id => {
            match store.cancel_host_work(claim.pid, Some(&claim.process_start), "host shut down") {
                Ok(settled) => {
                    if settled.turns + settled.tasks + settled.runs.len() > 0 {
                        eprintln!(
                            "baton: cancelled {} host turn(s), {} task(s), and {} run(s)",
                            settled.turns,
                            settled.tasks,
                            settled.runs.len()
                        );
                    }
                    let mut worktrees = Vec::new();
                    for run_id in &settled.runs {
                        match store.list_orchestration_tasks(run_id) {
                            Ok(tasks) => worktrees
                                .extend(tasks.into_iter().filter_map(|task| task.worktree_path)),
                            Err(error) => {
                                eprintln!("baton: worktree report failed for {run_id}: {error}")
                            }
                        }
                    }
                    if !worktrees.is_empty() {
                        eprintln!("baton: left {} worktree(s) in place", worktrees.len());
                        for path in worktrees {
                            eprintln!("baton: worktree {path}");
                        }
                    }
                }
                Err(error) => eprintln!("baton: host work cancellation failed: {error}"),
            }
        }
        Ok(_) => {}
        Err(error) => eprintln!("baton: host claim read failed: {error}"),
    }
    // Then settle work whose owner stopped while this host was running, so a
    // client cannot open the store to a turn or run that only looks live.
    match store.reconcile_running_turns(None) {
        Ok(turns) => {
            if !turns.is_empty() {
                eprintln!(
                    "baton: settled {} unfinished turn(s) on shutdown",
                    turns.len()
                );
            }
        }
        Err(error) => eprintln!("baton: turn shutdown settlement failed: {error}"),
    }
    match crate::orchestrate::reconcile_abandoned_runs(store) {
        Ok(runs) => {
            if !runs.is_empty() {
                eprintln!("baton: settled {} abandoned run(s) on shutdown", runs.len());
            }
        }
        Err(error) => eprintln!("baton: run shutdown settlement failed: {error}"),
    }
    if !store.release_host_claim(host_id)? {
        anyhow::bail!("host claim disappeared before shutdown");
    }
    loop_result
}

#[cfg(unix)]
pub fn socket_path(home: &Path) -> PathBuf {
    home.join("serve.sock")
}

/// Binds the host socket. A stale socket is replaced; any other kind of file
/// at the path is refused, so the host never deletes something it did not
/// create.
#[cfg(unix)]
pub fn bind_socket(path: &Path) -> Result<StdUnixListener> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        use std::os::unix::fs::FileTypeExt;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
            anyhow::bail!(
                "refusing to replace {}: the path is not a socket",
                path.display()
            );
        }
        std::fs::remove_file(path)
            .with_context(|| format!("remove stale socket {}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        crate::claim::create_private_dir(parent)?;
    }
    let listener =
        StdUnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    listener.set_nonblocking(true)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("set mode on {}", path.display()))?;
    }
    Ok(listener)
}

#[cfg(not(unix))]
pub fn socket_path(home: &Path) -> PathBuf {
    home.join("serve.sock")
}

#[cfg(not(unix))]
pub fn bind_socket(_path: &Path) -> Result<()> {
    anyhow::bail!("the host socket is Unix-only")
}

struct PendingApproval {
    id: String,
    thread_id: String,
    kind: String,
    summary: String,
    choices: Vec<crate::types::ApprovalChoice>,
    respond: tokio::sync::oneshot::Sender<crate::types::ApprovalDecision>,
    sequence: u64,
}

struct TakenApproval {
    pending: PendingApproval,
    choice: crate::types::ApprovalChoice,
}

struct PendingQuestion {
    id: String,
    thread_id: String,
    questions: Vec<crate::types::QuestionPrompt>,
    respond: tokio::sync::oneshot::Sender<Vec<crate::types::QuestionAnswer>>,
    sequence: u64,
}

struct TakenQuestion {
    pending: PendingQuestion,
    answers: Vec<crate::types::QuestionAnswer>,
}

enum QuestionTakeError {
    NotFound,
    Invalid(String),
}

/// Questions wait here until a paired client answers or the turn ends.
#[derive(Default)]
struct RemoteQuestions {
    pending: BTreeMap<String, PendingQuestion>,
    next_sequence: u64,
}

impl RemoteQuestions {
    fn insert(
        &mut self,
        thread_id: String,
        id: String,
        questions: Vec<crate::types::QuestionPrompt>,
        respond: tokio::sync::oneshot::Sender<Vec<crate::types::QuestionAnswer>>,
    ) {
        self.next_sequence += 1;
        let sequence = self.next_sequence;
        self.pending.insert(
            id.clone(),
            PendingQuestion {
                id,
                thread_id,
                questions,
                respond,
                sequence,
            },
        );
    }

    fn list(&mut self, thread: Option<&str>) -> Vec<Value> {
        self.pending
            .retain(|_, question| !question.respond.is_closed());
        let mut rows: Vec<&PendingQuestion> = self
            .pending
            .values()
            .filter(|question| thread.is_none_or(|thread| question.thread_id == thread))
            .collect();
        rows.sort_by_key(|question| question.sequence);
        rows.into_iter()
            .map(|question| {
                json!({
                    "request_id": question.id,
                    "thread_id": question.thread_id,
                    "questions": question.questions.iter().map(|prompt| json!({
                        "id": prompt.id,
                        "header": prompt.header,
                        "question": prompt.question,
                        "options": prompt.options.iter().map(|option| json!({
                            "label": option.label,
                            "description": option.description,
                        })).collect::<Vec<_>>(),
                        "allow_custom": prompt.allow_custom,
                        "multi_select": prompt.multi_select,
                        "secret": prompt.secret,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect()
    }

    fn take(
        &mut self,
        request: &str,
        answers: &[crate::types::QuestionAnswer],
    ) -> std::result::Result<TakenQuestion, QuestionTakeError> {
        let pending = self
            .pending
            .get(request)
            .ok_or(QuestionTakeError::NotFound)?;
        if pending.respond.is_closed() {
            self.pending.remove(request);
            return Err(QuestionTakeError::NotFound);
        }
        if answers.len() != pending.questions.len() {
            return Err(QuestionTakeError::Invalid(format!(
                "answer every prompt: expected {} answers, got {}",
                pending.questions.len(),
                answers.len()
            )));
        }
        let mut seen = Vec::with_capacity(answers.len());
        for answer in answers {
            if seen.contains(&answer.id) {
                return Err(QuestionTakeError::Invalid(format!(
                    "prompt {} is answered twice",
                    answer.id
                )));
            }
            seen.push(answer.id.clone());
            let Some(prompt) = pending
                .questions
                .iter()
                .find(|prompt| prompt.id == answer.id)
            else {
                return Err(QuestionTakeError::Invalid(format!(
                    "no prompt matches {}",
                    answer.id
                )));
            };
            if !prompt.accepts(&answer.answers) {
                return Err(QuestionTakeError::Invalid(format!(
                    "prompt {} does not accept that answer",
                    answer.id
                )));
            }
        }
        let pending = self
            .pending
            .remove(request)
            .ok_or(QuestionTakeError::NotFound)?;
        Ok(TakenQuestion {
            pending,
            answers: answers.to_vec(),
        })
    }

    fn put_back(&mut self, taken: TakenQuestion) {
        self.pending.insert(taken.pending.id.clone(), taken.pending);
    }

    fn clear_thread(&mut self, thread_id: &str) {
        self.pending
            .retain(|_, question| question.thread_id != thread_id);
    }
}

/// Approvals wait here until a paired client answers or the turn ends. A
/// dropped sender is a denial, so a closed entry is pruned instead of listed.
#[derive(Default)]
struct RemoteApprovals {
    pending: BTreeMap<String, PendingApproval>,
    next_sequence: u64,
}

impl RemoteApprovals {
    fn insert(
        &mut self,
        thread_id: String,
        id: String,
        kind: String,
        summary: String,
        choices: Vec<crate::types::ApprovalChoice>,
        respond: tokio::sync::oneshot::Sender<crate::types::ApprovalDecision>,
    ) {
        self.next_sequence += 1;
        let choices = if choices.is_empty() {
            crate::adapters::standard_approval_choices()
        } else {
            choices
        };
        let sequence = self.next_sequence;
        self.pending.insert(
            id.clone(),
            PendingApproval {
                id,
                thread_id,
                kind,
                summary,
                choices,
                respond,
                sequence,
            },
        );
    }

    fn list(&mut self, thread: Option<&str>) -> Vec<Value> {
        self.pending
            .retain(|_, approval| !approval.respond.is_closed());
        let mut rows: Vec<&PendingApproval> = self
            .pending
            .values()
            .filter(|approval| thread.is_none_or(|thread| approval.thread_id == thread))
            .collect();
        rows.sort_by_key(|approval| approval.sequence);
        rows.into_iter()
            .map(|approval| {
                json!({
                    "request_id": approval.id,
                    "thread_id": approval.thread_id,
                    "kind": approval.kind,
                    "summary": approval.summary,
                    "choices": approval.choices.iter().map(|choice| json!({
                        "id": choice.id,
                        "label": choice.label,
                        "kind": choice.kind,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect()
    }

    fn take(&mut self, request: &str, choice: &str) -> Option<TakenApproval> {
        let pending = self.pending.get(request)?;
        if pending.respond.is_closed() {
            self.pending.remove(request);
            return None;
        }
        let choice = pending
            .choices
            .iter()
            .find(|item| item.id == choice)?
            .clone();
        let pending = self.pending.remove(request)?;
        Some(TakenApproval { pending, choice })
    }

    fn put_back(&mut self, taken: TakenApproval) {
        self.pending.insert(taken.pending.id.clone(), taken.pending);
    }

    fn clear_thread(&mut self, thread_id: &str) {
        self.pending
            .retain(|_, approval| approval.thread_id != thread_id);
    }
}

/// The handle a socket client uses to submit work to the host engine.
#[derive(Clone)]
pub struct EngineLink {
    sender: tokio::sync::mpsc::UnboundedSender<crate::runtime::EngineCommand>,
    home: std::path::PathBuf,
    root: std::path::PathBuf,
    approvals: Arc<Mutex<RemoteApprovals>>,
    questions: Arc<Mutex<RemoteQuestions>>,
}

impl EngineLink {
    fn new(
        sender: tokio::sync::mpsc::UnboundedSender<crate::runtime::EngineCommand>,
        home: std::path::PathBuf,
        root: std::path::PathBuf,
        approvals: Arc<Mutex<RemoteApprovals>>,
        questions: Arc<Mutex<RemoteQuestions>>,
    ) -> EngineLink {
        EngineLink {
            sender,
            home,
            root,
            approvals,
            questions,
        }
    }

    pub fn list_approvals(&self, thread: Option<&str>) -> Vec<Value> {
        self.approvals
            .lock()
            .map(|mut approvals| approvals.list(thread))
            .unwrap_or_default()
    }

    fn take_approval(&self, request: &str, choice: &str) -> Option<TakenApproval> {
        self.approvals
            .lock()
            .ok()
            .and_then(|mut approvals| approvals.take(request, choice))
    }

    fn put_back_approval(&self, taken: TakenApproval) {
        if let Ok(mut approvals) = self.approvals.lock() {
            approvals.put_back(taken);
        }
    }

    pub fn list_questions(&self, thread: Option<&str>) -> Vec<Value> {
        self.questions
            .lock()
            .map(|mut questions| questions.list(thread))
            .unwrap_or_default()
    }

    fn take_question(
        &self,
        request: &str,
        answers: &[crate::types::QuestionAnswer],
    ) -> std::result::Result<TakenQuestion, QuestionTakeError> {
        self.questions
            .lock()
            .map_err(|_| QuestionTakeError::NotFound)?
            .take(request, answers)
    }

    fn put_back_question(&self, taken: TakenQuestion) {
        if let Ok(mut questions) = self.questions.lock() {
            questions.put_back(taken);
        }
    }

    pub fn submit(
        &self,
        thread_id: &str,
        text: &str,
        permission_mode: crate::types::PermissionMode,
    ) -> bool {
        self.sender
            .send(crate::runtime::EngineCommand::Submit {
                thread_id: thread_id.to_string(),
                text: text.to_string(),
                permission_mode,
                skip_handoff: false,
                expected_provider: None,
                retry: false,
                drain_queued: false,
            })
            .is_ok()
    }

    pub fn environment(&self) -> String {
        crate::claim::installation_id(&self.home).unwrap_or_else(|| "unclaimed".into())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// One provider engine per canonical project root. Engines start lazily when a
/// thread in that project is first used; the initial serve directory starts
/// eagerly so readiness includes a working engine.
#[derive(Clone)]
pub struct EnginePool {
    inner: Arc<Mutex<PoolState>>,
}

struct PoolState {
    store: Option<Arc<Mutex<BatonStore>>>,
    home: PathBuf,
    engines: BTreeMap<PathBuf, PooledEngine>,
}

struct PooledEngine {
    link: EngineLink,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EnginePool {
    fn new(store: Arc<Mutex<BatonStore>>, home: &Path) -> EnginePool {
        EnginePool {
            inner: Arc::new(Mutex::new(PoolState {
                store: Some(store),
                home: home.to_path_buf(),
                engines: BTreeMap::new(),
            })),
        }
    }

    #[cfg(test)]
    fn from_link(link: EngineLink) -> EnginePool {
        EnginePool::from_links(vec![link])
    }

    #[cfg(test)]
    fn from_links(links: Vec<EngineLink>) -> EnginePool {
        let home = links
            .first()
            .map(|link| link.home.clone())
            .unwrap_or_default();
        let mut engines = BTreeMap::new();
        for link in links {
            let root = link
                .root
                .canonicalize()
                .unwrap_or_else(|_| link.root.clone());
            engines.insert(root, PooledEngine { link, thread: None });
        }
        EnginePool {
            inner: Arc::new(Mutex::new(PoolState {
                store: None,
                home,
                engines,
            })),
        }
    }

    fn engine_for(&self, project: &Path) -> Result<EngineLink> {
        let project = project
            .canonicalize()
            .with_context(|| format!("resolve project {}", project.display()))?;
        if !project.is_dir() {
            anyhow::bail!("project {} is not a directory", project.display());
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("engine pool lock poisoned"))?;
        if let Some(engine) = state.engines.get(&project) {
            return Ok(engine.link.clone());
        }
        let Some(store) = state.store.clone() else {
            anyhow::bail!("no engine is registered for {}", project.display());
        };
        let home = state.home.clone();
        let (link, thread) = start_engine(store, &home, &project);
        state.engines.insert(
            project,
            PooledEngine {
                link: link.clone(),
                thread: Some(thread),
            },
        );
        Ok(link)
    }

    fn has_allowed_engine(&self, allowlist: &[String]) -> bool {
        let Ok(state) = self.inner.lock() else {
            return false;
        };
        state
            .engines
            .keys()
            .any(|root| crate::grants::allows_project(allowlist, root))
    }

    fn list_approvals(&self, allowlist: &[String], thread: Option<&str>) -> Vec<Value> {
        let Ok(state) = self.inner.lock() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for (root, engine) in &state.engines {
            if crate::grants::allows_project(allowlist, root) {
                rows.extend(engine.link.list_approvals(thread));
            }
        }
        rows
    }

    fn take_approval(
        &self,
        allowlist: &[String],
        request: &str,
        choice: &str,
    ) -> Option<(EngineLink, TakenApproval)> {
        let state = self.inner.lock().ok()?;
        for (root, engine) in &state.engines {
            if !crate::grants::allows_project(allowlist, root) {
                continue;
            }
            if let Some(taken) = engine.link.take_approval(request, choice) {
                return Some((engine.link.clone(), taken));
            }
        }
        None
    }

    fn list_questions(&self, allowlist: &[String], thread: Option<&str>) -> Vec<Value> {
        let Ok(state) = self.inner.lock() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for (root, engine) in &state.engines {
            if crate::grants::allows_project(allowlist, root) {
                rows.extend(engine.link.list_questions(thread));
            }
        }
        rows
    }

    fn take_question(
        &self,
        allowlist: &[String],
        request: &str,
        answers: &[crate::types::QuestionAnswer],
    ) -> std::result::Result<(EngineLink, TakenQuestion), QuestionTakeError> {
        let state = self.inner.lock().map_err(|_| QuestionTakeError::NotFound)?;
        for (root, engine) in &state.engines {
            if !crate::grants::allows_project(allowlist, root) {
                continue;
            }
            match engine.link.take_question(request, answers) {
                Ok(taken) => return Ok((engine.link.clone(), taken)),
                Err(QuestionTakeError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        Err(QuestionTakeError::NotFound)
    }

    fn quit_all(&self) {
        if let Ok(state) = self.inner.lock() {
            for engine in state.engines.values() {
                let _ = engine.link.sender.send(crate::runtime::EngineCommand::Quit);
            }
        }
    }

    fn stop_all(&self) {
        let engines = {
            let Ok(mut state) = self.inner.lock() else {
                return;
            };
            std::mem::take(&mut state.engines)
        };
        for (_, engine) in engines {
            let _ = engine.link.sender.send(crate::runtime::EngineCommand::Quit);
            if let Some(thread) = engine.thread {
                let _ = thread.join();
            }
        }
    }
}

fn start_engine(
    store: Arc<Mutex<BatonStore>>,
    home: &Path,
    root: &Path,
) -> (EngineLink, std::thread::JoinHandle<()>) {
    let adapters = crate::runtime::build_adapters(&store, home, root);
    let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (engine_event_sender, engine_event_receiver) = std::sync::mpsc::channel();
    let (turn_event_sender, turn_event_receiver) = std::sync::mpsc::channel();
    let approvals = Arc::new(Mutex::new(RemoteApprovals::default()));
    let questions = Arc::new(Mutex::new(RemoteQuestions::default()));
    // The host has no UI to consume engine events. Turn events are read to
    // keep remote interactions alive and to drop them when their turn ends.
    std::thread::spawn(move || for _ in engine_event_receiver {});
    let approval_sink = approvals.clone();
    let question_sink = questions.clone();
    std::thread::spawn(move || {
        for (thread_id, event) in turn_event_receiver {
            match event {
                crate::adapters::TurnEvent::ApprovalRequest {
                    id,
                    kind,
                    summary,
                    choices,
                    respond,
                } => {
                    if let Ok(mut approvals) = approval_sink.lock() {
                        approvals.insert(thread_id, id, kind, summary, choices, respond);
                    }
                }
                crate::adapters::TurnEvent::QuestionRequest {
                    id,
                    questions,
                    respond,
                } => {
                    if questions.is_empty() {
                        let _ = respond.send(Vec::new());
                    } else if let Ok(mut pending) = question_sink.lock() {
                        pending.insert(thread_id, id, questions, respond);
                    }
                }
                crate::adapters::TurnEvent::TurnCompleted { .. }
                | crate::adapters::TurnEvent::TurnFailed { .. }
                | crate::adapters::TurnEvent::TurnInterrupted { .. } => {
                    if let Ok(mut approvals) = approval_sink.lock() {
                        approvals.clear_thread(&thread_id);
                    }
                    if let Ok(mut questions) = question_sink.lock() {
                        questions.clear_thread(&thread_id);
                    }
                }
                _ => {}
            }
        }
    });
    let engine_store = store.clone();
    let engine_home = home.to_path_buf();
    let thread = std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("baton: engine runtime: {error}");
                return;
            }
        };
        let interrupts = runtime.block_on(async {
            let mut handles = Vec::new();
            for adapter in &adapters {
                let adapter = adapter.lock().await;
                handles.push((adapter.id(), adapter.interrupt_handle()));
            }
            handles
        });
        if let Err(error) = runtime.block_on(crate::runtime::run_engine(
            engine_store,
            engine_home,
            adapters,
            interrupts,
            Vec::new(),
            command_receiver,
            engine_event_sender,
            turn_event_sender,
        )) {
            eprintln!("baton: engine stopped: {error}");
        }
    });
    (
        EngineLink::new(
            command_sender,
            home.to_path_buf(),
            root.to_path_buf(),
            approvals,
            questions,
        ),
        thread,
    )
}

/// The host's provider engines, one per project.
pub struct ServeEngine {
    pool: EnginePool,
}

impl ServeEngine {
    pub fn start(store: Arc<Mutex<BatonStore>>, home: &Path, root: &Path) -> Result<ServeEngine> {
        let pool = EnginePool::new(store, home);
        pool.engine_for(root)?;
        Ok(ServeEngine { pool })
    }

    pub fn pool(&self) -> EnginePool {
        self.pool.clone()
    }

    pub fn stop(&mut self) {
        self.pool.stop_all();
    }
}

impl Drop for ServeEngine {
    fn drop(&mut self) {
        self.pool.quit_all();
    }
}

/// Accepts same-user clients until a signal arrives. Every client task is
/// aborted before the socket file is removed, so the store lock can be
/// released and shutdown cannot race a request.
#[cfg(unix)]
pub async fn socket_loop(
    listener: StdUnixListener,
    store: Arc<Mutex<BatonStore>>,
    home: &Path,
    path: &Path,
    engine: Option<EnginePool>,
) -> Result<()> {
    let listener = TokioUnixListener::from_std(listener).context("register the host socket")?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    // The host terminal is the pairing surface. A daemonized host sees EOF
    // immediately and keeps serving with no host commands.
    let (host_sender, mut host_receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if host_sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut stdin_open = true;
    let mut clients = Vec::new();
    let result = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break Ok(()),
            _ = terminate.recv() => break Ok(()),
            line = host_receiver.recv(), if stdin_open => {
                match line {
                    Some(line) => handle_host_command(&store, home, &line),
                    None => stdin_open = false,
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _address)) => {
                        let store = store.clone();
                        let engine = engine.clone();
                        clients.push(tokio::spawn(async move {
                            if let Err(error) = serve_socket_client(store, stream, engine).await {
                                eprintln!("baton: socket client ended: {error}");
                            }
                        }));
                    }
                    Err(error) => break Err(error).context("accept on the host socket"),
                }
            }
        }
    };
    for client in &clients {
        client.abort();
    }
    for client in clients {
        let _ = client.await;
    }
    drop(listener);
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!("baton: socket cleanup: {error}"),
    }
    result
}

#[cfg(not(unix))]
pub async fn socket_loop(
    _listener: (),
    _store: Arc<Mutex<BatonStore>>,
    _home: &Path,
    _path: &Path,
    _engine: Option<EnginePool>,
) -> Result<()> {
    anyhow::bail!("the host socket is Unix-only")
}

#[cfg(unix)]
fn handle_host_command(store: &Arc<Mutex<BatonStore>>, home: &Path, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    let tokens = host_tokens(trimmed);
    let command = tokens.first().map(String::as_str).unwrap_or_default();
    match command {
        "help" => eprintln!(
            "baton: host commands: pair [--label <name>] [--scope <s>]... [--ceiling <mode>] [--project <path>]... [--expires-days N]; renew <id> [--days N]; schedule add --thread <id> --cron \"<expr>\" --prompt \"<text>\"; schedule list; schedule pause <id>; schedule resume <id>; schedule remove <id>; help"
        ),
        "pair" => match parse_pair(tokens.iter().skip(1).map(String::as_str).collect()) {
            Ok(request) => {
                let minted = {
                    let store = store.lock().unwrap();
                    crate::grants::mint(&store, home, &request)
                };
                match minted {
                    Ok(minted) => {
                        println!("{}", pair_url(&minted));
                        eprintln!(
                            "baton: paired {} with {} scopes, ceiling {}, projects {}, expires {}",
                            minted.summary.grant_id,
                            minted.summary.scopes.join(","),
                            minted.summary.permission_ceiling,
                            minted.summary.project_allowlist.len(),
                            minted.summary.expires_at
                        );
                    }
                    Err(error) => eprintln!("baton: pair failed: {error}"),
                }
            }
            Err(error) => eprintln!("baton: {error}"),
        },
        "renew" => match parse_renew(tokens.iter().skip(1).map(String::as_str).collect()) {
            Ok((grant_id, days)) => {
                let renewed = {
                    let store = store.lock().unwrap();
                    crate::grants::renew(&store, &grant_id, days)
                };
                match renewed {
                    Ok(crate::grants::RenewOutcome::Renewed(payload)) => eprintln!(
                        "baton: renewed {} until {} (was {})",
                        payload.grant.grant_id, payload.grant.expires_at, payload.previous_expires_at
                    ),
                    Ok(crate::grants::RenewOutcome::Missing) => {
                        eprintln!("baton: no grant matches {grant_id}")
                    }
                    Ok(crate::grants::RenewOutcome::Revoked) => {
                        eprintln!("baton: {grant_id} is revoked; pair the client again")
                    }
                    Err(error) => eprintln!("baton: renew failed: {error}"),
                }
            }
            Err(error) => eprintln!("baton: {error}"),
        },
        "schedule" => handle_host_schedule(store, home, &tokens[1..]),
        other => eprintln!("baton: unknown host command {other}; type help"),
    }
}

/// Splits a host-terminal line into tokens. A single or double quoted token may
/// contain spaces, and a backslash escapes the next character.
fn host_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(open) = quote {
            if character == open {
                quote = None;
            } else {
                current.push(character);
            }
            continue;
        }
        if character == '"' || character == '\'' {
            quote = Some(character);
            continue;
        }
        if character.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(unix)]
fn handle_host_schedule(store: &Arc<Mutex<BatonStore>>, home: &Path, args: &[String]) {
    let Some(action) = args.first().map(String::as_str) else {
        eprintln!("baton: use schedule add|list|remove");
        return;
    };
    match action {
        "add" => {
            let parsed = match parse_host_schedule_add(&args[1..]) {
                Ok(parsed) => parsed,
                Err(error) => {
                    eprintln!("baton: {error}");
                    return;
                }
            };
            let thread = {
                let guard = store.lock().unwrap();
                match guard.get_thread(&parsed.thread) {
                    Ok(Some(thread)) => thread,
                    Ok(None) => match guard
                        .list_threads(None)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|thread| thread.id.starts_with(&parsed.thread))
                    {
                        Some(thread) => thread,
                        None => {
                            eprintln!("baton: no thread matches {}", parsed.thread);
                            return;
                        }
                    },
                    Err(error) => {
                        eprintln!("baton: cannot read thread: {error}");
                        return;
                    }
                }
            };
            let expression = match crate::schedule::parse(&parsed.cron) {
                Ok(expression) => expression,
                Err(error) => {
                    eprintln!("baton: {error}");
                    return;
                }
            };
            let next = match crate::schedule::next_after(&expression, chrono::Utc::now()) {
                Ok(next) => next.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                Err(error) => {
                    eprintln!("baton: {error}");
                    return;
                }
            };
            let row = {
                let guard = store.lock().unwrap();
                match guard.add_schedule(&thread.id, &parsed.prompt, &parsed.cron, &next) {
                    Ok(row) => row,
                    Err(error) => {
                        eprintln!("baton: cannot add schedule: {error}");
                        return;
                    }
                }
            };
            let audit = crate::audit::event(
                crate::audit::HOST_TERMINAL_ACTOR,
                &crate::claim::installation_id(home).unwrap_or_else(|| "unclaimed".into()),
                Some(&row.id),
                Some(&thread.id),
                crate::audit::AuditAction::ScheduleAdd,
                &serde_json::json!({
                    "thread_id": thread.id,
                    "expression": row.expression,
                    "next_run_at": row.next_run_at,
                }),
            );
            let audit = match audit {
                Ok(audit) => audit,
                Err(error) => {
                    let _ = store.lock().unwrap().remove_schedule(&row.id);
                    eprintln!("baton: cannot audit schedule: {error}; not added");
                    return;
                }
            };
            if let Err(error) = store.lock().unwrap().append_audit_event(&audit) {
                let _ = store.lock().unwrap().remove_schedule(&row.id);
                eprintln!("baton: cannot audit schedule: {error}; not added");
                return;
            }
            eprintln!(
                "baton: schedule {} next {} thread {} {}",
                &row.id[..8.min(row.id.len())],
                row.next_run_at,
                &thread.id[..8.min(thread.id.len())],
                row.expression
            );
        }
        "list" => {
            let schedules = store.lock().unwrap().list_schedules().unwrap_or_default();
            if schedules.is_empty() {
                eprintln!("baton: schedules: empty");
            } else {
                for row in schedules {
                    let prompt: String = row.prompt.chars().take(60).collect();
                    eprintln!(
                        "baton: {} next {} thread {} {} {}",
                        &row.id[..8.min(row.id.len())],
                        row.next_run_at,
                        &row.thread_id[..8.min(row.thread_id.len())],
                        row.expression,
                        prompt
                    );
                }
            }
        }
        "pause" => {
            let Some(prefix) = args.get(1) else {
                eprintln!("baton: schedule pause needs an id");
                return;
            };
            let Some(row) = resolve_host_schedule(store, prefix) else {
                return;
            };
            let paused = store.lock().unwrap().pause_schedule(&row.id);
            match paused {
                Ok(true) => {
                    let audit = crate::audit::event(
                        crate::audit::HOST_TERMINAL_ACTOR,
                        &crate::claim::installation_id(home).unwrap_or_else(|| "unclaimed".into()),
                        Some(&row.id),
                        Some(&row.thread_id),
                        crate::audit::AuditAction::SchedulePause,
                        &serde_json::json!({
                            "thread_id": row.thread_id,
                            "expression": row.expression,
                        }),
                    );
                    match audit.and_then(|audit| store.lock().unwrap().append_audit_event(&audit)) {
                        Ok(_) => {
                            eprintln!("baton: schedule {} paused", &row.id[..8.min(row.id.len())])
                        }
                        Err(error) => eprintln!("baton: schedule paused but audit failed: {error}"),
                    }
                }
                Ok(false) => eprintln!(
                    "baton: schedule {} is already paused",
                    &row.id[..8.min(row.id.len())]
                ),
                Err(error) => eprintln!("baton: cannot pause schedule: {error}"),
            }
        }
        "resume" => {
            let Some(prefix) = args.get(1) else {
                eprintln!("baton: schedule resume needs an id");
                return;
            };
            let Some(row) = resolve_host_schedule(store, prefix) else {
                return;
            };
            if row.enabled {
                eprintln!(
                    "baton: schedule {} is already running",
                    &row.id[..8.min(row.id.len())]
                );
                return;
            }
            let expression = match crate::schedule::parse(&row.expression) {
                Ok(expression) => expression,
                Err(error) => {
                    eprintln!("baton: {error}");
                    return;
                }
            };
            let next = match crate::schedule::next_after(&expression, chrono::Utc::now()) {
                Ok(next) => next.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                Err(error) => {
                    eprintln!("baton: {error}");
                    return;
                }
            };
            let resumed = store.lock().unwrap().resume_schedule(&row.id, &next);
            match resumed {
                Ok(true) => {
                    let audit = crate::audit::event(
                        crate::audit::HOST_TERMINAL_ACTOR,
                        &crate::claim::installation_id(home).unwrap_or_else(|| "unclaimed".into()),
                        Some(&row.id),
                        Some(&row.thread_id),
                        crate::audit::AuditAction::ScheduleResume,
                        &serde_json::json!({
                            "thread_id": row.thread_id,
                            "expression": row.expression,
                            "next_run_at": next,
                        }),
                    );
                    match audit.and_then(|audit| store.lock().unwrap().append_audit_event(&audit)) {
                        Ok(_) => eprintln!(
                            "baton: schedule {} resumed next {next}",
                            &row.id[..8.min(row.id.len())]
                        ),
                        Err(error) => {
                            eprintln!("baton: schedule resumed but audit failed: {error}")
                        }
                    }
                }
                Ok(false) => eprintln!(
                    "baton: schedule {} is already running",
                    &row.id[..8.min(row.id.len())]
                ),
                Err(error) => eprintln!("baton: cannot resume schedule: {error}"),
            }
        }
        "remove" => {
            let Some(prefix) = args.get(1) else {
                eprintln!("baton: schedule remove needs an id");
                return;
            };
            let Some(row) = resolve_host_schedule(store, prefix) else {
                return;
            };
            if let Err(error) = store.lock().unwrap().remove_schedule(&row.id) {
                eprintln!("baton: cannot remove schedule: {error}");
                return;
            }
            let audit = crate::audit::event(
                crate::audit::HOST_TERMINAL_ACTOR,
                &crate::claim::installation_id(home).unwrap_or_else(|| "unclaimed".into()),
                Some(&row.id),
                Some(&row.thread_id),
                crate::audit::AuditAction::ScheduleRemove,
                &serde_json::json!({ "thread_id": row.thread_id }),
            );
            match audit.and_then(|audit| store.lock().unwrap().append_audit_event(&audit)) {
                Ok(_) => eprintln!("baton: schedule {} removed", &row.id[..8.min(row.id.len())]),
                Err(error) => eprintln!("baton: schedule removed but audit failed: {error}"),
            }
        }
        other => eprintln!(
            "baton: unknown schedule action {other}; use add, list, pause, resume, or remove"
        ),
    }
}

#[cfg(unix)]
fn resolve_host_schedule(
    store: &Arc<Mutex<BatonStore>>,
    prefix: &str,
) -> Option<crate::store::ScheduleRow> {
    let schedules = store.lock().unwrap().list_schedules().unwrap_or_default();
    let matches: Vec<_> = schedules
        .into_iter()
        .filter(|row| row.id.starts_with(prefix))
        .collect();
    match matches.as_slice() {
        [] => {
            eprintln!("baton: no schedule matches {prefix}");
            None
        }
        [row] => Some(row.clone()),
        _ => {
            eprintln!("baton: schedule prefix {prefix} is ambiguous");
            None
        }
    }
}

#[cfg(unix)]
fn parse_host_schedule_add(args: &[String]) -> Result<HostScheduleAdd> {
    let mut thread = None;
    let mut cron = None;
    let mut prompt = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--thread" => {
                index += 1;
                thread = args.get(index).cloned();
                if thread.is_none() {
                    anyhow::bail!("--thread needs an id");
                }
            }
            "--cron" => {
                index += 1;
                cron = args.get(index).cloned();
                if cron.is_none() {
                    anyhow::bail!("--cron needs an expression");
                }
            }
            "--prompt" => {
                index += 1;
                prompt = args.get(index).cloned();
                if prompt.is_none() {
                    anyhow::bail!("--prompt needs text");
                }
            }
            other => anyhow::bail!("unknown schedule add option {other}"),
        }
        index += 1;
    }
    Ok(HostScheduleAdd {
        thread: thread.ok_or_else(|| anyhow::anyhow!("schedule add needs --thread"))?,
        cron: cron.ok_or_else(|| anyhow::anyhow!("schedule add needs --cron"))?,
        prompt: prompt.ok_or_else(|| anyhow::anyhow!("schedule add needs --prompt"))?,
    })
}

#[cfg(unix)]
struct HostScheduleAdd {
    thread: String,
    cron: String,
    prompt: String,
}

#[cfg(unix)]
fn parse_renew(args: Vec<&str>) -> Result<(String, i64)> {
    let mut grant_id = None;
    let mut days = crate::grants::DEFAULT_GRANT_DAYS;
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            "--days" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--days needs a number"))?;
                days = value
                    .parse::<i64>()
                    .map_err(|_| anyhow::anyhow!("--days needs a number"))?;
            }
            other if other.starts_with("--") => {
                anyhow::bail!("unknown renew option {other}")
            }
            other => {
                if grant_id.replace(other.to_string()).is_some() {
                    anyhow::bail!("renew takes exactly one grant id");
                }
            }
        }
        index += 1;
    }
    let grant_id = grant_id.ok_or_else(|| anyhow::anyhow!("renew needs a grant id"))?;
    Ok((grant_id, days))
}

#[cfg(unix)]
fn parse_pair(args: Vec<&str>) -> Result<crate::grants::GrantRequest> {
    use crate::grants::{expiry_after_days, GrantRequest, Scope, DEFAULT_GRANT_DAYS};
    use crate::types::PermissionMode;

    let mut scopes = vec![Scope::Read, Scope::Operate];
    let mut scopes_set = false;
    let mut label = None;
    let mut ceiling = PermissionMode::Default;
    let mut projects = Vec::new();
    let mut expires_days = DEFAULT_GRANT_DAYS;
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            "--label" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--label needs a name"))?;
                label = Some((*value).to_string());
            }
            "--scope" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--scope needs a scope"))?;
                let scope =
                    Scope::parse(value).ok_or_else(|| anyhow::anyhow!("unknown scope {value}"))?;
                if !scopes_set {
                    scopes.clear();
                    scopes_set = true;
                }
                if !scopes.contains(&scope) {
                    scopes.push(scope);
                }
            }
            "--ceiling" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--ceiling needs a mode"))?;
                if *value == "bypassPermissions" {
                    anyhow::bail!("ceiling: bypassPermissions is not grantable");
                }
                ceiling = PermissionMode::parse(value)
                    .ok_or_else(|| anyhow::anyhow!("unknown ceiling {value}"))?;
            }
            "--project" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--project needs a path"))?;
                let path = Path::new(value);
                if !path.is_absolute() {
                    anyhow::bail!("--project must be absolute: {value}");
                }
                let canonical = path
                    .canonicalize()
                    .with_context(|| format!("resolve project {}", path.display()))?;
                let canonical = canonical.to_string_lossy().to_string();
                if !projects.contains(&canonical) {
                    projects.push(canonical);
                }
            }
            "--expires-days" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--expires-days needs a number"))?;
                expires_days = value
                    .parse::<i64>()
                    .map_err(|_| anyhow::anyhow!("--expires-days needs a number"))?;
                if expires_days <= 0 {
                    anyhow::bail!("--expires-days must be positive");
                }
            }
            other => anyhow::bail!("unknown pair option {other}"),
        }
        index += 1;
    }
    let mut request = GrantRequest::new(scopes);
    request.label = label;
    request.permission_ceiling = ceiling;
    request.project_allowlist = projects;
    request.expires_at = expiry_after_days(expires_days);
    Ok(request)
}

#[cfg(unix)]
fn pair_url(minted: &crate::grants::MintedGrant) -> String {
    format!(
        "baton://local#grant={}&secret={}",
        minted.summary.grant_id, minted.secret
    )
}

#[cfg(unix)]
async fn serve_socket_client(
    store: Arc<Mutex<BatonStore>>,
    stream: UnixStream,
    engine: Option<EnginePool>,
) -> Result<()> {
    let peer = socket_peer_uid(&stream)?;
    let host = unsafe { libc::getuid() };
    if !same_user(peer, host) {
        let response = error_response(
            Value::Null,
            "peer_uid",
            "the client does not run as the host user",
        );
        let mut stream = stream;
        let mut text = serde_json::to_string(&response)?;
        text.push('\n');
        let _ = stream.write_all(text.as_bytes()).await;
        return Ok(());
    }
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let response = {
            let store = store.lock().unwrap();
            handle_socket_line(&store, &line, engine.as_ref())
        };
        let mut text = serde_json::to_string(&response)?;
        text.push('\n');
        write.write_all(text.as_bytes()).await?;
    }
    Ok(())
}

/// Socket requests carry a grant id and secret. `ping` is the only request
/// answered without one, so a readiness probe needs no credential.
fn handle_socket_line(store: &BatonStore, line: &str, engine: Option<&EnginePool>) -> Value {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return error_response(Value::Null, "malformed_line", "line is not JSON");
    };
    if value["v"].as_u64() != Some(PROTOCOL_VERSION) {
        return error_response(Value::Null, "bad_version", "unsupported protocol version");
    }
    let id = value["id"].clone();
    let command = value["command"].as_str().unwrap_or_default();
    let mut grant = None;
    if command != "ping" {
        let (Some(grant_id), Some(secret)) = (value["grant_id"].as_str(), value["secret"].as_str())
        else {
            return error_response(id, "unauthenticated", "grant_id and secret are required");
        };
        let required = match command {
            "submit" => "operate",
            "respond" | "answer" => "approve",
            _ => "read",
        };
        match crate::grants::verify(store, grant_id, secret) {
            Ok(verified) => {
                if !verified.scopes.iter().any(|scope| scope == required) {
                    return error_response(
                        id,
                        "not_permitted",
                        format!("the grant lacks the {required} scope"),
                    );
                }
                grant = Some(verified);
            }
            Err(error) => {
                if let Some(refusal) = error.downcast_ref::<crate::grants::GrantRefusal>() {
                    return error_response(id, refusal.code(), refusal.to_string());
                }
                return error_response(id, "internal", error.to_string());
            }
        }
    }
    if command == "submit" {
        let grant = grant.expect("submit requires a verified grant");
        return submit_request(store, engine, &grant, id, &value);
    }
    if command == "respond" {
        let grant = grant.expect("respond requires a verified grant");
        return respond_request(store, engine, &grant, id, &value);
    }
    if command == "approvals" {
        let grant = grant.expect("approvals requires a verified grant");
        return approvals_request(engine, &grant, id, &value);
    }
    if command == "answer" {
        let grant = grant.expect("answer requires a verified grant");
        return answer_request(store, engine, &grant, id, &value);
    }
    if command == "questions" {
        let grant = grant.expect("questions requires a verified grant");
        return questions_request(engine, &grant, id, &value);
    }
    handle_line(store, line).unwrap_or_else(|| {
        error_response(
            id,
            "not_implemented",
            format!("command {command} is not implemented"),
        )
    })
}

const SUBMIT_OPERATION: &str = "submit";
const RESPOND_OPERATION: &str = "respond";
const ANSWER_OPERATION: &str = "answer";

fn parse_question_answers(
    args: &Map<String, Value>,
) -> std::result::Result<Vec<crate::types::QuestionAnswer>, String> {
    let Some(Value::Object(answers)) = args.get("answers") else {
        return Err("answer args.answers must be an object mapped by prompt id".into());
    };
    let mut parsed = Vec::with_capacity(answers.len());
    for (prompt_id, value) in answers {
        if prompt_id.is_empty()
            || prompt_id.chars().count() > 128
            || prompt_id.chars().any(char::is_control)
        {
            return Err(
                "answer prompt ids must be non-empty strings of at most 128 characters".into(),
            );
        }
        let values = match value {
            Value::String(text) => {
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![text.clone()]
                }
            }
            Value::Array(items) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "answer values must be strings".to_string())
                })
                .collect::<std::result::Result<Vec<_>, _>>()?,
            _ => return Err("answer values must be a string or an array of strings".into()),
        };
        for text in &values {
            if text.chars().count() > 4096 || text.chars().any(char::is_control) {
                return Err(
                    "answer text is at most 4096 characters and cannot contain control characters"
                        .into(),
                );
            }
        }
        parsed.push(crate::types::QuestionAnswer {
            id: prompt_id.clone(),
            answers: values,
        });
    }
    Ok(parsed)
}

fn request_key<'a>(args: &'a Map<String, Value>, id: &Value) -> Result<&'a str, Value> {
    match args.get("idempotency_key") {
        Some(Value::String(key))
            if !key.is_empty()
                && key.chars().count() <= 128
                && !key.chars().any(char::is_control) =>
        {
            Ok(key.as_str())
        }
        _ => Err(error_response(
            id.clone(),
            "malformed_line",
            "args.idempotency_key must be a non-empty string of at most 128 characters",
        )),
    }
}

/// A replay returns the original result with `replayed: true`, so a client can
/// tell that no second turn started.
fn replay_response(id: Value, stored: &str) -> Value {
    match serde_json::from_str::<Value>(stored) {
        Ok(mut body @ Value::Object(_)) => {
            body["replayed"] = json!(true);
            response(id, body)
        }
        Ok(_) => error_response(id, "internal", "stored response is not an object"),
        Err(error) => error_response(
            id,
            "internal",
            format!("stored response is not JSON: {error}"),
        ),
    }
}

fn submit_request(
    store: &BatonStore,
    engine: Option<&EnginePool>,
    grant: &crate::store::GrantSummary,
    id: Value,
    value: &Value,
) -> Value {
    let Some(engine) = engine else {
        return error_response(
            id,
            "not_implemented",
            "this host has no engine; start baton serve without --stdio",
        );
    };
    let args = match value.get("args") {
        Some(Value::Object(args)) => args,
        _ => return error_response(id, "malformed_line", "submit args must be an object"),
    };
    let thread_id = match args.get("thread") {
        Some(Value::String(thread)) if !thread.is_empty() => thread.as_str(),
        _ => {
            return error_response(
                id,
                "malformed_line",
                "submit args.thread must be a non-empty string",
            )
        }
    };
    let text = match args.get("text") {
        Some(Value::String(text)) if !text.trim().is_empty() => text.as_str(),
        _ => {
            return error_response(
                id,
                "malformed_line",
                "submit args.text must be a non-empty string",
            )
        }
    };
    let key = match request_key(args, &id) {
        Ok(key) => key,
        Err(error) => return error,
    };
    match store.replay_idempotent_request(key, SUBMIT_OPERATION) {
        Ok(Some(stored)) => return replay_response(id, &stored),
        Ok(None) => {}
        Err(error) => return error_response(id, "idempotency_conflict", error.to_string()),
    }
    let thread = match store.get_thread(thread_id) {
        Ok(Some(thread)) => thread,
        Ok(None) => {
            return error_response(id, "not_found", format!("no thread matches {thread_id}"))
        }
        Err(error) => return error_response(id, "internal", error.to_string()),
    };
    let cwd = std::fs::canonicalize(&thread.cwd).unwrap_or_else(|_| thread.cwd.clone().into());
    if !crate::grants::allows_project(&grant.project_allowlist, &cwd) {
        return error_response(
            id,
            "not_permitted",
            format!("the grant does not allow project {}", cwd.display()),
        );
    }
    if thread.permission_mode == crate::types::PermissionMode::BypassPermissions {
        return error_response(
            id,
            "ceiling",
            "the thread runs with bypassPermissions; set a safe mode before submitting remotely",
        );
    }
    let ceiling = match crate::types::PermissionMode::parse(&grant.permission_ceiling) {
        Some(ceiling) => ceiling,
        None => {
            return error_response(
                id,
                "ceiling",
                "the grant carries an unknown permission ceiling",
            )
        }
    };
    if !thread.permission_mode.within_ceiling(ceiling) {
        return error_response(
            id,
            "ceiling",
            format!(
                "thread {thread_id} runs with {} above the grant ceiling {ceiling}",
                thread.permission_mode
            ),
        );
    }
    let engine = match engine.engine_for(&cwd) {
        Ok(engine) => engine,
        Err(error) => {
            return error_response(
                id,
                "wrong_project",
                format!("cannot use project {}: {error}", cwd.display()),
            )
        }
    };
    let body = json!({ "thread_id": thread_id, "accepted": true });
    match store.claim_idempotent_request(key, SUBMIT_OPERATION, thread_id, &body.to_string()) {
        Ok(Some(stored)) => return replay_response(id, &stored),
        Ok(None) => {}
        Err(error) => return error_response(id, "idempotency_conflict", error.to_string()),
    }
    let audit = match crate::audit::grant_event(
        &grant.grant_id,
        &engine.environment(),
        Some(thread_id),
        Some(thread_id),
        crate::audit::AuditAction::TurnSubmitRequest,
        &json!({
            "thread_id": thread_id,
            "text_len": text.chars().count(),
        }),
    ) {
        Ok(audit) => audit,
        Err(error) => {
            let _ = store.forget_idempotent_request(key, SUBMIT_OPERATION);
            return error_response(id, "internal", error.to_string());
        }
    };
    if let Err(error) = store.append_audit_event(&audit) {
        let _ = store.forget_idempotent_request(key, SUBMIT_OPERATION);
        return error_response(id, "internal", error.to_string());
    }
    if !engine.submit(thread_id, text, thread.permission_mode) {
        let _ = store.forget_idempotent_request(key, SUBMIT_OPERATION);
        return error_response(id, "internal", "the host engine is not running");
    }
    response(id, body)
}

fn approvals_request(
    engine: Option<&EnginePool>,
    grant: &crate::store::GrantSummary,
    id: Value,
    value: &Value,
) -> Value {
    let Some(engine) = engine else {
        return error_response(
            id,
            "not_implemented",
            "this host has no engine; start baton serve without --stdio",
        );
    };
    if !engine.has_allowed_engine(&grant.project_allowlist) {
        return error_response(
            id,
            "not_permitted",
            "the grant does not allow any served project",
        );
    }
    let thread = match value.get("args") {
        None | Some(Value::Null) => None,
        Some(Value::Object(args)) => match args.get("thread") {
            None | Some(Value::Null) => None,
            Some(Value::String(thread)) if !thread.is_empty() => Some(thread.as_str()),
            _ => {
                return error_response(
                    id,
                    "malformed_line",
                    "approvals args.thread must be a non-empty string",
                )
            }
        },
        _ => return error_response(id, "malformed_line", "approvals args must be an object"),
    };
    response(
        id,
        json!({ "approvals": engine.list_approvals(&grant.project_allowlist, thread) }),
    )
}

fn respond_request(
    store: &BatonStore,
    engine: Option<&EnginePool>,
    grant: &crate::store::GrantSummary,
    id: Value,
    value: &Value,
) -> Value {
    let Some(engine) = engine else {
        return error_response(
            id,
            "not_implemented",
            "this host has no engine; start baton serve without --stdio",
        );
    };
    if !engine.has_allowed_engine(&grant.project_allowlist) {
        return error_response(
            id,
            "not_permitted",
            "the grant does not allow any served project",
        );
    }
    let args = match value.get("args") {
        Some(Value::Object(args)) => args,
        _ => return error_response(id, "malformed_line", "respond args must be an object"),
    };
    let request_id = match args.get("request") {
        Some(Value::String(request))
            if !request.is_empty()
                && request.chars().count() <= 256
                && !request.chars().any(char::is_control) =>
        {
            request.as_str()
        }
        _ => {
            return error_response(
                id,
                "malformed_line",
                "respond args.request must be a non-empty string of at most 256 characters",
            )
        }
    };
    let choice_id = match args.get("choice") {
        Some(Value::String(choice))
            if !choice.is_empty()
                && choice.chars().count() <= 128
                && !choice.chars().any(char::is_control) =>
        {
            choice.as_str()
        }
        _ => {
            return error_response(
                id,
                "malformed_line",
                "respond args.choice must be a non-empty string of at most 128 characters",
            )
        }
    };
    let key = match request_key(args, &id) {
        Ok(key) => key,
        Err(error) => return error,
    };
    match store.replay_idempotent_request(key, RESPOND_OPERATION) {
        Ok(Some(stored)) => return replay_response(id, &stored),
        Ok(None) => {}
        Err(error) => return error_response(id, "idempotency_conflict", error.to_string()),
    }
    let Some((engine, taken)) =
        engine.take_approval(&grant.project_allowlist, request_id, choice_id)
    else {
        return match store.replay_idempotent_request(key, RESPOND_OPERATION) {
            Ok(Some(stored)) => replay_response(id, &stored),
            Ok(None) => error_response(
                id,
                "not_found",
                format!("no pending approval matches {request_id} with choice {choice_id}"),
            ),
            Err(error) => error_response(id, "idempotency_conflict", error.to_string()),
        };
    };
    let decision = taken.choice.decision();
    let body = json!({
        "request_id": taken.pending.id,
        "thread_id": taken.pending.thread_id,
        "choice": taken.choice.id,
        "decision": decision.as_str(),
        "answered": true,
    });
    match store.claim_idempotent_request(
        key,
        RESPOND_OPERATION,
        &taken.pending.thread_id,
        &body.to_string(),
    ) {
        Ok(Some(stored)) => {
            engine.put_back_approval(taken);
            return replay_response(id, &stored);
        }
        Ok(None) => {}
        Err(error) => {
            engine.put_back_approval(taken);
            return error_response(id, "idempotency_conflict", error.to_string());
        }
    }
    let audit = match crate::audit::grant_event(
        &grant.grant_id,
        &engine.environment(),
        Some(&taken.pending.id),
        Some(&taken.pending.thread_id),
        crate::audit::AuditAction::ApprovalDecide,
        &json!({
            "request_id": taken.pending.id,
            "kind": taken.pending.kind,
            "decision": decision.as_str(),
            "label": taken.choice.label,
        }),
    ) {
        Ok(audit) => audit,
        Err(error) => {
            engine.put_back_approval(taken);
            let _ = store.forget_idempotent_request(key, RESPOND_OPERATION);
            return error_response(id, "internal", error.to_string());
        }
    };
    if let Err(error) = store.append_audit_event(&audit) {
        engine.put_back_approval(taken);
        let _ = store.forget_idempotent_request(key, RESPOND_OPERATION);
        return error_response(id, "internal", error.to_string());
    }
    let _ = taken.pending.respond.send(decision);
    response(id, body)
}

fn questions_request(
    engine: Option<&EnginePool>,
    grant: &crate::store::GrantSummary,
    id: Value,
    value: &Value,
) -> Value {
    let Some(engine) = engine else {
        return error_response(
            id,
            "not_implemented",
            "this host has no engine; start baton serve without --stdio",
        );
    };
    if !engine.has_allowed_engine(&grant.project_allowlist) {
        return error_response(
            id,
            "not_permitted",
            "the grant does not allow any served project",
        );
    }
    let thread = match value.get("args") {
        None | Some(Value::Null) => None,
        Some(Value::Object(args)) => match args.get("thread") {
            None | Some(Value::Null) => None,
            Some(Value::String(thread)) if !thread.is_empty() => Some(thread.as_str()),
            _ => {
                return error_response(
                    id,
                    "malformed_line",
                    "questions args.thread must be a non-empty string",
                )
            }
        },
        _ => return error_response(id, "malformed_line", "questions args must be an object"),
    };
    response(
        id,
        json!({ "questions": engine.list_questions(&grant.project_allowlist, thread) }),
    )
}

fn answer_request(
    store: &BatonStore,
    engine: Option<&EnginePool>,
    grant: &crate::store::GrantSummary,
    id: Value,
    value: &Value,
) -> Value {
    let Some(engine) = engine else {
        return error_response(
            id,
            "not_implemented",
            "this host has no engine; start baton serve without --stdio",
        );
    };
    if !engine.has_allowed_engine(&grant.project_allowlist) {
        return error_response(
            id,
            "not_permitted",
            "the grant does not allow any served project",
        );
    }
    let args = match value.get("args") {
        Some(Value::Object(args)) => args,
        _ => return error_response(id, "malformed_line", "answer args must be an object"),
    };
    let request_id = match args.get("request") {
        Some(Value::String(request))
            if !request.is_empty()
                && request.chars().count() <= 256
                && !request.chars().any(char::is_control) =>
        {
            request.as_str()
        }
        _ => {
            return error_response(
                id,
                "malformed_line",
                "answer args.request must be a non-empty string of at most 256 characters",
            )
        }
    };
    let answers = match parse_question_answers(args) {
        Ok(answers) => answers,
        Err(error) => return error_response(id, "malformed_line", error),
    };
    let key = match request_key(args, &id) {
        Ok(key) => key,
        Err(error) => return error,
    };
    match store.replay_idempotent_request(key, ANSWER_OPERATION) {
        Ok(Some(stored)) => return replay_response(id, &stored),
        Ok(None) => {}
        Err(error) => return error_response(id, "idempotency_conflict", error.to_string()),
    }
    let (engine, taken) = match engine.take_question(&grant.project_allowlist, request_id, &answers)
    {
        Ok(taken) => taken,
        Err(QuestionTakeError::NotFound) => {
            return match store.replay_idempotent_request(key, ANSWER_OPERATION) {
                Ok(Some(stored)) => replay_response(id, &stored),
                Ok(None) => error_response(
                    id,
                    "not_found",
                    format!("no pending question matches {request_id}"),
                ),
                Err(error) => error_response(id, "idempotency_conflict", error.to_string()),
            }
        }
        Err(QuestionTakeError::Invalid(error)) => {
            return error_response(id, "malformed_line", error)
        }
    };
    let body = json!({
        "request_id": taken.pending.id,
        "thread_id": taken.pending.thread_id,
        "prompts": taken.pending.questions.len(),
        "answered": true,
    });
    match store.claim_idempotent_request(
        key,
        ANSWER_OPERATION,
        &taken.pending.thread_id,
        &body.to_string(),
    ) {
        Ok(Some(stored)) => {
            engine.put_back_question(taken);
            return replay_response(id, &stored);
        }
        Ok(None) => {}
        Err(error) => {
            engine.put_back_question(taken);
            return error_response(id, "idempotency_conflict", error.to_string());
        }
    }
    let audit = match crate::audit::grant_event(
        &grant.grant_id,
        &engine.environment(),
        Some(&taken.pending.id),
        Some(&taken.pending.thread_id),
        crate::audit::AuditAction::QuestionAnswer,
        &crate::audit::question_answer_payload(
            &taken.pending.id,
            &taken.pending.questions,
            &taken.answers,
        ),
    ) {
        Ok(audit) => audit,
        Err(error) => {
            engine.put_back_question(taken);
            let _ = store.forget_idempotent_request(key, ANSWER_OPERATION);
            return error_response(id, "internal", error.to_string());
        }
    };
    if let Err(error) = store.append_audit_event(&audit) {
        engine.put_back_question(taken);
        let _ = store.forget_idempotent_request(key, ANSWER_OPERATION);
        return error_response(id, "internal", error.to_string());
    }
    let _ = taken.pending.respond.send(taken.answers);
    response(id, body)
}

#[cfg(unix)]
fn same_user(peer_uid: u32, host_uid: u32) -> bool {
    peer_uid == host_uid
}

#[cfg(target_os = "linux")]
fn socket_peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    use std::os::unix::io::AsRawFd;
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut libc::ucred as *mut libc::c_void,
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

#[cfg(target_os = "macos")]
fn socket_peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    use std::os::unix::io::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(uid)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn socket_peer_uid(_stream: &UnixStream) -> std::io::Result<u32> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "peer credentials are unavailable on this platform",
    ))
}

fn response(id: Value, result: Value) -> Value {
    json!({
        "v": PROTOCOL_VERSION,
        "kind": "response",
        "id": id,
        "ok": true,
        "result": result,
    })
}

fn error_response(id: Value, code: &str, message: impl Into<String>) -> Value {
    json!({
        "v": PROTOCOL_VERSION,
        "kind": "response",
        "id": id,
        "ok": false,
        "error": {
            "code": code,
            "message": message.into(),
        },
    })
}

fn claim_json(claim: &HostClaimRow) -> Value {
    json!({
        "host_id": claim.host_id,
        "kind": claim.kind,
        "installation_id": claim.installation_id,
        "pid": claim.pid,
        "process_start": claim.process_start,
        "claimed_at": claim.claimed_at,
        "heartbeat_at": claim.heartbeat_at,
    })
}

fn status(store: &BatonStore, args: Option<&Value>) -> Result<Value> {
    let thread = match args {
        None | Some(Value::Null) => None,
        Some(Value::Object(args)) => match args.get("thread") {
            None | Some(Value::Null) => None,
            Some(Value::String(thread)) => Some(thread.as_str()),
            Some(_) => anyhow::bail!("status args.thread must be a string or null"),
        },
        Some(_) => anyhow::bail!("status args must be an object"),
    };
    match thread {
        Some(thread) => {
            if store.get_thread(thread)?.is_none() {
                return Ok(error_response(
                    Value::Null,
                    "not_found",
                    format!("no thread matches {thread}"),
                ));
            }
            Ok(response(
                Value::Null,
                json!({
                    "scope": "thread",
                    "threads": 1,
                    "running_turns": store.count_open_turns(Some(thread))?,
                }),
            ))
        }
        None => Ok(response(
            Value::Null,
            json!({
                "scope": "host",
                "threads": store.list_threads(None)?.len(),
                "running_turns": store.count_open_turns(None)?,
            }),
        )),
    }
}

fn message_json(message: &crate::store::MessageRow) -> Value {
    json!({
        "id": message.id,
        "turn": message.turn,
        "role": message.role,
        "provider": message.provider,
        "model": message.model,
        "text": message.text,
        "created_at": message.created_at,
    })
}

fn messages(store: &BatonStore, args: Option<&Value>) -> Result<Value> {
    let args = match args {
        None | Some(Value::Null) => anyhow::bail!("messages args must name a thread"),
        Some(Value::Object(args)) => args,
        Some(_) => anyhow::bail!("messages args must be an object"),
    };
    let thread = match args.get("thread") {
        Some(Value::String(thread)) if !thread.is_empty() => thread.as_str(),
        _ => anyhow::bail!("messages args.thread must be a non-empty string"),
    };
    let after = match args.get("after") {
        None | Some(Value::Null) => 0,
        Some(value) => value
            .as_i64()
            .filter(|after| *after >= 0)
            .ok_or_else(|| anyhow::anyhow!("messages args.after must be a non-negative integer"))?,
    };
    let limit = match args.get("limit") {
        None | Some(Value::Null) => 50,
        Some(value) => value
            .as_u64()
            .map(|limit| (limit as usize).clamp(1, 200))
            .ok_or_else(|| anyhow::anyhow!("messages args.limit must be an integer"))?,
    };
    if store.get_thread(thread)?.is_none() {
        return Ok(error_response(
            Value::Null,
            "not_found",
            format!("no thread matches {thread}"),
        ));
    }
    let rows = store.list_messages_after(thread, after, limit)?;
    let cursor = rows.last().map(|message| message.id).unwrap_or(after);
    Ok(response(
        Value::Null,
        json!({
            "thread_id": thread,
            "cursor": cursor,
            "running_turns": store.count_open_turns(Some(thread))?,
            "messages": rows.iter().map(message_json).collect::<Vec<_>>(),
        }),
    ))
}

/// Handles one protocol line. `None` means the line produced no response.
pub fn handle_line(store: &BatonStore, line: &str) -> Option<Value> {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => {
            return Some(error_response(
                Value::Null,
                "malformed_line",
                format!("invalid JSON: {error}"),
            ))
        }
    };
    let version = value.get("v").and_then(Value::as_u64);
    if version != Some(PROTOCOL_VERSION) {
        return Some(error_response(
            Value::Null,
            "bad_version",
            format!("expected protocol version {PROTOCOL_VERSION}"),
        ));
    }
    let object = match value.as_object() {
        Some(object) => object,
        None => {
            return Some(error_response(
                Value::Null,
                "malformed_line",
                "request must be a JSON object",
            ))
        }
    };
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    if object.get("kind").and_then(Value::as_str) != Some("request") {
        return Some(error_response(
            id,
            "not_implemented",
            "only request messages are supported",
        ));
    }
    let Some(command) = object.get("command").and_then(Value::as_str) else {
        return Some(error_response(
            id,
            "malformed_line",
            "request.command is required",
        ));
    };
    let result = match command {
        "ping" => Some(response(id.clone(), json!({ "pong": true }))),
        "claim-info" => match store.get_host_claim() {
            Ok(claim) => Some(response(
                id.clone(),
                claim.map(|claim| claim_json(&claim)).unwrap_or(Value::Null),
            )),
            Err(error) => Some(error_response(id.clone(), "internal", error.to_string())),
        },
        "status" => match status(store, object.get("args")) {
            Ok(mut value) => {
                value["id"] = id.clone();
                Some(value)
            }
            Err(error) => Some(error_response(id.clone(), "internal", error.to_string())),
        },
        "messages" => match messages(store, object.get("args")) {
            Ok(mut value) => {
                value["id"] = id.clone();
                Some(value)
            }
            Err(error) => Some(error_response(id.clone(), "internal", error.to_string())),
        },
        _ => Some(error_response(
            id.clone(),
            "not_implemented",
            format!("command {command} is not implemented"),
        )),
    };
    result
}

pub async fn stdio_loop(store: &BatonStore) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        let next = async { lines.next_line().await };
        #[cfg(unix)]
        let line = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
            line = next => line,
        };
        #[cfg(not(unix))]
        let line = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            line = next => line,
        };
        let line = match line {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                eprintln!("baton: protocol read failed: {error}");
                break;
            }
        };
        if let Some(value) = handle_line(store, &line) {
            let bytes = format!("{}\n", serde_json::to_string(&value)?);
            if stdout.write_all(bytes.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    }
    Ok(())
}

/// Runs the due-schedule tick for the host. The claim is a compare-and-set on
/// the due `next_run_at`, so a second scheduler that sees the same row loses.
pub async fn schedule_loop(store: Arc<Mutex<BatonStore>>, home: PathBuf, pool: EnginePool) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        interval.tick().await;
        if let Err(error) = fire_due_schedules(&store, &home, &pool) {
            eprintln!("baton: scheduler: {error}");
        }
    }
}

pub fn fire_due_schedules(
    store: &Arc<Mutex<BatonStore>>,
    home: &Path,
    pool: &EnginePool,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now();
    let now_text = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let due = store.lock().unwrap().due_schedules_at(&now_text)?;
    for row in due {
        let short = &row.id[..8.min(row.id.len())];
        let expression = match crate::schedule::parse(&row.expression) {
            Ok(expression) => expression,
            Err(error) => {
                let retry = (now + chrono::Duration::hours(1))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                let _ = store
                    .lock()
                    .unwrap()
                    .claim_schedule(&row.id, &row.next_run_at, &retry);
                eprintln!("baton: schedule {short} has an invalid expression: {error}");
                continue;
            }
        };
        let next = match crate::schedule::next_after(&expression, now) {
            Ok(next) => next.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            Err(error) => {
                let retry = (now + chrono::Duration::hours(1))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                let _ = store
                    .lock()
                    .unwrap()
                    .claim_schedule(&row.id, &row.next_run_at, &retry);
                eprintln!("baton: schedule {short} cannot compute its next run: {error}");
                continue;
            }
        };
        if !store
            .lock()
            .unwrap()
            .claim_schedule(&row.id, &row.next_run_at, &next)?
        {
            continue;
        }
        let thread = store.lock().unwrap().get_thread(&row.thread_id)?;
        let Some(thread) = thread else {
            eprintln!(
                "baton: schedule {short} points at missing thread {}",
                &row.thread_id[..8.min(row.thread_id.len())]
            );
            continue;
        };
        if thread.archived_at.is_some() {
            eprintln!("baton: schedule {short} skipped; its thread is archived");
            continue;
        }
        let payload = serde_json::json!({
            "thread_id": thread.id,
            "expression": row.expression,
            "fired_for": row.next_run_at,
            "next_run_at": next,
        });
        let audit = crate::audit::event(
            crate::audit::SCHEDULER_ACTOR,
            &crate::claim::installation_id(home).unwrap_or_else(|| "unclaimed".into()),
            Some(&row.id),
            Some(&thread.id),
            crate::audit::AuditAction::ScheduleTrigger,
            &payload,
        );
        let audit = match audit {
            Ok(audit) => audit,
            Err(error) => {
                eprintln!("baton: schedule {short} audit failed: {error}; run skipped");
                continue;
            }
        };
        if let Err(error) = store.lock().unwrap().append_audit_event(&audit) {
            eprintln!("baton: schedule {short} audit failed: {error}; run skipped");
            continue;
        }
        let link = pool.engine_for(Path::new(&thread.cwd))?;
        let text = match (&thread.goal, thread.goal_paused) {
            (Some(goal), false) => format!("[goal: {goal}]\n{}", row.prompt),
            _ => row.prompt.clone(),
        };
        if !link.submit(&thread.id, &text, thread.permission_mode) {
            eprintln!(
                "baton: schedule {short} could not start a turn for {}",
                &thread.id[..8.min(thread.id.len())]
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim::{acquire, ClaimOutcome, SystemProbe};
    use crate::store::NewThread;
    use crate::types::ProviderId;

    fn scheduled_thread(store: &BatonStore, work: &std::path::Path) -> crate::store::ThreadRow {
        store
            .create_thread(NewThread {
                title: "scheduled".into(),
                cwd: work.to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap()
    }

    fn due_expression() -> String {
        (chrono::Utc::now() - chrono::Duration::minutes(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn fake_pool(
        home: &std::path::Path,
        work: &std::path::Path,
    ) -> (
        EnginePool,
        tokio::sync::mpsc::UnboundedReceiver<crate::runtime::EngineCommand>,
    ) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let link = EngineLink::new(
            sender,
            home.to_path_buf(),
            work.to_path_buf(),
            Arc::new(Mutex::new(RemoteApprovals::default())),
            Arc::new(Mutex::new(RemoteQuestions::default())),
        );
        (EnginePool::from_link(link), receiver)
    }

    #[test]
    fn fire_due_schedules_submits_and_advances() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = scheduled_thread(&store.lock().unwrap(), work.path());
        let due = due_expression();
        let row = store
            .lock()
            .unwrap()
            .add_schedule(&thread.id, "run it", "* * * * *", &due)
            .unwrap();
        let (pool, mut receiver) = fake_pool(home.path(), work.path());
        fire_due_schedules(&store, home.path(), &pool).unwrap();
        match receiver.try_recv() {
            Ok(crate::runtime::EngineCommand::Submit {
                thread_id, text, ..
            }) => {
                assert_eq!(thread_id, thread.id);
                assert_eq!(text, "run it");
            }
            other => panic!("expected a schedule submit, got {:?}", other.is_ok()),
        }
        let rows = store.lock().unwrap().list_schedules().unwrap();
        assert_eq!(rows[0].last_run_at.as_deref(), Some(due.as_str()));
        assert_ne!(rows[0].next_run_at, due);
        let audit = store.lock().unwrap().list_audit_events(10).unwrap();
        assert!(audit
            .iter()
            .any(|event| event.action == "schedule.trigger"
                && event.target.as_deref() == Some(&row.id)));
        fire_due_schedules(&store, home.path(), &pool).unwrap();
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn fire_due_schedules_skips_an_archived_thread() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = scheduled_thread(&store.lock().unwrap(), work.path());
        assert_eq!(
            store.lock().unwrap().archive_thread(&thread.id).unwrap(),
            crate::store::ArchiveOutcome::Archived
        );
        store
            .lock()
            .unwrap()
            .add_schedule(&thread.id, "run it", "* * * * *", &due_expression())
            .unwrap();
        let (pool, mut receiver) = fake_pool(home.path(), work.path());
        fire_due_schedules(&store, home.path(), &pool).unwrap();
        assert!(receiver.try_recv().is_err());
        assert!(!store
            .lock()
            .unwrap()
            .list_audit_events(10)
            .unwrap()
            .iter()
            .any(|event| event.action == "schedule.trigger"));
    }

    #[test]
    fn host_tokens_keep_quoted_spaces() {
        assert_eq!(
            host_tokens("schedule add --prompt \"check the tests\" --cron '0 * * * *'"),
            vec![
                "schedule",
                "add",
                "--prompt",
                "check the tests",
                "--cron",
                "0 * * * *"
            ]
        );
        assert_eq!(host_tokens(r#"a\ b "c d""#), vec!["a b", "c d"]);
    }

    #[cfg(unix)]
    #[test]
    fn host_schedule_add_list_remove() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = scheduled_thread(&store.lock().unwrap(), work.path());
        handle_host_command(
            &store,
            home.path(),
            &format!(
                "schedule add --thread {} --cron \"*/5 * * * *\" --prompt \"check the tests\"",
                thread.id
            ),
        );
        let rows = store.lock().unwrap().list_schedules().unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].prompt, "check the tests");
        assert_eq!(rows[0].expression, "*/5 * * * *");
        let id = rows[0].id.clone();
        handle_host_command(&store, home.path(), "schedule list");
        handle_host_command(
            &store,
            home.path(),
            &format!("schedule remove {}", &id[..8]),
        );
        assert!(store.lock().unwrap().list_schedules().unwrap().is_empty());
        let audit = store.lock().unwrap().list_audit_events(10).unwrap();
        assert!(audit.iter().any(|event| {
            event.action == "schedule.add"
                && event.actor == crate::audit::HOST_TERMINAL_ACTOR
                && event.target.as_deref() == Some(&id)
        }));
        assert!(audit.iter().any(|event| {
            event.action == "schedule.remove"
                && event.actor == crate::audit::HOST_TERMINAL_ACTOR
                && event.target.as_deref() == Some(&id)
        }));
    }

    #[cfg(unix)]
    #[test]
    fn host_schedule_pause_and_resume() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(BatonStore::memory().unwrap()));
        let thread = scheduled_thread(&store.lock().unwrap(), work.path());
        handle_host_command(
            &store,
            home.path(),
            &format!(
                "schedule add --thread {} --cron \"*/5 * * * *\" --prompt \"check the tests\"",
                thread.id
            ),
        );
        let id = store.lock().unwrap().list_schedules().unwrap()[0]
            .id
            .clone();
        handle_host_command(&store, home.path(), &format!("schedule pause {}", &id[..8]));
        let rows = store.lock().unwrap().list_schedules().unwrap();
        assert!(!rows[0].enabled, "{rows:?}");
        handle_host_command(
            &store,
            home.path(),
            &format!("schedule resume {}", &id[..8]),
        );
        let rows = store.lock().unwrap().list_schedules().unwrap();
        assert!(rows[0].enabled, "{rows:?}");
        let audit = store.lock().unwrap().list_audit_events(10).unwrap();
        assert!(audit.iter().any(|event| {
            event.action == "schedule.pause"
                && event.actor == crate::audit::HOST_TERMINAL_ACTOR
                && event.target.as_deref() == Some(&id)
        }));
        assert!(audit.iter().any(|event| {
            event.action == "schedule.resume"
                && event.actor == crate::audit::HOST_TERMINAL_ACTOR
                && event.target.as_deref() == Some(&id)
        }));
    }

    #[cfg(unix)]
    fn claimed_store() -> (tempfile::TempDir, BatonStore) {
        let dir = tempfile::tempdir().unwrap();
        let host_id = uuid::Uuid::new_v4().to_string();
        let store = BatonStore::open_hosted(&dir.path().join("baton.sqlite"), &host_id).unwrap();
        assert!(matches!(
            acquire(&store, dir.path(), &host_id, &SystemProbe).unwrap(),
            ClaimOutcome::Claimed(_)
        ));
        let thread = store
            .create_thread(NewThread {
                title: "protocol".into(),
                cwd: dir.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .submit_turn(&thread.id, ProviderId::Codex, None, "open", false)
            .unwrap();
        (dir, store)
    }

    #[cfg(unix)]
    #[test]
    fn protocol_answers_ping_status_claim_info_and_errors() {
        let (_dir, store) = claimed_store();

        let ping = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":7,"command":"ping"}"#,
        )
        .unwrap();
        assert_eq!(ping["id"], 7);
        assert_eq!(ping["ok"], true);
        assert_eq!(ping["result"]["pong"], true);

        let status = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":8,"command":"status","args":{"thread":null}}"#,
        )
        .unwrap();
        assert_eq!(status["result"]["scope"], "host");
        assert_eq!(status["result"]["threads"], 1);
        assert_eq!(status["result"]["running_turns"], 1);

        let thread = store.list_threads(None).unwrap().remove(0).id;
        let thread_status = handle_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":12,"command":"status","args":{{"thread":"{thread}"}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(thread_status["result"]["scope"], "thread");
        assert_eq!(thread_status["result"]["threads"], 1);
        assert_eq!(thread_status["result"]["running_turns"], 1);

        let missing = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":13,"command":"status","args":{"thread":"missing"}}"#,
        )
        .unwrap();
        assert_eq!(missing["id"], 13);
        assert_eq!(missing["error"]["code"], "not_found");

        let claim = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":9,"command":"claim-info"}"#,
        )
        .unwrap();
        assert_eq!(claim["result"]["pid"], std::process::id());

        let malformed = handle_line(&store, "{").unwrap();
        assert_eq!(malformed["id"], Value::Null);
        assert_eq!(malformed["error"]["code"], "malformed_line");

        let version = handle_line(
            &store,
            r#"{"v":2,"kind":"request","id":10,"command":"ping"}"#,
        )
        .unwrap();
        assert_eq!(version["id"], Value::Null);
        assert_eq!(version["error"]["code"], "bad_version");

        let unknown = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":11,"command":"exec"}"#,
        )
        .unwrap();
        assert_eq!(unknown["id"], 11);
        assert_eq!(unknown["error"]["code"], "not_implemented");

        let open_id = store.list_messages(&thread).unwrap()[0].id;
        let first = store
            .add_message(&thread, 1, "user", Some("codex"), None, "first")
            .unwrap();
        let second = store
            .add_message(&thread, 1, "assistant", Some("codex"), None, "second")
            .unwrap();
        let page = handle_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":14,"command":"messages","args":{{"thread":"{thread}","after":0,"limit":2}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(page["id"], 14);
        assert_eq!(page["result"]["thread_id"], thread);
        assert_eq!(page["result"]["running_turns"], 1);
        assert_eq!(page["result"]["messages"].as_array().unwrap().len(), 2);
        assert_eq!(page["result"]["messages"][0]["id"], open_id);
        assert_eq!(page["result"]["messages"][1]["id"], first);
        assert_eq!(page["result"]["cursor"], first);

        let next = handle_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":15,"command":"messages","args":{{"thread":"{thread}","after":{first},"limit":50}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(next["result"]["cursor"], second);
        assert_eq!(next["result"]["messages"].as_array().unwrap().len(), 1);
        assert_eq!(next["result"]["messages"][0]["id"], second);

        let missing = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":16,"command":"messages","args":{"thread":"missing"}}"#,
        )
        .unwrap();
        assert_eq!(missing["error"]["code"], "not_found");
        let malformed = handle_line(
            &store,
            r#"{"v":1,"kind":"request","id":17,"command":"messages","args":{}}"#,
        )
        .unwrap();
        assert_eq!(malformed["error"]["code"], "internal");
    }

    #[cfg(unix)]
    #[test]
    fn pair_parses_labels_scopes_and_expiry() {
        use crate::grants::Scope;
        use crate::types::PermissionMode;

        let request = parse_pair(Vec::new()).unwrap();
        assert_eq!(request.scopes, vec![Scope::Read, Scope::Operate]);
        assert!(request.label.is_none());
        assert_eq!(request.permission_ceiling, PermissionMode::Default);

        let project = tempfile::tempdir().unwrap();
        let project_path = project.path().to_string_lossy().to_string();
        let request = parse_pair(vec![
            "--label",
            "laptop",
            "--scope",
            "read",
            "--scope",
            "git",
            "--ceiling",
            "acceptEdits",
            "--project",
            &project_path,
            "--expires-days",
            "7",
        ])
        .unwrap();
        assert_eq!(request.scopes, vec![Scope::Read, Scope::Git]);
        assert_eq!(request.label.as_deref(), Some("laptop"));
        assert_eq!(request.permission_ceiling, PermissionMode::AcceptEdits);
        assert_eq!(
            request.project_allowlist,
            vec![project
                .path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .to_string()]
        );
        let expiry = request
            .expires_at
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let expected = chrono::Utc::now() + chrono::Duration::days(7);
        assert!((expiry - expected).num_seconds().abs() < 60);

        let error = parse_pair(vec!["--scope", "nope"]).unwrap_err();
        assert!(error.to_string().contains("unknown scope"), "{error}");
        let error = parse_pair(vec!["--expires-days", "0"]).unwrap_err();
        assert!(error.to_string().contains("positive"), "{error}");
        let error = parse_pair(vec!["--ceiling", "bypassPermissions"]).unwrap_err();
        assert!(error.to_string().contains("not grantable"), "{error}");
        let error = parse_pair(vec!["--ceiling", "nope"]).unwrap_err();
        assert!(error.to_string().contains("unknown ceiling"), "{error}");
        let error = parse_pair(vec!["--project", "relative"]).unwrap_err();
        assert!(error.to_string().contains("absolute"), "{error}");
        let error = parse_pair(vec!["--bogus"]).unwrap_err();
        assert!(error.to_string().contains("unknown pair option"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn submit_requires_operate_scope_and_a_thread_in_the_project() {
        use crate::grants::{mint, GrantRequest, Scope};

        let (_dir, store) = claimed_store();
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let read = mint(&store, home.path(), &GrantRequest::new(vec![Scope::Read])).unwrap();
        let operate = mint(
            &store,
            home.path(),
            &GrantRequest::new(vec![Scope::Read, Scope::Operate]),
        )
        .unwrap();
        let thread = store.list_threads(None).unwrap().remove(0);
        let inside = store
            .create_thread(NewThread {
                title: "inside".into(),
                cwd: root.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let outside = store
            .create_thread(NewThread {
                title: "outside".into(),
                cwd: "/elsewhere".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let link = EngineLink::new(
            sender,
            home.path().to_path_buf(),
            root.path().to_path_buf(),
            Arc::new(Mutex::new(RemoteApprovals::default())),
            Arc::new(Mutex::new(RemoteQuestions::default())),
        );
        let engine = EnginePool::from_link(link.clone());
        let request = |grant: &crate::grants::MintedGrant, thread: &str, id: u64, key: &str| {
            format!(
                r#"{{"v":1,"kind":"request","id":{id},"command":"submit","grant_id":"{}","secret":"{}","args":{{"thread":"{thread}","text":"remote hello","idempotency_key":"{key}"}}}}"#,
                grant.summary.grant_id, grant.secret
            )
        };

        let denied = handle_socket_line(
            &store,
            &request(&read, &inside.id, 1, "k-denied"),
            Some(&engine),
        );
        assert_eq!(denied["error"]["code"], "not_permitted");
        assert!(receiver.try_recv().is_err());

        let missing = handle_socket_line(
            &store,
            &request(&operate, "missing", 2, "k-missing"),
            Some(&engine),
        );
        assert_eq!(missing["error"]["code"], "not_found");

        let elsewhere = handle_socket_line(
            &store,
            &request(&operate, &outside.id, 3, "k-elsewhere"),
            Some(&engine),
        );
        assert_eq!(elsewhere["error"]["code"], "wrong_project");

        let malformed = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":4,"command":"submit","grant_id":"{}","secret":"{}","args":{{}}}}"#,
                operate.summary.grant_id, operate.secret
            ),
            Some(&engine),
        );
        assert_eq!(malformed["error"]["code"], "malformed_line");

        let missing_key = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":5,"command":"submit","grant_id":"{}","secret":"{}","args":{{"thread":"{}","text":"remote hello"}}}}"#,
                operate.summary.grant_id, operate.secret, inside.id
            ),
            Some(&engine),
        );
        assert_eq!(missing_key["error"]["code"], "malformed_line");

        let long_key = "x".repeat(129);
        let long = handle_socket_line(
            &store,
            &request(&operate, &inside.id, 6, &long_key),
            Some(&engine),
        );
        assert_eq!(long["error"]["code"], "malformed_line");

        let accepted = handle_socket_line(
            &store,
            &request(&operate, &inside.id, 7, "k-accepted"),
            Some(&engine),
        );
        assert_eq!(accepted["ok"], true, "{accepted}");
        assert_eq!(accepted["result"]["accepted"], true);
        assert_eq!(accepted["result"].get("replayed"), None);
        let command = receiver.try_recv().expect("the engine receives the submit");
        match command {
            crate::runtime::EngineCommand::Submit {
                thread_id, text, ..
            } => {
                assert_eq!(thread_id, inside.id);
                assert_eq!(text, "remote hello");
            }
            _ => panic!("expected a submit command"),
        }
        let replay = handle_socket_line(
            &store,
            &request(&operate, &inside.id, 8, "k-accepted"),
            Some(&engine),
        );
        assert_eq!(replay["ok"], true, "{replay}");
        assert_eq!(replay["result"]["accepted"], true);
        assert_eq!(replay["result"]["replayed"], true);
        assert_eq!(replay["result"]["thread_id"], inside.id);
        assert!(
            receiver.try_recv().is_err(),
            "a replay must not start a second turn"
        );
        let events = store.list_audit_events(10).unwrap();
        let event = events
            .iter()
            .find(|event| event.action == "turn.submit_request")
            .expect("the remote request writes its row");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.action == "turn.submit_request")
                .count(),
            1,
            "a replay must not write a second request row"
        );
        assert_eq!(event.actor, operate.summary.grant_id);
        assert_eq!(event.confirmation, crate::audit::SOCKET_GRANT_CONFIRMATION);
        assert_eq!(event.scope, crate::audit::OPERATE_SCOPE);
        assert_eq!(event.target.as_deref(), Some(inside.id.as_str()));
        assert_eq!(event.thread_id.as_deref(), Some(inside.id.as_str()));
        assert_ne!(thread.id, inside.id);
    }

    #[cfg(unix)]
    #[test]
    fn submit_refuses_a_bypass_thread() {
        use crate::grants::{mint, GrantRequest, Scope};

        let (_dir, store) = claimed_store();
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let operate = mint(
            &store,
            home.path(),
            &GrantRequest::new(vec![Scope::Read, Scope::Operate]),
        )
        .unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "bypass".into(),
                cwd: root.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .set_thread_permission_mode(&thread.id, crate::types::PermissionMode::BypassPermissions)
            .unwrap();
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let link = EngineLink::new(
            sender,
            home.path().to_path_buf(),
            root.path().to_path_buf(),
            Arc::new(Mutex::new(RemoteApprovals::default())),
            Arc::new(Mutex::new(RemoteQuestions::default())),
        );
        let engine = EnginePool::from_link(link.clone());
        let refused = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":1,"command":"submit","grant_id":"{}","secret":"{}","args":{{"thread":"{}","text":"run","idempotency_key":"k-bypass"}}}}"#,
                operate.summary.grant_id, operate.secret, thread.id
            ),
            Some(&engine),
        );
        assert_eq!(refused["error"]["code"], "ceiling");
        assert!(!store
            .list_audit_events(10)
            .unwrap()
            .iter()
            .any(|event| event.action == "turn.submit_request"));
    }

    #[cfg(unix)]
    #[test]
    fn submit_refuses_a_thread_above_the_grant_ceiling() {
        use crate::grants::{mint, GrantRequest, Scope};
        use crate::types::PermissionMode;

        let (_dir, store) = claimed_store();
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let accepted_thread = store
            .create_thread(NewThread {
                title: "accept edits".into(),
                cwd: root.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store
            .set_thread_permission_mode(&accepted_thread.id, PermissionMode::AcceptEdits)
            .unwrap();
        let default_thread = store
            .create_thread(NewThread {
                title: "default".into(),
                cwd: root.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let mut default_request = GrantRequest::new(vec![Scope::Read, Scope::Operate]);
        default_request.permission_ceiling = PermissionMode::Default;
        let default_grant = mint(&store, home.path(), &default_request).unwrap();
        let mut accept_request = GrantRequest::new(vec![Scope::Read, Scope::Operate]);
        accept_request.permission_ceiling = PermissionMode::AcceptEdits;
        let accept_grant = mint(&store, home.path(), &accept_request).unwrap();
        let mut plan_request = GrantRequest::new(vec![Scope::Read, Scope::Operate]);
        plan_request.permission_ceiling = PermissionMode::Plan;
        let plan_grant = mint(&store, home.path(), &plan_request).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let link = EngineLink::new(
            sender,
            home.path().to_path_buf(),
            root.path().to_path_buf(),
            Arc::new(Mutex::new(RemoteApprovals::default())),
            Arc::new(Mutex::new(RemoteQuestions::default())),
        );
        let engine = EnginePool::from_link(link.clone());
        let request = |grant: &crate::grants::MintedGrant, thread: &str, id: u64| {
            format!(
                r#"{{"v":1,"kind":"request","id":{id},"command":"submit","grant_id":"{}","secret":"{}","args":{{"thread":"{thread}","text":"remote hello","idempotency_key":"k-{id}"}}}}"#,
                grant.summary.grant_id, grant.secret
            )
        };

        let above = handle_socket_line(
            &store,
            &request(&default_grant, &accepted_thread.id, 1),
            Some(&engine),
        );
        assert_eq!(above["error"]["code"], "ceiling");
        assert!(receiver.try_recv().is_err());

        let below = handle_socket_line(
            &store,
            &request(&plan_grant, &default_thread.id, 2),
            Some(&engine),
        );
        assert_eq!(below["error"]["code"], "ceiling");
        assert!(receiver.try_recv().is_err());

        let accepted = handle_socket_line(
            &store,
            &request(&accept_grant, &accepted_thread.id, 3),
            Some(&engine),
        );
        assert_eq!(accepted["ok"], true, "{accepted}");
        assert!(receiver.try_recv().is_ok());
    }

    #[test]
    fn a_finished_turn_drops_its_pending_approvals() {
        let mut approvals = RemoteApprovals::default();
        let (first, mut first_receiver) = tokio::sync::oneshot::channel();
        let (second, _second_receiver) = tokio::sync::oneshot::channel();
        approvals.insert(
            "thread-1".into(),
            "req-1".into(),
            "command".into(),
            "first".into(),
            Vec::new(),
            first,
        );
        approvals.insert(
            "thread-2".into(),
            "req-2".into(),
            "command".into(),
            "second".into(),
            Vec::new(),
            second,
        );
        approvals.clear_thread("thread-1");
        assert!(first_receiver.try_recv().is_err());
        let listed = approvals.list(None);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["request_id"], "req-2");
    }

    #[cfg(unix)]
    #[test]
    fn approvals_require_approve_scope_and_answer_once() {
        use crate::grants::{mint, GrantRequest, Scope};

        let (_dir, store) = claimed_store();
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let read = mint(&store, home.path(), &GrantRequest::new(vec![Scope::Read])).unwrap();
        let approve = mint(
            &store,
            home.path(),
            &GrantRequest::new(vec![Scope::Read, Scope::Approve]),
        )
        .unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut wrong_request = GrantRequest::new(vec![Scope::Read, Scope::Approve]);
        wrong_request.project_allowlist = vec![outside
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string()];
        let wrong = mint(&store, home.path(), &wrong_request).unwrap();
        let thread = store.list_threads(None).unwrap().remove(0);
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let link = EngineLink::new(
            sender,
            home.path().to_path_buf(),
            root.path().to_path_buf(),
            Arc::new(Mutex::new(RemoteApprovals::default())),
            Arc::new(Mutex::new(RemoteQuestions::default())),
        );
        let engine = EnginePool::from_link(link.clone());
        let (respond, mut decision_receiver) = tokio::sync::oneshot::channel();
        link.approvals.lock().unwrap().insert(
            thread.id.clone(),
            "req-1".into(),
            "command".into(),
            "run tests".into(),
            Vec::new(),
            respond,
        );
        let request = |grant: &crate::grants::MintedGrant, id: u64, command: &str, args: &str| {
            format!(
                r#"{{"v":1,"kind":"request","id":{id},"command":"{command}","grant_id":"{}","secret":"{}","args":{args}}}"#,
                grant.summary.grant_id, grant.secret
            )
        };

        let denied_list = handle_socket_line(
            &store,
            &request(&wrong, 1, "approvals", "{}"),
            Some(&engine),
        );
        assert_eq!(denied_list["error"]["code"], "not_permitted");
        let denied_answer = handle_socket_line(
            &store,
            &request(
                &wrong,
                2,
                "respond",
                r#"{"request":"req-1","choice":"accept","idempotency_key":"k-wrong-project"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(denied_answer["error"]["code"], "not_permitted");

        let listed =
            handle_socket_line(&store, &request(&read, 1, "approvals", "{}"), Some(&engine));
        assert_eq!(listed["ok"], true, "{listed}");
        assert_eq!(listed["result"]["approvals"][0]["request_id"], "req-1");
        assert_eq!(
            listed["result"]["approvals"][0]["choices"][0]["id"],
            "accept"
        );

        let denied = handle_socket_line(
            &store,
            &request(
                &read,
                2,
                "respond",
                r#"{"request":"req-1","choice":"accept","idempotency_key":"k-read"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(denied["error"]["code"], "not_permitted");

        let wrong = handle_socket_line(
            &store,
            &request(
                &approve,
                3,
                "respond",
                r#"{"request":"req-1","choice":"missing","idempotency_key":"k-wrong"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(wrong["error"]["code"], "not_found");
        assert!(engine.list_approvals(&[], None).len() == 1);

        let accepted = handle_socket_line(
            &store,
            &request(
                &approve,
                4,
                "respond",
                r#"{"request":"req-1","choice":"accept","idempotency_key":"k-respond"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(accepted["ok"], true, "{accepted}");
        assert_eq!(accepted["result"]["answered"], true);
        assert_eq!(accepted["result"]["decision"], "accept");
        assert_eq!(
            decision_receiver.try_recv().unwrap(),
            crate::types::ApprovalDecision::Accept
        );
        assert!(engine.list_approvals(&[], None).is_empty());

        let replay = handle_socket_line(
            &store,
            &request(
                &approve,
                5,
                "respond",
                r#"{"request":"req-1","choice":"accept","idempotency_key":"k-respond"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(replay["ok"], true, "{replay}");
        assert_eq!(replay["result"]["replayed"], true);

        let again = handle_socket_line(
            &store,
            &request(
                &approve,
                6,
                "respond",
                r#"{"request":"req-1","choice":"accept","idempotency_key":"k-other"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(again["error"]["code"], "not_found");

        let events = store.list_audit_events(20).unwrap();
        let event = events
            .iter()
            .find(|event| event.action == "approval.decide")
            .expect("the answer writes its row");
        assert_eq!(event.actor, approve.summary.grant_id);
        assert_eq!(event.confirmation, crate::audit::SOCKET_GRANT_CONFIRMATION);
        assert_eq!(event.scope, crate::audit::APPROVE_SCOPE);
        assert_eq!(event.target.as_deref(), Some("req-1"));
        assert_eq!(event.thread_id.as_deref(), Some(thread.id.as_str()));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.action == "approval.decide")
                .count(),
            1,
            "a replay does not write a second row"
        );
    }

    #[test]
    fn a_finished_turn_drops_its_pending_questions() {
        let mut questions = RemoteQuestions::default();
        let (first, mut first_receiver) = tokio::sync::oneshot::channel();
        questions.insert("thread-1".into(), "q-1".into(), Vec::new(), first);
        questions.clear_thread("thread-1");
        assert!(first_receiver.try_recv().is_err());
        assert!(questions.list(None).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn questions_require_approve_scope_and_answer_once() {
        use crate::grants::{mint, GrantRequest, Scope};
        use crate::types::{QuestionAnswer, QuestionOption, QuestionPrompt};

        let (_dir, store) = claimed_store();
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let read = mint(&store, home.path(), &GrantRequest::new(vec![Scope::Read])).unwrap();
        let approve = mint(
            &store,
            home.path(),
            &GrantRequest::new(vec![Scope::Read, Scope::Approve]),
        )
        .unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut wrong_request = GrantRequest::new(vec![Scope::Read, Scope::Approve]);
        wrong_request.project_allowlist = vec![outside
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string()];
        let wrong = mint(&store, home.path(), &wrong_request).unwrap();
        let thread = store.list_threads(None).unwrap().remove(0);
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let link = EngineLink::new(
            sender,
            home.path().to_path_buf(),
            root.path().to_path_buf(),
            Arc::new(Mutex::new(RemoteApprovals::default())),
            Arc::new(Mutex::new(RemoteQuestions::default())),
        );
        let engine = EnginePool::from_link(link.clone());
        let (respond, mut answer_receiver) = tokio::sync::oneshot::channel();
        let prompts = vec![
            QuestionPrompt {
                id: "features".into(),
                header: "Features".into(),
                question: "Which features?".into(),
                options: vec![
                    QuestionOption {
                        label: "CI".into(),
                        description: String::new(),
                    },
                    QuestionOption {
                        label: "Installer".into(),
                        description: String::new(),
                    },
                ],
                allow_custom: false,
                multi_select: true,
                secret: false,
            },
            QuestionPrompt {
                id: "note".into(),
                header: "Note".into(),
                question: "Anything else?".into(),
                options: Vec::new(),
                allow_custom: true,
                multi_select: false,
                secret: true,
            },
        ];
        link.questions.lock().unwrap().insert(
            thread.id.clone(),
            "q-1".into(),
            prompts.clone(),
            respond,
        );
        let request = |grant: &crate::grants::MintedGrant, id: u64, command: &str, args: &str| {
            format!(
                r#"{{"v":1,"kind":"request","id":{id},"command":"{command}","grant_id":"{}","secret":"{}","args":{args}}}"#,
                grant.summary.grant_id, grant.secret
            )
        };

        let denied_list = handle_socket_line(
            &store,
            &request(&wrong, 1, "questions", "{}"),
            Some(&engine),
        );
        assert_eq!(denied_list["error"]["code"], "not_permitted");
        let denied_answer = handle_socket_line(
            &store,
            &request(
                &wrong,
                2,
                "answer",
                r#"{"request":"q-1","answers":{"features":["CI"],"note":""},"idempotency_key":"q-wrong-project"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(denied_answer["error"]["code"], "not_permitted");

        let listed =
            handle_socket_line(&store, &request(&read, 1, "questions", "{}"), Some(&engine));
        assert_eq!(listed["ok"], true, "{listed}");
        assert_eq!(listed["result"]["questions"][0]["request_id"], "q-1");
        assert_eq!(
            listed["result"]["questions"][0]["questions"][0]["options"][0]["label"],
            "CI"
        );

        let denied = handle_socket_line(
            &store,
            &request(
                &read,
                2,
                "answer",
                r#"{"request":"q-1","answers":{"features":["CI"],"note":"x"},"idempotency_key":"q-read"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(denied["error"]["code"], "not_permitted");

        let missing = handle_socket_line(
            &store,
            &request(
                &approve,
                3,
                "answer",
                r#"{"request":"q-1","answers":{"features":["CI"]},"idempotency_key":"q-missing"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(missing["error"]["code"], "malformed_line");

        let unknown = handle_socket_line(
            &store,
            &request(
                &approve,
                4,
                "answer",
                r#"{"request":"q-1","answers":{"features":["Android"],"note":"x"},"idempotency_key":"q-unknown"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(unknown["error"]["code"], "malformed_line");

        let accepted = handle_socket_line(
            &store,
            &request(
                &approve,
                5,
                "answer",
                r#"{"request":"q-1","answers":{"features":["CI","Installer"],"note":"private"},"idempotency_key":"q-answer"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(accepted["ok"], true, "{accepted}");
        assert_eq!(accepted["result"]["answered"], true);
        assert_eq!(accepted["result"]["prompts"], 2);
        let answers = answer_receiver.try_recv().unwrap();
        assert_eq!(
            answers,
            vec![
                QuestionAnswer {
                    id: "features".into(),
                    answers: vec!["CI".into(), "Installer".into()]
                },
                QuestionAnswer {
                    id: "note".into(),
                    answers: vec!["private".into()]
                }
            ]
        );
        assert!(engine.list_questions(&[], None).is_empty());

        let replay = handle_socket_line(
            &store,
            &request(
                &approve,
                6,
                "answer",
                r#"{"request":"q-1","answers":{"features":["CI"],"note":""},"idempotency_key":"q-answer"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(replay["ok"], true, "{replay}");
        assert_eq!(replay["result"]["replayed"], true);

        let again = handle_socket_line(
            &store,
            &request(
                &approve,
                7,
                "answer",
                r#"{"request":"q-1","answers":{"features":["CI"],"note":""},"idempotency_key":"q-other"}"#,
            ),
            Some(&engine),
        );
        assert_eq!(again["error"]["code"], "not_found");

        let events = store.list_audit_events(20).unwrap();
        let event = events
            .iter()
            .find(|event| event.action == "question.answer")
            .expect("the answer writes its row");
        assert_eq!(event.actor, approve.summary.grant_id);
        assert_eq!(event.confirmation, crate::audit::SOCKET_GRANT_CONFIRMATION);
        assert_eq!(event.scope, crate::audit::APPROVE_SCOPE);
        assert_eq!(event.target.as_deref(), Some("q-1"));
        assert_eq!(event.thread_id.as_deref(), Some(thread.id.as_str()));
        let expected = crate::audit::payload_digest(&crate::audit::question_answer_payload(
            "q-1",
            &prompts,
            &[
                QuestionAnswer {
                    id: "features".into(),
                    answers: vec!["CI".into(), "Installer".into()],
                },
                QuestionAnswer {
                    id: "note".into(),
                    answers: vec!["private".into()],
                },
            ],
        ))
        .unwrap();
        assert_eq!(event.payload_digest, expected);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.action == "question.answer")
                .count(),
            1,
            "a replay does not write a second row"
        );
    }

    #[cfg(unix)]
    #[test]
    fn submit_refuses_a_project_outside_the_grant_allowlist() {
        use crate::grants::{mint, GrantRequest, Scope};

        let (_dir, store) = claimed_store();
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let sibling = tempfile::tempdir().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "allowed".into(),
                cwd: root.path().to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let mut wrong_request = GrantRequest::new(vec![Scope::Read, Scope::Operate]);
        wrong_request.project_allowlist = vec![sibling
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string()];
        let wrong = mint(&store, home.path(), &wrong_request).unwrap();
        let mut right_request = GrantRequest::new(vec![Scope::Read, Scope::Operate]);
        right_request.project_allowlist = vec![root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string()];
        let right = mint(&store, home.path(), &right_request).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let link = EngineLink::new(
            sender,
            home.path().to_path_buf(),
            root.path().to_path_buf(),
            Arc::new(Mutex::new(RemoteApprovals::default())),
            Arc::new(Mutex::new(RemoteQuestions::default())),
        );
        let engine = EnginePool::from_link(link.clone());
        let request = |grant: &crate::grants::MintedGrant, id: u64| {
            format!(
                r#"{{"v":1,"kind":"request","id":{id},"command":"submit","grant_id":"{}","secret":"{}","args":{{"thread":"{}","text":"remote hello","idempotency_key":"k-{id}"}}}}"#,
                grant.summary.grant_id, grant.secret, thread.id
            )
        };

        let refused = handle_socket_line(&store, &request(&wrong, 1), Some(&engine));
        assert_eq!(refused["error"]["code"], "not_permitted");
        assert!(receiver.try_recv().is_err());

        let accepted = handle_socket_line(&store, &request(&right, 2), Some(&engine));
        assert_eq!(accepted["ok"], true, "{accepted}");
        assert!(receiver.try_recv().is_ok());
    }

    #[test]
    fn pool_interactions_follow_the_project_allowlist() {
        let home = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let (first_sender, _first_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (second_sender, _second_receiver) = tokio::sync::mpsc::unbounded_channel();
        let first_link = EngineLink::new(
            first_sender,
            home.path().to_path_buf(),
            first.path().to_path_buf(),
            Arc::new(Mutex::new(RemoteApprovals::default())),
            Arc::new(Mutex::new(RemoteQuestions::default())),
        );
        let second_link = EngineLink::new(
            second_sender,
            home.path().to_path_buf(),
            second.path().to_path_buf(),
            Arc::new(Mutex::new(RemoteApprovals::default())),
            Arc::new(Mutex::new(RemoteQuestions::default())),
        );
        let (first_approval, _first_decision) = tokio::sync::oneshot::channel();
        first_link.approvals.lock().unwrap().insert(
            "thread-a".into(),
            "approval-a".into(),
            "command".into(),
            "first".into(),
            Vec::new(),
            first_approval,
        );
        let (second_approval, _second_decision) = tokio::sync::oneshot::channel();
        second_link.approvals.lock().unwrap().insert(
            "thread-b".into(),
            "approval-b".into(),
            "command".into(),
            "second".into(),
            Vec::new(),
            second_approval,
        );
        let (second_question, _second_answers) = tokio::sync::oneshot::channel();
        second_link.questions.lock().unwrap().insert(
            "thread-b".into(),
            "question-b".into(),
            Vec::new(),
            second_question,
        );
        let pool = EnginePool::from_links(vec![first_link, second_link]);
        let first_root = first
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let second_root = second
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let missing = first.path().join("missing").to_string_lossy().to_string();

        let allowed_first = std::slice::from_ref(&first_root);
        let allowed_second = std::slice::from_ref(&second_root);
        assert!(pool.has_allowed_engine(allowed_first));
        assert!(!pool.has_allowed_engine(&[missing]));
        let listed = pool.list_approvals(allowed_first, None);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["request_id"], "approval-a");
        assert_eq!(pool.list_questions(allowed_first, None).len(), 0);
        assert_eq!(
            pool.list_questions(allowed_second, None)[0]["request_id"],
            "question-b"
        );
        assert!(
            pool.take_approval(allowed_first, "approval-b", "accept")
                .is_none(),
            "a request from an excluded project is not visible"
        );
        assert!(
            pool.take_approval(allowed_second, "approval-b", "accept")
                .is_some(),
            "the allowed project can answer its own request"
        );
    }

    #[cfg(unix)]
    #[test]
    fn renew_parses_a_grant_id_and_days() {
        let (grant_id, days) = parse_renew(vec!["00000000-0000-4000-8000-000000000000"]).unwrap();
        assert_eq!(grant_id, "00000000-0000-4000-8000-000000000000");
        assert_eq!(days, crate::grants::DEFAULT_GRANT_DAYS);

        let (grant_id, days) = parse_renew(vec!["grant", "--days", "7"]).unwrap();
        assert_eq!(grant_id, "grant");
        assert_eq!(days, 7);

        let error = parse_renew(Vec::new()).unwrap_err();
        assert!(error.to_string().contains("needs a grant id"), "{error}");
        let error = parse_renew(vec!["a", "b"]).unwrap_err();
        assert!(
            error.to_string().contains("exactly one grant id"),
            "{error}"
        );
        let error = parse_renew(vec!["a", "--days"]).unwrap_err();
        assert!(
            error.to_string().contains("--days needs a number"),
            "{error}"
        );
        let error = parse_renew(vec!["a", "--bogus"]).unwrap_err();
        assert!(
            error.to_string().contains("unknown renew option"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn socket_lines_require_a_live_read_grant() {
        use crate::grants::{mint, revoke, GrantRequest, Scope};

        let dir = tempfile::tempdir().unwrap();
        let store = BatonStore::memory().unwrap();
        let read = mint(&store, dir.path(), &GrantRequest::new(vec![Scope::Read])).unwrap();
        let operate = mint(&store, dir.path(), &GrantRequest::new(vec![Scope::Operate])).unwrap();

        let ping = handle_socket_line(
            &store,
            r#"{"v":1,"kind":"request","id":1,"command":"ping"}"#,
            None,
        );
        assert_eq!(ping["result"]["pong"], true);

        let bare = handle_socket_line(
            &store,
            r#"{"v":1,"kind":"request","id":2,"command":"status"}"#,
            None,
        );
        assert_eq!(bare["error"]["code"], "unauthenticated");

        let with = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":3,"command":"status","grant_id":"{}","secret":"{}"}}"#,
                read.summary.grant_id, read.secret
            ),
            None,
        );
        assert_eq!(with["ok"], true, "{with}");
        assert_eq!(with["result"]["scope"], "host");

        let wrong = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":4,"command":"status","grant_id":"{}","secret":"wrong"}}"#,
                read.summary.grant_id
            ),
            None,
        );
        assert_eq!(wrong["error"]["code"], "bad_secret");

        let narrow = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":5,"command":"status","grant_id":"{}","secret":"{}"}}"#,
                operate.summary.grant_id, operate.secret
            ),
            None,
        );
        assert_eq!(narrow["error"]["code"], "not_permitted");

        revoke(&store, dir.path(), &read.summary.grant_id).unwrap();
        let revoked = handle_socket_line(
            &store,
            &format!(
                r#"{{"v":1,"kind":"request","id":6,"command":"status","grant_id":"{}","secret":"{}"}}"#,
                read.summary.grant_id, read.secret
            ),
            None,
        );
        assert_eq!(revoked["error"]["code"], "revoked");
    }

    #[cfg(unix)]
    #[test]
    fn peer_credentials_accept_only_the_host_user() {
        let host = unsafe { libc::getuid() };
        assert!(same_user(host, host));
        assert!(!same_user(host.wrapping_add(1), host));
    }
    #[cfg(unix)]
    #[test]
    fn finish_host_settles_dead_owner_work_before_release() {
        let (dir, store) = claimed_store();
        let host_id = store.get_host_claim().unwrap().unwrap().host_id;
        let thread_id = store.list_threads(None).unwrap().remove(0).id;
        {
            let db = rusqlite::Connection::open(dir.path().join("baton.sqlite")).unwrap();
            db.execute(
                "UPDATE turns SET owner_pid = ?1, owner_start = NULL",
                rusqlite::params![u32::MAX],
            )
            .unwrap();
        }
        finish_host(&store, &host_id, Ok(())).unwrap();
        let turn = store.get_turn_record(&thread_id, 1).unwrap().unwrap();
        assert_eq!(turn.phase, crate::types::TurnPhase::Unverified);
        assert!(store.get_host_claim().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn finish_host_releases_the_claim_when_the_loop_failed() {
        let (_dir, store) = claimed_store();
        let host_id = store.get_host_claim().unwrap().unwrap().host_id;
        let thread_id = store.list_threads(None).unwrap().remove(0).id;
        let error =
            finish_host(&store, &host_id, Err(anyhow::anyhow!("stdout closed"))).unwrap_err();
        assert_eq!(error.to_string(), "stdout closed");
        let turn = store.get_turn_record(&thread_id, 1).unwrap().unwrap();
        assert_eq!(turn.phase, crate::types::TurnPhase::Unverified);
        assert_eq!(turn.error.as_deref(), Some("host shut down"));
        assert!(store.get_host_claim().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn finish_host_cancels_its_own_work_before_release() {
        let (_dir, store) = claimed_store();
        let claim = store.get_host_claim().unwrap().unwrap();
        let thread_id = store.list_threads(None).unwrap().remove(0).id;
        store
            .create_orchestration_run_owned(
                "run-host",
                "host goal",
                ProviderId::Codex,
                "model",
                None,
                "{}",
                false,
                Some(claim.pid),
                Some(&claim.process_start),
            )
            .unwrap();
        store
            .add_orchestration_task(
                "task-host",
                "run-host",
                1,
                "host task",
                ProviderId::Codex,
                "model",
                "do host",
                Some("/tmp/host-worktree"),
                None,
                None,
                Some(&thread_id),
                "queued",
                None,
                None,
                "2026-11-26T00:00:00.000Z",
                None,
                None,
                None,
                None,
            )
            .unwrap();

        finish_host(&store, &claim.host_id, Ok(())).unwrap();
        let turn = store.get_turn_record(&thread_id, 1).unwrap().unwrap();
        assert_eq!(turn.phase, crate::types::TurnPhase::Unverified);
        assert_eq!(turn.error.as_deref(), Some("host shut down"));
        let tasks = store.list_orchestration_tasks("run-host").unwrap();
        assert_eq!(tasks[0].status, "cancelled");
        assert_eq!(tasks[0].error.as_deref(), Some("host shut down"));
        let run = store
            .find_orchestration_runs("run-host")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(run.status, "cancelled");
        assert!(store.get_host_claim().unwrap().is_none());
    }
}
