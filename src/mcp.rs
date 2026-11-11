//! Minimal MCP stdio server exposing baton's durable thread state to coding
//! agents. The server is read-only except for the explicit queue-clear tool.

use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::config::resolve_config;
use crate::process;
use crate::status::{self, ThreadStatus, TurnStatus};
use crate::store::{BatonStore, MessageRow, ThreadRow, TurnRow};
use crate::usage::{format_usage_line, sum_usage};

pub struct McpServer {
    store: BatonStore,
    home: PathBuf,
    cwd: PathBuf,
}

impl McpServer {
    pub fn new(store: BatonStore, home: PathBuf, cwd: PathBuf) -> Self {
        Self { store, home, cwd }
    }

    pub fn handle(&self, frame: Value) -> Option<Value> {
        let id = frame.get("id").cloned();
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
        if id.is_none() {
            return None;
        }
        let id = id.unwrap_or(Value::Null);
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2024-11-05"),
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "baton",
                    "version": env!("CARGO_PKG_VERSION"),
                    "title": "Baton thread state"
                },
                "instructions": "Use baton_status, baton_threads, baton_turns, baton_queue, baton_messages, baton_search, baton_processes, and baton_latest_diff to inspect the local baton store."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_descriptors() })),
            "tools/call" => self.call_tool(&params),
            _ => {
                return Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("unknown method {method}") }
                }));
            }
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32602, "message": error.to_string() }
            }),
        })
    }

    fn call_tool(&self, params: &Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing tool name"))?;
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        match self.tool_value(name, &arguments) {
            Ok(value) => Ok(tool_content(&value, false)),
            Err(error) => Ok(tool_content(&json!({ "error": error.to_string() }), true)),
        }
    }

    fn tool_value(&self, name: &str, arguments: &Value) -> Result<Value> {
        match name {
            "baton_status" => {
                let thread = self.resolve_thread(argument_thread(arguments))?;
                Ok(status::to_json(&self.status_for(&thread)?))
            }
            "baton_threads" => {
                let all = arguments.get("all").and_then(Value::as_bool).unwrap_or(false);
                let threads = if all {
                    self.store.list_threads(None)?
                } else {
                    self.store.list_threads_for_path(&self.cwd)?
                };
                let mut values = Vec::new();
                for thread in &threads {
                    values.push(status::to_json(&self.status_for(thread)?));
                }
                Ok(json!({
                    "schema_version": status::SCHEMA_VERSION,
                    "count": values.len(),
                    "threads": values,
                }))
            }
            "baton_turns" => {
                let thread = self.resolve_thread(argument_thread(arguments))?;
                let turns = self.store.list_turn_records(&thread.id)?;
                Ok(json!({
                    "schema_version": status::SCHEMA_VERSION,
                    "thread_id": thread.id,
                    "title": thread.title,
                    "count": turns.len(),
                    "turns": turns.iter().map(turn_json).collect::<Vec<_>>(),
                }))
            }
            "baton_queue" => {
                let thread = self.resolve_thread(argument_thread(arguments))?;
                if arguments.get("clear").and_then(Value::as_bool).unwrap_or(false) {
                    self.store.clear_queued_messages(&thread.id)?;
                    return Ok(json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "thread_id": thread.id,
                        "title": thread.title,
                        "cleared": true,
                        "count": 0,
                        "messages": [],
                    }));
                }
                let messages = self.store.list_queued_messages(&thread.id)?;
                Ok(json!({
                    "schema_version": status::SCHEMA_VERSION,
                    "thread_id": thread.id,
                    "title": thread.title,
                    "cleared": false,
                    "count": messages.len(),
                    "messages": messages,
                }))
            }
            "baton_messages" => {
                let thread = self.resolve_thread(argument_thread(arguments))?;
                let limit = arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(20)
                    .clamp(1, 200) as usize;
                let messages = self.store.list_recent_messages(&thread.id, limit)?;
                Ok(json!({
                    "schema_version": status::SCHEMA_VERSION,
                    "thread_id": thread.id,
                    "title": thread.title,
                    "count": messages.len(),
                    "messages": messages.iter().map(message_json).collect::<Vec<_>>(),
                }))
            }
            "baton_search" => {
                let query = arguments
                    .get("query")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|query| !query.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("query is required"))?;
                let all = arguments.get("all").and_then(Value::as_bool).unwrap_or(false);
                let limit = arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(20)
                    .clamp(1, 200) as usize;
                let cwd = (!all).then(|| self.cwd.to_string_lossy().to_string());
                let results = self.store.search_messages(cwd.as_deref(), query, limit)?;
                Ok(json!({
                    "schema_version": status::SCHEMA_VERSION,
                    "query": query,
                    "all": all,
                    "count": results.len(),
                    "results": results.iter().map(|result| json!({
                        "thread_id": result.thread_id,
                        "title": result.title,
                        "message_id": result.message_id,
                        "turn": result.turn,
                        "role": result.role,
                        "provider": result.provider,
                        "model": result.model,
                        "text": result.text,
                        "created_at": result.created_at,
                    })).collect::<Vec<_>>(),
                }))
            }
            "baton_latest_diff" => {
                let thread = self.resolve_thread(argument_thread(arguments))?;
                let diff = self
                    .store
                    .list_messages(&thread.id)?
                    .into_iter()
                    .rev()
                    .find(|message| message.role == "diff");
                Ok(match diff {
                    Some(diff) => json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "thread_id": thread.id,
                        "title": thread.title,
                        "turn": diff.turn,
                        "diff": diff.text,
                    }),
                    None => json!({
                        "schema_version": status::SCHEMA_VERSION,
                        "thread_id": thread.id,
                        "title": thread.title,
                        "turn": null,
                        "diff": null,
                    }),
                })
            }
            "baton_processes" => {
                let rows = self.store.list_provider_processes()?;
                let processes = rows
                    .iter()
                    .map(|row| {
                        let state = process::probe(row);
                        json!({
                            "pid": row.pid,
                            "provider": row.provider.as_str(),
                            "port": row.port,
                            "cwd": row.cwd,
                            "owner_pid": row.owner_pid,
                            "owner_running": state.owner_running,
                            "process_alive": state.alive,
                            "command_matches": state.command_matches,
                            "command": state.command.unwrap_or_else(|| row.command.clone()),
                            "started_at": row.started_at,
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(json!({
                    "schema_version": status::SCHEMA_VERSION,
                    "count": processes.len(),
                    "processes": processes,
                }))
            }
            _ => anyhow::bail!("unknown baton tool {name}"),
        }
    }

    fn resolve_thread(&self, requested: Option<&str>) -> Result<ThreadRow> {
        match requested {
            Some(thread_id) => {
                if let Some(thread) = self.store.get_thread(thread_id)? {
                    return Ok(thread);
                }
                self.store
                    .list_threads(None)?
                    .into_iter()
                    .find(|thread| thread.id.starts_with(thread_id))
                    .ok_or_else(|| anyhow::anyhow!("no thread matches {thread_id}"))
            }
            None => self
                .store
                .list_threads_for_path(&self.cwd)?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("no threads in {}", self.cwd.display())),
        }
    }

    fn status_for(&self, thread: &ThreadRow) -> Result<ThreadStatus> {
        let resolved = resolve_config(&self.home, Path::new(&thread.cwd));
        let model = thread
            .current_model
            .clone()
            .unwrap_or_else(|| resolved.for_provider(thread.current_provider).model.clone());
        let turn_records = self.store.list_turn_records(&thread.id)?;
        let turn_count = turn_records.len();
        let latest_turn = turn_records.into_iter().last().map(|turn| TurnStatus {
            turn: turn.turn,
            phase: turn.phase,
            started_at: turn.started_at,
            ended_at: turn.ended_at,
            error: turn.error,
            provider_turn_id: turn.provider_turn_id,
        });
        let usage_rows = self.store.list_usage(Some(&thread.id))?;
        let usage = if usage_rows.is_empty() {
            None
        } else {
            Some(format_usage_line(&sum_usage(&usage_rows)))
        };
        Ok(ThreadStatus {
            thread_id: thread.id.clone(),
            title: thread.title.clone(),
            provider: thread.current_provider,
            model,
            permission_mode: thread.permission_mode,
            goal: thread.goal.clone(),
            goal_paused: thread.goal_paused,
            latest_turn,
            turn_count,
            queued: self.store.list_queued_messages(&thread.id)?,
            usage,
        })
    }
}

