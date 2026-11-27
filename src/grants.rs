//! D19 grants: a revocable record in the store plus a secret file the client
//! holds. The row is the authority; the file is the credential.

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::store::{now_iso, BatonStore, GrantRow, GrantSummary};
use crate::types::PermissionMode;

pub const DEFAULT_GRANT_DAYS: i64 = 30;
pub const MAX_LABEL_CHARS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Read,
    Operate,
    Approve,
    Git,
    Orchestrate,
    Settings,
    Admin,
    Fs,
}

impl Scope {
    pub const ALL: [Scope; 8] = [
        Scope::Read,
        Scope::Operate,
        Scope::Approve,
        Scope::Git,
        Scope::Orchestrate,
        Scope::Settings,
        Scope::Admin,
        Scope::Fs,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Operate => "operate",
            Scope::Approve => "approve",
            Scope::Git => "git",
            Scope::Orchestrate => "orchestrate",
            Scope::Settings => "settings",
            Scope::Admin => "admin",
            Scope::Fs => "fs",
        }
    }

    pub fn parse(value: &str) -> Option<Scope> {
        Scope::ALL.into_iter().find(|scope| scope.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRequest {
    pub scopes: Vec<Scope>,
    pub permission_ceiling: PermissionMode,
    pub project_allowlist: Vec<String>,
    pub label: Option<String>,
    pub expires_at: String,
}

impl GrantRequest {
    /// The pairing default: read and operate for 30 days.
    pub fn new(scopes: Vec<Scope>) -> GrantRequest {
        GrantRequest {
            scopes,
            permission_ceiling: PermissionMode::Default,
            project_allowlist: Vec::new(),
            label: None,
            expires_at: expiry_after_days(DEFAULT_GRANT_DAYS),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MintedGrant {
    pub summary: GrantSummary,
    pub secret: String,
}

impl fmt::Debug for MintedGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MintedGrant")
            .field("summary", &self.summary)
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantRefusal {
    NotFound,
    Revoked,
    Expired,
    BadSecret,
}

impl GrantRefusal {
    pub fn code(self) -> &'static str {
        match self {
            GrantRefusal::NotFound => "not_found",
            GrantRefusal::Revoked => "revoked",
            GrantRefusal::Expired => "expired",
            GrantRefusal::BadSecret => "bad_secret",
        }
    }
}

impl fmt::Display for GrantRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GrantRefusal::NotFound => write!(formatter, "no grant matches that id"),
            GrantRefusal::Revoked => write!(formatter, "the grant was revoked"),
            GrantRefusal::Expired => write!(formatter, "the grant expired"),
            GrantRefusal::BadSecret => write!(formatter, "the grant secret does not match"),
        }
    }
}

impl std::error::Error for GrantRefusal {}

pub fn expiry_after_days(days: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::days(days))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn grants_dir(home: &Path) -> PathBuf {
    home.join("grants")
}

/// A grant id names a file, so only the canonical UUID form is allowed.
pub fn valid_grant_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .map(|id| id.to_string() == value)
        .unwrap_or(false)
}

fn secret_path(home: &Path, grant_id: &str) -> Option<PathBuf> {
    valid_grant_id(grant_id).then(|| grants_dir(home).join(grant_id))
}

/// Mints a grant. The secret file is written first: a row without its secret
/// is unusable, while a stray secret file has no authority.
pub fn mint(store: &BatonStore, home: &Path, request: &GrantRequest) -> Result<MintedGrant> {
    validate(request)?;
    let installation_id = crate::claim::ensure_installation_id(home)?;
    let grant_id = uuid::Uuid::new_v4().to_string();
    let secret = new_secret();
    let secret_hash = hash_secret(&secret);
    let issued_at = now_iso();

    crate::claim::create_private_dir(home)?;
    let dir = grants_dir(home);
    crate::claim::create_private_dir(&dir)?;
    let path = secret_path(home, &grant_id).expect("generated grant ids are UUIDs");
    {
        let mut file = crate::claim::open_private_new(&path)
            .with_context(|| format!("create {}", path.display()))?;
        file.write_all(secret.as_bytes())?;
        file.write_all(b"\n")?;
    }

    let row = GrantRow {
        grant_id: grant_id.clone(),
        installation_id,
        secret_hash,
        scopes: request
            .scopes
            .iter()
            .map(|scope| scope.as_str().to_string())
            .collect(),
        permission_ceiling: request.permission_ceiling.as_str().to_string(),
        project_allowlist: request.project_allowlist.clone(),
        issued_at,
        expires_at: request.expires_at.clone(),
        revoked_at: None,
        label: request.label.clone(),
    };
    if let Err(error) = store.insert_grant(&row) {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(MintedGrant {
        summary: row.summary(),
        secret,
    })
}

pub fn verify(store: &BatonStore, grant_id: &str, secret: &str) -> Result<GrantSummary> {
    let row = store
        .find_grant_row(grant_id)?
        .ok_or(GrantRefusal::NotFound)?;
    if row.revoked_at.is_some() {
        return Err(GrantRefusal::Revoked.into());
    }
    if expired(&row.expires_at) {
        return Err(GrantRefusal::Expired.into());
    }
    if hash_secret(secret) != row.secret_hash {
        return Err(GrantRefusal::BadSecret.into());
    }
    Ok(row.summary())
}

pub fn state(summary: &GrantSummary) -> &'static str {
    if summary.revoked_at.is_some() {
        "revoked"
    } else if expired(&summary.expires_at) {
        "expired"
    } else {
        "live"
    }
}

