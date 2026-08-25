//! Scoped, expiring credentials for named-Brain participants.
//!
//! The human-managed Brain password is only a bootstrap credential. Ordinary
//! remote Brain operations use these narrower bearer credentials, signed by a
//! daemon-owned secret and revocable by stable credential ID.

use anyhow::{Context, Result};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::shared::{AttachmentId, AttachmentRole, BrainId, ConnectionId};

const CREDENTIAL_VERSION: u32 = 1;
const TOKEN_PREFIX: &str = "finch-brain-v1";
const SIGNING_KEY_FILE: &str = "brain-credential.key";
const REVOCATIONS_FILE: &str = "brain-credential-revocations.json";
const MAX_DELEGATION_DEPTH: usize = 8;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BrainCredentialScope {
    #[serde(rename = "brain:read")]
    BrainRead,
    #[serde(rename = "brain:attach")]
    BrainAttach,
    #[serde(rename = "brain:submit")]
    BrainSubmit,
    #[serde(rename = "brain:approve")]
    BrainApprove,
    #[serde(rename = "brain:control")]
    BrainControl,
    #[serde(rename = "environment:execute")]
    EnvironmentExecute,
    #[serde(rename = "environment:admin")]
    EnvironmentAdmin,
    #[serde(rename = "compute:submit")]
    ComputeSubmit,
}

/// Baseline authority granted when the bootstrap administrator selects only a
/// participant role. Connection lifecycle is independent from runner control,
/// and consultant approval is opt-in rather than implied by the role.
pub fn default_participant_scopes(role: AttachmentRole) -> BTreeSet<BrainCredentialScope> {
    match role {
        AttachmentRole::Driver => [
            BrainCredentialScope::BrainRead,
            BrainCredentialScope::BrainAttach,
            BrainCredentialScope::BrainSubmit,
            BrainCredentialScope::BrainApprove,
        ]
        .into_iter()
        .collect(),
        AttachmentRole::Consultant => [
            BrainCredentialScope::BrainRead,
            BrainCredentialScope::BrainAttach,
            BrainCredentialScope::BrainSubmit,
        ]
        .into_iter()
        .collect(),
        AttachmentRole::Observer => [
            BrainCredentialScope::BrainRead,
            BrainCredentialScope::BrainAttach,
        ]
        .into_iter()
        .collect(),
        AttachmentRole::Runner => BTreeSet::new(),
    }
}