fn argument_thread(arguments: &Value) -> Option<&str> {
    arguments
        .get("thread")
        .and_then(Value::as_str)
        .filter(|thread| !thread.trim().is_empty())
}

fn tool_content(value: &Value, is_error: bool) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }],
        "isError": is_error
    })
}

fn turn_json(turn: &TurnRow) -> Value {
    let open = !turn.phase.is_terminal();
    json!({
        "schema_version": status::SCHEMA_VERSION,
        "turn": turn.turn,
        "phase": turn.phase.as_str(),
        "open": open,
        "stale_hint": if open { Some(status::STALE_HINT) } else { None },
        "provider": turn.provider.as_str(),
        "model": turn.model,
        "started_at": turn.started_at,
        "ended_at": turn.ended_at,
        "provider_turn_id": turn.provider_turn_id,
        "error": turn.error,
    })
}

fn message_json(message: &MessageRow) -> Value {
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

fn thread_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "thread": { "type": "string", "description": "Thread id or unique prefix. Omit for the newest thread in the server cwd." }
        },
        "additionalProperties": false
    })
}

fn tool_descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": "baton_status",
            "description": "Read one thread's provider, model, permission mode, goal, latest turn phase, queue, usage, and stale-turn warning.",
            "inputSchema": thread_schema(),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "baton_threads",
            "description": "List durable threads for the server cwd, or every thread with all=true.",
            "inputSchema": {
                "type": "object",
                "properties": { "all": { "type": "boolean", "description": "List threads across every project." } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "baton_turns",
            "description": "List durable turn phases for a thread, including provider turn ids, timestamps, and errors.",
            "inputSchema": thread_schema(),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "baton_queue",
            "description": "List a thread's durable queued follow-ups. Set clear=true to remove them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "thread": { "type": "string", "description": "Thread id or unique prefix." },
                    "clear": { "type": "boolean", "description": "Remove every queued follow-up." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "baton_messages",
            "description": "Read the newest conversation rows for a thread, including user, assistant, tool, and diff rows.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "thread": { "type": "string", "description": "Thread id or unique prefix." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 20 }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "baton_search",
            "description": "Search conversation text and thread titles in the server cwd, or every project with all=true.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Case-insensitive substring to search for." },
                    "all": { "type": "boolean", "description": "Search every project instead of the server cwd." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 20 }
                },
                "required": ["query"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "baton_latest_diff",
            "description": "Read the newest stored turn diff for a thread.",
            "inputSchema": thread_schema(),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "baton_processes",
            "description": "List provider server processes tracked by baton, with process, owner, and command-line verification state.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true }
        }),
    ]
}

