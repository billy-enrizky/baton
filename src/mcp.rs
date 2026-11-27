//! Minimal MCP server exposing baton's durable thread state to coding agents.
//! The default is read-only except for explicit queue clear and fork; `--read-only`
//! hides fork and refuses queue clear.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::config::resolve_config;
use crate::orchestrate;
use crate::process;
use crate::status::{self, ThreadStatus, TurnStatus};
use crate::store::{BatonStore, MessageRow, NewAuditEvent, ThreadRow, TurnRow};
use crate::usage::{format_usage_line, sum_usage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpAccess {
    ReadWrite,
    ReadOnly,
}

pub fn known_tool_names() -> Vec<String> {
    tool_descriptors(McpAccess::ReadWrite)
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect()
}

pub struct McpServer {
    store: BatonStore,
    home: PathBuf,
    cwd: PathBuf,
    access: McpAccess,
    allowed_tools: Option<BTreeSet<String>>,
}

impl McpServer {
    pub fn new(store: BatonStore, home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            store,
            home,
            cwd,
            access: McpAccess::ReadWrite,
            allowed_tools: None,
        }
    }

    pub fn read_only(mut self) -> Self {
        self.access = McpAccess::ReadOnly;
        self
    }

    pub fn with_allowed_tools(mut self, tools: impl IntoIterator<Item = String>) -> Self {
        self.allowed_tools = Some(tools.into_iter().collect());
        self
    }

    fn mutation_audit(
        &self,
        thread_id: &str,
        action: crate::audit::AuditAction,
        payload: &Value,
    ) -> Result<NewAuditEvent> {
        let environment = crate::claim::ensure_installation_id(&self.home)?;
        crate::audit::event(
            crate::audit::MCP_STDIO_ACTOR,
            &environment,
            Some(thread_id),
            Some(thread_id),
            action,
            payload,
        )
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
            "initialize" => {
                let mut instructions = if self.access == McpAccess::ReadOnly {
                    "Use baton_doctor, baton_status, baton_threads, baton_turns, baton_queue, baton_messages, baton_search, baton_processes, baton_latest_diff, baton_agents, baton_orchestration_runs, and baton_orchestration_report to inspect the local baton store, provider readiness, and durable orchestration runs. This server is read-only; baton_fork and queue clearing are disabled.".to_string()
                } else {
                    "Use baton_doctor, baton_status, baton_threads, baton_turns, baton_queue, baton_messages, baton_search, baton_processes, baton_latest_diff, baton_agents, baton_orchestration_runs, and baton_orchestration_report to inspect the local baton store, provider readiness, and durable orchestration runs. baton_fork creates a new thread from an existing conversation; set native=true for a Codex or OpenCode native fork.".to_string()
                };
                if let Some(allowed) = &self.allowed_tools {
                    instructions.push_str(&format!(
                        " This server allows only these tools: {}.",
                        allowed.iter().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
                Ok(json!({
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
                    "instructions": instructions
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.visible_tools() })),
            "tools/call" => {
                if let Some(reason) = self.refusal_reason(&params) {
                    return Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32003, "message": reason }
                    }));
                }
                self.call_tool(&params)
            }
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

    fn refusal_reason(&self, params: &Value) -> Option<String> {
        let name = params.get("name").and_then(Value::as_str)?;
        if let Some(allowed) = &self.allowed_tools {
            if !allowed.contains(name) {
                return Some(format!("MCP server does not allow {name}"));
            }
        }
        if self.access != McpAccess::ReadOnly {
            return None;
        }
        if name == "baton_fork" {
            return Some("read-only MCP server refuses baton_fork".to_string());
        }
        if name == "baton_queue"
            && params
                .get("arguments")
                .and_then(|arguments| arguments.get("clear"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return Some("read-only MCP server refuses baton_queue".to_string());
        }
        None
    }

    fn visible_tools(&self) -> Vec<Value> {
        let mut tools = tool_descriptors(self.access);
        if let Some(allowed) = &self.allowed_tools {
            tools.retain(|tool| {
                tool.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| allowed.contains(name))
            });
        }
        tools
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
                let all = arguments
                    .get("all")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
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
                let all = arguments
                    .get("all")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
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
                if arguments
                    .get("clear")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    let audit = self.mutation_audit(
                        &thread.id,
                        crate::audit::AuditAction::QueueClear,
                        &json!({ "thread_id": thread.id, "clear": true }),
                    )?;
                    self.store
                        .clear_queued_messages_with_audit(&thread.id, &audit)?;
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
                let all = arguments
                    .get("all")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
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
                            "owner_start": row.owner_start,
                            "host_id": row.host_id,
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
                let audit = self.mutation_audit(
                    &thread.id,
                    crate::audit::AuditAction::ThreadFork,
                    &json!({
                        "source_thread_id": thread.id,
                        "title": title,
                        "through_turn": turn,
                        "native": native,
                    }),
                )?;
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
                let (forked, _) = self
                    .store
                    .fork_thread_through_turn_with_audit(&thread.id, &title, turn, &audit)?;
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

fn tool_descriptors(access: McpAccess) -> Vec<Value> {
    let mut tools = vec![
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
    ];
    if access == McpAccess::ReadOnly {
        tools.retain(|tool| tool["name"] != "baton_fork");
        if let Some(queue) = tools.iter_mut().find(|tool| tool["name"] == "baton_queue") {
            queue["description"] = json!(
                "List a thread's durable queued follow-ups. This server is read-only, so clear is refused."
            );
            queue["inputSchema"]["properties"]["clear"] = json!({
                "type": "boolean",
                "description": "Refused because this server is read-only."
            });
        }
    }
    tools
}

const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;

const MAX_SECRET_FILE_BYTES: u64 = 1024 * 1024;

fn read_regular_file(path: &Path, label: &str, private: bool) -> Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("read {label} file {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("{label} file cannot be a symlink");
    }
    if !metadata.is_file() {
        anyhow::bail!("{label} file must be a regular file");
    }
    if metadata.len() > MAX_SECRET_FILE_BYTES {
        anyhow::bail!("{label} file is larger than 1 MiB");
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            anyhow::bail!("{label} file must be mode 0600");
        }
    }
    std::fs::read(path).with_context(|| format!("read {label} file {}", path.display()))
}

/// Load the bearer token from a private regular file. The token is a static
/// credential; revocation is replacing the file and restarting the server.
pub fn read_token_file(path: &Path) -> Result<String> {
    let text = String::from_utf8(read_regular_file(path, "token", true)?)
        .with_context(|| format!("token file {} is not UTF-8", path.display()))?;
    let token = text.trim();
    if !(32..=512).contains(&token.len()) {
        anyhow::bail!("token must be 32 to 512 characters");
    }
    if token
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        anyhow::bail!("token cannot contain whitespace or control characters");
    }
    Ok(token.to_string())
}

