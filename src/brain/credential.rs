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
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::shared::{AttachmentId, AttachmentRole, BrainId, ConnectionId};

const CREDENTIAL_VERSION: u32 = 1;
const TOKEN_PREFIX: &str = "finch-brain-v1";
const INVITATION_PREFIX: &str = "finch-brain-invite-v1";
const SIGNING_KEY_FILE: &str = "brain-credential.key";
const REVOCATIONS_FILE: &str = "brain-credential-revocations.json";
const CONSUMED_INVITATIONS_FILE: &str = "brain-invitation-consumed.json";
const MAX_DELEGATION_DEPTH: usize = 8;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BrainCredentialScope {
    #[serde(rename = "brain:read")]
    BrainRead,
    #[serde(rename = "brain:attach")]
    BrainAttach,
    #[serde(rename = "brain:detach")]
    BrainDetach,
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
            BrainCredentialScope::BrainDetach,
            BrainCredentialScope::BrainSubmit,
            BrainCredentialScope::BrainApprove,
        ]
        .into_iter()
        .collect(),
        AttachmentRole::Consultant => [
            BrainCredentialScope::BrainRead,
            BrainCredentialScope::BrainAttach,
            BrainCredentialScope::BrainDetach,
            BrainCredentialScope::BrainSubmit,
        ]
        .into_iter()
        .collect(),
        AttachmentRole::Observer => [
            BrainCredentialScope::BrainRead,
            BrainCredentialScope::BrainAttach,
            BrainCredentialScope::BrainDetach,
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
        if self.attachment_id != Some(attachment_id) || self.connection_id != Some(connection_id) {
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

/// A short-lived, single-use bootstrap grant that can be handed to a
/// collaborator without disclosing the daemon-wide Brain password. The
/// recipient chooses only its participant subject; role, audience, scopes,
/// environment generation, and expiry are fixed by the issuer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainInvitationClaims {
    pub version: u32,
    pub invitation_id: uuid::Uuid,
    pub issuer: String,
    pub brain_id: BrainId,
    pub brain: String,
    pub environment_generation: u64,
    pub role: AttachmentRole,
    pub scopes: BTreeSet<BrainCredentialScope>,
    #[serde(default)]
    pub delegation_chain: Vec<uuid::Uuid>,
    /// Base64url DER for the exact self-signed TLS certificate the recipient
    /// must trust. This value is covered by the node's invitation signature.
    pub tls_certificate_der: String,
    pub issued_ms: u64,
    pub expires_ms: u64,
}

#[derive(Debug, Clone)]
pub struct BrainInvitationRequest {
    pub issuer: String,
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

#[derive(Debug, Serialize, Deserialize)]
struct ConsumedInvitationFile {
    version: u32,
    #[serde(default)]
    invitation_ids: BTreeSet<uuid::Uuid>,
    #[serde(default)]
    redemptions: BTreeMap<uuid::Uuid, InvitationRedemption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InvitationRedemption {
    subject: String,
}

#[derive(Debug, Default)]
struct ConsumedInvitations {
    /// Invitations consumed before retry-safe redemption was introduced.
    legacy_burned: BTreeSet<uuid::Uuid>,
    redemptions: BTreeMap<uuid::Uuid, InvitationRedemption>,
}

impl ConsumedInvitations {
    fn is_consumed(&self, invitation_id: uuid::Uuid) -> bool {
        self.legacy_burned.contains(&invitation_id) || self.redemptions.contains_key(&invitation_id)
    }
}

#[derive(Clone)]
pub struct BrainCredentialAuthority {
    signing_key: Arc<[u8; 32]>,
    invitation_signer: Arc<crate::node::identity::NodeSigningIdentity>,
    invitation_tls: Arc<crate::node::tls::NodeTlsIdentity>,
    revoked: Arc<Mutex<BTreeSet<uuid::Uuid>>>,
    revocations_path: Option<Arc<PathBuf>>,
    consumed_invitations: Arc<Mutex<ConsumedInvitations>>,
    consumed_invitations_path: Option<Arc<PathBuf>>,
}

impl BrainCredentialAuthority {
    pub fn invitation_public_key(&self) -> [u8; 32] {
        self.invitation_signer.public_key_bytes()
    }

    pub(crate) fn invitation_tls_identity(&self) -> &crate::node::tls::NodeTlsIdentity {
        &self.invitation_tls
    }

    /// Load the daemon credential authority from a private state directory.
    /// The signing key is generated once and survives daemon restarts.
    pub fn load_or_create(state_directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_directory)
            .with_context(|| format!("create {}", state_directory.display()))?;
        let key_path = state_directory.join(SIGNING_KEY_FILE);
        let signing_key = load_or_create_signing_key(&key_path)?;
        let invitation_signer =
            crate::node::identity::NodeSigningIdentity::load_or_create(state_directory)?;
        let hostname = hostname::get()
            .ok()
            .and_then(|name| name.into_string().ok())
            .unwrap_or_else(|| "localhost".to_string());
        let invitation_tls = crate::node::tls::NodeTlsIdentity::from_signing_identity(
            &invitation_signer,
            &hostname,
        )?;
        let revocations_path = state_directory.join(REVOCATIONS_FILE);
        let revoked = load_revocations(&revocations_path)?;
        let consumed_invitations_path = state_directory.join(CONSUMED_INVITATIONS_FILE);
        let consumed_invitations = load_consumed_invitations(&consumed_invitations_path)?;
        Ok(Self {
            signing_key: Arc::new(signing_key),
            invitation_signer: Arc::new(invitation_signer),
            invitation_tls: Arc::new(invitation_tls),
            revoked: Arc::new(Mutex::new(revoked)),
            revocations_path: Some(Arc::new(revocations_path)),
            consumed_invitations: Arc::new(Mutex::new(consumed_invitations)),
            consumed_invitations_path: Some(Arc::new(consumed_invitations_path)),
        })
    }

    #[cfg(test)]
    pub(crate) fn ephemeral(signing_key: [u8; 32]) -> Self {
        let invitation_signer =
            crate::node::identity::NodeSigningIdentity::from_secret(signing_key);
        let invitation_tls = crate::node::tls::NodeTlsIdentity::from_signing_identity(
            &invitation_signer,
            "localhost",
        )
        .expect("test node identity creates TLS material");
        Self {
            signing_key: Arc::new(signing_key),
            invitation_signer: Arc::new(invitation_signer),
            invitation_tls: Arc::new(invitation_tls),
            revoked: Arc::new(Mutex::new(BTreeSet::new())),
            revocations_path: None,
            consumed_invitations: Arc::new(Mutex::new(ConsumedInvitations::default())),
            consumed_invitations_path: None,
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

    pub fn issue_invitation(
        &self,
        request: BrainInvitationRequest,
        now_ms: u64,
    ) -> Result<(String, BrainInvitationClaims)> {
        if request.ttl_ms == 0 {
            anyhow::bail!("Brain invitation lifetime must be greater than zero");
        }
        if request.role == AttachmentRole::Runner {
            anyhow::bail!("runner authority cannot be delegated through a Brain invitation");
        }
        if request.scopes.is_empty()
            || !request
                .scopes
                .is_subset(&permitted_participant_scopes(request.role))
            || !request.scopes.contains(&BrainCredentialScope::BrainAttach)
        {
            anyhow::bail!("Brain invitation scopes are invalid for its participant role");
        }
        if request.delegation_chain.len() > MAX_DELEGATION_DEPTH
            || request
                .delegation_chain
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != request.delegation_chain.len()
        {
            anyhow::bail!("Brain invitation delegation chain is invalid");
        }
        let claims = BrainInvitationClaims {
            version: CREDENTIAL_VERSION,
            invitation_id: uuid::Uuid::new_v4(),
            issuer: request.issuer,
            brain_id: request.brain_id,
            brain: request.brain,
            environment_generation: request.environment_generation,
            role: request.role,
            scopes: request.scopes,
            delegation_chain: request.delegation_chain,
            tls_certificate_der: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(self.invitation_tls.certificate_der()),
            issued_ms: now_ms,
            expires_ms: now_ms
                .checked_add(request.ttl_ms)
                .context("Brain invitation expiry overflow")?,
        };
        Ok((self.sign_invitation(&claims)?, claims))
    }

    /// Verify an invitation without consuming it. This is suitable for UI
    /// preview only; authority is created exclusively by `redeem_invitation`.
    pub fn inspect_invitation(&self, token: &str, now_ms: u64) -> Result<BrainInvitationClaims> {
        let claims = self.decode_invitation(token, now_ms)?;
        if self
            .consumed_invitations
            .lock()
            .expect("Brain invitation lock poisoned")
            .is_consumed(claims.invitation_id)
        {
            anyhow::bail!("Brain invitation has already been redeemed");
        }
        Ok(claims)
    }

    /// Atomically bind one invitation to a participant and mint the ordinary
    /// credential used by every later attachment operation. A retry by the
    /// same subject returns that exact credential (including after restart),
    /// while a different subject can never receive a second grant.
    pub fn redeem_invitation(
        &self,
        token: &str,
        subject: &str,
        now_ms: u64,
    ) -> Result<(String, BrainCredentialClaims)> {
        if subject.trim().is_empty() {
            anyhow::bail!("Brain invitation subject cannot be empty");
        }
        let invitation = self.decode_invitation(token, now_ms)?;
        let mut consumed = self
            .consumed_invitations
            .lock()
            .expect("Brain invitation lock poisoned");
        if consumed.legacy_burned.contains(&invitation.invitation_id) {
            anyhow::bail!("Brain invitation was redeemed before retry-safe recovery was available");
        }
        if let Some(redemption) = consumed.redemptions.get(&invitation.invitation_id) {
            if redemption.subject != subject {
                anyhow::bail!("Brain invitation has already been redeemed by another participant");
            }
            let claims = invitation_credential_claims(&invitation, subject);
            return Ok((self.sign(&claims)?, claims));
        }
        let claims = invitation_credential_claims(&invitation, subject);
        let credential = self.sign(&claims)?;
        consumed.redemptions.insert(
            invitation.invitation_id,
            InvitationRedemption {
                subject: subject.to_string(),
            },
        );
        if let Some(path) = &self.consumed_invitations_path {
            if let Err(error) = persist_consumed_invitations(path, &consumed) {
                consumed.redemptions.remove(&invitation.invitation_id);
                return Err(error);
            }
        }
        Ok((credential, claims))
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

    fn sign_invitation(&self, claims: &BrainInvitationClaims) -> Result<String> {
        let encoded = serde_json::to_vec(claims).context("serialize Brain invitation")?;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(encoded);
        let public_key = self.invitation_signer.public_key_bytes();
        let public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public_key);
        let signature = self
            .invitation_signer
            .sign(invitation_signature_message(&payload).as_slice());
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature);
        Ok(format!(
            "{INVITATION_PREFIX}.{public_key}.{payload}.{signature}"
        ))
    }

    fn decode_invitation(&self, token: &str, now_ms: u64) -> Result<BrainInvitationClaims> {
        let (claims, public_key) = verify_portable_invitation(token, now_ms)?;
        if public_key != self.invitation_signer.public_key_bytes() {
            anyhow::bail!("Brain invitation was issued by a different node identity");
        }
        let revoked = self
            .revoked
            .lock()
            .expect("Brain credential revocation lock poisoned");
        if claims
            .delegation_chain
            .iter()
            .any(|ancestor| revoked.contains(ancestor))
        {
            anyhow::bail!("Brain invitation delegator has been revoked");
        }
        Ok(claims)
    }
}

/// Verify the self-contained Ed25519 envelope before contacting its issuer.
/// This proves that all invitation claims came from the returned node key; a
/// transport still has to pin its authenticated channel to that same key.
pub fn verify_portable_invitation(
    token: &str,
    now_ms: u64,
) -> Result<(BrainInvitationClaims, [u8; 32])> {
    let mut parts = token.split('.');
    let prefix = parts.next().unwrap_or_default();
    let public_key = parts
        .next()
        .context("Brain invitation node key is missing")?;
    let payload = parts
        .next()
        .context("Brain invitation payload is missing")?;
    let signature = parts
        .next()
        .context("Brain invitation signature is missing")?;
    if prefix != INVITATION_PREFIX || parts.next().is_some() {
        anyhow::bail!("Brain invitation envelope is invalid");
    }
    let public_key: [u8; 32] = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(public_key)
        .context("Brain invitation node key is invalid")?
        .try_into()
        .map_err(|key: Vec<u8>| {
            anyhow::anyhow!(
                "Brain invitation node key has {} bytes, expected 32",
                key.len()
            )
        })?;
    let supplied: [u8; 64] = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature)
        .context("Brain invitation signature is invalid")?
        .try_into()
        .map_err(|signature: Vec<u8>| {
            anyhow::anyhow!(
                "Brain invitation signature has {} bytes, expected 64",
                signature.len()
            )
        })?;
    crate::node::identity::NodeSigningIdentity::verify(
        public_key,
        invitation_signature_message(payload).as_slice(),
        supplied,
    )
    .context("Brain invitation signature does not match")?;

    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .context("Brain invitation payload is invalid")?;
    let claims: BrainInvitationClaims =
        serde_json::from_slice(&encoded).context("Brain invitation claims are invalid")?;
    if claims.version != CREDENTIAL_VERSION {
        anyhow::bail!("unsupported Brain invitation version {}", claims.version);
    }
    if now_ms < claims.issued_ms {
        anyhow::bail!("Brain invitation is not valid yet");
    }
    if now_ms >= claims.expires_ms {
        anyhow::bail!("Brain invitation has expired");
    }
    if claims.role == AttachmentRole::Runner
        || claims.scopes.is_empty()
        || !claims
            .scopes
            .is_subset(&permitted_participant_scopes(claims.role))
        || !claims.scopes.contains(&BrainCredentialScope::BrainAttach)
    {
        anyhow::bail!("Brain invitation claims have invalid participant authority");
    }
    invitation_tls_certificate_der(&claims)?;
    Ok((claims, public_key))
}

pub fn invitation_tls_certificate_der(claims: &BrainInvitationClaims) -> Result<Vec<u8>> {
    let certificate = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&claims.tls_certificate_der)
        .context("Brain invitation TLS certificate is invalid")?;
    if certificate.is_empty() {
        anyhow::bail!("Brain invitation TLS certificate is empty");
    }
    Ok(certificate)
}

fn invitation_signature_message(payload: &str) -> Vec<u8> {
    let mut message = Vec::with_capacity(INVITATION_PREFIX.len() + 1 + payload.len());
    message.extend_from_slice(INVITATION_PREFIX.as_bytes());
    message.push(0);
    message.extend_from_slice(payload.as_bytes());
    message
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

fn invitation_credential_claims(
    invitation: &BrainInvitationClaims,
    subject: &str,
) -> BrainCredentialClaims {
    BrainCredentialClaims {
        version: CREDENTIAL_VERSION,
        // The invitation ID is already a random, domain-separated authority
        // identity. Reusing it here makes retry reconstruction deterministic
        // without persisting a bearer token at rest.
        credential_id: invitation.invitation_id,
        issuer: invitation.issuer.clone(),
        subject: subject.to_string(),
        brain_id: invitation.brain_id,
        brain: invitation.brain.clone(),
        environment_generation: invitation.environment_generation,
        role: invitation.role,
        scopes: invitation.scopes.clone(),
        attachment_id: None,
        connection_id: None,
        delegation_chain: invitation.delegation_chain.clone(),
        issued_ms: invitation.issued_ms,
        expires_ms: invitation.expires_ms,
    }
}

fn load_consumed_invitations(path: &Path) -> Result<ConsumedInvitations> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let file: ConsumedInvitationFile = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?;
            if file.version != CREDENTIAL_VERSION {
                anyhow::bail!(
                    "unsupported consumed Brain invitation version {}",
                    file.version
                );
            }
            Ok(ConsumedInvitations {
                legacy_burned: file.invitation_ids,
                redemptions: file.redemptions,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ConsumedInvitations::default())
        }
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn persist_consumed_invitations(path: &Path, consumed: &ConsumedInvitations) -> Result<()> {
    let file = ConsumedInvitationFile {
        version: CREDENTIAL_VERSION,
        invitation_ids: consumed.legacy_burned.clone(),
        redemptions: consumed.redemptions.clone(),
    };
    let encoded = serde_json::to_vec_pretty(&file)?;
    let parent = path
        .parent()
        .context("consumed Brain invitation path has no parent")?;
    let temporary = parent.join(format!(
        ".brain-invitation-consumed.{}.tmp",
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

    fn invitation_request(ttl_ms: u64) -> BrainInvitationRequest {
        BrainInvitationRequest {
            issuer: "machine.local".into(),
            brain_id: BrainId(uuid::Uuid::new_v4()),
            brain: "shared-work".into(),
            environment_generation: 7,
            role: AttachmentRole::Consultant,
            scopes: default_participant_scopes(AttachmentRole::Consultant),
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
        credential_request
            .scopes
            .insert(BrainCredentialScope::BrainDetach);
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
        assert!(bound.permits(BrainCredentialScope::BrainDetach));
        assert!(bound
            .require_attachment(AttachmentId(uuid::Uuid::new_v4()), connection_id,)
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
        parent_request
            .scopes
            .insert(BrainCredentialScope::BrainControl);
        parent_request
            .scopes
            .insert(BrainCredentialScope::BrainApprove);
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

    #[test]
    fn invitation_redeems_once_into_an_ordinary_scoped_credential() {
        let authority = BrainCredentialAuthority::ephemeral([11; 32]);
        let (invitation, invitation_claims) = authority
            .issue_invitation(invitation_request(1_000), 10_000)
            .unwrap();
        let (portable_claims, issuer_key) =
            verify_portable_invitation(&invitation, 10_100).unwrap();
        assert_eq!(portable_claims, invitation_claims);
        assert_eq!(
            issuer_key,
            crate::node::identity::NodeSigningIdentity::from_secret([11; 32]).public_key_bytes()
        );
        assert_eq!(
            authority.inspect_invitation(&invitation, 10_100).unwrap(),
            invitation_claims
        );

        let (credential, claims) = authority
            .redeem_invitation(&invitation, "bob@desktop.local", 10_100)
            .unwrap();
        assert_eq!(claims.subject, "bob@desktop.local");
        assert_eq!(claims.role, AttachmentRole::Consultant);
        assert_eq!(claims.scopes, invitation_claims.scopes);
        assert_eq!(claims.expires_ms, invitation_claims.expires_ms);
        assert_eq!(authority.verify(&credential, 10_200).unwrap(), claims);
        assert!(authority.inspect_invitation(&invitation, 10_200).is_err());
        let (retried, retried_claims) = authority
            .redeem_invitation(&invitation, "bob@desktop.local", 10_200)
            .unwrap();
        assert_eq!(retried, credential);
        assert_eq!(retried_claims, claims);
        assert!(authority
            .redeem_invitation(&invitation, "mallory@box.local", 10_200)
            .is_err());
    }

    #[test]
    fn invitation_consumption_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let authority = BrainCredentialAuthority::load_or_create(temp.path()).unwrap();
        let (invitation, _) = authority
            .issue_invitation(invitation_request(10_000), 10_000)
            .unwrap();
        let (credential, claims) = authority
            .redeem_invitation(&invitation, "bob@desktop.local", 10_100)
            .unwrap();

        let restarted = BrainCredentialAuthority::load_or_create(temp.path()).unwrap();
        let (retried, retried_claims) = restarted
            .redeem_invitation(&invitation, "bob@desktop.local", 10_200)
            .unwrap();
        assert_eq!(retried, credential);
        assert_eq!(retried_claims, claims);
        assert!(restarted
            .redeem_invitation(&invitation, "mallory@box.local", 10_200)
            .is_err());
    }

    #[test]
    fn concurrent_invitation_replay_mints_exactly_one_credential() {
        let authority = BrainCredentialAuthority::ephemeral([12; 32]);
        let (invitation, _) = authority
            .issue_invitation(invitation_request(10_000), 10_000)
            .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let attempts = ["bob@one.local", "carol@two.local"]
            .into_iter()
            .map(|subject| {
                let authority = authority.clone();
                let invitation = invitation.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    authority.redeem_invitation(&invitation, subject, 10_100)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        assert_eq!(
            attempts
                .into_iter()
                .map(|attempt| attempt.join().unwrap())
                .filter(Result::is_ok)
                .count(),
            1
        );
    }

    #[test]
    fn invitation_tampering_expiry_and_invalid_role_fail_closed() {
        let authority = BrainCredentialAuthority::ephemeral([13; 32]);
        let (mut invitation, _) = authority
            .issue_invitation(invitation_request(1_000), 10_000)
            .unwrap();
        let index = invitation.find('.').unwrap() + 2;
        invitation.replace_range(index..=index, "x");
        assert!(authority.inspect_invitation(&invitation, 10_100).is_err());

        let (expired, _) = authority
            .issue_invitation(invitation_request(1_000), 10_000)
            .unwrap();
        assert!(authority.inspect_invitation(&expired, 11_000).is_err());

        let mut runner = invitation_request(1_000);
        runner.role = AttachmentRole::Runner;
        runner.scopes = [BrainCredentialScope::BrainAttach].into_iter().collect();
        assert!(authority.issue_invitation(runner, 10_000).is_err());

        let foreign = BrainCredentialAuthority::ephemeral([99; 32]);
        let (foreign_invitation, _) = foreign
            .issue_invitation(invitation_request(1_000), 10_000)
            .unwrap();
        assert!(authority
            .inspect_invitation(&foreign_invitation, 10_100)
            .is_err());
        assert!(verify_portable_invitation(&foreign_invitation, 10_100).is_ok());
    }

    #[test]
    fn revoking_inviter_invalidates_invitation_and_redeemed_descendant() {
        let authority = BrainCredentialAuthority::ephemeral([14; 32]);
        let mut controller_request = request(10_000);
        controller_request.scopes = default_participant_scopes(AttachmentRole::Driver);
        controller_request
            .scopes
            .insert(BrainCredentialScope::BrainControl);
        let controller_token = authority.issue(controller_request, 10_000).unwrap();
        let controller = authority.verify(&controller_token, 10_100).unwrap();

        let mut invited = invitation_request(5_000);
        invited.brain_id = controller.brain_id;
        invited.brain = controller.brain.clone();
        invited.environment_generation = controller.environment_generation;
        invited.delegation_chain = controller
            .attenuate(&invited.scopes, invited.ttl_ms, 10_100)
            .unwrap();
        let (invitation, _) = authority.issue_invitation(invited.clone(), 10_100).unwrap();
        let (credential, _) = authority
            .redeem_invitation(&invitation, "bob@desktop.local", 10_200)
            .unwrap();

        let (unredeemed, _) = authority.issue_invitation(invited, 10_200).unwrap();
        authority.revoke(controller.credential_id).unwrap();
        assert!(authority.inspect_invitation(&unredeemed, 10_300).is_err());
        assert!(authority.verify(&credential, 10_300).is_err());
    }
}