pub fn run_stdio(server: McpServer) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(frame) => server.handle(frame),
            Err(error) => Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": format!("parse error: {error}") }
            })),
        };
        if let Some(response) = response {
            writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewThread;
    use crate::types::{PermissionMode, ProviderId};

    fn server() -> McpServer {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let store = BatonStore::memory().unwrap();
        let thread = store
            .create_thread(NewThread {
                title: "mcp thread".into(),
                cwd: cwd.to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: Some("model-1".into()),
            })
            .unwrap();
        store
            .set_thread_permission_mode(&thread.id, PermissionMode::Plan)
            .unwrap();
        store
            .add_message(&thread.id, 1, "user", Some("codex"), Some("model-1"), "hello")
            .unwrap();
        store
            .add_message(&thread.id, 1, "assistant", Some("codex"), Some("model-1"), "world")
            .unwrap();
        store.start_turn_record(&thread.id, 1, ProviderId::Codex, Some("model-1")).unwrap();
        store.finish_turn_record(&thread.id, 1, crate::types::TurnPhase::Completed, None).unwrap();
        store
            .add_message(&thread.id, 1, "diff", None, None, "diff --git a/a b/a")
            .unwrap();
        store.enqueue_message(&thread.id, "follow up").unwrap();
        McpServer::new(store, dir.path().to_path_buf(), cwd)
    }

    fn request(server: &McpServer, id: u64, method: &str, params: Value) -> Value {
        server
            .handle(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .unwrap()
    }

    fn tool_json(server: &McpServer, name: &str, arguments: Value) -> Value {
        let response = request(
            server,
            3,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn initialize_and_list_tools() {
        let server = server();
        let initialized = request(
            &server,
            1,
            "initialize",
            json!({ "protocolVersion": "2025-06-18" }),
        );
        assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(initialized["result"]["serverInfo"]["name"], "baton");
        let listed = request(&server, 2, "tools/list", Value::Null);
        let names: Vec<_> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"baton_status"));
        assert!(names.contains(&"baton_messages"));
        assert!(names.contains(&"baton_latest_diff"));
        assert!(names.contains(&"baton_processes"));
    }

    #[test]
    fn status_and_turns_use_the_thread_contract() {
        let server = server();
        let status = tool_json(&server, "baton_status", json!({}));
        assert_eq!(status["schema_version"], status::SCHEMA_VERSION);
        assert_eq!(status["provider"], "codex");
        assert_eq!(status["permission_mode"], "plan");
        assert_eq!(status["queue_count"], 1);
        let turns = tool_json(&server, "baton_turns", json!({}));
        assert_eq!(turns["count"], 1);
        assert_eq!(turns["turns"][0]["phase"], "completed");
    }

    #[test]
    fn messages_queue_and_diff_are_readable() {
        let server = server();
        let messages = tool_json(&server, "baton_messages", json!({ "limit": 2 }));
        assert_eq!(messages["count"], 2);
        assert_eq!(messages["messages"][0]["role"], "assistant");
        let diff = tool_json(&server, "baton_latest_diff", json!({}));
        assert!(diff["diff"].as_str().unwrap().contains("diff --git"));
        let found = tool_json(&server, "baton_search", json!({ "query": "world" }));
        assert_eq!(found["count"], 1);
        assert_eq!(found["results"][0]["role"], "assistant");
        let cleared = tool_json(&server, "baton_queue", json!({ "clear": true }));
        assert_eq!(cleared["cleared"], true);
        assert_eq!(cleared["count"], 0);
    }

    #[test]
    fn processes_report_tracked_provider_servers() {
        let server = server();
        server
            .store
            .record_provider_process(
                u32::MAX,
                ProviderId::OpenCode,
                Some(41234),
                "baton-test-process",
                "/work",
                u32::MAX,
            )
            .unwrap();
        let processes = tool_json(&server, "baton_processes", json!({}));
        assert_eq!(processes["schema_version"], status::SCHEMA_VERSION);
        assert_eq!(processes["count"], 1);
        assert_eq!(processes["processes"][0]["provider"], "opencode");
        assert_eq!(processes["processes"][0]["process_alive"], false);
        assert_eq!(processes["processes"][0]["command_matches"], false);
    }

    #[test]
    fn notifications_have_no_response() {
        let server = server();
        assert!(server
            .handle(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none());
    }
}