/// Load a PEM certificate chain and a private PKCS#8 key for HTTPS.
pub fn read_tls_identity(cert_path: &Path, key_path: &Path) -> Result<native_tls::Identity> {
    let cert = read_regular_file(cert_path, "TLS certificate", false)?;
    let key = read_regular_file(key_path, "TLS key", true)?;
    native_tls::Identity::from_pkcs8(&cert, &key).context("load the TLS certificate and key")
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn check_http_bind(addr: std::net::IpAddr, trusted_overlay: bool, encrypted: bool) -> Result<()> {
    if !addr.is_loopback() && !trusted_overlay && !encrypted {
        anyhow::bail!("--http outside loopback needs --tls-cert or --trusted-overlay");
    }
    Ok(())
}

pub fn run_http(
    server: McpServer,
    addr: &str,
    token_file: &Path,
    trusted_overlay: bool,
    tls: Option<(&Path, &Path)>,
) -> Result<()> {
    let token = read_token_file(token_file)?;
    let tls_acceptor = tls
        .map(|(cert_path, key_path)| {
            let identity = read_tls_identity(cert_path, key_path)?;
            native_tls::TlsAcceptor::new(identity)
                .context("build the TLS acceptor")
                .map(tokio_native_tls::TlsAcceptor::from)
        })
        .transpose()?;
    let encrypted = tls_acceptor.is_some();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build the MCP HTTP runtime")?;
    runtime.block_on(async move {
        let mut addresses = tokio::net::lookup_host(addr)
            .await
            .with_context(|| format!("--http needs a resolvable host:port, got {addr}"))?;
        let addr = addresses
            .next()
            .ok_or_else(|| anyhow::anyhow!("--http resolved to no address"))?;
        check_http_bind(addr.ip(), trusted_overlay, encrypted)?;
        if !addr.ip().is_loopback() && !encrypted {
            eprintln!(
                "baton: MCP HTTP on {addr} uses a static bearer token without TLS; use it only on a trusted overlay"
            );
        }
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("bind MCP HTTP on {addr}"))?;
        let local = listener.local_addr()?;
        let scheme = if encrypted { "https" } else { "http" };
        eprintln!("baton: MCP HTTP listening on {scheme}://{local}");
        let server = Arc::new(Mutex::new(server));
        let token = Arc::new(token);
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                accepted = listener.accept() => {
                    let (stream, _) = accepted.context("accept MCP HTTP connection")?;
                    let server = server.clone();
                    let token = token.clone();
                    let tls_acceptor = tls_acceptor.clone();
                    tokio::spawn(async move {
                        let result = match tls_acceptor {
                            Some(acceptor) => match acceptor.accept(stream).await {
                                Ok(stream) => serve_http_connection(stream, server, token).await,
                                Err(error) => Err(anyhow::Error::new(error)
                                    .context("MCP TLS handshake")),
                            },
                            None => serve_http_connection(stream, server, token).await,
                        };
                        if let Err(error) = result {
                            eprintln!("baton: MCP HTTP connection: {error}");
                        }
                    });
                }
            }
        }
        Ok(())
    })
}

