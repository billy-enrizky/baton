//! Read-only environment and provider readiness report for humans and agents.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::config::{get_default_provider, load_config_file, resolve_config};
use crate::process::probe;
use crate::store::BatonStore;
use crate::types::ProviderId;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct ProviderCheck {
    pub provider: ProviderId,
    pub executable: Option<PathBuf>,
    pub model: String,
    pub auth_mode: String,
    pub permission_mode: String,
    pub config_file: Option<PathBuf>,
}

impl ProviderCheck {
    pub fn installed(&self) -> bool {
        self.executable.is_some()
    }

    fn config_state(&self) -> &'static str {
        match self.config_file.as_ref() {
            Some(path) if path.is_file() => "config present",
            Some(_) => "config missing",
            None => "config n/a",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessCheck {
    pub pid: u32,
    pub provider: ProviderId,
    pub port: Option<u16>,
    pub process_alive: bool,
    pub owner_running: bool,
    pub command_matches: bool,
    pub start_matches: bool,
}

#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub db_path: PathBuf,
    pub db_bytes: u64,
    pub thread_count: usize,
    pub config_file: PathBuf,
    pub config_exists: bool,
    pub default_provider: Option<ProviderId>,
    pub providers: Vec<ProviderCheck>,
    pub processes: Vec<ProcessCheck>,
    pub warnings: Vec<String>,
}

pub fn build(home: &Path, cwd: &Path, store: &BatonStore) -> Result<DoctorReport> {
    build_with_lookup(home, cwd, store, find_executable)
}

fn build_with_lookup(
    home: &Path,
    cwd: &Path,
    store: &BatonStore,
    lookup: impl Fn(&str) -> Option<PathBuf>,
) -> Result<DoctorReport> {
    let config = resolve_config(home, cwd);
    let mut providers = Vec::new();
    for provider in ProviderId::all() {
        let effective = config.for_provider(*provider);
        providers.push(ProviderCheck {
            provider: *provider,
            executable: lookup(provider_executable(*provider)),
            model: effective.model.clone(),
            auth_mode: effective.mode.as_str().to_string(),
            permission_mode: effective.permission_mode.as_str().to_string(),
            config_file: provider_config_file(*provider),
        });
    }

    let db_path = home.join("baton.sqlite");
    let db_bytes = std::fs::metadata(&db_path).map(|meta| meta.len()).unwrap_or(0);
    let thread_count = store.list_threads(None)?.len();
    let config_file = home.join("config.json");
    let config_exists = config_file.is_file();
    let default_provider = get_default_provider(home);
    let processes = store
        .list_provider_processes()?
        .into_iter()
        .map(|row| {
            let state = probe(&row);
            ProcessCheck {
                pid: row.pid,
                provider: row.provider,
                port: row.port,
                process_alive: state.alive,
                owner_running: state.owner_running,
                command_matches: state.command_matches,
                start_matches: state.start_matches,
            }
        })
        .collect::<Vec<_>>();

    let mut warnings = Vec::new();
    if config_exists && load_config_file(home).is_none() {
        warnings.push(format!(
            "baton config {} exists but could not be parsed",
            config_file.display()
        ));
    }
    if let Some(provider) = default_provider {
        if let Some(check) = providers.iter().find(|check| check.provider == provider) {
            if !check.installed() {
                warnings.push(format!(
                    "default provider {provider} executable `{}` not found on PATH",
                    provider_executable(provider)
                ));
            }
        }
    }
    for process in &processes {
        if !process.process_alive {
            warnings.push(format!(
                "tracked {} process {} is not alive",
                process.provider, process.pid
            ));
        } else if !process.command_matches || !process.start_matches {
            warnings.push(format!(
                "tracked {} process {} no longer matches its recorded identity",
                process.provider, process.pid
            ));
        }
    }

    Ok(DoctorReport {
        home: home.to_path_buf(),
        cwd: cwd.to_path_buf(),
        db_path,
        db_bytes,
        thread_count,
        config_file,
        config_exists,
        default_provider,
        providers,
        processes,
        warnings,
    })
}

/// One plain-language answer to "what should I do next?" for setup.
pub fn next_action(report: &DoctorReport) -> String {
    let ready = report
        .providers
        .iter()
        .filter(|provider| provider.installed())
        .collect::<Vec<_>>();
    if ready.is_empty() {
        return "install at least one harness, then run `baton doctor` again".to_string();
    }
    if let Some(default) = report.default_provider {
        if let Some(provider) = report.providers.iter().find(|provider| provider.provider == default) {
            if !provider.installed() {
                let suggestion = ready[0].provider;
                return format!(
                    "default provider {default} is missing; set it to {suggestion} with `baton default {suggestion}`"
                );
            }
        }
    }
    if report
        .processes
        .iter()
        .any(|process| !process.process_alive || !process.owner_running)
    {
        return "run `baton ps` to inspect stale provider processes".to_string();
    }
    "ready; start with `baton` or `baton exec`".to_string()
}

pub fn render(report: &DoctorReport) -> String {
    let mut lines = vec![
        "baton doctor".to_string(),
        format!("now: {}", next_action(report)),
        format!("home: {}", report.home.display()),
        format!("cwd: {}", report.cwd.display()),
        format!(
            "db: {} ({} bytes, {} thread(s))",
            report.db_path.display(),
            report.db_bytes,
            report.thread_count
        ),
        format!(
            "config: {} ({})",
            report.config_file.display(),
            if report.config_exists { "present" } else { "missing" }
        ),
        format!(
            "default provider: {}",
            report
                .default_provider
                .map(|provider| provider.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
    ];
    let ready = report
        .providers
        .iter()
        .filter(|provider| provider.installed())
        .map(|provider| provider.provider.to_string())
        .collect::<Vec<_>>();
    let missing = report
        .providers
        .iter()
        .filter(|provider| !provider.installed())
        .map(|provider| provider.provider.to_string())
        .collect::<Vec<_>>();
    lines.push(if ready.is_empty() {
        "ready: none; install a harness and run baton doctor again".to_string()
    } else {
        format!("ready: {}", ready.join(", "))
    });
    if !missing.is_empty() {
        lines.push(format!("missing: {}", missing.join(", ")));
    }
    lines.push("providers:".to_string());
    for provider in &report.providers {
        let status = provider
            .executable
            .as_ref()
            .map(|path| format!("ready ({})", path.display()))
            .unwrap_or_else(|| "missing".to_string());
        lines.push(format!(
            "  {}: {} · model={} · auth={} · mode={} · {}",
            provider.provider,
            status,
            provider.model,
            provider.auth_mode,
            provider.permission_mode,
            provider.config_state()
        ));
    }
    if report.processes.is_empty() {
        lines.push("processes: none tracked".to_string());
    } else {
        lines.push(format!("processes: {} tracked", report.processes.len()));
        for process in &report.processes {
            lines.push(format!(
                "  {} pid={} port={} alive={} owner={} command={} start={}",
                process.provider,
                process.pid,
                process.port.map(|port| port.to_string()).unwrap_or_else(|| "-".to_string()),
                process.process_alive,
                process.owner_running,
                process.command_matches,
                process.start_matches
            ));
        }
    }
    if report.warnings.is_empty() {
        lines.push("warnings: none".to_string());
    } else {
        lines.push("warnings:".to_string());
        for warning in &report.warnings {
            lines.push(format!("  - {warning}"));
        }
    }
    lines.join("\n")
}

pub fn to_json(report: &DoctorReport) -> serde_json::Value {
    let providers = report
        .providers
        .iter()
        .map(|provider| {
            serde_json::json!({
                "provider": provider.provider.as_str(),
                "installed": provider.installed(),
                "executable": provider.executable.as_ref().map(|path| path.display().to_string()),
                "model": provider.model,
                "auth_mode": provider.auth_mode,
                "permission_mode": provider.permission_mode,
                "config_file": provider.config_file.as_ref().map(|path| path.display().to_string()),
                "config_exists": provider.config_file.as_ref().map(|path| path.is_file()).unwrap_or(false),
            })
        })
        .collect::<Vec<_>>();
    let processes = report
        .processes
        .iter()
        .map(|process| {
            serde_json::json!({
                "pid": process.pid,
                "provider": process.provider.as_str(),
                "port": process.port,
                "process_alive": process.process_alive,
                "owner_running": process.owner_running,
                "command_matches": process.command_matches,
                "start_matches": process.start_matches,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "home": report.home.display().to_string(),
        "cwd": report.cwd.display().to_string(),
        "db": {
            "path": report.db_path.display().to_string(),
            "bytes": report.db_bytes,
            "thread_count": report.thread_count,
        },
        "config": {
            "path": report.config_file.display().to_string(),
            "exists": report.config_exists,
            "default_provider": report.default_provider.map(|provider| provider.as_str()),
        },
        "providers": providers,
        "next_action": next_action(report),
        "ready": report.providers.iter().filter(|provider| provider.installed()).map(|provider| provider.provider.as_str()).collect::<Vec<_>>(),
        "missing": report.providers.iter().filter(|provider| !provider.installed()).map(|provider| provider.provider.as_str()).collect::<Vec<_>>(),
        "processes": processes,
        "healthy": report.warnings.is_empty(),
        "warnings": report.warnings,
    })
}

fn provider_executable(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Claude => "claude",
        ProviderId::Codex => "codex",
        ProviderId::Cursor => "cursor-agent",
        ProviderId::Grok => "grok",
        ProviderId::OpenCode => "opencode",
        ProviderId::Antigravity => "antigravity",
    }
}

fn provider_config_file(provider: ProviderId) -> Option<PathBuf> {
    match provider {
        ProviderId::Claude => Some(crate::paths::claude_global_settings_dir().join("settings.json")),
        ProviderId::Codex => Some(crate::paths::codex_global_home().join("config.toml")),
        ProviderId::OpenCode => {
            let dir = crate::paths::opencode_global_config_dir();
            let json = dir.join("opencode.json");
            let jsonc = dir.join("opencode.jsonc");
            if json.is_file() || !jsonc.is_file() {
                Some(json)
            } else {
                Some(jsonc)
            }
        }
        ProviderId::Cursor | ProviderId::Grok | ProviderId::Antigravity => None,
    }
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.components().count() > 1 {
        return is_executable_file(direct).then(|| direct.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for suffix in executable_suffixes() {
            let candidate = directory.join(format!("{name}{suffix}"));
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn executable_suffixes() -> &'static [&'static str] {
    if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat", ".com"]
    } else {
        &[""]
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// A plain-language install hint for a provider whose CLI is not on PATH.
pub fn provider_install_hint(provider: ProviderId) -> String {
    format!(
        "{} is not installed. Install the {} CLI, then run `baton doctor` to check it.",
        provider.binary_name(),
        provider
    )
}

/// Returns the install hint only when the provider binary is missing.
pub fn missing_provider_hint(provider: ProviderId) -> Option<String> {
    find_executable(provider.binary_name())
        .is_none()
        .then(|| provider_install_hint(provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> DoctorReport {
        DoctorReport {
            home: PathBuf::from("/home/billy/.baton"),
            cwd: PathBuf::from("/work"),
            db_path: PathBuf::from("/home/billy/.baton/baton.sqlite"),
            db_bytes: 4096,
            thread_count: 3,
            config_file: PathBuf::from("/home/billy/.baton/config.json"),
            config_exists: true,
            default_provider: Some(ProviderId::Codex),
            providers: vec![
                ProviderCheck {
                    provider: ProviderId::Codex,
                    executable: Some(PathBuf::from("/usr/local/bin/codex")),
                    model: "gpt-6.1-sol".into(),
                    auth_mode: "subscription".into(),
                    permission_mode: "default".into(),
                    config_file: Some(PathBuf::from("/home/billy/.codex/config.toml")),
                },
                ProviderCheck {
                    provider: ProviderId::Antigravity,
                    executable: None,
                    model: "gemini-3.8-flash-high".into(),
                    auth_mode: "subscription".into(),
                    permission_mode: "default".into(),
                    config_file: None,
                },
            ],
            processes: vec![ProcessCheck {
                pid: 42,
                provider: ProviderId::OpenCode,
                port: Some(4096),
                process_alive: true,
                owner_running: true,
                command_matches: true,
                start_matches: true,
            }],
            warnings: vec!["antigravity is optional".into()],
        }
    }

    #[test]
    fn renders_provider_readiness_and_processes() {
        let text = render(&report());
        assert!(text.contains("baton doctor"));
        assert!(text.contains("default provider: codex"));
        assert!(text.contains("now: ready; start with"));
        assert!(text.contains("ready: codex"));
        assert!(text.contains("missing: antigravity"));
        assert!(text.contains("codex: ready (/usr/local/bin/codex)"));
        assert!(text.contains("antigravity: missing"));
        assert!(text.contains("processes: 1 tracked"));
        assert!(text.contains("warnings:"));
    }

    #[test]
    fn json_exposes_stable_schema() {
        let value = to_json(&report());
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["config"]["default_provider"], "codex");
        assert_eq!(value["db"]["thread_count"], 3);
        assert_eq!(value["providers"][0]["provider"], "codex");
        assert_eq!(value["providers"][0]["installed"], true);
        assert_eq!(value["providers"][1]["installed"], false);
        assert_eq!(value["next_action"], "ready; start with `baton` or `baton exec`");
        assert_eq!(value["ready"], serde_json::json!(["codex"]));
        assert_eq!(value["missing"], serde_json::json!(["antigravity"]));
        assert_eq!(value["processes"][0]["pid"], 42);
        assert_eq!(value["healthy"], false);
    }

    #[test]
    fn next_action_covers_setup_states() {
        let mut no_harness = report();
        for provider in &mut no_harness.providers {
            provider.executable = None;
        }
        assert!(next_action(&no_harness).contains("install at least one harness"));

        let mut missing_default = report();
        missing_default.default_provider = Some(ProviderId::Antigravity);
        assert!(next_action(&missing_default).contains("baton default codex"));

        let mut stale_process = report();
        stale_process.processes[0].owner_running = false;
        assert!(next_action(&stale_process).contains("baton ps"));

        assert!(next_action(&report()).starts_with("ready"));
    }

    #[cfg(unix)]
    #[test]
    fn find_executable_requires_the_execute_bit() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let plain = root.path().join("baton-doctor-plain");
        std::fs::write(&plain, "#!/bin/sh\n").unwrap();
        assert!(!is_executable_file(&plain));
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable_file(&plain));
    }

    #[test]
    fn missing_provider_hint_names_the_binary_and_the_next_step() {
        let hint = provider_install_hint(ProviderId::Cursor);
        assert!(hint.contains("cursor-agent"), "{hint}");
        assert!(hint.contains("baton doctor"), "{hint}");
    }
}
