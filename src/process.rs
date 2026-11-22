use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::store::{BatonStore, ProviderProcessRow};
use crate::types::ProviderId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessState {
    pub alive: bool,
    pub owner_running: bool,
    pub command: Option<String>,
    pub command_matches: bool,
    pub process_start: Option<String>,
    pub start_matches: bool,
}

#[derive(Debug, Default)]
pub struct ReapReport {
    pub reaped: Vec<ProviderProcessRow>,
    pub already_gone: usize,
    pub live_owners: usize,
    pub identity_mismatches: Vec<(ProviderProcessRow, String)>,
    pub unverifiable: usize,
    pub errors: Vec<String>,
}

impl ReapReport {
    pub fn summary(&self) -> Option<String> {
        if self.reaped.is_empty() {
            return None;
        }
        let pids = self
            .reaped
            .iter()
            .map(|row| row.pid.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "reaped {} orphaned provider process(es): {pids}",
            self.reaped.len()
        ))
    }
}

/// A small recorder passed into provider spawners. It is intentionally
/// independent of the engine so a child can be registered at the exact point
/// it is spawned, before a turn can hold the adapter mutex.
#[derive(Clone)]
pub struct ProcessRecorder {
    store: Option<Arc<Mutex<BatonStore>>>,
    cwd: String,
}

impl ProcessRecorder {
    pub fn new(store: Arc<Mutex<BatonStore>>, cwd: impl Into<String>) -> Self {
        Self {
            store: Some(store),
            cwd: cwd.into(),
        }
    }

    pub fn disabled() -> Self {
        Self {
            store: None,
            cwd: String::new(),
        }
    }

    pub fn record(
        &self,
        pid: u32,
        provider: ProviderId,
        port: Option<u16>,
        command: &str,
        process_start: Option<&str>,
    ) -> Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let store = store
            .lock()
            .map_err(|_| anyhow::anyhow!("baton store lock poisoned"))?;
        store.record_provider_process(
            pid,
            provider,
            port,
            command,
            process_start,
            &self.cwd,
            std::process::id(),
        )
    }

    pub fn forget(&self, pid: u32) {
        let Some(store) = &self.store else {
            return;
        };
        if let Ok(store) = store.lock() {
            let _ = store.remove_provider_process(pid);
        }
    }
}

pub fn probe(row: &ProviderProcessRow) -> ProcessState {
    let alive = pid_alive(row.pid);
    let command = command_line(row.pid);
    let process_start = process_start(row.pid);
    ProcessState {
        alive,
        owner_running: pid_alive(row.owner_pid),
        command_matches: command.as_deref() == Some(row.command.as_str()),
        start_matches: row
            .process_start
            .as_deref()
            .map_or(false, |start| process_start.as_deref() == Some(start)),
        process_start,
        command,
    }
}

/// Reaps provider children whose owning baton process is gone. A row is only
/// eligible when the live process's command line and recorded start time both
/// still match. A missing start time is not proof of identity, so that row is
/// kept and reported instead of killed. A reused PID or a different process is
/// left alone.
#[cfg(unix)]
pub fn reap_orphans(store: &BatonStore) -> Result<ReapReport> {
    let mut report = ReapReport::default();
    let current_pid = std::process::id();
    for row in store.list_provider_processes()? {
        if row.owner_pid == current_pid || pid_alive(row.owner_pid) {
            report.live_owners += 1;
            continue;
        }
        if !pid_alive(row.pid) {
            store.remove_provider_process(row.pid)?;
            report.already_gone += 1;
            continue;
        }
        let command = command_line(row.pid);
        match command {
            Some(actual) if actual != row.command => {
                // The PID now belongs to a different command. The row is
                // stale, but the process is not ours to stop.
                store.remove_provider_process(row.pid)?;
                report.identity_mismatches.push((row, actual));
            }
            Some(_) if row.process_start.is_none() => {
                // Without a recorded start time, KILL could hit a reused PID.
                // Keep the row for a human, and never signal it.
                report.unverifiable += 1;
            }
            Some(actual) => match terminate_verified(&row) {
                TerminateOutcome::Gone => {
                    store.remove_provider_process(row.pid)?;
                    report.reaped.push(row);
                }
                TerminateOutcome::IdentityChanged => {
                    store.remove_provider_process(row.pid)?;
                    report.identity_mismatches.push((row, actual));
                }
                TerminateOutcome::Failed => {
                    report.errors.push(format!(
                        "could not stop provider process {} ({})",
                        row.pid, row.provider
                    ));
                }
            },
            None => {
                // The process exists but its command line is not readable.
                // Keep the row and do nothing; guessing here is unsafe.
                report.unverifiable += 1;
            }
        }
    }
    Ok(report)
}