async fn serve_http_connection<S>(
    stream: S,
    server: Arc<Mutex<McpServer>>,
    token: Arc<String>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let mut request_line = String::new();
    let read = reader
        .read_line(&mut request_line)
        .await
        .context("read MCP HTTP request line")?;
    if read == 0 {
        return Ok(());
    }
    if request_line.len() > 8192 {
        return write_http_response(
            &mut write,
            "431 Request Header Fields Too Large",
            "application/json",
            json!({"error": "request line too large"})
                .to_string()
                .as_bytes(),
        )
        .await;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    if parts.next().is_none() {
        return write_http_response(
            &mut write,
            "400 Bad Request",
            "application/json",
            json!({"error": "malformed request line"})
                .to_string()
                .as_bytes(),
        )
        .await;
    }

    let mut content_length = None;
    let mut authorization = None;
    let mut expect_continue = false;
    let mut header_bytes = 0usize;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .await
            .context("read MCP HTTP header")?;
        if read == 0 {
            return write_http_response(
                &mut write,
                "400 Bad Request",
                "application/json",
                json!({"error": "request ended in headers"})
                    .to_string()
                    .as_bytes(),
            )
            .await;
        }
        header_bytes += read;
        if header_bytes > MAX_HTTP_HEADER_BYTES {
            return write_http_response(
                &mut write,
                "431 Request Header Fields Too Large",
                "application/json",
                json!({"error": "headers too large"}).to_string().as_bytes(),
            )
            .await;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return write_http_response(
                &mut write,
                "400 Bad Request",
                "application/json",
                json!({"error": "malformed header"}).to_string().as_bytes(),
            )
            .await;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => {
                let Ok(length) = value.trim().parse::<usize>() else {
                    return write_http_response(
                        &mut write,
                        "400 Bad Request",
                        "application/json",
                        json!({"error": "content-length must be a number"})
                            .to_string()
                            .as_bytes(),
                    )
                    .await;
                };
                content_length = Some(length);
            }
            "authorization" => authorization = Some(value.trim().to_string()),
            "expect" => expect_continue = value.trim().eq_ignore_ascii_case("100-continue"),
            _ => {}
        }
    }

    if method == "GET" && path == "/health" {
        return write_http_response(
            &mut write,
            "200 OK",
            "application/json",
            json!({"ok": true, "server": "baton", "version": env!("CARGO_PKG_VERSION")})
                .to_string()
                .as_bytes(),
        )
        .await;
    }
    if path != "/mcp" {
        return write_http_response(
            &mut write,
            "404 Not Found",
            "application/json",
            json!({"error": "not found"}).to_string().as_bytes(),
        )
        .await;
    }
    if method != "POST" {
        return write_http_response(
            &mut write,
            "405 Method Not Allowed",
            "application/json",
            json!({"error": "use POST /mcp"}).to_string().as_bytes(),
        )
        .await;
    }

    let provided = authorization
        .as_deref()
        .and_then(|header| header.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, value)| value.trim())
        .unwrap_or_default();
    if !constant_time_eq(provided.as_bytes(), token.as_bytes()) {
        let body = json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32001, "message": "unauthorized" }
        })
        .to_string();
        let header = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nWWW-Authenticate: Bearer\r\nConnection: close\r\n\r\n",
            body.len()
        );
        write.write_all(header.as_bytes()).await?;
        write.write_all(body.as_bytes()).await?;
        write.flush().await?;
        return Ok(());
    }

    let Some(content_length) = content_length else {
        return write_http_response(
            &mut write,
            "411 Length Required",
            "application/json",
            json!({"error": "content-length is required"})
                .to_string()
                .as_bytes(),
        )
        .await;
    };
    if content_length > MAX_HTTP_BODY_BYTES {
        return write_http_response(
            &mut write,
            "413 Payload Too Large",
            "application/json",
            json!({"error": "body too large"}).to_string().as_bytes(),
        )
        .await;
    }
    if expect_continue {
        write.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
        write.flush().await?;
    }
    let mut body = vec![0u8; content_length];
    reader
        .read_exact(&mut body)
        .await
        .context("read MCP HTTP body")?;
    let frame: Value = match serde_json::from_slice(&body) {
        Ok(frame) => frame,
        Err(error) => {
            return write_http_response(
                &mut write,
                "400 Bad Request",
                "application/json",
                json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {error}") }
                })
                .to_string()
                .as_bytes(),
            )
            .await
        }
    };
    let response = server
        .lock()
        .map_err(|_| anyhow::anyhow!("MCP server lock poisoned"))?
        .handle(frame);
    match response {
        Some(response) => {
            write_http_response(
                &mut write,
                "200 OK",
                "application/json",
                serde_json::to_string(&response)?.as_bytes(),
            )
            .await
        }
        None => write_http_response(&mut write, "202 Accepted", "application/json", b"").await,
    }
}

