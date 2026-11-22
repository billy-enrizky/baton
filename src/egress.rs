//! Every path by which Baton data can leave this machine, or leave Baton.
//!
//! The list is deliberately explicit. A test can cover the Baton-owned calls,
//! but it cannot enumerate what a provider CLI, a shell command, or a
//! repository hook does with the data it is given. Naming those as `delegated`
//! is the honest boundary, and `baton doctor --json` shows the same list.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Baton makes the request itself.
    Baton,
    /// A child process makes the request; Baton only hands it the data.
    Delegated,
    /// Baton stays on this machine.
    Local,
}

impl Reach {
    pub fn as_str(self) -> &'static str {
        match self {
            Reach::Baton => "baton",
            Reach::Delegated => "delegated",
            Reach::Local => "local",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Path_ {
    pub name: &'static str,
    pub reach: Reach,
    pub detail: &'static str,
    /// Baton proves what it hands over, not what the child does with it.
    pub payload_tested: bool,
}

pub const PATHS: &[Path_] = &[
    Path_ {
        name: "provider turn",
        reach: Reach::Delegated,
        detail: "the provider CLI receives the prompt, the working directory, and its own auth; its network traffic is outside Baton",
        payload_tested: false,
    },
    Path_ {
        name: "handoff",
        reach: Reach::Delegated,
        detail: "prior messages and the working-tree summary go to another provider, redacted of configured keys",
        payload_tested: true,
    },
    Path_ {
        name: "pull request creation",
        reach: Reach::Delegated,
        detail: "`git push -u` and `gh pr create` run in the thread worktree",
        payload_tested: false,
    },
    Path_ {
        name: "pull request listing",
        reach: Reach::Delegated,
        detail: "`gh pr list` runs before a worker PR is created",
        payload_tested: false,
    },
    Path_ {
        name: "self update",
        reach: Reach::Delegated,
        detail: "`cargo install` fetches and builds the requested version",
        payload_tested: false,
    },
    Path_ {
        name: "MCP stdio server",
        reach: Reach::Local,
        detail: "stdin and stdout only; no socket and no token",
        payload_tested: false,
    },
    Path_ {
        name: "thread store",
        reach: Reach::Local,
        detail: "SQLite under BATON_HOME",
        payload_tested: false,
    },
];

pub fn json() -> serde_json::Value {
    serde_json::Value::Array(
        PATHS
            .iter()
            .map(|path| {
                serde_json::json!({
                    "name": path.name,
                    "reach": path.reach.as_str(),
                    "detail": path.detail,
                    "payload_tested": path.payload_tested,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_path_has_a_reach_and_a_detail() {
        for path in PATHS {
            assert!(!path.name.is_empty());
            assert!(
                path.detail.len() > 20,
                "{} needs a real explanation",
                path.name
            );
        }
    }

    #[test]
    fn only_a_payload_that_leaves_baton_can_be_payload_tested() {
        assert!(
            PATHS.iter().any(|path| path.payload_tested),
            "the inventory must mark at least the handoff payload as tested"
        );
        for path in PATHS {
            if path.payload_tested {
                assert_eq!(
                    path.reach,
                    Reach::Delegated,
                    "{} does not leave Baton, so there is no payload to redact",
                    path.name
                );
            }
            if path.reach == Reach::Local {
                assert!(!path.payload_tested, "{} stays on this machine", path.name);
            }
        }
    }

    #[test]
    fn the_inventory_names_the_non_testable_paths() {
        let names: Vec<&str> = PATHS.iter().map(|path| path.name).collect();
        for expected in [
            "provider turn",
            "handoff",
            "pull request creation",
            "pull request listing",
            "self update",
            "MCP stdio server",
            "thread store",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }
}
