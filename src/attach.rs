//! Read-only attach client for a paired local host.

use std::fmt;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::grants::valid_grant_id;

pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
const PAIRING_PREFIX: &str = "baton://local#";
const STATUS_REQUEST_ID: u64 = 1;
const MESSAGES_REQUEST_ID: u64 = 2;
const SUBMIT_REQUEST_ID: u64 = 3;
const APPROVALS_REQUEST_ID: u64 = 4;
const RESPOND_REQUEST_ID: u64 = 5;
const QUESTIONS_REQUEST_ID: u64 = 6;
const ANSWER_REQUEST_ID: u64 = 7;

#[derive(Clone, PartialEq, Eq)]
pub struct PairingCredential {
    pub grant_id: String,
    pub secret: String,
}

impl fmt::Debug for PairingCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingCredential")
            .field("grant_id", &self.grant_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachStatus {
    pub scope: String,
    pub threads: u64,
    pub running_turns: u64,
}

impl AttachStatus {
    pub fn from_response(value: &Value) -> Result<AttachStatus> {
        let result = value
            .get("result")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("host response has no result object"))?;
        let scope = result
            .get("scope")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("host response has no scope"))?;
        if !matches!(scope, "host" | "thread") {
            anyhow::bail!("host response has an unknown scope");
        }
        let threads = result
            .get("threads")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("host response has no thread count"))?;
        let running_turns = result
            .get("running_turns")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("host response has no running turn count"))?;
        Ok(AttachStatus {
            scope: scope.to_string(),
            threads,
            running_turns,
        })
    }

    pub fn json(&self) -> Value {
        json!({
            "schema_version": crate::status::SCHEMA_VERSION,
            "scope": self.scope,
            "threads": self.threads,
            "running_turns": self.running_turns,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachSubmit {
    pub thread_id: String,
    pub accepted: bool,
    pub replayed: bool,
}

impl AttachSubmit {
    pub fn from_response(value: &Value) -> Result<AttachSubmit> {
        let result = value
            .get("result")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("host response has no result object"))?;
        let thread_id = result
            .get("thread_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("host response has no thread id"))?
            .to_string();
        let accepted = result
            .get("accepted")
            .and_then(Value::as_bool)
            .ok_or_else(|| anyhow::anyhow!("host response has no accepted flag"))?;
        let replayed = result
            .get("replayed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(AttachSubmit {
            thread_id,
            accepted,
            replayed,
        })
    }

    pub fn json(&self) -> Value {
        json!({
            "schema_version": crate::status::SCHEMA_VERSION,
            "thread_id": self.thread_id,
            "accepted": self.accepted,
            "replayed": self.replayed,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteApprovalChoice {
    pub id: String,
    pub label: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteApproval {
    pub request_id: String,
    pub thread_id: String,
    pub kind: String,
    pub summary: String,
    pub choices: Vec<RemoteApprovalChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachApprovals {
    pub approvals: Vec<RemoteApproval>,
}

impl AttachApprovals {
    pub fn from_response(value: &Value) -> Result<AttachApprovals> {
        let result = value
            .get("result")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("host response has no result object"))?;
        let raw = result
            .get("approvals")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("host response has no approvals array"))?;
        let mut approvals = Vec::with_capacity(raw.len());
        for approval in raw {
            let object = approval
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("host response has a malformed approval"))?;
            let text = |field: &str| {
                object
                    .get(field)
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("host approval has no {field}"))
            };
            let choices = object
                .get("choices")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("host approval has no choices array"))?
                .iter()
                .map(|choice| {
                    let choice = choice
                        .as_object()
                        .ok_or_else(|| anyhow::anyhow!("host approval has a malformed choice"))?;
                    let field = |name: &str| {
                        choice
                            .get(name)
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow::anyhow!("host choice has no {name}"))
                    };
                    Ok(RemoteApprovalChoice {
                        id: field("id")?.to_string(),
                        label: field("label")?.to_string(),
                        kind: field("kind")?.to_string(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            approvals.push(RemoteApproval {
                request_id: text("request_id")?.to_string(),
                thread_id: text("thread_id")?.to_string(),
                kind: text("kind")?.to_string(),
                summary: text("summary")?.to_string(),
                choices,
            });
        }
        Ok(AttachApprovals { approvals })
    }

    pub fn json(&self) -> Value {
        json!({
            "schema_version": crate::status::SCHEMA_VERSION,
            "approvals": self.approvals.iter().map(|approval| json!({
                "request_id": approval.request_id,
                "thread_id": approval.thread_id,
                "kind": approval.kind,
                "summary": approval.summary,
                "choices": approval.choices.iter().map(|choice| json!({
                    "id": choice.id,
                    "label": choice.label,
                    "kind": choice.kind,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }

    pub fn text(&self) -> String {
        if self.approvals.is_empty() {
            return "no pending approvals\n".to_string();
        }
        let mut out = format!("{} pending approval(s)\n", self.approvals.len());
        for approval in &self.approvals {
            out.push_str(&format!(
                "  {}  {}  {}\n",
                crate::sanitize::sanitize_single_line(&approval.request_id),
                crate::sanitize::sanitize_single_line(&approval.thread_id),
                crate::sanitize::sanitize_single_line(&approval.kind),
            ));
            out.push_str(&format!(
                "    {}\n",
                crate::sanitize::sanitize_single_line(&approval.summary)
            ));
            let choices = approval
                .choices
                .iter()
                .map(|choice| {
                    format!(
                        "{} ({})",
                        crate::sanitize::sanitize_single_line(&choice.id),
                        crate::sanitize::sanitize_single_line(&choice.label)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("    choices: {choices}\n"));
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachAnswer {
    pub request_id: String,
    pub thread_id: String,
    pub choice: String,
    pub decision: String,
    pub replayed: bool,
}

impl AttachAnswer {
    pub fn from_response(value: &Value) -> Result<AttachAnswer> {
        let result = value
            .get("result")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("host response has no result object"))?;
        let field = |name: &str| {
            result
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("host response has no {name}"))
        };
        let answered = result
            .get("answered")
            .and_then(Value::as_bool)
            .ok_or_else(|| anyhow::anyhow!("host response has no answered flag"))?;
        if !answered {
            anyhow::bail!("host did not answer the approval");
        }
        Ok(AttachAnswer {
            request_id: field("request_id")?,
            thread_id: field("thread_id")?,
            choice: field("choice")?,
            decision: field("decision")?,
            replayed: result
                .get("replayed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    pub fn json(&self) -> Value {
        json!({
            "schema_version": crate::status::SCHEMA_VERSION,
            "request_id": self.request_id,
            "thread_id": self.thread_id,
            "choice": self.choice,
            "decision": self.decision,
            "answered": true,
            "replayed": self.replayed,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteQuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteQuestionPrompt {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<RemoteQuestionOption>,
    pub allow_custom: bool,
    pub multi_select: bool,
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteQuestion {
    pub request_id: String,
    pub thread_id: String,
    pub questions: Vec<RemoteQuestionPrompt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachQuestions {
    pub questions: Vec<RemoteQuestion>,
}

impl AttachQuestions {
    pub fn from_response(value: &Value) -> Result<AttachQuestions> {
        let result = value
            .get("result")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("host response has no result object"))?;
        let raw = result
            .get("questions")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("host response has no questions array"))?;
        let mut questions = Vec::with_capacity(raw.len());
        for request in raw {
            let object = request
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("host response has a malformed question"))?;
            let text = |field: &str| {
                object
                    .get(field)
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("host question has no {field}"))
            };
            let prompts = object
                .get("questions")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("host question has no prompts array"))?
                .iter()
                .map(|prompt| {
                    let prompt = prompt
                        .as_object()
                        .ok_or_else(|| anyhow::anyhow!("host question has a malformed prompt"))?;
                    let field = |name: &str| {
                        prompt
                            .get(name)
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow::anyhow!("host prompt has no {name}"))
                    };
                    let options = prompt
                        .get("options")
                        .and_then(Value::as_array)
                        .ok_or_else(|| anyhow::anyhow!("host prompt has no options array"))?
                        .iter()
                        .map(|option| {
                            let option = option.as_object().ok_or_else(|| {
                                anyhow::anyhow!("host prompt has a malformed option")
                            })?;
                            let field = |name: &str| {
                                option
                                    .get(name)
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| anyhow::anyhow!("host option has no {name}"))
                            };
                            Ok(RemoteQuestionOption {
                                label: field("label")?.to_string(),
                                description: field("description").unwrap_or_default().to_string(),
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Ok(RemoteQuestionPrompt {
                        id: field("id")?.to_string(),
                        header: field("header")?.to_string(),
                        question: field("question")?.to_string(),
                        options,
                        allow_custom: prompt
                            .get("allow_custom")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        multi_select: prompt
                            .get("multi_select")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        secret: prompt
                            .get("secret")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            questions.push(RemoteQuestion {
                request_id: text("request_id")?.to_string(),
                thread_id: text("thread_id")?.to_string(),
                questions: prompts,
            });
        }
        Ok(AttachQuestions { questions })
    }

    pub fn json(&self) -> Value {
        json!({
            "schema_version": crate::status::SCHEMA_VERSION,
            "questions": self.questions.iter().map(|request| json!({
                "request_id": request.request_id,
                "thread_id": request.thread_id,
                "questions": request.questions.iter().map(|prompt| json!({
                    "id": prompt.id,
                    "header": prompt.header,
                    "question": prompt.question,
                    "options": prompt.options.iter().map(|option| json!({
                        "label": option.label,
                        "description": option.description,
                    })).collect::<Vec<_>>(),
                    "allow_custom": prompt.allow_custom,
                    "multi_select": prompt.multi_select,
                    "secret": prompt.secret,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }

    pub fn text(&self) -> String {
        if self.questions.is_empty() {
            return "no pending questions
"
            .to_string();
        }
        let mut out = format!(
            "{} pending question(s)
",
            self.questions.len()
        );
        for request in &self.questions {
            out.push_str(&format!(
                "  {}  {}
",
                crate::sanitize::sanitize_single_line(&request.request_id),
                crate::sanitize::sanitize_single_line(&request.thread_id),
            ));
            for prompt in &request.questions {
                out.push_str(&format!(
                    "    [{}] {}: {}
",
                    crate::sanitize::sanitize_single_line(&prompt.id),
                    crate::sanitize::sanitize_single_line(&prompt.header),
                    crate::sanitize::sanitize_single_line(&prompt.question),
                ));
                if !prompt.options.is_empty() {
                    let options = prompt
                        .options
                        .iter()
                        .map(|option| {
                            let label = crate::sanitize::sanitize_single_line(&option.label);
                            let description =
                                crate::sanitize::sanitize_single_line(&option.description);
                            if description.is_empty() {
                                label
                            } else {
                                format!("{label} ({description})")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    out.push_str(&format!(
                        "      options: {options}
"
                    ));
                }
                let mut flags = Vec::new();
                if prompt.multi_select {
                    flags.push("multi-select");
                }
                if prompt.allow_custom {
                    flags.push("custom");
                }
                if prompt.secret {
                    flags.push("secret");
                }
                if !flags.is_empty() {
                    out.push_str(&format!(
                        "      flags: {}
",
                        flags.join(", ")
                    ));
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachQuestionAnswer {
    pub request_id: String,
    pub thread_id: String,
    pub prompts: u64,
    pub replayed: bool,
}

impl AttachQuestionAnswer {
    pub fn from_response(value: &Value) -> Result<AttachQuestionAnswer> {
        let result = value
            .get("result")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("host response has no result object"))?;
        let field = |name: &str| {
            result
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("host response has no {name}"))
        };
        let prompts = result
            .get("prompts")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("host response has no prompt count"))?;
        let answered = result
            .get("answered")
            .and_then(Value::as_bool)
            .ok_or_else(|| anyhow::anyhow!("host response has no answered flag"))?;
        if !answered {
            anyhow::bail!("host did not answer the question");
        }
        Ok(AttachQuestionAnswer {
            request_id: field("request_id")?,
            thread_id: field("thread_id")?,
            prompts,
            replayed: result
                .get("replayed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    pub fn json(&self) -> Value {
        json!({
            "schema_version": crate::status::SCHEMA_VERSION,
            "request_id": self.request_id,
            "thread_id": self.thread_id,
            "prompts": self.prompts,
            "answered": true,
            "replayed": self.replayed,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMessage {
    pub id: i64,
    pub turn: i64,
    pub role: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub text: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachMessages {
    pub thread_id: String,
    pub cursor: i64,
    pub running_turns: u64,
    pub messages: Vec<RemoteMessage>,
}

impl AttachMessages {
    pub fn from_response(value: &Value) -> Result<AttachMessages> {
        let result = value
            .get("result")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("host response has no result object"))?;
        let thread_id = result
            .get("thread_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("host response has no thread id"))?
            .to_string();
        let cursor = result
            .get("cursor")
            .and_then(Value::as_i64)
            .filter(|cursor| *cursor >= 0)
            .ok_or_else(|| anyhow::anyhow!("host response has no message cursor"))?;
        let running_turns = result
            .get("running_turns")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("host response has no running turn count"))?;
        let raw = result
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("host response has no messages array"))?;
        let mut messages = Vec::with_capacity(raw.len());
        for message in raw {
            let object = message
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("host response has a malformed message"))?;
            let id = object
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("host response message has no id"))?;
            messages.push(RemoteMessage {
                id,
                turn: object.get("turn").and_then(Value::as_i64).unwrap_or(0),
                role: object
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                provider: object
                    .get("provider")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                model: object
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                text: object
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                created_at: object
                    .get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        if messages.iter().any(|message| message.id > cursor) {
            anyhow::bail!("host response cursor is behind its messages");
        }
        Ok(AttachMessages {
            thread_id,
            cursor,
            running_turns,
            messages,
        })
    }

    pub fn json(&self) -> Value {
        json!({
            "schema_version": crate::status::SCHEMA_VERSION,
            "thread_id": self.thread_id,
            "cursor": self.cursor,
            "running_turns": self.running_turns,
            "messages": self.messages.iter().map(|message| json!({
                "id": message.id,
                "turn": message.turn,
                "role": message.role,
                "provider": message.provider,
                "model": message.model,
                "text": message.text,
                "created_at": message.created_at,
            })).collect::<Vec<_>>(),
        })
    }
}

pub fn parse_pairing_url(value: &str) -> Result<PairingCredential> {
    if value.chars().any(char::is_control) {
        anyhow::bail!("pairing URL contains control characters");
    }
    let fragment = value
        .strip_prefix(PAIRING_PREFIX)
        .ok_or_else(|| anyhow::anyhow!("pairing URL must start with {PAIRING_PREFIX}"))?;
    let mut grant_id = None;
    let mut secret = None;
    for field in fragment.split('&') {
        let (key, field_value) = field
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("pairing URL has a field without a value"))?;
        match key {
            "grant" if grant_id.is_none() => grant_id = Some(field_value.to_string()),
            "secret" if secret.is_none() => secret = Some(field_value.to_string()),
            "grant" | "secret" => anyhow::bail!("pairing URL repeats the {key} field"),
            _ => anyhow::bail!("pairing URL has an unknown field"),
        }
    }
    let grant_id = grant_id.ok_or_else(|| anyhow::anyhow!("pairing URL has no grant id"))?;
    let secret = secret.ok_or_else(|| anyhow::anyhow!("pairing URL has no secret"))?;
    if !valid_grant_id(&grant_id) {
        anyhow::bail!("pairing URL has an invalid grant id");
    }
    if secret.len() != 64
        || !secret
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        anyhow::bail!("pairing URL has an invalid secret");
    }
    Ok(PairingCredential { grant_id, secret })
}

#[cfg(unix)]
fn request(
    home: &Path,
    credential: &PairingCredential,
    command: &str,
    args: Value,
    request_id: u64,
    timeout: Duration,
) -> Result<Value> {
    let path = crate::serve::socket_path(home);
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "connect to {}; start the host with baton serve",
            path.display()
        )
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .context("set the attach read timeout")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("set the attach write timeout")?;
    let request = json!({
        "v": crate::serve::PROTOCOL_VERSION,
        "kind": "request",
        "id": request_id,
        "command": command,
        "args": args,
        "grant_id": credential.grant_id,
        "secret": credential.secret,
    });
    let mut request_text = serde_json::to_string(&request)?;
    request_text.push('\n');
    stream
        .write_all(request_text.as_bytes())
        .context("send the attach request")?;
    let mut response_text = String::new();
    BufReader::new(stream)
        .read_line(&mut response_text)
        .context("read the attach response")?;
    if response_text.trim().is_empty() {
        anyhow::bail!("host closed the connection without a response");
    }
    let response: Value =
        serde_json::from_str(&response_text).context("attach response is not JSON")?;
    validate_response(&response, request_id)?;
    Ok(response)
}

#[cfg(unix)]
pub fn query_status(
    home: &Path,
    credential: &PairingCredential,
    timeout: Duration,
) -> Result<AttachStatus> {
    let response = request(
        home,
        credential,
        "status",
        Value::Null,
        STATUS_REQUEST_ID,
        timeout,
    )?;
    AttachStatus::from_response(&response)
}

#[cfg(unix)]
pub fn query_thread_status(
    home: &Path,
    credential: &PairingCredential,
    thread: &str,
    timeout: Duration,
) -> Result<AttachStatus> {
    let response = request(
        home,
        credential,
        "status",
        json!({ "thread": thread }),
        STATUS_REQUEST_ID,
        timeout,
    )?;
    AttachStatus::from_response(&response)
}

#[cfg(unix)]
pub fn submit(
    home: &Path,
    credential: &PairingCredential,
    thread: &str,
    text: &str,
    idempotency_key: &str,
    timeout: Duration,
) -> Result<AttachSubmit> {
    let response = request(
        home,
        credential,
        "submit",
        json!({
            "thread": thread,
            "text": text,
            "idempotency_key": idempotency_key,
        }),
        SUBMIT_REQUEST_ID,
        timeout,
    )?;
    AttachSubmit::from_response(&response)
}

#[cfg(unix)]
pub fn query_approvals(
    home: &Path,
    credential: &PairingCredential,
    thread: Option<&str>,
    timeout: Duration,
) -> Result<AttachApprovals> {
    let args = match thread {
        Some(thread) => json!({ "thread": thread }),
        None => json!({}),
    };
    let response = request(
        home,
        credential,
        "approvals",
        args,
        APPROVALS_REQUEST_ID,
        timeout,
    )?;
    AttachApprovals::from_response(&response)
}

#[cfg(unix)]
pub fn respond(
    home: &Path,
    credential: &PairingCredential,
    request_id: &str,
    choice: &str,
    idempotency_key: &str,
    timeout: Duration,
) -> Result<AttachAnswer> {
    let response = request(
        home,
        credential,
        "respond",
        json!({
            "request": request_id,
            "choice": choice,
            "idempotency_key": idempotency_key,
        }),
        RESPOND_REQUEST_ID,
        timeout,
    )?;
    AttachAnswer::from_response(&response)
}

#[cfg(unix)]
pub fn query_questions(
    home: &Path,
    credential: &PairingCredential,
    thread: Option<&str>,
    timeout: Duration,
) -> Result<AttachQuestions> {
    let args = match thread {
        Some(thread) => json!({ "thread": thread }),
        None => json!({}),
    };
    let response = request(
        home,
        credential,
        "questions",
        args,
        QUESTIONS_REQUEST_ID,
        timeout,
    )?;
    AttachQuestions::from_response(&response)
}

#[cfg(unix)]
pub fn answer_questions(
    home: &Path,
    credential: &PairingCredential,
    request_id: &str,
    answers: &Value,
    idempotency_key: &str,
    timeout: Duration,
) -> Result<AttachQuestionAnswer> {
    let response = request(
        home,
        credential,
        "answer",
        json!({
            "request": request_id,
            "answers": answers,
            "idempotency_key": idempotency_key,
        }),
        ANSWER_REQUEST_ID,
        timeout,
    )?;
    AttachQuestionAnswer::from_response(&response)
}

#[cfg(unix)]
pub fn query_messages(
    home: &Path,
    credential: &PairingCredential,
    thread: &str,
    after: i64,
    limit: usize,
    timeout: Duration,
) -> Result<AttachMessages> {
    let response = request(
        home,
        credential,
        "messages",
        json!({ "thread": thread, "after": after, "limit": limit }),
        MESSAGES_REQUEST_ID,
        timeout,
    )?;
    AttachMessages::from_response(&response)
}

#[cfg(not(unix))]
pub fn query_status(
    _home: &Path,
    _credential: &PairingCredential,
    _timeout: Duration,
) -> Result<AttachStatus> {
    anyhow::bail!("attach is supported only on Unix")
}

#[cfg(not(unix))]
pub fn submit(
    _home: &Path,
    _credential: &PairingCredential,
    _thread: &str,
    _text: &str,
    _idempotency_key: &str,
    _timeout: Duration,
) -> Result<AttachSubmit> {
    anyhow::bail!("attach is supported only on Unix")
}

#[cfg(not(unix))]
pub fn query_approvals(
    _home: &Path,
    _credential: &PairingCredential,
    _thread: Option<&str>,
    _timeout: Duration,
) -> Result<AttachApprovals> {
    anyhow::bail!("attach is supported only on Unix")
}

#[cfg(not(unix))]
pub fn respond(
    _home: &Path,
    _credential: &PairingCredential,
    _request_id: &str,
    _choice: &str,
    _idempotency_key: &str,
    _timeout: Duration,
) -> Result<AttachAnswer> {
    anyhow::bail!("attach is supported only on Unix")
}

#[cfg(not(unix))]
pub fn query_questions(
    _home: &Path,
    _credential: &PairingCredential,
    _thread: Option<&str>,
    _timeout: Duration,
) -> Result<AttachQuestions> {
    anyhow::bail!("attach is supported only on Unix")
}

#[cfg(not(unix))]
pub fn answer_questions(
    _home: &Path,
    _credential: &PairingCredential,
    _request_id: &str,
    _answers: &Value,
    _idempotency_key: &str,
    _timeout: Duration,
) -> Result<AttachQuestionAnswer> {
    anyhow::bail!("attach is supported only on Unix")
}

#[cfg(not(unix))]
pub fn query_thread_status(
    _home: &Path,
    _credential: &PairingCredential,
    _thread: &str,
    _timeout: Duration,
) -> Result<AttachStatus> {
    anyhow::bail!("attach is supported only on Unix")
}

#[cfg(not(unix))]
pub fn query_messages(
    _home: &Path,
    _credential: &PairingCredential,
    _thread: &str,
    _after: i64,
    _limit: usize,
    _timeout: Duration,
) -> Result<AttachMessages> {
    anyhow::bail!("attach is supported only on Unix")
}

fn validate_response(value: &Value, expected_id: u64) -> Result<()> {
    if value["v"].as_u64() != Some(crate::serve::PROTOCOL_VERSION) {
        anyhow::bail!("host response has an unsupported protocol version");
    }
    if value["kind"].as_str() != Some("response") {
        anyhow::bail!("host response has the wrong message kind");
    }
    if value["id"].as_u64() != Some(expected_id) {
        anyhow::bail!("host response does not match the attach request");
    }
    match value["ok"].as_bool() {
        Some(true) => Ok(()),
        Some(false) => {
            let code = value["error"]["code"].as_str().unwrap_or("host_error");
            let message = value["error"]["message"]
                .as_str()
                .unwrap_or("host refused the attach request");
            anyhow::bail!(
                "host refused attach ({}): {}",
                crate::sanitize::sanitize_terminal_text(code),
                crate::sanitize::sanitize_terminal_text(message)
            )
        }
        None => anyhow::bail!("host response has no ok field"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(secret: &str) -> String {
        format!("baton://local#grant=00000000-0000-4000-8000-000000000000&secret={secret}")
    }

    #[test]
    fn approvals_response_is_typed_and_text_is_sanitized() {
        let response = json!({
            "v": crate::serve::PROTOCOL_VERSION,
            "kind": "response",
            "id": APPROVALS_REQUEST_ID,
            "ok": true,
            "result": {
                "approvals": [{
                    "request_id": "req-1",
                    "thread_id": "thread",
                    "kind": "command",
                    "summary": "run\u{1b}[31m tests\nnow",
                    "choices": [
                        {"id":"accept","label":"Allow once","kind":"allow_once"},
                        {"id":"deny","label":"Deny","kind":"reject_once"}
                    ]
                }]
            }
        });
        let approvals = AttachApprovals::from_response(&response).unwrap();
        assert_eq!(approvals.approvals.len(), 1);
        assert_eq!(approvals.approvals[0].request_id, "req-1");
        assert_eq!(approvals.approvals[0].choices[0].id, "accept");
        let text = approvals.text();
        assert!(text.contains("req-1"), "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");
        assert!(!text.contains("tests\nnow"), "{text}");
        assert_eq!(approvals.json()["approvals"][0]["kind"], "command");
    }

    #[test]
    fn answer_response_is_typed_and_requires_answered() {
        let response = json!({
            "v": crate::serve::PROTOCOL_VERSION,
            "kind": "response",
            "id": RESPOND_REQUEST_ID,
            "ok": true,
            "result": {
                "request_id": "req-1",
                "thread_id": "thread",
                "choice": "accept",
                "decision": "accept",
                "answered": true,
                "replayed": false
            }
        });
        let answer = AttachAnswer::from_response(&response).unwrap();
        assert_eq!(answer.request_id, "req-1");
        assert_eq!(answer.decision, "accept");
        assert!(!answer.replayed);
        let mut refused = response.clone();
        refused["result"]["answered"] = json!(false);
        assert!(AttachAnswer::from_response(&refused).is_err());
    }

    #[test]
    fn questions_response_is_typed_and_text_is_sanitized() {
        let response = json!({
            "v": crate::serve::PROTOCOL_VERSION,
            "kind": "response",
            "id": QUESTIONS_REQUEST_ID,
            "ok": true,
            "result": {
                "questions": [{
                    "request_id": "question-1",
                    "thread_id": "thread",
                    "questions": [{
                        "id": "features",
                        "header": "Features",
                        "question": "Which features?\u{1b}[31m",
                        "options": [
                            {"label":"CI","description":"Keep it fast"},
                            {"label":"Installer","description":"Ship it"}
                        ],
                        "allow_custom": true,
                        "multi_select": true,
                        "secret": false
                    }]
                }]
            }
        });
        let questions = AttachQuestions::from_response(&response).unwrap();
        assert_eq!(questions.questions[0].questions[0].id, "features");
        assert_eq!(questions.questions[0].questions[0].options[0].label, "CI");
        let text = questions.text();
        assert!(text.contains("features"), "{text}");
        assert!(text.contains("multi-select"), "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");
        assert_eq!(questions.json()["questions"][0]["request_id"], "question-1");
    }

    #[test]
    fn question_answer_response_is_typed_and_requires_answered() {
        let response = json!({
            "v": crate::serve::PROTOCOL_VERSION,
            "kind": "response",
            "id": ANSWER_REQUEST_ID,
            "ok": true,
            "result": {
                "request_id": "question-1",
                "thread_id": "thread",
                "prompts": 2,
                "answered": true,
                "replayed": false
            }
        });
        let answer = AttachQuestionAnswer::from_response(&response).unwrap();
        assert_eq!(answer.request_id, "question-1");
        assert_eq!(answer.prompts, 2);
        assert!(!answer.replayed);
        let mut refused = response.clone();
        refused["result"]["answered"] = json!(false);
        assert!(AttachQuestionAnswer::from_response(&refused).is_err());
    }

    #[test]
    fn pairing_url_parses_and_redacts_the_secret() {
        let secret = "a".repeat(64);
        let credential = parse_pairing_url(&url(&secret)).unwrap();
        assert_eq!(credential.grant_id, "00000000-0000-4000-8000-000000000000");
        assert_eq!(credential.secret, secret);
        let debug = format!("{credential:?}");
        assert!(!debug.contains(&secret), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }

    #[test]
    fn pairing_url_rejects_an_ambiguous_or_invalid_credential() {
        let secret = "a".repeat(64);
        let short = "a".repeat(63);
        let uppercase = "A".repeat(64);
        let cases = [
            ("baton://remote#grant=00000000-0000-4000-8000-000000000000&secret=".to_string()
                + &secret),
            ("baton://local#grant=not-a-uuid&secret=".to_string() + &secret),
            ("baton://local#secret=".to_string() + &secret),
            ("baton://local#grant=00000000-0000-4000-8000-000000000000".to_string()),
            ("baton://local#grant=00000000-0000-4000-8000-000000000000&secret=".to_string()
                + &short),
            ("baton://local#grant=00000000-0000-4000-8000-000000000000&secret=".to_string()
                + &uppercase),
            ("baton://local#grant=00000000-0000-4000-8000-000000000000&secret=".to_string()
                + &secret
                + "&secret="
                + &secret),
            ("baton://local#grant=00000000-0000-4000-8000-000000000000&secret=".to_string()
                + &secret
                + "&extra=1"),
            ("baton://local#grant=00000000-0000-4000-8000-000000000000&secret=".to_string()
                + &secret
                + "\n"),
        ];
        for case in cases {
            assert!(parse_pairing_url(&case).is_err(), "{case}");
        }
    }

    #[test]
    fn status_response_is_typed() {
        let response = json!({
            "v": crate::serve::PROTOCOL_VERSION,
            "kind": "response",
            "id": STATUS_REQUEST_ID,
            "ok": true,
            "result": {"scope": "host", "threads": 2, "running_turns": 0}
        });
        validate_response(&response, STATUS_REQUEST_ID).unwrap();
        let status = AttachStatus::from_response(&response).unwrap();
        assert_eq!(
            status,
            AttachStatus {
                scope: "host".into(),
                threads: 2,
                running_turns: 0,
            }
        );
        assert_eq!(status.json()["schema_version"], 1);
        let unknown = json!({
            "v": crate::serve::PROTOCOL_VERSION,
            "kind": "response",
            "id": STATUS_REQUEST_ID,
            "ok": true,
            "result": {"scope": "elsewhere", "threads": 2, "running_turns": 0}
        });
        assert!(AttachStatus::from_response(&unknown).is_err());
    }

    #[test]
    fn submit_response_is_typed() {
        let response = json!({
            "v": crate::serve::PROTOCOL_VERSION,
            "kind": "response",
            "id": SUBMIT_REQUEST_ID,
            "ok": true,
            "result": {"thread_id": "thread", "accepted": true}
        });
        validate_response(&response, SUBMIT_REQUEST_ID).unwrap();
        let accepted = AttachSubmit::from_response(&response).unwrap();
        assert_eq!(accepted.thread_id, "thread");
        assert!(accepted.accepted);
        assert!(!accepted.replayed);
        assert_eq!(accepted.json()["accepted"], true);
        let missing = json!({
            "v": crate::serve::PROTOCOL_VERSION,
            "kind": "response",
            "id": SUBMIT_REQUEST_ID,
            "ok": true,
            "result": {"thread_id": "thread"}
        });
        assert!(AttachSubmit::from_response(&missing).is_err());
    }

    #[test]
    fn message_response_is_typed_and_rejects_a_cursor_behind_its_rows() {
        let response = json!({
            "v": crate::serve::PROTOCOL_VERSION,
            "kind": "response",
            "id": MESSAGES_REQUEST_ID,
            "ok": true,
            "result": {
                "thread_id": "thread",
                "cursor": 7,
                "running_turns": 1,
                "messages": [
                    {"id": 6, "turn": 1, "role": "user", "provider": "codex", "model": null, "text": "hello", "created_at": "now"},
                    {"id": 7, "turn": 1, "role": "assistant", "provider": "codex", "model": "m", "text": "hi", "created_at": "now"}
                ]
            }
        });
        validate_response(&response, MESSAGES_REQUEST_ID).unwrap();
        let page = AttachMessages::from_response(&response).unwrap();
        assert_eq!(page.thread_id, "thread");
        assert_eq!(page.cursor, 7);
        assert_eq!(page.running_turns, 1);
        assert_eq!(page.messages.len(), 2);
        assert_eq!(page.messages[1].text, "hi");
        assert_eq!(page.json()["cursor"], 7);

        let mut behind = response.clone();
        behind["result"]["cursor"] = json!(5);
        assert!(AttachMessages::from_response(&behind).is_err());
    }
}