/// Maximum scopes this participant credential endpoint may mint for a role.
/// Environment execution and distributed compute remain separate authorities.
pub fn permitted_participant_scopes(role: AttachmentRole) -> BTreeSet<BrainCredentialScope> {
    let mut scopes = default_participant_scopes(role);
    match role {
        AttachmentRole::Driver => {
            scopes.insert(BrainCredentialScope::BrainControl);
            scopes.insert(BrainCredentialScope::EnvironmentAdmin);
        }
        AttachmentRole::Consultant => {
            scopes.insert(BrainCredentialScope::BrainApprove);
        }
        AttachmentRole::Observer | AttachmentRole::Runner => {}
    }
    scopes
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainCredentialClaims {
    pub version: u32,
    pub credential_id: uuid::Uuid,
    pub issuer: String,
    pub subject: String,
    pub brain_id: BrainId,
    pub brain: String,
    pub environment_generation: u64,
    pub role: AttachmentRole,
    pub scopes: BTreeSet<BrainCredentialScope>,
    /// Present only after the daemon narrows a bootstrap participant
    /// credential to one exact remote attachment connection. These opaque
    /// identities are correlation data until covered by this signed claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_id: Option<AttachmentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<ConnectionId>,
    /// Oldest-to-newest credential IDs that delegated this credential.
    /// Revoking any ancestor therefore revokes every descendant without a
    /// mutable child index.
    #[serde(default)]
    pub delegation_chain: Vec<uuid::Uuid>,
    pub issued_ms: u64,
    pub expires_ms: u64,
}

impl BrainCredentialClaims {
    pub fn permits(&self, scope: BrainCredentialScope) -> bool {
        self.scopes.contains(&scope)
    }

    pub fn require_audience(
        &self,
        brain_id: BrainId,
        brain: &str,
        environment_generation: u64,
        scope: BrainCredentialScope,
    ) -> Result<()> {
        if self.brain_id != brain_id || self.brain != brain {
            anyhow::bail!("Brain credential has a different Brain audience");
        }
        if self.environment_generation != environment_generation {
            anyhow::bail!("Brain credential environment generation is no longer current");
        }
        if !self.permits(scope) {
            anyhow::bail!("Brain credential does not grant the required scope");
        }
        Ok(())
    }

    pub fn require_participant(&self, subject: &str, role: AttachmentRole) -> Result<()> {
        if self.subject != subject || self.role != role {
            anyhow::bail!("Brain participant identity does not match the credential");
        }
        Ok(())
    }

    pub fn require_attachment(
        &self,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<()> {
        if self.attachment_id != Some(attachment_id)
            || self.connection_id != Some(connection_id)
        {
            anyhow::bail!("Brain credential is not bound to this attachment connection");
        }
        Ok(())
    }

    /// Derive the ancestry for an attenuated child credential. Delegation is
    /// itself controlled authority: a child may neither gain scopes nor
    /// outlive its parent.
    pub fn attenuate(
        &self,
        child_scopes: &BTreeSet<BrainCredentialScope>,
        child_ttl_ms: u64,
        now_ms: u64,
    ) -> Result<Vec<uuid::Uuid>> {
        if !self.permits(BrainCredentialScope::BrainControl) {
            anyhow::bail!("Brain credential does not grant delegation control");
        }
        if !child_scopes.is_subset(&self.scopes) {
            anyhow::bail!("delegated Brain credential scopes exceed the delegator");
        }
        if child_ttl_ms > self.expires_ms.saturating_sub(now_ms) {
            anyhow::bail!("delegated Brain credential outlives the delegator");
        }
        let mut chain = self.delegation_chain.clone();
        chain.push(self.credential_id);
        if chain.len() > MAX_DELEGATION_DEPTH {
            anyhow::bail!("Brain credential delegation chain is too deep");
        }
        Ok(chain)
    }
}

#[derive(Debug, Clone)]
pub struct BrainCredentialRequest {
    pub issuer: String,
    pub subject: String,
    pub brain_id: BrainId,
    pub brain: String,
    pub environment_generation: u64,
    pub role: AttachmentRole,
    pub scopes: BTreeSet<BrainCredentialScope>,
    pub delegation_chain: Vec<uuid::Uuid>,
    pub ttl_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct RevocationFile {
    version: u32,
    credential_ids: BTreeSet<uuid::Uuid>,
}

#[derive(Clone)]
pub struct BrainCredentialAuthority {
    signing_key: Arc<[u8; 32]>,
    revoked: Arc<Mutex<BTreeSet<uuid::Uuid>>>,
    revocations_path: Option<Arc<PathBuf>>,
}

impl BrainCredentialAuthority {
    /// Load the daemon credential authority from a private state directory.
    /// The signing key is generated once and survives daemon restarts.
    pub fn load_or_create(state_directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_directory)
            .with_context(|| format!("create {}", state_directory.display()))?;
        let key_path = state_directory.join(SIGNING_KEY_FILE);
        let signing_key = load_or_create_signing_key(&key_path)?;
        let revocations_path = state_directory.join(REVOCATIONS_FILE);
        let revoked = load_revocations(&revocations_path)?;
        Ok(Self {
            signing_key: Arc::new(signing_key),
            revoked: Arc::new(Mutex::new(revoked)),
            revocations_path: Some(Arc::new(revocations_path)),
        })
    }

    #[cfg(test)]
    fn ephemeral(signing_key: [u8; 32]) -> Self {
        Self {
            signing_key: Arc::new(signing_key),
            revoked: Arc::new(Mutex::new(BTreeSet::new())),
            revocations_path: None,
        }
    }

    pub fn issue(&self, request: BrainCredentialRequest, now_ms: u64) -> Result<String> {
        if request.ttl_ms == 0 {
            anyhow::bail!("Brain credential lifetime must be greater than zero");
        }
        if request.subject.trim().is_empty() {
            anyhow::bail!("Brain credential subject cannot be empty");
        }
        if request.scopes.is_empty() {
            anyhow::bail!("Brain credential must grant at least one scope");
        }
        if request.delegation_chain.len() > MAX_DELEGATION_DEPTH {
            anyhow::bail!("Brain credential delegation chain is too deep");
        }
        let unique_ancestors = request
            .delegation_chain
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if unique_ancestors.len() != request.delegation_chain.len() {
            anyhow::bail!("Brain credential delegation chain contains a cycle");
        }
        let claims = BrainCredentialClaims {
            version: CREDENTIAL_VERSION,
            credential_id: uuid::Uuid::new_v4(),
            issuer: request.issuer,
            subject: request.subject,
            brain_id: request.brain_id,
            brain: request.brain,
            environment_generation: request.environment_generation,
            role: request.role,
            scopes: request.scopes,
            attachment_id: None,
            connection_id: None,
            delegation_chain: request.delegation_chain,
            issued_ms: now_ms,
            expires_ms: now_ms
                .checked_add(request.ttl_ms)
                .context("Brain credential expiry overflow")?,
        };
        self.sign(&claims)
    }

    /// Narrow an already verified participant credential to one pending
    /// remote attachment. The derived credential cannot create or operate a
    /// sibling attachment, cannot outlive its parent, and is invalidated by
    /// revoking any ancestor in the signed chain.
    pub fn bind_attachment(
        &self,
        parent: &BrainCredentialClaims,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        now_ms: u64,
    ) -> Result<(String, BrainCredentialClaims)> {
        if parent.attachment_id.is_some() || parent.connection_id.is_some() {
            anyhow::bail!("Brain credential is already bound to an attachment");
        }
        if !parent.permits(BrainCredentialScope::BrainAttach) {
            anyhow::bail!("Brain credential cannot bind an attachment without brain:attach");
        }
        if now_ms < parent.issued_ms || now_ms >= parent.expires_ms {
            anyhow::bail!("Brain credential is outside its validity interval");
        }
        let mut delegation_chain = parent.delegation_chain.clone();
        delegation_chain.push(parent.credential_id);
        if delegation_chain.len() > MAX_DELEGATION_DEPTH {
            anyhow::bail!("Brain credential delegation chain is too deep");
        }
        let mut scopes = parent.scopes.clone();
        scopes.remove(&BrainCredentialScope::BrainAttach);
        let claims = BrainCredentialClaims {
            version: CREDENTIAL_VERSION,
            credential_id: uuid::Uuid::new_v4(),
            issuer: parent.issuer.clone(),
            subject: parent.subject.clone(),
            brain_id: parent.brain_id,
            brain: parent.brain.clone(),
            environment_generation: parent.environment_generation,
            role: parent.role,
            scopes,
            attachment_id: Some(attachment_id),
            connection_id: Some(connection_id),
            delegation_chain,
            issued_ms: now_ms,
            expires_ms: parent.expires_ms,
        };
        Ok((self.sign(&claims)?, claims))
    }

    pub fn verify(&self, token: &str, now_ms: u64) -> Result<BrainCredentialClaims> {
        let mut parts = token.split('.');
        let prefix = parts.next().unwrap_or_default();
        let payload = parts
            .next()
            .context("Brain credential payload is missing")?;
        let signature = parts
            .next()
            .context("Brain credential signature is missing")?;
        if prefix != TOKEN_PREFIX || parts.next().is_some() {
            anyhow::bail!("Brain credential envelope is invalid");
        }

        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(signature)
            .context("Brain credential signature is invalid")?;
        let mut mac = HmacSha256::new_from_slice(self.signing_key.as_slice())
            .expect("HMAC accepts a 32-byte key");
        mac.update(payload.as_bytes());
        mac.verify_slice(&signature)
            .map_err(|_| anyhow::anyhow!("Brain credential signature does not match"))?;

        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .context("Brain credential payload is invalid")?;
        let claims: BrainCredentialClaims =
            serde_json::from_slice(&encoded).context("Brain credential claims are invalid")?;
        if claims.version != CREDENTIAL_VERSION {
            anyhow::bail!("unsupported Brain credential version {}", claims.version);
        }
        if claims.attachment_id.is_some() != claims.connection_id.is_some() {
            anyhow::bail!("Brain credential attachment binding is incomplete");
        }
        if now_ms < claims.issued_ms {
            anyhow::bail!("Brain credential is not valid yet");
        }
        if now_ms >= claims.expires_ms {
            anyhow::bail!("Brain credential has expired");
        }
        let revoked = self
            .revoked
            .lock()
            .expect("Brain credential revocation lock poisoned");
        if revoked.contains(&claims.credential_id)
            || claims
                .delegation_chain
                .iter()
                .any(|ancestor| revoked.contains(ancestor))
        {
            anyhow::bail!("Brain credential has been revoked");
        }
        Ok(claims)
    }

    pub fn revoke(&self, credential_id: uuid::Uuid) -> Result<()> {
        let mut revoked = self
            .revoked
            .lock()
            .expect("Brain credential revocation lock poisoned");
        if !revoked.insert(credential_id) {
            return Ok(());
        }
        if let Some(path) = &self.revocations_path {
            if let Err(error) = persist_revocations(path, &revoked) {
                revoked.remove(&credential_id);
                return Err(error);
            }
        }
        Ok(())
    }

    fn sign(&self, claims: &BrainCredentialClaims) -> Result<String> {
        let encoded = serde_json::to_vec(claims).context("serialize Brain credential")?;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(encoded);
        let mut mac = HmacSha256::new_from_slice(self.signing_key.as_slice())
            .expect("HMAC accepts a 32-byte key");
        mac.update(payload.as_bytes());
        let signature =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        Ok(format!("{TOKEN_PREFIX}.{payload}.{signature}"))
    }
}

fn load_or_create_signing_key(path: &Path) -> Result<[u8; 32]> {
    match std::fs::read(path) {
        Ok(bytes) => return key_from_bytes(path, bytes),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(error).with_context(|| format!("read {}", path.display()));
        }
        Err(_) => {}
    }

    let mut key = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(&key)
                .with_context(|| format!("write {}", path.display()))?;
            file.sync_all()
                .with_context(|| format!("sync {}", path.display()))?;
            Ok(key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            key_from_bytes(path, std::fs::read(path)?)
        }
        Err(error) => Err(error).with_context(|| format!("create {}", path.display())),
    }
}

