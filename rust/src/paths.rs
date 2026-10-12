use std::path::PathBuf;

pub fn baton_home() -> PathBuf {
    if let Ok(override_dir) = std::env::var("BATON_HOME") {
        let trimmed = override_dir.trim().to_string();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".baton")
}

pub fn baton_db_path() -> PathBuf {
    baton_home().join("baton.sqlite")
}

pub fn claude_global_settings_dir() -> PathBuf {
    if let Ok(config_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let trimmed = config_dir.trim().to_string();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".claude")
}

pub fn codex_global_home() -> PathBuf {
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        let trimmed = codex_home.trim().to_string();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".codex")
}