async fn write_http_response<W>(
    write: &mut W,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    write.write_all(header.as_bytes()).await?;
    write.write_all(body).await?;
    write.flush().await?;
    Ok(())
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
            .add_message(
                &thread.id,
                1,
                "user",
                Some("codex"),
                Some("model-1"),
                "hello",
            )
            .unwrap();
        store
            .add_message(
                &thread.id,
                1,
                "assistant",
                Some("codex"),
                Some("model-1"),
                "world",
            )
            .unwrap();
        store
            .start_turn_record(&thread.id, 1, ProviderId::Codex, Some("model-1"))
            .unwrap();
        store
            .finish_turn_record(&thread.id, 1, crate::types::TurnPhase::Completed, None)
            .unwrap();
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

    #[cfg(unix)]
    #[test]
    fn token_file_requires_a_private_regular_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        let token = "a".repeat(64);
        std::fs::write(&path, format!("{token}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token_file(&path).unwrap(), token);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = read_token_file(&path).unwrap_err();
        assert!(error.to_string().contains("0600"), "{error}");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("token-link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let error = read_token_file(&link).unwrap_err();
        assert!(error.to_string().contains("symlink"), "{error}");
    }

    #[cfg(unix)]
    fn tls_fixture(dir: &tempfile::TempDir) -> (PathBuf, PathBuf, String) {
        use std::os::unix::fs::PermissionsExt;

        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        let status = std::process::Command::new("openssl")
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout"])
            .arg(&key_path)
            .arg("-out")
            .arg(&cert_path)
            .args([
                "-days",
                "1",
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost,IP:127.0.0.1",
                "-addext",
                "extendedKeyUsage=serverAuth",
                "-addext",
                "keyUsage=digitalSignature,keyEncipherment",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("openssl is required for the TLS tests");
        assert!(status.success(), "openssl could not create the TLS fixture");
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let cert_pem = std::fs::read_to_string(&cert_path).unwrap();
        (cert_path, key_path, cert_pem)
    }

    #[cfg(unix)]
    #[test]
    fn tls_identity_requires_a_private_key() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let (cert_path, key_path, _) = tls_fixture(&dir);
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = match read_tls_identity(&cert_path, &key_path) {
            Ok(_) => panic!("a public TLS key must be refused"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("TLS key"), "{error}");
        assert!(error.to_string().contains("0600"), "{error}");

        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        if let Err(error) = read_tls_identity(&cert_path, &key_path) {
            panic!("valid TLS identity was refused: {error:#}");
        }

        let link = dir.path().join("key-link");
        std::os::unix::fs::symlink(&key_path, &link).unwrap();
        let error = match read_tls_identity(&cert_path, &link) {
            Ok(_) => panic!("a symlinked TLS key must be refused"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("symlink"), "{error}");
    }

    #[test]
    fn tls_identity_rejects_invalid_pem() {
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, "not a certificate").unwrap();
        std::fs::write(&key_path, "not a key").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let error = match read_tls_identity(&cert_path, &key_path) {
            Ok(_) => panic!("invalid PEM must be refused"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("TLS"), "{error}");
    }

    #[test]
    fn tls_bind_policy_requires_encryption_or_an_overlay() {
        use std::net::{IpAddr, Ipv4Addr};

        assert!(check_http_bind(IpAddr::V4(Ipv4Addr::LOCALHOST), false, false).is_ok());
        assert!(
            check_http_bind(IpAddr::V4(Ipv4Addr::UNSPECIFIED), false, false).is_err(),
            "a plain non-loopback bind needs an overlay"
        );
        assert!(check_http_bind(IpAddr::V4(Ipv4Addr::UNSPECIFIED), true, false).is_ok());
        assert!(check_http_bind(IpAddr::V4(Ipv4Addr::UNSPECIFIED), false, true).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tls_serves_initialize_after_a_handshake() {
        let dir = tempfile::tempdir().unwrap();
        let (cert_path, key_path, cert_pem) = tls_fixture(&dir);
        let identity = read_tls_identity(&cert_path, &key_path).unwrap();
        let acceptor =
            tokio_native_tls::TlsAcceptor::from(native_tls::TlsAcceptor::new(identity).unwrap());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let server = Arc::new(Mutex::new(server()));
        let token = "a".repeat(64);
        let auth = format!("Bearer {token}");
        let expected_token = token.clone();
        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let stream = acceptor.accept(stream).await.unwrap();
            serve_http_connection(stream, server, Arc::new(expected_token))
                .await
                .unwrap();
        });

        let root = native_tls::Certificate::from_pem(cert_pem.as_bytes()).unwrap();
        let connector = native_tls::TlsConnector::builder()
            .add_root_certificate(root)
            .build()
            .unwrap();
        let connector = tokio_native_tls::TlsConnector::from(connector);
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut stream = connector.connect("localhost", stream).await.unwrap();
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" }
        })
        .to_string();
        stream
            .write_all(http_post(Some(&auth), &initialize).as_bytes())
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("\"serverInfo\""), "{response}");
        server_task.await.unwrap();
    }

    async fn http_request(server: Arc<Mutex<McpServer>>, token: &str, request: &str) -> String {
        let (mut client, server_side) = tokio::io::duplex(16 * 1024);
        let token = Arc::new(token.to_string());
        let handle = tokio::spawn(serve_http_connection(server_side, server, token));
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        handle.await.unwrap().unwrap();
        String::from_utf8_lossy(&response).to_string()
    }

    fn http_post(auth: Option<&str>, body: &str) -> String {
        let auth = auth
            .map(|value| format!("Authorization: {value}\r\n"))
            .unwrap_or_default();
        format!(
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn http_requires_a_bearer_and_answers_initialize() {
        let server = Arc::new(Mutex::new(server()));
        let token = "a".repeat(64);
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}
        })
        .to_string();

        let unauthorized = http_request(
            server.clone(),
            &token,
            &http_post(Some("Bearer wrong"), &initialize),
        )
        .await;
        assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");

        let accepted = http_request(
            server.clone(),
            &token,
            &http_post(Some(&format!("Bearer {token}")), &initialize),
        )
        .await;
        assert!(accepted.starts_with("HTTP/1.1 200"), "{accepted}");
        assert!(accepted.contains("\"serverInfo\""), "{accepted}");
        assert!(accepted.contains("2025-06-18"), "{accepted}");

        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        })
        .to_string();
        let accepted = http_request(
            server.clone(),
            &token,
            &http_post(Some(&format!("Bearer {token}")), &notification),
        )
        .await;
        assert!(accepted.starts_with("HTTP/1.1 202"), "{accepted}");

        let health = http_request(
            server.clone(),
            &token,
            "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");
        assert!(health.contains("\"ok\":true"), "{health}");

        let malformed = http_request(
            server,
            &token,
            &http_post(Some(&format!("Bearer {token}")), "{"),
        )
        .await;
        assert!(malformed.starts_with("HTTP/1.1 400"), "{malformed}");
        assert!(malformed.contains("-32700"), "{malformed}");
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
        let status = tool_json(&server, "baton_status", json!({}));
        let thread_id = status["thread_id"].as_str().unwrap().to_string();
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
        let audit = crate::audit::list(&server.store, 10).unwrap();
        assert_eq!(audit[0].action, "queue.clear");
        assert_eq!(audit[0].actor, "mcp_stdio");
        assert_eq!(audit[0].confirmation, "stdio_spawner");
        assert_eq!(audit[0].thread_id.as_deref(), Some(thread_id.as_str()));
    }

    #[test]
    fn read_only_scope_hides_fork_and_refuses_mutations() {
        let server = server().read_only();
        let tools = request(&server, 1, "tools/list", json!({}));
        let names = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(!names.contains(&"baton_fork"));
        assert!(names.contains(&"baton_queue"));

        let init = request(&server, 2, "initialize", json!({}));
        assert!(init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("read-only"));

        let status = tool_json(&server, "baton_status", json!({}));
        let thread_id = status["thread_id"].as_str().unwrap().to_string();
        assert_eq!(
            server.store.list_queued_messages(&thread_id).unwrap().len(),
            1
        );
        let refused = request(
            &server,
            3,
            "tools/call",
            json!({ "name": "baton_queue", "arguments": { "clear": true } }),
        );
        assert_eq!(refused["error"]["code"], -32003);
        assert_eq!(
            server.store.list_queued_messages(&thread_id).unwrap().len(),
            1
        );

        let fork = request(
            &server,
            4,
            "tools/call",
            json!({ "name": "baton_fork", "arguments": {} }),
        );
        assert_eq!(fork["error"]["code"], -32003);
        assert_eq!(server.store.list_threads(None).unwrap().len(), 1);
        assert!(crate::audit::list(&server.store, 10).unwrap().is_empty());

        let queue = tool_json(&server, "baton_queue", json!({}));
        assert_eq!(queue["count"], 1);
    }

    #[test]
    fn allowlist_limits_the_callable_tools() {
        let server = server()
            .with_allowed_tools(vec!["baton_status".to_string(), "baton_queue".to_string()]);
        let tools = request(&server, 1, "tools/list", json!({}));
        let names = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["baton_status", "baton_queue"]);

        let init = request(&server, 2, "initialize", json!({}));
        let instructions = init["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("baton_status"));
        assert!(instructions.contains("baton_queue"));

        let status = tool_json(&server, "baton_status", json!({}));
        assert_eq!(status["provider"], "codex");
        let refused = request(
            &server,
            3,
            "tools/call",
            json!({ "name": "baton_turns", "arguments": {} }),
        );
        assert_eq!(refused["error"]["code"], -32003);
        let fork = request(
            &server,
            4,
            "tools/call",
            json!({ "name": "baton_fork", "arguments": {} }),
        );
        assert_eq!(fork["error"]["code"], -32003);
        assert_eq!(server.store.list_threads(None).unwrap().len(), 1);
        assert!(crate::audit::list(&server.store, 10).unwrap().is_empty());

        let queue = tool_json(&server, "baton_queue", json!({}));
        assert_eq!(queue["count"], 1);
    }

    #[test]
    fn known_tool_names_match_the_tool_list() {
        let names = known_tool_names();
        let descriptors = tool_descriptors(McpAccess::ReadWrite)
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_string))
            .collect::<Vec<_>>();
        assert_eq!(names, descriptors);
        assert_eq!(names.len(), 15);
        assert!(names.contains(&"baton_fork".to_string()));
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
                None,
                None,
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
        let audit = crate::audit::list(&server.store, 10).unwrap();
        assert_eq!(audit[0].action, "thread.fork");
        assert_eq!(audit[0].actor, "mcp_stdio");
        assert_eq!(audit[0].confirmation, "stdio_spawner");
        assert_eq!(audit[0].thread_id.as_deref(), forked["thread_id"].as_str());
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