pub fn list(store: &BatonStore) -> Result<Vec<GrantSummary>> {
    Ok(store
        .list_grant_rows()?
        .iter()
        .map(GrantRow::summary)
        .collect())
}

pub fn revoke(store: &BatonStore, home: &Path, grant_id: &str) -> Result<bool> {
    let Some(path) = secret_path(home, grant_id) else {
        anyhow::bail!("grant id must be a lowercase UUID: {grant_id}");
    };
    let existed = store.revoke_grant_row(grant_id)?;
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
    }
    Ok(existed)
}

fn validate(request: &GrantRequest) -> Result<()> {
    if request.scopes.is_empty() {
        anyhow::bail!("a grant needs at least one scope");
    }
    if request.permission_ceiling == PermissionMode::BypassPermissions {
        anyhow::bail!("ceiling: bypassPermissions is not grantable");
    }
    if request
        .expires_at
        .parse::<chrono::DateTime<chrono::Utc>>()
        .is_err()
    {
        anyhow::bail!("expires_at must be RFC 3339: {}", request.expires_at);
    }
    if expired(&request.expires_at) {
        anyhow::bail!("expires_at is in the past");
    }
    for path in &request.project_allowlist {
        if !Path::new(path).is_absolute() {
            anyhow::bail!("project allowlist entries must be absolute: {path}");
        }
    }
    if let Some(label) = &request.label {
        if label.chars().count() > MAX_LABEL_CHARS {
            anyhow::bail!("a grant label is at most {MAX_LABEL_CHARS} characters");
        }
    }
    Ok(())
}

fn expired(expires_at: &str) -> bool {
    match expires_at.parse::<chrono::DateTime<chrono::Utc>>() {
        Ok(expiry) => expiry <= chrono::Utc::now(),
        Err(_) => true,
    }
}