#[cfg(not(unix))]
pub fn reap_orphans(_store: &BatonStore) -> Result<ReapReport> {
    Ok(ReapReport::default())
}

#[cfg(unix)]
pub fn command_line(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let output = std::process::Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!line.is_empty()).then_some(line)
}

#[cfg(unix)]
pub fn process_start(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let output = std::process::Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!line.is_empty()).then_some(line)
}

#[cfg(not(unix))]
pub fn command_line(_pid: u32) -> Option<String> {
    None
}

#[cfg(not(unix))]
pub fn process_start(_pid: u32) -> Option<String> {
    None
}

/// True only when the PID is live and its start time still matches the
/// identity recorded by the original owner. Used to avoid reconciling a run
/// whose orchestrator is still active in another Baton process.
#[cfg(unix)]
pub fn process_identity_alive(pid: u32, expected_start: Option<&str>) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    match expected_start {
        Some(expected) => process_start(pid).map_or(true, |actual| actual == expected),
        None => true,
    }
}

/// Without a process identity API, assume the owner is live so a running run
/// is never clobbered by a different Baton process.
#[cfg(not(unix))]
pub fn process_identity_alive(pid: u32, _expected_start: Option<&str>) -> bool {
    pid != 0
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // `kill -0` still succeeds for a zombie that its parent has not waited
    // on. Treat a zombie as gone so a crashed child can be reaped without
    // relying on the new parent's wait timing.
    let output = std::process::Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "stat="])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
            !state.is_empty() && !state.starts_with('Z')
        }
        Ok(_) => false,
        Err(_) => std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false),
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: &str) -> bool {
    std::process::Command::new("kill")
        .args([signal, &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn send_group_signal(pgid: u32, signal: &str) -> bool {
    std::process::Command::new("kill")
        .args([signal, "--", &format!("-{pgid}")])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn group_alive(pgid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", "--", &format!("-{pgid}")])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Stops every process in a worker's process group. The worker runs `baton
/// exec` in its own group, so its provider child dies with it.
#[cfg(unix)]
pub fn terminate_group(pgid: u32) -> bool {
    if pgid == 0 {
        return false;
    }
    let dump = || {
        if let Ok(output) = std::process::Command::new("ps")
            .args(["-eo", "pid,ppid,pgid,stat,cmd"])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let fields: Vec<_> = line.split_whitespace().collect();
                if fields.len() > 2 && fields[2] == pgid.to_string() {
                    eprintln!("group member: {line}");
                }
            }
        }
    };
    eprintln!("terminate_group({pgid}) before signal");
    dump();
    let term = send_group_signal(pgid, "-TERM");
    eprintln!("send_group_signal TERM -> {term}");
    if !group_alive(pgid) {
        eprintln!("group gone before wait");
        return true;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if !group_alive(pgid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let kill = send_group_signal(pgid, "-KILL");
    eprintln!("send_group_signal KILL -> {kill}");
    dump();
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !group_alive(pgid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    !group_alive(pgid)
}

#[cfg(not(unix))]
pub fn terminate_group(_pgid: u32) -> bool {
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminateOutcome {
    Gone,
    IdentityChanged,
    Failed,
}

/// True only when the row records a start time and the live PID still has the
/// same command line and the same start time. Without a recorded start there
/// is no proof that the PID is the original child.
#[cfg(unix)]
fn recorded_identity_matches(row: &ProviderProcessRow) -> bool {
    let Some(expected_start) = row.process_start.as_deref() else {
        return false;
    };
    command_line(row.pid).as_deref() == Some(row.command.as_str())
        && process_start(row.pid).as_deref() == Some(expected_start)
}

/// Stops one verified child. Identity is checked again before TERM and again
/// before KILL: the original process can exit and its PID be reused during the
/// grace period, and KILL must never hit a stranger.
#[cfg(unix)]
fn terminate_verified(row: &ProviderProcessRow) -> TerminateOutcome {
    let pid = row.pid;
    if !pid_alive(pid) {
        return TerminateOutcome::Gone;
    }
    if !recorded_identity_matches(row) {
        return if pid_alive(pid) {
            TerminateOutcome::IdentityChanged
        } else {
            TerminateOutcome::Gone
        };
    }
    if !send_signal(pid, "-TERM") {
        return if pid_alive(pid) {
            TerminateOutcome::Failed
        } else {
            TerminateOutcome::Gone
        };
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return TerminateOutcome::Gone;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if !recorded_identity_matches(row) {
        return if pid_alive(pid) {
            TerminateOutcome::IdentityChanged
        } else {
            TerminateOutcome::Gone
        };
    }
    let _ = send_signal(pid, "-KILL");
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return TerminateOutcome::Gone;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if pid_alive(pid) {
        TerminateOutcome::Failed
    } else {
        TerminateOutcome::Gone
    }
}

/// Kills a provider child when its Baton parent disappears. `kill_on_drop`
/// does not run when Baton is SIGKILLed, so a tiny detached shell watchdog
/// closes that orphan window. Best-effort: a missing `sh` must not block a
/// provider turn.
#[cfg(unix)]
pub fn spawn_parent_watchdog(server_pid: u32) -> Result<()> {
    spawn_watchdog_for(std::process::id(), server_pid)
}

#[cfg(not(unix))]
pub fn spawn_parent_watchdog(_server_pid: u32) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn spawn_watchdog_for(parent_pid: u32, server_pid: u32) -> Result<()> {
    let script = watchdog_script(parent_pid, server_pid);
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Reap the watchdog when it exits normally; after a SIGKILL of Baton it
    // reparents to init and exits on its own.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(unix)]
fn watchdog_script(parent_pid: u32, server_pid: u32) -> String {
    format!(
        "while kill -0 {parent_pid} 2>/dev/null && kill -0 {server_pid} 2>/dev/null; do sleep 1; done; if kill -0 {server_pid} 2>/dev/null; then kill -TERM {server_pid} 2>/dev/null; sleep 1; kill -KILL {server_pid} 2>/dev/null; fi"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ChildGuard(std::process::Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn sleeper() -> (ChildGuard, u32, String, String) {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleeper");
        let pid = child.id();
        let command = command_line(pid).expect("read sleeper command line");
        let start = process_start(pid).expect("read sleeper start time");
        (ChildGuard(child), pid, command, start)
    }

    #[test]
    fn reaps_a_verified_orphan() {
        let store = BatonStore::memory().unwrap();
        let (mut child, pid, command, start) = sleeper();
        store
            .record_provider_process(
                pid,
                ProviderId::OpenCode,
                Some(41234),
                &command,
                Some(&start),
                "/tmp/project",
                u32::MAX,
            )
            .unwrap();

        let report = reap_orphans(&store).unwrap();

        assert_eq!(report.reaped.len(), 1);
        assert_eq!(report.reaped[0].pid, pid);
        assert!(report.errors.is_empty());
        assert!(store.list_provider_processes().unwrap().is_empty());
        child.0.wait().expect("reaped child must exit");
    }

    #[test]
    fn leaves_a_live_owners_process_alone() {
        let store = BatonStore::memory().unwrap();
        let (mut child, pid, command, start) = sleeper();
        store
            .record_provider_process(
                pid,
                ProviderId::OpenCode,
                None,
                &command,
                Some(&start),
                "/tmp/project",
                std::process::id(),
            )
            .unwrap();

        let report = reap_orphans(&store).unwrap();

        assert_eq!(report.live_owners, 1);
        assert!(report.reaped.is_empty());
        assert_eq!(store.list_provider_processes().unwrap().len(), 1);
        assert!(child.0.try_wait().unwrap().is_none());
    }

    #[test]
    fn does_not_kill_a_reused_pid_with_a_different_command() {
        let store = BatonStore::memory().unwrap();
        let (mut child, pid, _, start) = sleeper();
        store
            .record_provider_process(
                pid,
                ProviderId::OpenCode,
                None,
                "definitely-not-this-process",
                Some(&start),
                "/tmp/project",
                u32::MAX,
            )
            .unwrap();

        let report = reap_orphans(&store).unwrap();

        assert_eq!(report.identity_mismatches.len(), 1);
        assert!(report.reaped.is_empty());
        assert!(store.list_provider_processes().unwrap().is_empty());
        assert!(child.0.try_wait().unwrap().is_none());
    }

    #[test]
    fn probe_does_not_claim_a_missing_start_matches() {
        let (mut child, pid, command, _) = sleeper();
        let row = ProviderProcessRow {
            pid,
            provider: ProviderId::Codex,
            port: None,
            command,
            process_start: None,
            cwd: "/tmp/project".to_string(),
            owner_pid: u32::MAX,
            started_at: String::new(),
        };

        let state = probe(&row);

        assert!(state.alive);
        assert!(state.command_matches);
        assert!(!state.start_matches, "a missing start is not an identity match");
        let _ = child.0.try_wait();
    }

    #[test]
    fn keeps_a_row_with_no_recorded_start_instead_of_killing_it() {
        let store = BatonStore::memory().unwrap();
        let (mut child, pid, command, _) = sleeper();
        store
            .record_provider_process(
                pid,
                ProviderId::Codex,
                None,
                &command,
                None,
                "/tmp/project",
                u32::MAX,
            )
            .unwrap();

        let report = reap_orphans(&store).unwrap();

        assert_eq!(report.unverifiable, 1);
        assert!(report.reaped.is_empty());
        assert!(report.errors.is_empty());
        assert_eq!(
            store.list_provider_processes().unwrap().len(),
            1,
            "an unverifiable row is kept for a human, not silently dropped"
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "a row without a recorded start must never be signaled"
        );
    }

    #[test]
    fn does_not_kill_a_reused_pid_with_a_different_start_time() {
        let store = BatonStore::memory().unwrap();
        let (mut child, pid, command, _) = sleeper();
        store
            .record_provider_process(
                pid,
                ProviderId::Codex,
                None,
                &command,
                Some("not the real start time"),
                "/tmp/project",
                u32::MAX,
            )
            .unwrap();

        let report = reap_orphans(&store).unwrap();

        assert_eq!(report.identity_mismatches.len(), 1);
        assert!(report.errors.is_empty());
        assert!(store.list_provider_processes().unwrap().is_empty());
        assert!(child.0.try_wait().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn watchdog_kills_the_server_when_the_parent_is_gone() {
        use std::time::{Duration, Instant};

        let mut parent = std::process::Command::new("true").spawn().unwrap();
        let parent_pid = parent.id();
        parent.wait().unwrap();
        let mut server = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let server_pid = server.id();
        spawn_watchdog_for(parent_pid, server_pid).unwrap();

        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = server.try_wait().unwrap() {
                assert!(!status.success(), "the watchdog terminates the server");
                break;
            }
            assert!(Instant::now() < deadline, "watchdog did not kill the server");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn removes_a_row_whose_process_already_exited() {
        let store = BatonStore::memory().unwrap();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        store
            .record_provider_process(pid, ProviderId::OpenCode, None, "true", None, "/tmp", u32::MAX)
            .unwrap();

        let report = reap_orphans(&store).unwrap();

        assert_eq!(report.already_gone, 1);
        assert!(store.list_provider_processes().unwrap().is_empty());
    }
}
