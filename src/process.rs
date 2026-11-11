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
}

#[derive(Debug, Default)]
pub struct ReapReport {
    pub reaped: Vec<ProviderProcessRow>,
    pub already_gone: usize,
    pub live_owners: usize,
    pub command_mismatches: Vec<(ProviderProcessRow, String)>,
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
    ProcessState {
        alive,
        owner_running: pid_alive(row.owner_pid),
        command_matches: command.as_deref() == Some(row.command.as_str()),
        command,
    }
}

/// Reaps provider children whose owning baton process is gone. A row is only
/// eligible when the live process's command line still matches the command
/// line recorded at spawn time. A reused PID or a different process is left
/// alone.
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
        match command_line(row.pid) {
            Some(actual) if actual == row.command => {
                if terminate_verified(row.pid) {
                    store.remove_provider_process(row.pid)?;
                    report.reaped.push(row);
                } else {
                    report.errors.push(format!(
                        "could not stop provider process {} ({})",
                        row.pid, row.provider
                    ));
                }
            }
            Some(actual) => {
                // The PID now belongs to a different command. The row is
                // stale, but the process is not ours to stop.
                store.remove_provider_process(row.pid)?;
                report.command_mismatches.push((row, actual));
            }
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

#[cfg(not(unix))]
pub fn command_line(_pid: u32) -> Option<String> {
    None
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
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn terminate_verified(pid: u32) -> bool {
    if !send_signal(pid, "-TERM") {
        return !pid_alive(pid);
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = send_signal(pid, "-KILL");
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    !pid_alive(pid)
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

    fn sleeper() -> (ChildGuard, u32, String) {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleeper");
        let pid = child.id();
        let command = command_line(pid).expect("read sleeper command line");
        (ChildGuard(child), pid, command)
    }

    #[test]
    fn reaps_a_verified_orphan() {
        let store = BatonStore::memory().unwrap();
        let (mut child, pid, command) = sleeper();
        store
            .record_provider_process(
                pid,
                ProviderId::OpenCode,
                Some(41234),
                &command,
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
        let (mut child, pid, command) = sleeper();
        store
            .record_provider_process(
                pid,
                ProviderId::OpenCode,
                None,
                &command,
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
        let (mut child, pid, _) = sleeper();
        store
            .record_provider_process(
                pid,
                ProviderId::OpenCode,
                None,
                "definitely-not-this-process",
                "/tmp/project",
                u32::MAX,
            )
            .unwrap();

        let report = reap_orphans(&store).unwrap();

        assert_eq!(report.command_mismatches.len(), 1);
        assert!(report.reaped.is_empty());
        assert!(store.list_provider_processes().unwrap().is_empty());
        assert!(child.0.try_wait().unwrap().is_none());
    }

    #[test]
    fn removes_a_row_whose_process_already_exited() {
        let store = BatonStore::memory().unwrap();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        store
            .record_provider_process(pid, ProviderId::OpenCode, None, "true", "/tmp", u32::MAX)
            .unwrap();

        let report = reap_orphans(&store).unwrap();

        assert_eq!(report.already_gone, 1);
        assert!(store.list_provider_processes().unwrap().is_empty());
    }
}
