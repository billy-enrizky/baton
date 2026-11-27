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
const REQUEST_ID: u64 = 1;

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
pub fn query_status(
    home: &Path,
    credential: &PairingCredential,
    timeout: Duration,
) -> Result<AttachStatus> {
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
        "id": REQUEST_ID,
        "command": "status",
        "args": null,
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
    validate_response(&response)?;
    AttachStatus::from_response(&response)
}

#[cfg(not(unix))]
pub fn query_status(
    _home: &Path,
    _credential: &PairingCredential,
    _timeout: Duration,
) -> Result<AttachStatus> {
    anyhow::bail!("attach is supported only on Unix")
}

fn validate_response(value: &Value) -> Result<()> {
    if value["v"].as_u64() != Some(crate::serve::PROTOCOL_VERSION) {
        anyhow::bail!("host response has an unsupported protocol version");
    }
    if value["kind"].as_str() != Some("response") {
        anyhow::bail!("host response has the wrong message kind");
    }
    if value["id"].as_u64() != Some(REQUEST_ID) {
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
            "id": REQUEST_ID,
            "ok": true,
            "result": {"scope": "host", "threads": 2, "running_turns": 0}
        });
        validate_response(&response).unwrap();
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
            "id": REQUEST_ID,
            "ok": true,
            "result": {"scope": "elsewhere", "threads": 2, "running_turns": 0}
        });
        assert!(AttachStatus::from_response(&unknown).is_err());
    }
}
