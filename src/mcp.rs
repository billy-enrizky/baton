//! Minimal MCP stdio server exposing baton's durable thread state to coding
//! agents. The server is read-only except for explicit queue clear and fork.

use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::config::resolve_config;
use crate::orchestrate;
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
                "instructions": "Use baton_doctor, baton_status, baton_threads, baton_turns, baton_queue, baton_messages, baton_search, baton_processes, baton_latest_diff, baton_agents, baton_orchestration_runs, and baton_orchestration_report to inspect the local baton store, provider readiness, and durable orchestration runs. baton_fork creates a new thread from an existing conversation; set native=true for a Codex or OpenCode native fork."
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
            "baton_agents" => {
                let thread = self.resolve_thread(argument_thread(arguments))?;
                let rows = self.store.list_subagents(&thread.id)?;
                Ok(json!({
                    "schema_version": status::SCHEMA_VERSION,
                    "thread_id": thread.id,
                    "title": thread.title,
                    "count": rows.len(),
                    "subagents": rows.iter().map(|row| json!({
                        "id": row.id,
                        "turn": row.turn,
                        "provider": row.provider,
                        "call_id": row.call_id,
                        "name": row.name,
                        "detail": row.detail,
                        "status": row.status,
                        "started_at": row.started_at,
                        "finished_at": row.finished_at,
                    })).collect::<Vec<_>>(),
                }))
            }
            "baton_activity" => {
                let thread = self.resolve_thread(argument_thread(arguments))?;
                let turns = self.store.list_turn_records(&thread.id)?;
                let messages = self.store.list_messages(&thread.id)?;
                let usage = self.store.list_usage(Some(&thread.id))?;
                let rows = crate::activity::build(&thread, &turns, &messages, &usage);
                Ok(crate::activity::to_json(&thread.id, &rows))
            }
            "baton_memories" => {
                let all = arguments.get("all").and_then(Value::as_bool).unwrap_or(false);
                let cwd = arguments
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        argument_thread(arguments)
                            .and_then(|id| self.store.get_thread(id).ok().flatten())
                            .map(|thread| thread.cwd)
                    })
                    .unwrap_or_else(|| self.cwd.to_string_lossy().to_string());
                let rows = if all {
                    self.store.list_all_memories()?
                } else {
                    self.store
                        .list_memories(&crate::memory::project_key(&cwd))?
                };
                Ok(json!({
                    "schema_version": 1,
                    "count": rows.len(),
                    "memories": rows.iter().map(memory_json).collect::<Vec<_>>(),
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
            "baton_doctor" => {
                let report = crate::doctor::build(&self.home, &self.cwd, &self.store)?;
                Ok(crate::doctor::to_json(&report))
            }
            "baton_orchestration_runs" => {
                let limit = arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(10)
                    .clamp(1, 100) as usize;
                let all = arguments
                    .get("all")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mut runs = self.store.list_orchestration_runs(500)?;
                if !all {
                    runs.retain(|run| run.archived_at.is_none());
                }
                runs.truncate(limit);
                let values = runs
                    .iter()
                    .map(|run| {
                        let tasks = self
                            .store
                            .list_orchestration_tasks(&run.id)
                            .unwrap_or_default();
                        let completed = tasks
                            .iter()
                            .filter(|task| task.status == "completed")
                            .count();
                        let failed = tasks
                            .iter()
                            .filter(|task| task.status == "failed")
                            .count();
                        let cancelled = tasks
                            .iter()
                            .filter(|task| task.status == "cancelled")
                            .count();
                        let needs_input = tasks
                            .iter()
                            .filter(|task| task.status == "needs_input")
                            .count();
                        let running = tasks
                            .iter()
                            .filter(|task| task.status == "running")
                            .count();
                        json!({
                            "run_id": run.id,
                            "goal": run.goal,
                            "status": run.status,
                            "coordinator_provider": run.coordinator_provider.as_str(),
                            "coordinator_model": run.coordinator_model,
                            "summary": run.summary,
                            "dirty_source": run.dirty_source,
                            "worker_baseline": if run.dirty_source { "source snapshot" } else { "HEAD" },
                            "created_at": run.created_at,
                            "finished_at": run.finished_at,
                            "archived_at": run.archived_at,
                            "owner_pid": run.owner_pid,
                            "task_count": tasks.len(),
                            "completed": completed,
                            "failed": failed,
                            "cancelled": cancelled,
                            "needs_input": needs_input,
                            "running": running,
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(json!({
                    "schema_version": orchestrate::SCHEMA_VERSION,
                    "count": values.len(),
                    "runs": values,
                }))
            }
            "baton_orchestration_report" => {
                let run = arguments
                    .get("run")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|run| !run.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("run is required"))?;
                let report = orchestrate::report_from_rows(&self.store, &self.cwd, run)?;
                Ok(orchestrate::to_json(&report))
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
                            "process_start": state.process_start,
                            "start_matches": state.start_matches,
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
            "baton_fork" => {
                let thread = self.resolve_thread(argument_thread(arguments))?;
                let turn = arguments.get("turn").and_then(Value::as_i64);
                let title = arguments
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| match turn {
                        Some(turn) => format!("{} (fork at turn {turn})", thread.title),
                        None => format!("{} (fork)", thread.title),
                    });
                let native = arguments
                    .get("native")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let native_session = if native {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(crate::provider_runtime::fork_native_session(
                        &self.store,
                        &self.home,
                        &thread,
                        turn,
                        crate::process::ProcessRecorder::disabled(),
                    ))?
                } else {
                    None
                };
                let forked = self
                    .store
                    .fork_thread_through_turn(&thread.id, &title, turn)?;
                if let Some((native_provider, native_id, model)) = native_session.as_ref() {
                    self.store.upsert_provider_session(
                        &forked.id,
                        *native_provider,
                        native_id,
                        Some(model.clone()),
                        None,
                    )?;
                }
                Ok(json!({
                    "schema_version": status::SCHEMA_VERSION,
                    "source_thread_id": thread.id,
                    "thread_id": forked.id,
                    "title": forked.title,
                    "cwd": forked.cwd,
                    "provider": forked.current_provider.as_str(),
                    "model": forked.current_model,
                    "through_turn": turn,
                    "native": native,
                    "native_session_provider": native_session
                        .as_ref()
                        .map(|(provider, _, _)| provider.as_str()),
                    "native_session_id": native_session.as_ref().map(|(_, id, _)| id),
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
            reasoning_effort: thread.reasoning_effort.clone(),
            permission_mode: thread.permission_mode,
            session_mode: thread.session_mode,
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

fn memory_json(memory: &crate::store::MemoryRow) -> Value {
    json!({
        "id": memory.id,
        "scope": memory.scope,
        "cwd": memory.cwd,
        "text": memory.text,
        "created_at": memory.created_at,
        "updated_at": memory.updated_at,
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
            "name": "baton_agents",
            "description": "List the subagent calls recorded for a thread, with turn, provider, call id, name, detail, and status.",
            "inputSchema": thread_schema(),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "baton_activity",
            "description": "Inspect every turn's provider, model, phase, tool calls, diff presence, and usage. Imported message-only turns are marked imported.",
            "inputSchema": thread_schema(),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "baton_memories",
            "description": "List governed project and global memories. Defaults to the server cwd's project; all lists every scope.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "thread": { "type": "string", "description": "Thread id or unique prefix; its cwd selects the project." },
                    "cwd": { "type": "string", "description": "Project cwd to look up. Defaults to the thread or the server cwd." },
                    "all": { "type": "boolean", "description": "List every memory across projects." }
                },
                "additionalProperties": false
            },
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
            "name": "baton_doctor",
            "description": "Check provider binaries, effective models/auth/modes, config files, database size, thread count, and tracked provider process health.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        }),
        json!({
            "name": "baton_processes",
            "description": "List provider server processes tracked by baton, with process, owner, command-line, and process-start verification state.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "baton_orchestration_runs",
            "description": "List durable orchestration runs with status, coordinator, task counts, and timestamps. Archived runs require all=true.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 10 },
                    "all": { "type": "boolean", "description": "Include archived runs." }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "baton_orchestration_report",
            "description": "Read one orchestration run's plan, worker tasks, commits, worktree paths, PR links, text, and errors.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run": { "type": "string", "description": "Run id or unique prefix." }
                },
                "required": ["run"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "baton_fork",
            "description": "Create a new thread by copying a conversation, optionally only through a completed turn. The source thread is unchanged.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "thread": { "type": "string", "description": "Source thread id or unique prefix. Omit for the newest thread in the server cwd." },
                    "title": { "type": "string", "description": "Title for the new thread." },
                    "turn": { "type": "integer", "minimum": 1, "description": "Copy only messages and turns through this completed turn." },
                    "native": { "type": "boolean", "description": "Fork the provider's native session when supported (currently Codex)." }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false }
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
        assert!(names.contains(&"baton_doctor"));
        assert!(names.contains(&"baton_messages"));
        assert!(names.contains(&"baton_latest_diff"));
        assert!(names.contains(&"baton_processes"));
        assert!(names.contains(&"baton_orchestration_runs"));
        assert!(names.contains(&"baton_orchestration_report"));
        assert!(names.contains(&"baton_fork"));
        assert!(names.contains(&"baton_agents"));
        assert!(names.contains(&"baton_activity"));
        assert!(names.contains(&"baton_memories"));
        let fork_tool = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "baton_fork")
            .unwrap();
        assert!(fork_tool["inputSchema"]["properties"]["native"].is_object());
    }

    #[test]
    fn agents_are_a_read_only_view() {
        let server = server();
        server
            .store
            .add_subagent(
                &server.store.list_threads_for_path(&server.cwd).unwrap()[0].id,
                1,
                "claude",
                Some("toolu_1"),
                "Explore",
                Some("Find the parser"),
                "done",
                "2026-01-01T00:00:00Z",
                Some("2026-01-01T00:01:00Z"),
            )
            .unwrap();
        let agents = tool_json(&server, "baton_agents", json!({}));
        assert_eq!(agents["schema_version"], status::SCHEMA_VERSION);
        assert_eq!(agents["count"], 1);
        assert_eq!(agents["subagents"][0]["name"], "Explore");
        assert_eq!(agents["subagents"][0]["status"], "done");
        assert_eq!(agents["subagents"][0]["call_id"], "toolu_1");
    }

    #[test]
    fn activity_and_memories_are_read_only_views() {
        let server = server();
        let activity = tool_json(&server, "baton_activity", json!({}));
        assert_eq!(activity["schema_version"], 1);
        assert_eq!(activity["count"], 1);
        assert_eq!(activity["turns"][0]["provider"], "codex");
        assert_eq!(activity["turns"][0]["phase"], "completed");
        assert_eq!(activity["turns"][0]["has_diff"], true);

        server
            .store
            .add_memory(
                "project",
                &crate::memory::project_key(&server.cwd.to_string_lossy()),
                "Use tabs",
            )
            .unwrap();
        let memories = tool_json(&server, "baton_memories", json!({}));
        assert_eq!(memories["count"], 1);
        assert_eq!(memories["memories"][0]["text"], "Use tabs");
        let all = tool_json(&server, "baton_memories", json!({ "all": true }));
        assert_eq!(all["count"], 1);
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
    fn default_status_keeps_newest_thread_when_older_is_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let store = BatonStore::memory().unwrap();
        let older = store
            .create_thread(NewThread {
                title: "older".into(),
                cwd: cwd.to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        let newer = store
            .create_thread(NewThread {
                title: "newer".into(),
                cwd: cwd.to_string_lossy().to_string(),
                current_provider: ProviderId::Codex,
                current_model: None,
            })
            .unwrap();
        store.set_thread_pinned(&older.id, true).unwrap();
        let server = McpServer::new(store, dir.path().to_path_buf(), cwd);
        let status = tool_json(&server, "baton_status", json!({}));
        assert_eq!(status["thread_id"], newer.id);
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
    fn doctor_reports_provider_readiness() {
        let server = server();
        let doctor = tool_json(&server, "baton_doctor", json!({}));
        assert_eq!(doctor["schema_version"], crate::doctor::SCHEMA_VERSION);
        assert_eq!(doctor["db"]["thread_count"], 1);
        assert!(doctor["providers"].as_array().unwrap().len() >= 6);
        assert_eq!(doctor["processes"].as_array().unwrap().len(), 0);
        assert!(doctor["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|provider| provider["provider"] == "codex"));
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
                None,
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
    fn orchestration_runs_and_reports_are_readable() {
        let server = server();
        let plan = r#"{"summary":"one worker","tasks":[{"title":"worker","provider":"codex","prompt":"do it"}]}"#;
        server
            .store
            .create_orchestration_run(
                "run-mcp-1234",
                "read orchestration",
                ProviderId::Codex,
                "model-1",
                Some("one worker"),
                plan,
                false,
            )
            .unwrap();
        server
            .store
            .add_orchestration_task(
                "task-mcp-1",
                "run-mcp-1234",
                1,
                "worker",
                ProviderId::Codex,
                "model-1",
                "do it",
                Some("/tmp/worktree"),
                Some("baton/run-mcp/worker"),
                Some("abc123"),
                Some("thread-mcp"),
                "completed",
                Some("done"),
                None,
                "2026-11-13T00:00:00Z",
                Some("2026-11-13T00:00:01Z"),
                None,
                None,
                None,
            )
            .unwrap();
        server
            .store
            .finish_orchestration_run("run-mcp-1234", "completed")
            .unwrap();

        let runs = tool_json(&server, "baton_orchestration_runs", json!({}));
        assert_eq!(runs["schema_version"], orchestrate::SCHEMA_VERSION);
        assert_eq!(runs["count"], 1);
        assert_eq!(runs["runs"][0]["run_id"], "run-mcp-1234");
        assert_eq!(runs["runs"][0]["status"], "completed");
        assert_eq!(runs["runs"][0]["task_count"], 1);
        assert_eq!(runs["runs"][0]["completed"], 1);

        let report = tool_json(
            &server,
            "baton_orchestration_report",
            json!({ "run": "run-mcp" }),
        );
        assert_eq!(report["schema_version"], orchestrate::SCHEMA_VERSION);
        assert_eq!(report["run_id"], "run-mcp-1234");
        assert_eq!(report["tasks"][0]["commit"], "abc123");
        assert_eq!(report["tasks"][0]["thread_id"], "thread-mcp");

        server
            .store
            .archive_orchestration_run("run-mcp-1234")
            .unwrap();
        let visible = tool_json(&server, "baton_orchestration_runs", json!({}));
        assert_eq!(visible["count"], 0);
        let all = tool_json(&server, "baton_orchestration_runs", json!({ "all": true }));
        assert_eq!(all["count"], 1);

        let missing = tool_json(
            &server,
            "baton_orchestration_report",
            json!({ "run": "does-not-exist" }),
        );
        assert!(
            missing["error"]
                .as_str()
                .unwrap_or("")
                .contains("no orchestration run"),
            "missing report error: {missing}"
        );
    }

    #[test]
    fn fork_creates_a_branch_through_a_completed_turn() {
        let server = server();
        let source = tool_json(&server, "baton_status", json!({}));
        let source_id = source["thread_id"].as_str().unwrap().to_string();
        let forked = tool_json(
            &server,
            "baton_fork",
            json!({ "thread": source_id, "turn": 1, "title": "mcp branch" }),
        );
        assert_eq!(forked["through_turn"], 1);
        assert_eq!(forked["source_thread_id"], source_id);
        assert_ne!(forked["thread_id"], source_id);
        assert_eq!(forked["title"], "mcp branch");
        let branch = tool_json(
            &server,
            "baton_status",
            json!({ "thread": forked["thread_id"].as_str().unwrap() }),
        );
        assert_eq!(branch["turn_count"], 1);
        assert_eq!(branch["latest_turn"]["phase"], "completed");
        let messages = tool_json(
            &server,
            "baton_messages",
            json!({ "thread": forked["thread_id"].as_str().unwrap(), "limit": 20 }),
        );
        assert_eq!(messages["count"], 3, "user, assistant, and diff rows copy");
    }

    #[test]
    fn notifications_have_no_response() {
        let server = server();
        assert!(server
            .handle(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none());
    }
}