fn new_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn hash_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::BatonStore;

    fn temp_home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn mint_writes_a_private_secret_and_a_hash_only_row() {
        let home = temp_home();
        let store = BatonStore::memory().unwrap();
        let mut request = GrantRequest::new(vec![Scope::Read, Scope::Operate]);
        request.label = Some("laptop".into());
        let minted = mint(&store, home.path(), &request).unwrap();

        assert_eq!(minted.summary.scopes, vec!["read", "operate"]);
        assert_eq!(minted.summary.label.as_deref(), Some("laptop"));
        assert_eq!(minted.summary.permission_ceiling, "default");
        assert_eq!(minted.secret.len(), 64);
        let path = secret_path(home.path(), &minted.summary.grant_id).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&grants_dir(home.path())), 0o700);
        let stored = fs::read_to_string(&path).unwrap();
        assert_eq!(stored.trim(), minted.secret);

        let row = store
            .find_grant_row(&minted.summary.grant_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.secret_hash, hash_secret(&minted.secret));
        assert_ne!(row.secret_hash, minted.secret);
        assert_eq!(row.secret_hash.len(), 64);
    }

    #[test]
    fn a_minted_grant_never_prints_its_secret() {
        let home = temp_home();
        let store = BatonStore::memory().unwrap();
        let minted = mint(&store, home.path(), &GrantRequest::new(vec![Scope::Read])).unwrap();
        let debug = format!("{minted:?}");
        assert!(!debug.contains(&minted.secret), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }

    #[test]
    fn verify_accepts_the_minted_secret_and_refuses_a_wrong_one() {
        let home = temp_home();
        let store = BatonStore::memory().unwrap();
        let minted = mint(&store, home.path(), &GrantRequest::new(vec![Scope::Read])).unwrap();
        let verified = verify(&store, &minted.summary.grant_id, &minted.secret).unwrap();
        assert_eq!(verified.grant_id, minted.summary.grant_id);

        let wrong = verify(&store, &minted.summary.grant_id, "not-the-secret").unwrap_err();
        assert_eq!(
            wrong.downcast_ref::<GrantRefusal>(),
            Some(&GrantRefusal::BadSecret)
        );
        let missing = verify(&store, "missing", &minted.secret).unwrap_err();
        assert_eq!(
            missing.downcast_ref::<GrantRefusal>(),
            Some(&GrantRefusal::NotFound)
        );
    }

    #[test]
    fn revoke_deletes_the_secret_and_refuses_the_grant() {
        let home = temp_home();
        let store = BatonStore::memory().unwrap();
        let minted = mint(
            &store,
            home.path(),
            &GrantRequest::new(vec![Scope::Operate]),
        )
        .unwrap();
        assert!(revoke(&store, home.path(), &minted.summary.grant_id).unwrap());
        assert!(!secret_path(home.path(), &minted.summary.grant_id)
            .unwrap()
            .exists());
        let row = store
            .find_grant_row(&minted.summary.grant_id)
            .unwrap()
            .unwrap();
        assert!(row.revoked_at.is_some());
        let refused = verify(&store, &minted.summary.grant_id, &minted.secret).unwrap_err();
        assert_eq!(
            refused.downcast_ref::<GrantRefusal>(),
            Some(&GrantRefusal::Revoked)
        );
    }

    #[test]
    fn revoke_refuses_a_path_like_grant_id() {
        let home = temp_home();
        let store = BatonStore::memory().unwrap();
        crate::claim::create_private_dir(&grants_dir(home.path())).unwrap();
        let outside = home.path().join("outside.txt");
        fs::write(&outside, "keep").unwrap();
        let error = revoke(&store, home.path(), outside.to_str().unwrap()).unwrap_err();
        assert!(error.to_string().contains("grant id"), "{error}");
        assert_eq!(fs::read_to_string(&outside).unwrap(), "keep");

        let error = revoke(&store, home.path(), "../installation-id").unwrap_err();
        assert!(error.to_string().contains("grant id"), "{error}");
    }

    #[test]
    fn verify_refuses_an_expired_grant() {
        let store = BatonStore::memory().unwrap();
        let secret = "a".repeat(64);
        store
            .insert_grant(&GrantRow {
                grant_id: "expired".into(),
                installation_id: "installation".into(),
                secret_hash: hash_secret(&secret),
                scopes: vec!["read".into()],
                permission_ceiling: "default".into(),
                project_allowlist: Vec::new(),
                issued_at: "2000-01-01T00:00:00.000Z".into(),
                expires_at: "2000-01-01T00:00:00.000Z".into(),
                revoked_at: None,
                label: None,
            })
            .unwrap();
        let refused = verify(&store, "expired", &secret).unwrap_err();
        assert_eq!(
            refused.downcast_ref::<GrantRefusal>(),
            Some(&GrantRefusal::Expired)
        );
    }

    #[test]
    fn mint_refuses_a_bypass_ceiling_and_a_relative_allowlist() {
        let home = temp_home();
        let store = BatonStore::memory().unwrap();
        let mut bypass = GrantRequest::new(vec![Scope::Operate]);
        bypass.permission_ceiling = PermissionMode::BypassPermissions;
        let error = mint(&store, home.path(), &bypass).unwrap_err();
        assert!(error.to_string().contains("ceiling"), "{error}");

        let mut relative = GrantRequest::new(vec![Scope::Operate]);
        relative.project_allowlist = vec!["relative/path".into()];
        let error = mint(&store, home.path(), &relative).unwrap_err();
        assert!(error.to_string().contains("absolute"), "{error}");

        let error = mint(&store, home.path(), &GrantRequest::new(Vec::new())).unwrap_err();
        assert!(error.to_string().contains("scope"), "{error}");
    }

    #[test]
    fn list_reports_summaries_in_issue_order() {
        let home = temp_home();
        let store = BatonStore::memory().unwrap();
        let first = mint(&store, home.path(), &GrantRequest::new(vec![Scope::Read])).unwrap();
        let second = mint(
            &store,
            home.path(),
            &GrantRequest::new(vec![Scope::Operate]),
        )
        .unwrap();
        let grants = list(&store).unwrap();
        assert_eq!(grants.len(), 2);
        assert_eq!(grants[0].grant_id, first.summary.grant_id);
        assert_eq!(grants[1].grant_id, second.summary.grant_id);
    }
}
