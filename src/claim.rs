use anyhow::{Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::store::{BatonStore, HostClaimAttempt, HostClaimRow};

pub(crate) const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_RETRY: Duration = Duration::from_millis(50);

pub trait ProcessProbe {
    fn start(&self, pid: u32) -> Option<String>;
    fn alive(&self, pid: u32, expected_start: Option<&str>) -> bool;
}

pub struct SystemProbe;

impl ProcessProbe for SystemProbe {
    fn start(&self, pid: u32) -> Option<String> {
        crate::process::process_start(pid)
    }

    fn alive(&self, pid: u32, expected_start: Option<&str>) -> bool {
        crate::process::process_identity_alive(pid, expected_start)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRefusal {
    pub code: &'static str,
    pub message: String,
}

impl ClaimRefusal {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn exit_code(&self) -> i32 {
        3
    }
}

impl std::fmt::Display for ClaimRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClaimRefusal {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed(HostClaimRow),
    Refused(ClaimRefusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeoverOutcome {
    Taken(HostClaimRow),
    NoClaim,
    Changed { pid: u32, process_start: String },
    NotLive { pid: u32 },
}

pub fn acquire(
    store: &BatonStore,
    home: &Path,
    host_id: &str,
    probe: &dyn ProcessProbe,
) -> Result<ClaimOutcome> {
    uuid::Uuid::parse_str(host_id).context("host id must be a UUID")?;
    if store.host_id() != Some(host_id) {
        anyhow::bail!("host store must be opened with open_hosted and the same host id");
    }
    let installation_id = ensure_installation_id(home)?;
    let _lock = match acquire_write_lock(home, LOCK_WAIT)? {
        LockAttempt::Acquired(lock) => lock,
        LockAttempt::Busy => {
            return Ok(ClaimOutcome::Refused(ClaimRefusal::new(
                "lock_busy",
                "the Baton write lock is held by another process",
            )))
        }
        LockAttempt::Unsupported => {
            return Ok(ClaimOutcome::Refused(ClaimRefusal::new(
                "lock_unsupported",
                "host claiming needs Unix file locking in this build",
            )))
        }
    };
    let pid = std::process::id();
    let Some(process_start) = probe.start(pid) else {
        return Ok(ClaimOutcome::Refused(ClaimRefusal::new(
            "no_process_start",
            "cannot verify this process start time; host claiming is Unix-only in this build",
        )));
    };
    let alive = |pid: u32, expected: Option<&str>| probe.alive(pid, expected);
    match store.claim_host_row(host_id, &installation_id, pid, &process_start, &alive) {
        Ok(HostClaimAttempt::Claimed(claim)) => Ok(ClaimOutcome::Claimed(claim)),
        Ok(HostClaimAttempt::Owned { pid, process_start }) => {
            Ok(ClaimOutcome::Refused(ClaimRefusal::new(
                "owned",
                format!("the store is already claimed by pid {pid} (start {process_start})"),
            )))
        }
        Ok(HostClaimAttempt::OpenWork { pid }) => Ok(ClaimOutcome::Refused(ClaimRefusal::new(
            "open_work",
            format!("pid {pid} has an open turn or orchestration run"),
        ))),
        Err(error) if is_store_busy(&error) => Ok(ClaimOutcome::Refused(ClaimRefusal::new(
            "store_busy",
            "the store is busy with another writer",
        ))),
        Err(error) => Err(error),
    }
}

fn is_store_busy(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(code, _))
                if matches!(
                    code.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                )
        )
    })
}

/// Opens the TUI store. A live host claim makes the TUI a read-only client
/// and returns the exact owner notice for its startup line.
pub fn open_tui_store(path: &Path) -> Result<(BatonStore, Option<String>)> {
    match BatonStore::open_client(path, None) {
        Ok(store) => Ok((store, None)),
        Err(error) => {
            let Some(refusal) = error.downcast_ref::<crate::store::ClientRefusal>() else {
                return Err(error);
            };
            let store = BatonStore::open_read_only(path)?;
            Ok((store, Some(refusal.to_string())))
        }
    }
}

/// Replaces a live claim after the caller names and confirms its exact owner.
/// The takeover store must already carry the new host id, and that claim stays
/// live until the caller settles old work and releases it.
pub fn takeover(
    store: &BatonStore,
    home: &Path,
    expected_pid: u32,
    new_host_id: &str,
    probe: &dyn ProcessProbe,
) -> Result<TakeoverOutcome> {
    uuid::Uuid::parse_str(new_host_id).context("host id must be a UUID")?;
    if store.host_id() != Some(new_host_id) {
        anyhow::bail!("host store must be opened with open_hosted and the new host id");
    }
    let _lock = match acquire_write_lock(home, LOCK_WAIT)? {
        LockAttempt::Acquired(lock) => lock,
        LockAttempt::Busy => {
            anyhow::bail!("the Baton write lock is held by another process; retry takeover")
        }
        LockAttempt::Unsupported => {
            anyhow::bail!("host takeover needs Unix file locking in this build")
        }
    };
    let pid = std::process::id();
    let Some(process_start) = probe.start(pid) else {
        anyhow::bail!("cannot verify this process start time; host takeover is Unix-only")
    };
    let current = store.get_host_claim()?;
    let Some(current) = current else {
        return Ok(TakeoverOutcome::NoClaim);
    };
    match store.takeover_host_row(
        new_host_id,
        expected_pid,
        &current.process_start,
        pid,
        &process_start,
    )? {
        crate::store::TakeoverAttempt::Taken(claim) => Ok(TakeoverOutcome::Taken(claim)),
        crate::store::TakeoverAttempt::NoClaim => Ok(TakeoverOutcome::NoClaim),
        crate::store::TakeoverAttempt::Changed { pid, process_start } => {
            Ok(TakeoverOutcome::Changed { pid, process_start })
        }
        crate::store::TakeoverAttempt::NotLive { pid } => Ok(TakeoverOutcome::NotLive { pid }),
    }
}

pub fn ensure_installation_id(home: &Path) -> Result<String> {
    create_private_dir(home)?;
    let path = home.join("installation-id");
    match open_private_new(&path) {
        Ok(mut file) => {
            let id = uuid::Uuid::new_v4().to_string();
            file.write_all(id.as_bytes())?;
            file.write_all(b"\n")?;
            Ok(id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut last_error = None;
            for _ in 0..20 {
                match read_installation_id(&path) {
                    Ok(id) => return Ok(id),
                    Err(error) => last_error = Some(error),
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(last_error.expect("a read failure creates a recorded error"))
        }
        Err(error) => Err(error).with_context(|| format!("create {}", path.display())),
    }
}

fn read_installation_id(path: &Path) -> Result<String> {
    let mut file = open_private_read(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 128 {
        anyhow::bail!(
            "installation id is not a small regular file: {}",
            path.display()
        );
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let id = uuid::Uuid::parse_str(text.trim())
        .with_context(|| format!("installation id is not a UUID: {}", path.display()))?;
    Ok(id.to_string())
}

pub(crate) fn create_private_dir(path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            anyhow::bail!("Baton home is not a directory: {}", path.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .with_context(|| format!("create Baton home {}", path.display()))
}

pub(crate) fn open_private_new(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn open_private_read(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

#[allow(dead_code)]
pub(crate) enum LockAttempt {
    Acquired(WriteLock),
    Busy,
    Unsupported,
}

#[cfg(unix)]
pub(crate) struct WriteLock {
    file: File,
}

#[cfg(unix)]
impl Drop for WriteLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(not(unix))]
pub(crate) struct WriteLock;

pub(crate) fn acquire_write_lock(home: &Path, timeout: Duration) -> Result<LockAttempt> {
    create_private_dir(home)?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let path = home.join("write.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let file = options
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let started = Instant::now();
        loop {
            let code = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if code == 0 {
                return Ok(LockAttempt::Acquired(WriteLock { file }));
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK)
                && error.raw_os_error() != Some(libc::EAGAIN)
            {
                return Err(error).with_context(|| format!("lock {}", path.display()));
            }
            if started.elapsed() >= timeout {
                return Ok(LockAttempt::Busy);
            }
            std::thread::sleep(LOCK_RETRY);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (home, timeout);
        Ok(LockAttempt::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewThread;
    use crate::types::ProviderId;
    use std::collections::HashMap;

    struct FakeProbe {
        starts: HashMap<u32, String>,
        live: HashMap<u32, String>,
    }

    impl FakeProbe {
        fn new() -> Self {
            Self {
                starts: HashMap::new(),
                live: HashMap::new(),
            }
        }

        fn live(mut self, pid: u32, start: &str) -> Self {
            self.starts.insert(pid, start.to_string());
            self.live.insert(pid, start.to_string());
            self
        }
    }

    impl ProcessProbe for FakeProbe {
        fn start(&self, pid: u32) -> Option<String> {
            self.starts.get(&pid).cloned()
        }

        fn alive(&self, pid: u32, expected_start: Option<&str>) -> bool {
            match expected_start {
                Some(expected) => self.live.get(&pid).map(String::as_str) == Some(expected),
                None => self.live.contains_key(&pid),
            }
        }
    }

    fn host_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn takeover_replaces_only_the_named_live_claim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("baton.sqlite");
        let old_id = host_id();
        let new_id = host_id();
        let host = BatonStore::open_hosted(&path, &old_id).unwrap();
        let claim = match acquire(&host, dir.path(), &old_id, &SystemProbe).unwrap() {
            ClaimOutcome::Claimed(claim) => claim,
            other => panic!("expected claim, got {other:?}"),
        };
        let takeover_store = BatonStore::open_hosted(&path, &new_id).unwrap();

        let changed = takeover(
            &takeover_store,
            dir.path(),
            claim.pid + 1,
            &new_id,
            &SystemProbe,
        )
        .unwrap();
        assert!(matches!(changed, TakeoverOutcome::Changed { .. }));
        assert_eq!(host.get_host_claim().unwrap().as_ref(), Some(&claim));

        let taken = takeover(
            &takeover_store,
            dir.path(),
            claim.pid,
            &new_id,
            &SystemProbe,
        )
        .unwrap();
        assert!(matches!(taken, TakeoverOutcome::Taken(_)));
        let current = host.get_host_claim().unwrap().expect("replacement claim");
        assert_eq!(current.host_id, new_id);
        assert!(host
            .create_thread(NewThread {
                title: "old host".into(),
                cwd: "/old".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap_err()
            .downcast_ref::<crate::store::NotTheHost>()
            .is_some());
        takeover_store
            .create_thread(NewThread {
                title: "new host".into(),
                cwd: "/new".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        assert!(takeover_store.release_host_claim(&new_id).unwrap());
        assert!(host.get_host_claim().unwrap().is_none());
        assert!(matches!(
            takeover(
                &takeover_store,
                dir.path(),
                claim.pid,
                &new_id,
                &SystemProbe
            )
            .unwrap(),
            TakeoverOutcome::NoClaim
        ));
    }

    #[cfg(unix)]
    #[test]
    fn open_tui_store_returns_a_read_only_client_with_the_owner_notice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("baton.sqlite");
        let host_id = host_id();
        let seed = BatonStore::open(&path).unwrap();
        seed.create_thread(NewThread {
            title: "host".into(),
            cwd: "/host".into(),
            current_provider: ProviderId::Codex,
            current_model: None,
        })
        .unwrap();
        drop(seed);
        let host = BatonStore::open_hosted(&path, &host_id).unwrap();
        assert!(matches!(
            acquire(&host, dir.path(), &host_id, &SystemProbe).unwrap(),
            ClaimOutcome::Claimed(_)
        ));

        let (client, refusal) = open_tui_store(&path).unwrap();
        let refusal = refusal.expect("the owner notice");
        assert!(refusal.contains("owns this store"), "{refusal}");
        assert_eq!(client.list_threads(None).unwrap().len(), 1);
        assert!(client
            .create_thread(NewThread {
                title: "refused".into(),
                cwd: "/client".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .is_err());

        assert!(host.release_host_claim(&host_id).unwrap());
        let (client, refusal) = open_tui_store(&path).unwrap();
        assert!(refusal.is_none());
        client
            .create_thread(NewThread {
                title: "after release".into(),
                cwd: "/client".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn claim_writes_the_row_and_installation_id() {
        let dir = tempfile::tempdir().unwrap();
        let id = host_id();
        let store = BatonStore::open_hosted(&dir.path().join("baton.sqlite"), &id).unwrap();
        let probe = SystemProbe;
        let outcome = acquire(&store, dir.path(), &id, &probe).unwrap();
        let claim = match outcome {
            ClaimOutcome::Claimed(claim) => claim,
            other => panic!("expected claim, got {other:?}"),
        };
        assert_eq!(claim.host_id, id);
        assert_eq!(claim.pid, std::process::id());
        assert_eq!(
            claim.process_start,
            crate::process::process_start(std::process::id()).unwrap()
        );
        assert_eq!(claim.kind, "serve");
        let installation_id = ensure_installation_id(dir.path()).unwrap();
        assert_eq!(claim.installation_id, installation_id);
        assert!(ensure_installation_id(dir.path()).unwrap() == installation_id);
        assert!(store.release_host_claim(&id).unwrap());
        assert!(store.get_host_claim().unwrap().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join("installation-id"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
            let mode = fs::metadata(dir.path().join("write.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
            let mode = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }

    #[test]
    fn a_live_claim_refuses_a_second_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("baton.sqlite");
        let id = host_id();
        let first = BatonStore::open_hosted(&path, &id).unwrap();
        let second_id = host_id();
        let second = BatonStore::open_hosted(&path, &second_id).unwrap();
        let probe = FakeProbe::new().live(std::process::id(), "host-start");
        assert!(matches!(
            acquire(&first, dir.path(), &id, &probe).unwrap(),
            ClaimOutcome::Claimed(_)
        ));
        let refused = match acquire(&second, dir.path(), &second_id, &probe).unwrap() {
            ClaimOutcome::Refused(refusal) => refusal,
            other => panic!("expected refusal, got {other:?}"),
        };
        assert_eq!(refused.code, "owned");
        assert_eq!(refused.exit_code(), 3);
        assert!(refused.message.contains(&std::process::id().to_string()));
    }

    #[test]
    fn a_stale_claim_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("baton.sqlite");
        let first = host_id();
        let first_store = BatonStore::open_hosted(&path, &first).unwrap();
        let first_probe = FakeProbe::new().live(std::process::id(), "old-start");
        assert!(matches!(
            acquire(&first_store, dir.path(), &first, &first_probe).unwrap(),
            ClaimOutcome::Claimed(_)
        ));
        let second = host_id();
        let second_store = BatonStore::open_hosted(&path, &second).unwrap();
        let second_probe = FakeProbe::new().live(std::process::id(), "new-start");
        let claim = match acquire(&second_store, dir.path(), &second, &second_probe).unwrap() {
            ClaimOutcome::Claimed(claim) => claim,
            other => panic!("expected replacement claim, got {other:?}"),
        };
        assert_eq!(claim.host_id, second);
        assert_eq!(claim.process_start, "new-start");
    }

    #[cfg(unix)]
    #[test]
    fn a_live_local_turn_blocks_the_claim() {
        let dir = tempfile::tempdir().unwrap();
        let id = host_id();
        let path = dir.path().join("baton.sqlite");
        let setup = BatonStore::open(&path).unwrap();
        let host = BatonStore::open_hosted(&path, &id).unwrap();
        let thread = setup
            .create_thread(NewThread {
                title: "local work".into(),
                cwd: "/work".into(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        setup
            .start_turn_record(&thread.id, 1, ProviderId::Codex, None)
            .unwrap();
        let actual_start = crate::process::process_start(std::process::id()).unwrap();
        let probe = FakeProbe::new()
            .live(std::process::id(), "host-start")
            .live(std::process::id(), &actual_start);
        let refused = match acquire(&host, dir.path(), &id, &probe).unwrap() {
            ClaimOutcome::Refused(refusal) => refusal,
            other => panic!("expected refusal, got {other:?}"),
        };
        assert_eq!(refused.code, "open_work");
        setup
            .finish_turn_record(&thread.id, 1, crate::types::TurnPhase::Completed, None)
            .unwrap();
        assert!(matches!(
            acquire(&host, dir.path(), &id, &probe).unwrap(),
            ClaimOutcome::Claimed(_)
        ));
    }

    #[test]
    fn a_claim_without_a_readable_start_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let id = host_id();
        let store = BatonStore::open_hosted(&dir.path().join("baton.sqlite"), &id).unwrap();
        let refused = match acquire(&store, dir.path(), &id, &FakeProbe::new()).unwrap() {
            ClaimOutcome::Refused(refusal) => refusal,
            other => panic!("expected refusal, got {other:?}"),
        };
        assert_eq!(refused.code, "no_process_start");
        assert!(store.get_host_claim().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn an_installation_id_symlink_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, "not-an-id\n").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("installation-id")).unwrap();
        assert!(ensure_installation_id(dir.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_held_write_lock_is_reported_busy() {
        use std::os::unix::io::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        create_private_dir(dir.path()).unwrap();
        let path = dir.path().join("write.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let code = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(code, 0);
        let attempt = acquire_write_lock(dir.path(), Duration::from_millis(10)).unwrap();
        assert!(matches!(attempt, LockAttempt::Busy));
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