fn key_from_bytes(path: &Path, bytes: Vec<u8>) -> Result<[u8; 32]> {
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!(
            "Brain credential key {} has {} bytes; expected 32",
            path.display(),
            bytes.len()
        )
    })
}

fn load_revocations(path: &Path) -> Result<BTreeSet<uuid::Uuid>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let file: RevocationFile = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?;
            if file.version != CREDENTIAL_VERSION {
                anyhow::bail!("unsupported Brain revocation version {}", file.version);
            }
            Ok(file.credential_ids)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn persist_revocations(path: &Path, revoked: &BTreeSet<uuid::Uuid>) -> Result<()> {
    let file = RevocationFile {
        version: CREDENTIAL_VERSION,
        credential_ids: revoked.clone(),
    };
    let encoded = serde_json::to_vec_pretty(&file)?;
    let parent = path.parent().context("revocation path has no parent")?;
    let temporary = parent.join(format!(
        ".brain-credential-revocations.{}.tmp",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&temporary, encoded)
        .with_context(|| format!("write {}", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("commit {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(ttl_ms: u64) -> BrainCredentialRequest {
        BrainCredentialRequest {
            issuer: "machine.local".into(),
            subject: "alice@laptop.local".into(),
            brain_id: BrainId(uuid::Uuid::new_v4()),
            brain: "shared-work".into(),
            environment_generation: 7,
            role: AttachmentRole::Driver,
            scopes: [
                BrainCredentialScope::BrainRead,
                BrainCredentialScope::BrainSubmit,
            ]
            .into_iter()
            .collect(),
            delegation_chain: Vec::new(),
            ttl_ms,
        }
    }

    #[test]
    fn issued_credential_round_trips_all_authority_boundaries() {
        let authority = BrainCredentialAuthority::ephemeral([3; 32]);
        let token = authority.issue(request(1_000), 10_000).unwrap();
        let claims = authority.verify(&token, 10_500).unwrap();
        assert_eq!(claims.subject, "alice@laptop.local");
        assert_eq!(claims.brain, "shared-work");
        assert_eq!(claims.environment_generation, 7);
        assert_eq!(claims.role, AttachmentRole::Driver);
        assert!(claims.permits(BrainCredentialScope::BrainRead));
        assert!(claims.permits(BrainCredentialScope::BrainSubmit));
        assert!(!claims.permits(BrainCredentialScope::BrainApprove));
    }

    #[test]
    fn attachment_credential_is_narrowed_to_one_connection_and_parent_revocation() {
        let authority = BrainCredentialAuthority::ephemeral([10; 32]);
        let now = 1_000;
        let mut credential_request = request(60_000);
        credential_request
            .scopes
            .insert(BrainCredentialScope::BrainAttach);
        let parent_token = authority.issue(credential_request, now).unwrap();
        let parent = authority.verify(&parent_token, now).unwrap();
        let attachment_id = AttachmentId(uuid::Uuid::new_v4());
        let connection_id = ConnectionId(uuid::Uuid::new_v4());
        let (token, bound) = authority
            .bind_attachment(&parent, attachment_id, connection_id, now + 1)
            .unwrap();

        assert!(bound
            .require_attachment(attachment_id, connection_id)
            .is_ok());
        assert!(!bound.permits(BrainCredentialScope::BrainAttach));
        assert!(bound
            .require_attachment(
                AttachmentId(uuid::Uuid::new_v4()),
                connection_id,
            )
            .is_err());
        assert!(authority
            .bind_attachment(&bound, attachment_id, connection_id, now + 2)
            .is_err());
        assert_eq!(authority.verify(&token, now + 2).unwrap(), bound);

        authority.revoke(parent.credential_id).unwrap();
        assert!(authority.verify(&token, now + 3).is_err());
    }

    #[test]
    fn tampering_fails_before_claims_are_trusted() {
        let authority = BrainCredentialAuthority::ephemeral([4; 32]);
        let mut token = authority.issue(request(1_000), 10_000).unwrap();
        let index = token.find('.').unwrap() + 2;
        token.replace_range(index..=index, "x");
        assert!(authority.verify(&token, 10_500).is_err());
    }

    #[test]
    fn expiry_is_exclusive_and_future_credentials_fail() {
        let authority = BrainCredentialAuthority::ephemeral([5; 32]);
        let token = authority.issue(request(1_000), 10_000).unwrap();
        assert!(authority.verify(&token, 9_999).is_err());
        assert!(authority.verify(&token, 10_999).is_ok());
        assert!(authority.verify(&token, 11_000).is_err());
    }

    #[test]
    fn revocation_survives_authority_restart() {
        let temp = tempfile::tempdir().unwrap();
        let authority = BrainCredentialAuthority::load_or_create(temp.path()).unwrap();
        let token = authority.issue(request(1_000), 10_000).unwrap();
        let credential_id = authority.verify(&token, 10_500).unwrap().credential_id;
        authority.revoke(credential_id).unwrap();

        let restarted = BrainCredentialAuthority::load_or_create(temp.path()).unwrap();
        assert!(restarted.verify(&token, 10_500).is_err());
    }

    #[test]
    fn revoking_an_ancestor_revokes_every_delegated_descendant() {
        let authority = BrainCredentialAuthority::ephemeral([7; 32]);
        let parent_token = authority.issue(request(10_000), 10_000).unwrap();
        let parent = authority.verify(&parent_token, 10_100).unwrap();

        let mut child_request = request(5_000);
        child_request.subject = "bob@desktop.local".into();
        child_request.role = AttachmentRole::Observer;
        child_request.scopes = default_participant_scopes(AttachmentRole::Observer);
        child_request.delegation_chain = vec![parent.credential_id];
        let child_token = authority.issue(child_request, 10_100).unwrap();
        let child = authority.verify(&child_token, 10_200).unwrap();

        let mut grandchild_request = request(1_000);
        grandchild_request.subject = "carol@tablet.local".into();
        grandchild_request.role = AttachmentRole::Observer;
        grandchild_request.scopes = default_participant_scopes(AttachmentRole::Observer);
        grandchild_request.delegation_chain = vec![parent.credential_id, child.credential_id];
        let grandchild_token = authority.issue(grandchild_request, 10_200).unwrap();
        assert!(authority.verify(&grandchild_token, 10_300).is_ok());

        authority.revoke(parent.credential_id).unwrap();
        assert!(authority.verify(&child_token, 10_300).is_err());
        assert!(authority.verify(&grandchild_token, 10_300).is_err());
    }

    #[test]
    fn malformed_or_unbounded_delegation_chains_fail_closed() {
        let authority = BrainCredentialAuthority::ephemeral([8; 32]);
        let repeated = uuid::Uuid::new_v4();
        let mut cyclic = request(1_000);
        cyclic.delegation_chain = vec![repeated, repeated];
        assert!(authority.issue(cyclic, 10_000).is_err());

        let mut too_deep = request(1_000);
        too_deep.delegation_chain = (0..=MAX_DELEGATION_DEPTH)
            .map(|_| uuid::Uuid::new_v4())
            .collect();
        assert!(authority.issue(too_deep, 10_000).is_err());
    }

    #[test]
    fn attenuation_cannot_add_authority_or_outlive_its_parent() {
        let authority = BrainCredentialAuthority::ephemeral([9; 32]);
        let mut parent_request = request(5_000);
        parent_request.scopes.insert(BrainCredentialScope::BrainControl);
        parent_request.scopes.insert(BrainCredentialScope::BrainApprove);
        let parent_token = authority.issue(parent_request, 10_000).unwrap();
        let parent = authority.verify(&parent_token, 10_100).unwrap();

        let child_scopes = [BrainCredentialScope::BrainRead]
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            parent.attenuate(&child_scopes, 1_000, 10_100).unwrap(),
            vec![parent.credential_id]
        );

        let expanded = [
            BrainCredentialScope::BrainRead,
            BrainCredentialScope::EnvironmentAdmin,
        ]
        .into_iter()
        .collect();
        assert!(parent.attenuate(&expanded, 1_000, 10_100).is_err());
        assert!(parent.attenuate(&child_scopes, 5_000, 10_100).is_err());

        let ordinary = authority
            .verify(&authority.issue(request(5_000), 10_000).unwrap(), 10_100)
            .unwrap();
        assert!(ordinary.attenuate(&child_scopes, 1_000, 10_100).is_err());
    }

    #[test]
    fn signing_key_survives_authority_restart() {
        let temp = tempfile::tempdir().unwrap();
        let token = BrainCredentialAuthority::load_or_create(temp.path())
            .unwrap()
            .issue(request(1_000), 10_000)
            .unwrap();
        let restarted = BrainCredentialAuthority::load_or_create(temp.path()).unwrap();
        assert!(restarted.verify(&token, 10_500).is_ok());
    }

    #[test]
    fn audience_scope_generation_and_participant_are_independent_boundaries() {
        let authority = BrainCredentialAuthority::ephemeral([6; 32]);
        let token = authority.issue(request(1_000), 10_000).unwrap();
        let claims = authority.verify(&token, 10_500).unwrap();
        assert!(claims
            .require_audience(
                claims.brain_id,
                "shared-work",
                7,
                BrainCredentialScope::BrainRead,
            )
            .is_ok());
        assert!(claims
            .require_audience(
                BrainId(uuid::Uuid::new_v4()),
                "shared-work",
                7,
                BrainCredentialScope::BrainRead,
            )
            .is_err());
        assert!(claims
            .require_audience(claims.brain_id, "other", 7, BrainCredentialScope::BrainRead,)
            .is_err());
        assert!(claims
            .require_audience(
                claims.brain_id,
                "shared-work",
                8,
                BrainCredentialScope::BrainRead,
            )
            .is_err());
        assert!(claims
            .require_audience(
                claims.brain_id,
                "shared-work",
                7,
                BrainCredentialScope::BrainApprove,
            )
            .is_err());
        assert!(claims
            .require_participant("alice@laptop.local", AttachmentRole::Driver)
            .is_ok());
        assert!(claims
            .require_participant("mallory@laptop.local", AttachmentRole::Driver)
            .is_err());
        assert!(claims
            .require_participant("alice@laptop.local", AttachmentRole::Observer)
            .is_err());
    }
}
