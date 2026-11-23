use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::types::{normalize_reasoning_effort, ProviderId};

pub const HOOK_EVENTS: &[&str] = &[
    "before_plan",
    "after_plan",
    "before_implement",
    "after_implement",
    "before_review",
    "after_review",
];

pub const MAX_ROLE_PROMPT_BYTES: usize = 6 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleId {
    Orchestrator,
    Planner,
    Implementor,
    Reviewer,
}

impl RoleId {
    pub const ALL: [RoleId; 4] = [
        RoleId::Orchestrator,
        RoleId::Planner,
        RoleId::Implementor,
        RoleId::Reviewer,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RoleId::Orchestrator => "orchestrator",
            RoleId::Planner => "planner",
            RoleId::Implementor => "implementor",
            RoleId::Reviewer => "reviewer",
        }
    }

    pub fn parse(value: &str) -> Option<RoleId> {
        match value {
            "orchestrator" => Some(RoleId::Orchestrator),
            "planner" => Some(RoleId::Planner),
            "implementor" => Some(RoleId::Implementor),
            "reviewer" => Some(RoleId::Reviewer),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleSource {
    Default,
    User,
}

impl RoleSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RoleSource::Default => "default",
            RoleSource::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSpec {
    pub id: RoleId,
    pub provider: ProviderId,
    pub model: String,
    pub effort: Option<String>,
    pub hook_events: Vec<String>,
    pub prompt: String,
    pub config_source: RoleSource,
    pub config_path: PathBuf,
    pub prompt_source: RoleSource,
    pub prompt_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRegistry {
    pub roles: Vec<RoleSpec>,
    pub hooks: BTreeMap<String, Vec<HookSpec>>,
    pub config_path: PathBuf,
}

impl RoleRegistry {
    pub fn get(&self, id: RoleId) -> &RoleSpec {
        self.roles
            .iter()
            .find(|role| role.id == id)
            .unwrap_or_else(|| panic!("role registry missing {}", id.as_str()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpec {
    pub argv: Vec<String>,
    #[serde(default = "default_hook_timeout")]
    pub timeout_secs: u64,
}

fn default_hook_timeout() -> u64 {
    30
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RolesFile {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    roles: BTreeMap<String, RoleOverride>,
    #[serde(default)]
    hooks: BTreeMap<String, Vec<HookSpec>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleOverride {
    provider: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

struct RoleDefaults {
    provider: ProviderId,
    model: &'static str,
    prompt: &'static str,
}

fn defaults(id: RoleId) -> RoleDefaults {
    match id {
        RoleId::Orchestrator => RoleDefaults {
            provider: ProviderId::Claude,
            model: "claude-opus-5-5",
            prompt: "You are Baton's orchestrator. You are the only role that speaks to the user. Own the goal, ask for missing decisions, delegate implementation, and accept work only after the reviewer returns a supported verdict. Never implement or review the work yourself.",
        },
        RoleId::Planner => RoleDefaults {
            provider: ProviderId::Claude,
            model: "claude-opus-5-5",
            prompt: "You are Baton's planner. Turn the user's goal into the smallest set of independent, self-contained implementor tasks. Use only installed harnesses, state disjoint file ownership, and return the plan JSON requested by Baton. Do not implement the work.",
        },
        RoleId::Implementor => RoleDefaults {
            provider: ProviderId::Codex,
            model: "gpt-6.2-sol",
            prompt: "You are Baton's implementor. Work only in the assigned worktree. Complete the task, run the relevant checks, and report the changed files, commands, and observed results. Ask the orchestrator with `BATON_QUESTION:` when a user decision is required. Do not review or approve your own work.",
        },
        RoleId::Reviewer => RoleDefaults {
            provider: ProviderId::Codex,
            model: "gpt-6.2-astra",
            prompt: "You are Baton's reviewer. Inspect the implementation diff and the implementor's evidence. Try to falsify the completion claim, run focused checks when needed, and return `APPROVE` or `REQUEST_CHANGES` with concrete evidence. Do not edit the implementation.",
        },
    }
}

pub fn config_path(home: &Path) -> PathBuf {
    home.join("roles.json")
}

pub fn prompt_path(home: &Path, id: RoleId) -> PathBuf {
    home.join("roles").join(format!("{}.md", id.as_str()))
}

fn read_config(home: &Path) -> Result<RolesFile> {
    let path = config_path(home);
    if !regular_file_or_absent(&path)? {
        return Ok(RolesFile::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))
}

fn regular_file_or_absent(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) if !path.is_file() => {
            anyhow::bail!("{} is not a regular file", path.display());
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("cannot inspect {}", path.display())),
    }
}

fn validate_hooks(hooks: &BTreeMap<String, Vec<HookSpec>>) -> Result<()> {
    for (event, specs) in hooks {
        if !HOOK_EVENTS.contains(&event.as_str()) {
            anyhow::bail!("unknown role hook event {event}");
        }
        for spec in specs {
            if spec.argv.is_empty() || spec.argv[0].trim().is_empty() {
                anyhow::bail!("role hook {event} has an empty command");
            }
            if !(1..=300).contains(&spec.timeout_secs) {
                anyhow::bail!("role hook {event} timeout must be between 1 and 300 seconds");
            }
        }
    }
    Ok(())
}

pub(crate) fn safe_model_token(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 200
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '/' | ':')
        })
}

pub fn load(home: &Path) -> Result<RoleRegistry> {
    let file = read_config(home)?;
    if file.schema_version.is_some_and(|version| version != 1) {
        anyhow::bail!("unsupported roles schema version");
    }
    validate_hooks(&file.hooks)?;
    let mut roles = Vec::new();
    for id in RoleId::ALL {
        let defaults = defaults(id);
        let override_spec = file.roles.get(id.as_str());
        let provider_override = override_spec.and_then(|spec| spec.provider.as_deref());
        let model_override = override_spec.and_then(|spec| spec.model.as_deref());
        if provider_override.is_some() != model_override.is_some() {
            anyhow::bail!("role {} must set provider and model together", id.as_str());
        }
        let provider = provider_override
            .map(|provider| {
                ProviderId::parse(provider.trim()).ok_or_else(|| {
                    anyhow::anyhow!("unknown provider for role {}: {provider}", id.as_str())
                })
            })
            .transpose()?
            .unwrap_or(defaults.provider);
        let model = match model_override {
            Some(model) if safe_model_token(model) => model.trim().to_string(),
            Some(model) => anyhow::bail!("invalid model for role {}: {model:?}", id.as_str()),
            None => defaults.model.to_string(),
        };
        let effort = override_spec
            .and_then(|spec| spec.effort.as_deref())
            .map(|effort| {
                normalize_reasoning_effort(effort)
                    .ok_or_else(|| anyhow::anyhow!("invalid effort for role {}", id.as_str()))
            })
            .transpose()?;
        if effort.is_some() && !provider.supports_effort() {
            anyhow::bail!(
                "role {} uses effort with unsupported provider {provider}",
                id.as_str()
            );
        }
        let path = prompt_path(home, id);
        let (prompt, prompt_source) = if regular_file_or_absent(&path)? {
            (
                std::fs::read_to_string(&path)
                    .with_context(|| format!("cannot read {}", path.display()))?,
                RoleSource::User,
            )
        } else {
            (defaults.prompt.to_string(), RoleSource::Default)
        };
        if prompt.trim().is_empty() {
            anyhow::bail!("role {} has an empty prompt", id.as_str());
        }
        if prompt.len() > MAX_ROLE_PROMPT_BYTES {
            anyhow::bail!(
                "role {} prompt exceeds {} bytes",
                id.as_str(),
                MAX_ROLE_PROMPT_BYTES
            );
        }
        roles.push(RoleSpec {
            id,
            provider,
            model,
            effort,
            hook_events: file.hooks.keys().cloned().collect(),
            prompt,
            config_source: if override_spec.is_some() {
                RoleSource::User
            } else {
                RoleSource::Default
            },
            config_path: config_path(home),
            prompt_source,
            prompt_path: path,
        });
    }
    for name in file.roles.keys() {
        if RoleId::parse(name).is_none() {
            anyhow::bail!("unknown role {name}");
        }
    }
    Ok(RoleRegistry {
        roles,
        hooks: file.hooks,
        config_path: config_path(home),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_role_models() {
        let home = tempfile::tempdir().unwrap();
        let registry = load(home.path()).unwrap();
        let models = registry
            .roles
            .iter()
            .map(|role| (role.id, role.provider, role.model.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            models,
            vec![
                (RoleId::Orchestrator, ProviderId::Claude, "claude-opus-5-5"),
                (RoleId::Planner, ProviderId::Claude, "claude-opus-5-5"),
                (RoleId::Implementor, ProviderId::Codex, "gpt-6.2-sol"),
                (RoleId::Reviewer, ProviderId::Codex, "gpt-6.2-astra"),
            ]
        );
    }

    #[test]
    fn user_settings_override_every_role_and_prompt() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("roles")).unwrap();
        std::fs::write(
            config_path(home.path()),
            r#"{"roles":{"orchestrator":{"provider":"codex","model":"rits/deepseek-v4.1-flash"},"planner":{"provider":"codex","model":"rits/deepseek-v4.1-flash"},"implementor":{"provider":"codex","model":"rits/deepseek-v4.1-flash"},"reviewer":{"provider":"codex","model":"rits/deepseek-v4.1-flash"}}}"#,
        )
        .unwrap();
        std::fs::write(prompt_path(home.path(), RoleId::Reviewer), "custom review").unwrap();

        let registry = load(home.path()).unwrap();
        assert!(registry
            .roles
            .iter()
            .all(|role| role.provider == ProviderId::Codex));
        assert!(registry
            .roles
            .iter()
            .all(|role| role.model == "rits/deepseek-v4.1-flash"));
        let reviewer = registry
            .roles
            .iter()
            .find(|role| role.id == RoleId::Reviewer)
            .unwrap();
        assert_eq!(reviewer.prompt, "custom review");
        assert_eq!(reviewer.prompt_source, RoleSource::User);
    }

    #[test]
    fn malformed_role_config_and_hooks_fail_closed() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(config_path(home.path()), "not json").unwrap();
        assert!(load(home.path()).is_err());

        std::fs::write(
            config_path(home.path()),
            r#"{"hooks":{"after_everything":[{"argv":["true"]}]}}"#,
        )
        .unwrap();
        assert!(load(home.path())
            .unwrap_err()
            .to_string()
            .contains("unknown role hook event"));
    }

    #[test]
    fn partial_and_unsafe_role_overrides_fail_closed() {
        for config in [
            r#"{"roles":{"reviewer":{"provider":"claude"}}}"#,
            r#"{"roles":{"reviewer":{"provider":"claude","model":""}}}"#,
            r#"{"roles":{"reviewer":{"provider":"codex","model":"bad\nmodel"}}}"#,
            r#"{"roles":{"reviewer":{"provider":"cursor","model":"composer-2.5","effort":"high"}}}"#,
            r#"{"roles":{"reviewer":{"provider":"codex","model":"gpt-6.2-astra"},"unknown":{}}}"#,
            r#"{"schema_version":2}"#,
        ] {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(config_path(home.path()), config).unwrap();
            assert!(load(home.path()).is_err(), "config should fail: {config}");
        }

        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("roles")).unwrap();
        std::fs::write(prompt_path(home.path(), RoleId::Planner), " \n").unwrap();
        assert!(load(home.path())
            .unwrap_err()
            .to_string()
            .contains("empty prompt"));

        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(config_path(home.path())).unwrap();
        assert!(load(home.path())
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));

        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(prompt_path(home.path(), RoleId::Planner)).unwrap();
        assert!(load(home.path())
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));

        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("roles")).unwrap();
        std::fs::write(
            prompt_path(home.path(), RoleId::Planner),
            "x".repeat(MAX_ROLE_PROMPT_BYTES + 1),
        )
        .unwrap();
        assert!(load(home.path())
            .unwrap_err()
            .to_string()
            .contains("prompt exceeds"));
    }
}
