//! Client for attaching a Finch TUI to a named brain on another daemon.

use anyhow::{Context, Result};
use fs2::FileExt;
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::store::{
    AttachmentId, AttachmentRole, BrainAttachment, BrainEnvironment, BrainEventKind, BrainId,
    BrainSnapshot, BrainWireMessage,
};

pub const DEFAULT_BRAIN_PORT: u16 = crate::config::constants::DEFAULT_BRAIN_TLS_PORT;
const ATTACHMENT_IDENTITIES_VERSION: u32 = 1;

/// Dynamic node information returned only after Brain-scoped authentication.
/// The audience fields prevent a client from accepting capabilities obtained
/// through a different Brain or environment generation on the same node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteBrainCapabilities {
    pub schema_version: u32,
    pub brain_id: BrainId,
    pub brain: String,
    pub environment: BrainEnvironment,
    /// Hex-encoded Ed25519 node identity used to sign invitations.
    pub node_public_key: String,
    pub node: crate::node::NodeCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AttachmentIdentityRecord {
    brain_id: BrainId,
    client_slot: String,
    subject: String,
    role: AttachmentRole,
    attachment_id: AttachmentId,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AttachmentIdentityFile {
    version: u32,
    entries: Vec<AttachmentIdentityRecord>,
}

#[derive(Debug, Clone)]
struct AttachmentIdentityStore {
    path: PathBuf,
}

impl AttachmentIdentityStore {
    fn default_path() -> Result<PathBuf> {
        dirs::home_dir()
            .map(|home| home.join(".finch").join("brain-attachments.json"))
            .context("cannot persist Brain attachment identity without a home directory")
    }

    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> Result<AttachmentIdentityFile> {
        if !self.path.exists() {
            return Ok(AttachmentIdentityFile {
                version: ATTACHMENT_IDENTITIES_VERSION,
                entries: Vec::new(),
            });
        }
        let bytes =
            std::fs::read(&self.path).with_context(|| format!("read {}", self.path.display()))?;
        let file: AttachmentIdentityFile = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", self.path.display()))?;
        if file.version != ATTACHMENT_IDENTITIES_VERSION {
            anyhow::bail!(
                "unsupported Brain attachment identity version {}",
                file.version
            );
        }
        Ok(file)
    }

    fn find(
        &self,
        brain_id: BrainId,
        client_slot: &str,
        subject: &str,
        role: AttachmentRole,
    ) -> Result<Option<AttachmentId>> {
        Ok(self
            .load()?
            .entries
            .into_iter()
            .find(|entry| {
                entry.brain_id == brain_id
                    && entry.client_slot == client_slot
                    && entry.subject == subject
                    && entry.role == role
            })
            .map(|entry| entry.attachment_id))
    }

    fn save(&self, record: AttachmentIdentityRecord) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("Brain attachment identity path has no parent")?;
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        let lock_path = parent.join("brain-attachments.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        lock.lock_exclusive()
            .with_context(|| format!("lock {}", lock_path.display()))?;

        let mut identities = self.load()?;
        identities.entries.retain(|entry| {
            !(entry.brain_id == record.brain_id
                && entry.client_slot == record.client_slot
                && entry.subject == record.subject
                && entry.role == record.role)
        });
        identities.entries.push(record);
        let encoded = serde_json::to_vec_pretty(&identities)?;
        let temporary = parent.join(format!(".brain-attachments.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, encoded)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::rename(&temporary, &self.path)
            .with_context(|| format!("commit {}", self.path.display()))?;
        lock.unlock()
            .with_context(|| format!("unlock {}", lock_path.display()))?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBrainTarget {
    pub brain: String,
    pub machine: String,
    pub address: String,
    pub secure: bool,
}

impl RemoteBrainTarget {
    pub fn parse(value: &str) -> Result<Self> {
        let (brain, host) = value
            .trim()
            .split_once('@')
            .context("brain target must be NAME@MACHINE[:PORT]")?;
        super::store::BrainStore::validate_name(brain)?;
        let host = host.trim();
        if host.is_empty() || host.contains('/') || host.contains(char::is_whitespace) {
            anyhow::bail!("brain machine must be a hostname or host:port");
        }
        let address = if has_explicit_port(host) {
            host.to_string()
        } else {
            format!("{host}:{DEFAULT_BRAIN_PORT}")
        };
        let machine = host
            .rsplit_once(':')
            .filter(|(_, port)| port.parse::<u16>().is_ok())
            .map(|(host, _)| host)
            .unwrap_or(host)
            .to_string();
        Ok(Self {
            brain: brain.to_string(),
            machine,
            address,
            secure: true,
        })
    }

    pub fn display_name(&self) -> String {
        format!("{}@{}", self.brain, self.machine)
    }

    /// Build the endpoint printed with an invitation minted through the local
    /// plaintext daemon. The daemon bind address is intentionally *not* a
    /// recipient address: it is commonly `0.0.0.0` while the administration
    /// endpoint is loopback-only. Invitations must name the TLS certificate's
    /// machine hostname and the configured collaboration-listener port.
    pub fn invitation_recipient(
        brain: &str,
        certificate_hostname: &str,
        brain_bind_address: &str,
    ) -> Result<Self> {
        let bind: std::net::SocketAddr = brain_bind_address
            .parse()
            .with_context(|| format!("invalid Brain TLS bind address '{brain_bind_address}'"))?;
        anyhow::ensure!(bind.port() != 0, "Brain TLS listener has no stable port");

        let hostname = certificate_hostname.trim().trim_end_matches('.');
        anyhow::ensure!(
            !hostname.is_empty()
                && hostname.parse::<std::net::IpAddr>().is_err()
                && !hostname.contains('/')
                && !hostname.contains(char::is_whitespace),
            "Brain TLS certificate has no recipient-reachable hostname"
        );
        let hostname = if hostname.contains('.') {
            hostname.to_string()
        } else {
            format!("{hostname}.local")
        };
        Self::parse(&format!("{brain}@{hostname}:{}", bind.port()))
    }

    /// Exact target spelling suitable for `/brain join`. Unlike
    /// `display_name`, this always preserves a configured non-default port.
    pub fn command_target(&self) -> String {
        format!("{}@{}", self.brain, self.address)
    }

    /// Resolve a bare Brain name through the already-connected local daemon.
    pub fn local(brain: &str, daemon_base_url: &str) -> Result<Self> {
        let address = daemon_base_url
            .trim()
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/');
        if address.is_empty() || address.contains('/') {
            anyhow::bail!("local daemon address is invalid");
        }
        let mut target = Self::parse(&format!("{brain}@{address}"))?;
        target.secure = false;
        Ok(target)
    }

    fn http_scheme(&self) -> &'static str {
        if self.secure {
            "https"
        } else {
            "http"
        }
    }

    fn websocket_scheme(&self) -> &'static str {
        if self.secure {
            "wss"
        } else {
            "ws"
        }
    }

    fn http_url(&self) -> String {
        format!(
            "{}://{}/v1/brains/named/{}",
            self.http_scheme(),
            self.address,
            self.brain
        )
    }

    fn collection_url(&self) -> String {
        format!("{}://{}/v1/brains/named", self.http_scheme(), self.address)
    }

    fn attachments_url(&self) -> String {
        format!("{}/attachments", self.http_url())
    }

    fn capabilities_url(&self) -> String {
        format!("{}/capabilities", self.http_url())
    }

    fn credentials_url(&self) -> String {
        format!("{}/credentials", self.http_url())
    }

    fn invitations_url(&self) -> String {
        format!("{}/invitations", self.http_url())
    }

    fn delegated_credential_url(&self, credential_id: uuid::Uuid) -> String {
        format!("{}/credentials/{credential_id}", self.http_url())
    }

    fn invitation_redemption_url(&self) -> String {
        format!(
            "{}://{}/v1/brains/invitations/redeem",
            self.http_scheme(),
            self.address
        )
    }

    fn ws_url(&self, attachment: &BrainAttachment) -> Result<String> {
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        Ok(format!(
            "{}://{}/v1/brains/named/{}/ws?attachment_id={}&connection_id={}",
            self.websocket_scheme(),
            self.address,
            self.brain,
            attachment.attachment_id.0,
            connection_id.0
        ))
    }
}

fn has_explicit_port(host: &str) -> bool {
    host.rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
}

fn target_address_is_loopback(address: &str) -> bool {
    if let Ok(socket) = address.parse::<std::net::SocketAddr>() {
        return socket.ip().is_loopback();
    }
    address
        .rsplit_once(':')
        .map(|(host, port)| port.parse::<u16>().is_ok() && host.eq_ignore_ascii_case("localhost"))
        .unwrap_or_else(|| address.eq_ignore_ascii_case("localhost"))
}

fn issued_interval_is_valid(
    issued_ms: u64,
    expires_ms: u64,
    requested_ttl_ms: Option<u64>,
    endpoint_default_ttl_ms: u64,
    now_ms: u64,
) -> bool {
    let maximum_ttl = requested_ttl_ms.unwrap_or(endpoint_default_ttl_ms);
    issued_ms <= now_ms.saturating_add(super::credential::MAX_SIGNED_CLOCK_SKEW_MS)
        && expires_ms.saturating_add(super::credential::MAX_SIGNED_CLOCK_SKEW_MS) > now_ms
        && expires_ms > issued_ms
        && expires_ms.saturating_sub(issued_ms) <= maximum_ttl
}

#[derive(Clone)]
pub struct RemoteBrainClient {
    pub target: RemoteBrainTarget,
    bootstrap: RemoteBrainBootstrap,
    credential: std::sync::Arc<tokio::sync::Mutex<Option<RemoteBrainCredential>>>,
    http: Client,
    websocket_connector: Option<tokio_tungstenite::Connector>,
    attachment: Option<BrainAttachment>,
    connection: std::sync::Arc<tokio::sync::Mutex<Option<RemoteBrainConnection>>>,
}

/// Caller-owned immutable identity for one durable mutation. Persist this
/// value before sending when retry must survive connection or process loss.
/// Bearer and connection authority are intentionally excluded and are freshly
/// validated on every attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainMutationHandle {
    pub idempotency_key: uuid::Uuid,
    pub brain_id: BrainId,
    pub attachment_id: AttachmentId,
    pub expected_revision: u64,
    pub environment_generation: u64,
    pub command_sha256: String,
}

#[derive(Clone)]
enum RemoteBrainBootstrap {
    Password(String),
    Invitation {
        token: String,
        node_public_key: [u8; 32],
    },
}

#[derive(Clone)]
struct RemoteBrainCredential {
    token: String,
    claims: super::credential::BrainCredentialClaims,
}

#[derive(Deserialize)]
struct IssuedBrainCredential {
    token: String,
    claims: super::credential::BrainCredentialClaims,
}

#[derive(Clone)]
struct RemoteBrainConnection {
    id: uuid::Uuid,
    commands: mpsc::UnboundedSender<RemoteBrainRequest>,
}

struct RemoteBrainRequest {
    kind: crate::ipc::brain_codec::BrainRemoteCommandKind,
    mutation: Option<crate::ipc::brain_codec::BrainRemoteMutation>,
    response:
        oneshot::Sender<std::result::Result<crate::ipc::brain_codec::BrainRemoteReply, String>>,
}

impl RemoteBrainClient {
    pub fn new(target: RemoteBrainTarget, password: impl Into<String>) -> Result<Self> {
        anyhow::ensure!(
            !target.secure,
            "remote Brain password bootstrap is disabled; join with a signed invitation"
        );
        anyhow::ensure!(
            target_address_is_loopback(&target.address),
            "Brain password bootstrap is restricted to a loopback address"
        );
        Ok(Self {
            target,
            bootstrap: RemoteBrainBootstrap::Password(password.into()),
            credential: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(180))
                .build()?,
            websocket_connector: None,
            attachment: None,
            connection: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    pub fn new_with_invitation(
        target: RemoteBrainTarget,
        invitation: impl Into<String>,
    ) -> Result<Self> {
        let invitation = invitation.into();
        let (claims, node_public_key) =
            super::credential::verify_portable_invitation(&invitation, unix_epoch_millis())?;
        if claims.brain != target.brain {
            anyhow::bail!("Brain invitation names a different Brain target");
        }
        let certificate_der = super::credential::invitation_tls_certificate_der(&claims)?;
        let (http, websocket_connector) = if target.secure {
            let certificate = reqwest::Certificate::from_der(&certificate_der)
                .context("Brain invitation contains an invalid TLS certificate")?;
            let http = Client::builder()
                .timeout(std::time::Duration::from_secs(180))
                .redirect(reqwest::redirect::Policy::none())
                .tls_built_in_root_certs(false)
                .add_root_certificate(certificate)
                .build()?;
            let mut roots = rustls::RootCertStore::empty();
            roots
                .add(rustls::pki_types::CertificateDer::from(certificate_der))
                .context("Brain invitation TLS certificate cannot be trusted")?;
            let tls = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            (
                http,
                Some(tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(
                    tls,
                ))),
            )
        } else {
            (
                Client::builder()
                    .timeout(std::time::Duration::from_secs(180))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()?,
                None,
            )
        };
        Ok(Self {
            target,
            bootstrap: RemoteBrainBootstrap::Invitation {
                token: invitation,
                node_public_key,
            },
            credential: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            http,
            websocket_connector,
            attachment: None,
            connection: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    pub fn attachment(&self) -> Option<&BrainAttachment> {
        self.attachment.as_ref()
    }

    pub fn invited_node_public_key(&self) -> Option<[u8; 32]> {
        match &self.bootstrap {
            RemoteBrainBootstrap::Invitation {
                node_public_key, ..
            } => Some(*node_public_key),
            RemoteBrainBootstrap::Password(_) => None,
        }
    }

    /// Explicitly create this target alias in the remote daemon's own
    /// environment. The request deliberately contains no machine/workspace.
    pub async fn create(&self) -> Result<BrainSnapshot> {
        #[derive(Serialize)]
        struct Create<'a> {
            name: &'a str,
        }

        self.http
            .post(self.target.collection_url())
            .bearer_auth(self.bootstrap_password()?)
            .json(&Create {
                name: &self.target.brain,
            })
            .send()
            .await
            .context("could not reach brain host")?
            .error_for_status()
            .context("remote Brain creation rejected")?
            .json()
            .await
            .context("invalid created Brain snapshot")
    }

    /// Create a short-lived, single-participant invitation without exposing
    /// the daemon bootstrap password to the recipient.
    pub async fn issue_invitation(
        &self,
        role: AttachmentRole,
        ttl_ms: Option<u64>,
    ) -> Result<(String, super::credential::BrainInvitationClaims)> {
        self.issue_invitation_with_scopes(role, None, ttl_ms).await
    }

    /// Delegate an explicitly attenuated participant invitation. On the
    /// restricted TLS transport this uses the client's already redeemed,
    /// unbound `brain:control` credential; the bootstrap password is never
    /// sent to a remote host.
    pub async fn issue_invitation_with_scopes(
        &self,
        role: AttachmentRole,
        scopes: Option<std::collections::BTreeSet<super::credential::BrainCredentialScope>>,
        ttl_ms: Option<u64>,
    ) -> Result<(String, super::credential::BrainInvitationClaims)> {
        #[derive(Serialize)]
        struct Issue {
            role: AttachmentRole,
            scopes: Option<std::collections::BTreeSet<super::credential::BrainCredentialScope>>,
            ttl_ms: Option<u64>,
        }
        #[derive(Deserialize)]
        struct Issued {
            invitation: String,
            claims: super::credential::BrainInvitationClaims,
        }

        let requested_scopes = scopes
            .clone()
            .unwrap_or_else(|| super::credential::default_participant_scopes(role));
        let (authorization, delegator) = self.delegation_authorization().await?;
        let issued = self
            .http
            .post(self.target.invitations_url())
            .bearer_auth(authorization)
            .json(&Issue {
                role,
                scopes: scopes.clone(),
                ttl_ms,
            })
            .send()
            .await
            .context("could not reach Brain invitation issuer")?
            .error_for_status()
            .context("Brain invitation request rejected")?
            .json::<Issued>()
            .await
            .context("invalid Brain invitation response")?;
        let response_received_ms = unix_epoch_millis();
        if issued.claims.brain != self.target.brain
            || issued.claims.role != role
            || issued.claims.scopes != requested_scopes
            || !issued_interval_is_valid(
                issued.claims.issued_ms,
                issued.claims.expires_ms,
                ttl_ms,
                15 * 60 * 1_000,
                response_received_ms,
            )
        {
            anyhow::bail!("Brain invitation issuer returned the wrong participant audience");
        }
        if delegator.as_ref().is_some_and(|parent| {
            let mut expected_ancestry = parent.delegation_chain.clone();
            expected_ancestry.push(parent.credential_id);
            issued.claims.brain_id != parent.brain_id
                || issued.claims.environment_generation != parent.environment_generation
                || issued.claims.expires_ms > parent.expires_ms
                || issued.claims.delegation_chain != expected_ancestry
        }) {
            anyhow::bail!("Brain invitation issuer returned invalid delegation ancestry");
        }
        let (portable_claims, _) =
            super::credential::verify_portable_invitation(&issued.invitation, unix_epoch_millis())?;
        if portable_claims != issued.claims {
            anyhow::bail!("Brain invitation envelope does not match the issuer response");
        }
        Ok((issued.invitation, issued.claims))
    }

    /// Mint an explicitly attenuated participant credential using an
    /// unbound `brain:control` credential already held by this client.
    pub async fn issue_credential(
        &self,
        subject: &str,
        role: AttachmentRole,
        scopes: std::collections::BTreeSet<super::credential::BrainCredentialScope>,
        ttl_ms: Option<u64>,
    ) -> Result<(String, super::credential::BrainCredentialClaims)> {
        #[derive(Serialize)]
        struct Issue<'a> {
            subject: &'a str,
            role: AttachmentRole,
            scopes: std::collections::BTreeSet<super::credential::BrainCredentialScope>,
            ttl_ms: Option<u64>,
        }

        let (authorization, delegator) = self.delegation_authorization().await?;
        let issued = self
            .http
            .post(self.target.credentials_url())
            .bearer_auth(authorization)
            .json(&Issue {
                subject,
                role,
                scopes: scopes.clone(),
                ttl_ms,
            })
            .send()
            .await
            .context("could not reach Brain credential issuer")?
            .error_for_status()
            .context("Brain credential request rejected")?
            .json::<IssuedBrainCredential>()
            .await
            .context("invalid Brain credential response")?;
        let response_received_ms = unix_epoch_millis();
        let envelope_claims =
            super::credential::decode_unverified_credential_claims(&issued.token)?;
        if issued.claims.subject != subject
            || issued.claims.role != role
            || issued.claims.brain != self.target.brain
            || issued.claims.scopes != scopes
            || issued.claims.attachment_id.is_some()
            || issued.claims.connection_id.is_some()
            || issued.claims != envelope_claims
            || !issued_interval_is_valid(
                issued.claims.issued_ms,
                issued.claims.expires_ms,
                ttl_ms,
                8 * 60 * 60 * 1_000,
                response_received_ms,
            )
        {
            anyhow::bail!("Brain credential issuer returned the wrong participant audience");
        }
        if delegator.as_ref().is_some_and(|parent| {
            let mut expected_ancestry = parent.delegation_chain.clone();
            expected_ancestry.push(parent.credential_id);
            issued.claims.brain_id != parent.brain_id
                || issued.claims.environment_generation != parent.environment_generation
                || issued.claims.expires_ms > parent.expires_ms
                || issued.claims.delegation_chain != expected_ancestry
        }) {
            anyhow::bail!("Brain credential issuer returned invalid delegation ancestry");
        }
        Ok((issued.token, issued.claims))
    }

    /// Revoke a credential descended from this client's unbound controlling
    /// credential. The server validates the signed target token and refuses
    /// sibling, ancestor, self, and cross-audience revocation.
    pub async fn revoke_delegated_credential(&self, credential: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Revoke<'a> {
            credential: Option<&'a str>,
            invitation: Option<&'a str>,
        }

        let target = super::credential::decode_unverified_credential_claims(credential)?;
        let (authorization, delegator) = self.delegation_authorization().await?;
        let delegator =
            delegator.context("delegated revocation requires a scoped controlling credential")?;
        anyhow::ensure!(
            target.brain_id == delegator.brain_id
                && target.brain == delegator.brain
                && target.environment_generation == delegator.environment_generation
                && target.delegation_chain.contains(&delegator.credential_id),
            "a controlling credential may revoke only its own descendants"
        );
        self.http
            .delete(self.target.delegated_credential_url(target.credential_id))
            .bearer_auth(authorization)
            .json(&Revoke {
                credential: Some(credential),
                invitation: None,
            })
            .send()
            .await
            .context("could not reach Brain credential revocation endpoint")?
            .error_for_status()
            .context("Brain credential revocation rejected")?;
        Ok(())
    }

    /// Revoke a signed invitation descended from this controller. The stable
    /// invitation ID is also the redeemed credential ID, so the same action
    /// works before redemption and invalidates an already redeemed bearer.
    pub async fn revoke_delegated_invitation(&self, invitation: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Revoke<'a> {
            credential: Option<&'a str>,
            invitation: Option<&'a str>,
        }

        let (target, _) =
            super::credential::verify_portable_invitation(invitation, unix_epoch_millis())?;
        let (authorization, delegator) = self.delegation_authorization().await?;
        let delegator =
            delegator.context("delegated revocation requires a scoped controlling credential")?;
        anyhow::ensure!(
            target.brain_id == delegator.brain_id
                && target.brain == delegator.brain
                && target.environment_generation == delegator.environment_generation
                && target.delegation_chain.contains(&delegator.credential_id),
            "a controlling credential may revoke only its own descendants"
        );
        self.http
            .delete(self.target.delegated_credential_url(target.invitation_id))
            .bearer_auth(authorization)
            .json(&Revoke {
                credential: None,
                invitation: Some(invitation),
            })
            .send()
            .await
            .context("could not reach Brain invitation revocation endpoint")?
            .error_for_status()
            .context("Brain invitation revocation rejected")?;
        Ok(())
    }

    /// Redeem this client's invitation and attach using the role and scopes
    /// fixed by its issuer. The invitation is exchanged for the same ordinary
    /// scoped credential used by password-bootstrapped clients.
    pub async fn attach_invited_persistent(
        &mut self,
        subject: &str,
        client_slot: &str,
    ) -> Result<(AttachmentRole, BrainAttachment)> {
        let role = self.redeem_invitation(subject).await?;
        let attachment = self.attach_persistent(subject, role, client_slot).await?;
        Ok((role, attachment))
    }

    /// Redeem the signed invitation into an unbound scoped credential without
    /// creating an attachment. Controllers can use this explicit phase to
    /// delegate narrower authority or perform Brain-scoped administration.
    pub async fn redeem_invitation(&self, subject: &str) -> Result<AttachmentRole> {
        let mut credential = self.credential.lock().await;
        if let Some(existing) = credential.as_ref() {
            if existing.claims.subject == subject
                && existing.claims.brain == self.target.brain
                && existing.claims.expires_ms > unix_epoch_millis()
            {
                return Ok(existing.claims.role);
            }
        }
        let issued = self.request_invitation_credential(subject).await?;
        let role = issued.claims.role;
        *credential = Some(issued);
        Ok(role)
    }

    async fn request_invitation_credential(&self, subject: &str) -> Result<RemoteBrainCredential> {
        #[derive(Serialize)]
        struct Redeem<'a> {
            invitation: &'a str,
            subject: &'a str,
        }

        let RemoteBrainBootstrap::Invitation {
            token: invitation, ..
        } = &self.bootstrap
        else {
            anyhow::bail!("this Brain client was not created with an invitation");
        };
        let issued = self
            .http
            .post(self.target.invitation_redemption_url())
            .json(&Redeem {
                invitation,
                subject,
            })
            .send()
            .await
            .with_context(|| {
                format!(
                    "could not reach Brain invitation redemption endpoint at {}",
                    self.target.address
                )
            })?
            .error_for_status()
            .context("Brain invitation redemption rejected")?
            .json::<IssuedBrainCredential>()
            .await
            .context("invalid Brain invitation redemption response")?;
        if issued.claims.subject != subject
            || issued.claims.brain != self.target.brain
            || issued.claims.role == AttachmentRole::Runner
            || !issued
                .claims
                .permits(super::credential::BrainCredentialScope::BrainAttach)
            || issued.claims.attachment_id.is_some()
            || issued.claims.connection_id.is_some()
        {
            anyhow::bail!("Brain invitation returned the wrong participant audience");
        }
        Ok(RemoteBrainCredential {
            token: issued.token,
            claims: issued.claims,
        })
    }

    /// Verify that an invitation's advertised TLS endpoint is reachable and
    /// presents the exact certificate embedded in the invitation, without
    /// consuming the single-participant token. The redemption route accepts
    /// POST only, so a method-mismatch response proves that the intended route
    /// was reached after DNS, TCP, and TLS validation.
    pub async fn probe_invitation_endpoint(&self) -> Result<()> {
        anyhow::ensure!(
            self.target.secure
                && matches!(&self.bootstrap, RemoteBrainBootstrap::Invitation { .. }),
            "invitation endpoint probes require a signed invitation client"
        );
        let response = self
            .http
            .get(self.target.invitation_redemption_url())
            .send()
            .await
            .with_context(|| {
                format!(
                    "could not reach certificate-valid Brain TLS listener at {}",
                    self.target.address
                )
            })?;
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED,
            "Brain TLS listener at {} does not expose invitation redemption (HTTP {})",
            self.target.address,
            response.status()
        );
        Ok(())
    }

    pub async fn attach(
        &mut self,
        subject: &str,
        role: AttachmentRole,
        attachment_id: Option<AttachmentId>,
    ) -> Result<BrainAttachment> {
        #[derive(Serialize)]
        struct Attach<'a> {
            subject: &'a str,
            role: AttachmentRole,
            attachment_id: Option<AttachmentId>,
        }
        #[derive(Deserialize)]
        struct Attached {
            attachment: BrainAttachment,
            token: String,
            claims: super::credential::BrainCredentialClaims,
        }

        self.ensure_credential(subject, role).await?;
        let token = self.authorized_token().await?;
        let attached = self
            .http
            .post(self.target.attachments_url())
            .bearer_auth(token)
            .json(&Attach {
                subject,
                role,
                attachment_id,
            })
            .send()
            .await
            .context("could not reach brain host")?
            .error_for_status()
            .context("brain attachment rejected")?
            .json::<Attached>()
            .await
            .context("invalid brain attachment")?;
        let connection_id = attached
            .attachment
            .connection_id
            .context("remote Brain attachment omitted its pending connection")?;
        attached
            .claims
            .require_participant(subject, role)
            .and_then(|()| {
                attached
                    .claims
                    .require_attachment(attached.attachment.attachment_id, connection_id)
            })
            .context("remote Brain returned the wrong attachment credential")?;
        *self.credential.lock().await = Some(RemoteBrainCredential {
            token: attached.token,
            claims: attached.claims,
        });
        let attachment = attached.attachment;
        self.attachment = Some(attachment.clone());
        Ok(attachment)
    }

    /// Reuse this console slot's daemon-owned attachment identity across
    /// frontend restarts. The local file stores only an opaque ID; the daemon
    /// remains authoritative for role, cursor, connection state, and whether
    /// the ID may be rebound.
    pub async fn attach_persistent(
        &mut self,
        subject: &str,
        role: AttachmentRole,
        client_slot: &str,
    ) -> Result<BrainAttachment> {
        self.ensure_credential(subject, role).await?;
        let snapshot = self.snapshot().await?;
        let store = AttachmentIdentityStore::new(AttachmentIdentityStore::default_path()?);
        let attachment_id = store.find(snapshot.brain_id, client_slot, subject, role)?;
        let attachment = self
            .attach(subject, role, attachment_id)
            .await
            .context("persistent Brain attachment rejected")?;
        if let Err(error) = store.save(AttachmentIdentityRecord {
            brain_id: snapshot.brain_id,
            client_slot: client_slot.to_string(),
            subject: subject.to_string(),
            role,
            attachment_id: attachment.attachment_id,
        }) {
            let _ = self.disconnect().await;
            return Err(error.context("persist Brain attachment identity"));
        }
        Ok(attachment)
    }

    pub async fn snapshot(&self) -> Result<BrainSnapshot> {
        self.http
            .get(self.target.http_url())
            .bearer_auth(self.authorized_token().await?)
            .send()
            .await
            .context("could not reach brain host")?
            .error_for_status()
            .context("brain attach rejected")?
            .json()
            .await
            .context("invalid brain snapshot")
    }

    /// Retrieve live node/model availability after authenticating to the exact
    /// Brain audience named by this client's scoped credential.
    pub async fn capabilities(&self) -> Result<RemoteBrainCapabilities> {
        let credential = self
            .credential
            .lock()
            .await
            .clone()
            .context("client has not bootstrapped a scoped Brain credential")?;
        if credential.claims.expires_ms <= unix_epoch_millis() {
            anyhow::bail!("scoped Brain credential expired; reconnect the attachment");
        }
        let capabilities = self
            .http
            .get(self.target.capabilities_url())
            .bearer_auth(&credential.token)
            .send()
            .await
            .context("could not reach Brain capability endpoint")?
            .error_for_status()
            .context("Brain capability query rejected")?
            .json::<RemoteBrainCapabilities>()
            .await
            .context("invalid Brain capability response")?;
        validate_remote_capabilities(
            &capabilities,
            &credential.claims,
            self.invited_node_public_key(),
        )?;
        Ok(capabilities)
    }

    pub async fn push(&self, kind: BrainEventKind) -> Result<()> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};

        match self
            .send_remote_command(BrainRemoteCommandKind::Submit(kind))
            .await?
        {
            BrainRemoteReply::Submitted { .. } => Ok(()),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    pub async fn start_speculative(&self, prompt: String) -> Result<super::store::BrainRun> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};

        match self
            .send_remote_command(BrainRemoteCommandKind::Submit(
                BrainEventKind::SpeculativePrompt { text: prompt },
            ))
            .await?
        {
            BrainRemoteReply::Submitted { run: Some(run), .. } => Ok(run),
            BrainRemoteReply::Submitted { .. } => {
                anyhow::bail!("speculative Brain submission did not create a run")
            }
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    pub async fn prepare_push_mutation(
        &self,
        kind: &BrainEventKind,
    ) -> Result<BrainMutationHandle> {
        self.prepare_mutation(&crate::ipc::brain_codec::BrainRemoteCommandKind::Submit(
            kind.clone(),
        ))
        .await
    }

    /// Retry-safe submission using a caller-persisted immutable envelope.
    pub async fn push_with_handle(
        &self,
        kind: BrainEventKind,
        handle: &BrainMutationHandle,
    ) -> Result<()> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};
        match self
            .send_remote_command_with_handle(BrainRemoteCommandKind::Submit(kind), Some(handle))
            .await?
        {
            BrainRemoteReply::Submitted { .. } => Ok(()),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    /// Advance this live connection's projection cursor. This is deliberately
    /// outside durable mutation replay: a reconnect obtains fresh attachment
    /// state and may acknowledge that current connection again.
    pub async fn acknowledge(&mut self, seq: u64) -> Result<()> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};

        match self
            .send_remote_command(BrainRemoteCommandKind::Acknowledge(seq))
            .await?
        {
            BrainRemoteReply::Acknowledged { attachment, .. } => {
                self.attachment = Some(attachment);
                Ok(())
            }
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    /// Explicitly replace this client's ordinary participant credential with
    /// one that may request or cancel an addressed runner handoff. This must
    /// happen before opening the WebSocket because its authorization is bound
    /// for the lifetime of that connection.
    pub async fn authorize_runner_handoff_control(
        &self,
        subject: &str,
        role: AttachmentRole,
    ) -> Result<()> {
        if self.connection.lock().await.is_some() {
            anyhow::bail!(
                "disconnect the remote Brain event stream before changing control authority"
            );
        }
        let mut scopes = super::credential::default_participant_scopes(role);
        scopes.insert(super::credential::BrainCredentialScope::BrainControl);
        self.ensure_credential_with_scopes(subject, role, Some(scopes))
            .await
    }

    pub async fn request_runner_handoff(
        &self,
        target_subject: &str,
        expected_lease_id: super::store::RunnerLeaseId,
        environment_generation: u64,
        ttl_ms: u64,
    ) -> Result<super::store::BrainRunnerHandoff> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};

        match self
            .send_remote_command(BrainRemoteCommandKind::RequestRunnerHandoff {
                target_subject: target_subject.to_string(),
                expected_lease_id,
                environment_generation,
                ttl_ms,
            })
            .await?
        {
            BrainRemoteReply::HandoffRequested { handoff, .. } => Ok(handoff),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    pub async fn prepare_runner_handoff_mutation(
        &self,
        target_subject: &str,
        expected_lease_id: super::store::RunnerLeaseId,
        environment_generation: u64,
        ttl_ms: u64,
    ) -> Result<BrainMutationHandle> {
        self.prepare_mutation(
            &crate::ipc::brain_codec::BrainRemoteCommandKind::RequestRunnerHandoff {
                target_subject: target_subject.to_string(),
                expected_lease_id,
                environment_generation,
                ttl_ms,
            },
        )
        .await
    }

    pub async fn request_runner_handoff_with_handle(
        &self,
        target_subject: &str,
        expected_lease_id: super::store::RunnerLeaseId,
        environment_generation: u64,
        ttl_ms: u64,
        handle: &BrainMutationHandle,
    ) -> Result<super::store::BrainRunnerHandoff> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};
        match self
            .send_remote_command_with_handle(
                BrainRemoteCommandKind::RequestRunnerHandoff {
                    target_subject: target_subject.to_string(),
                    expected_lease_id,
                    environment_generation,
                    ttl_ms,
                },
                Some(handle),
            )
            .await?
        {
            BrainRemoteReply::HandoffRequested { handoff, .. } => Ok(handoff),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    pub async fn cancel_runner_handoff(
        &self,
        handoff_id: super::store::RunnerHandoffId,
    ) -> Result<()> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};

        match self
            .send_remote_command(BrainRemoteCommandKind::CancelRunnerHandoff(handoff_id))
            .await?
        {
            BrainRemoteReply::HandoffCancelled { .. } => Ok(()),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    pub async fn prepare_cancel_runner_handoff_mutation(
        &self,
        handoff_id: super::store::RunnerHandoffId,
    ) -> Result<BrainMutationHandle> {
        self.prepare_mutation(
            &crate::ipc::brain_codec::BrainRemoteCommandKind::CancelRunnerHandoff(handoff_id),
        )
        .await
    }

    pub async fn cancel_runner_handoff_with_handle(
        &self,
        handoff_id: super::store::RunnerHandoffId,
        handle: &BrainMutationHandle,
    ) -> Result<()> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};
        match self
            .send_remote_command_with_handle(
                BrainRemoteCommandKind::CancelRunnerHandoff(handoff_id),
                Some(handle),
            )
            .await?
        {
            BrainRemoteReply::HandoffCancelled { .. } => Ok(()),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    pub async fn cancel_run(&self, run_id: super::store::RunId) -> Result<super::store::BrainRun> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};

        match self
            .send_remote_command(BrainRemoteCommandKind::CancelRun(run_id))
            .await?
        {
            BrainRemoteReply::RunCancelled { run, .. } => Ok(run),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    pub async fn prepare_cancel_run_mutation(
        &self,
        run_id: super::store::RunId,
    ) -> Result<BrainMutationHandle> {
        self.prepare_mutation(&crate::ipc::brain_codec::BrainRemoteCommandKind::CancelRun(
            run_id,
        ))
        .await
    }

    pub async fn cancel_run_with_handle(
        &self,
        run_id: super::store::RunId,
        handle: &BrainMutationHandle,
    ) -> Result<super::store::BrainRun> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};
        match self
            .send_remote_command_with_handle(
                BrainRemoteCommandKind::CancelRun(run_id),
                Some(handle),
            )
            .await?
        {
            BrainRemoteReply::RunCancelled { run, .. } => Ok(run),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_schedule(
        &self,
        language: super::store::ProgramLanguage,
        source: String,
        grant_ceiling: crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: super::store::BrainScheduleDeliveryPolicy,
    ) -> Result<super::store::BrainSchedule> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};

        match self
            .send_remote_command(BrainRemoteCommandKind::CreateSchedule {
                language,
                source,
                grant_ceiling,
                next_due_ms,
                interval_ms,
                delivery_policy,
            })
            .await?
        {
            BrainRemoteReply::ScheduleCreated { schedule, .. } => Ok(schedule),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn prepare_create_schedule_mutation(
        &self,
        language: super::store::ProgramLanguage,
        source: &str,
        grant_ceiling: &crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: super::store::BrainScheduleDeliveryPolicy,
    ) -> Result<BrainMutationHandle> {
        self.prepare_mutation(
            &crate::ipc::brain_codec::BrainRemoteCommandKind::CreateSchedule {
                language,
                source: source.to_string(),
                grant_ceiling: grant_ceiling.clone(),
                next_due_ms,
                interval_ms,
                delivery_policy,
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_schedule_with_handle(
        &self,
        language: super::store::ProgramLanguage,
        source: String,
        grant_ceiling: crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: super::store::BrainScheduleDeliveryPolicy,
        handle: &BrainMutationHandle,
    ) -> Result<super::store::BrainSchedule> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};
        match self
            .send_remote_command_with_handle(
                BrainRemoteCommandKind::CreateSchedule {
                    language,
                    source,
                    grant_ceiling,
                    next_due_ms,
                    interval_ms,
                    delivery_policy,
                },
                Some(handle),
            )
            .await?
        {
            BrainRemoteReply::ScheduleCreated { schedule, .. } => Ok(schedule),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    pub async fn cancel_schedule(&self, schedule_id: super::store::ScheduleId) -> Result<bool> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};

        match self
            .send_remote_command(BrainRemoteCommandKind::CancelSchedule(schedule_id))
            .await?
        {
            BrainRemoteReply::ScheduleCancelled { cancelled, .. } => Ok(cancelled),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    pub async fn prepare_cancel_schedule_mutation(
        &self,
        schedule_id: super::store::ScheduleId,
    ) -> Result<BrainMutationHandle> {
        self.prepare_mutation(
            &crate::ipc::brain_codec::BrainRemoteCommandKind::CancelSchedule(schedule_id),
        )
        .await
    }

    pub async fn cancel_schedule_with_handle(
        &self,
        schedule_id: super::store::ScheduleId,
        handle: &BrainMutationHandle,
    ) -> Result<bool> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};
        match self
            .send_remote_command_with_handle(
                BrainRemoteCommandKind::CancelSchedule(schedule_id),
                Some(handle),
            )
            .await?
        {
            BrainRemoteReply::ScheduleCancelled { cancelled, .. } => Ok(cancelled),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    pub async fn schedule_initialization(
        &self,
        next_due_ms: u64,
    ) -> Result<super::store::BrainSchedule> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};

        match self
            .send_remote_command(BrainRemoteCommandKind::ScheduleInitialization { next_due_ms })
            .await?
        {
            BrainRemoteReply::InitializationScheduled { schedule, .. } => Ok(schedule),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    pub async fn prepare_schedule_initialization_mutation(
        &self,
        next_due_ms: u64,
    ) -> Result<BrainMutationHandle> {
        self.prepare_mutation(
            &crate::ipc::brain_codec::BrainRemoteCommandKind::ScheduleInitialization {
                next_due_ms,
            },
        )
        .await
    }

    pub async fn schedule_initialization_with_handle(
        &self,
        next_due_ms: u64,
        handle: &BrainMutationHandle,
    ) -> Result<super::store::BrainSchedule> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};
        match self
            .send_remote_command_with_handle(
                BrainRemoteCommandKind::ScheduleInitialization { next_due_ms },
                Some(handle),
            )
            .await?
        {
            BrainRemoteReply::InitializationScheduled { schedule, .. } => Ok(schedule),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        }
    }

    /// Detach the current transport projection. Like acknowledgement this is
    /// connection lifecycle, not a canonical Brain mutation, so it carries no
    /// durable idempotency handle across reconnects.
    pub async fn disconnect(&self) -> Result<()> {
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};

        let temporary_events = if self.connection.lock().await.is_none() {
            Some(self.watch().await?)
        } else {
            None
        };
        let result = match self
            .send_remote_command(BrainRemoteCommandKind::Detach)
            .await?
        {
            BrainRemoteReply::Detached { .. } => Ok(()),
            reply => anyhow::bail!("remote Brain returned the wrong reply: {reply:?}"),
        };
        drop(temporary_events);
        result
    }

    /// Connect to the brain's snapshot/live-event stream.
    pub async fn watch(&self) -> Result<mpsc::UnboundedReceiver<BrainWireMessage>> {
        use crate::ipc::brain_codec::{BrainRemoteCommand, BrainRemoteEnvelope, BrainRemoteReply};

        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        if self.connection.lock().await.is_some() {
            anyhow::bail!("remote Brain event stream is already connected");
        }
        let mut request = self.target.ws_url(attachment)?.into_client_request()?;
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            format!("Bearer {}", self.authorized_token().await?).parse()?,
        );
        let (mut socket, _) = tokio_tungstenite::connect_async_tls_with_config(
            request,
            None,
            false,
            self.websocket_connector.clone(),
        )
        .await
        .context("could not open brain event stream")?;
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel::<RemoteBrainRequest>();
        let connection_id = uuid::Uuid::new_v4();
        *self.connection.lock().await = Some(RemoteBrainConnection {
            id: connection_id,
            commands: command_tx,
        });
        let connection = self.connection.clone();
        tokio::spawn(async move {
            let mut next_request_id = 1_u64;
            let mut pending = HashMap::<
                u64,
                oneshot::Sender<std::result::Result<BrainRemoteReply, String>>,
            >::new();
            loop {
                tokio::select! {
                    _ = event_tx.closed() => {
                        let _ = socket.close(None).await;
                        break;
                    }
                    request = command_rx.recv() => {
                        let Some(request) = request else {
                            break;
                        };
                        let request_id = next_request_id;
                        next_request_id = next_request_id.checked_add(1).unwrap_or(1);
                        let envelope = BrainRemoteEnvelope::Command(BrainRemoteCommand {
                            request_id,
                            mutation: request.mutation,
                            kind: request.kind,
                        });
                        let encoded = match crate::ipc::brain_codec::encode_brain_remote_envelope(&envelope) {
                            Ok(encoded) => encoded,
                            Err(error) => {
                                let _ = request.response.send(Err(error.to_string()));
                                continue;
                            }
                        };
                        pending.insert(request_id, request.response);
                        if let Err(error) = socket
                            .send(tokio_tungstenite::tungstenite::Message::Binary(encoded.into()))
                            .await
                        {
                            if let Some(response) = pending.remove(&request_id) {
                                let _ = response.send(Err(error.to_string()));
                            }
                            break;
                        }
                    }
                    incoming = socket.next() => {
                        let Some(Ok(message)) = incoming else {
                            break;
                        };
                        match message {
                            tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                                match crate::ipc::brain_codec::decode_brain_remote_envelope(&bytes) {
                                    Ok(BrainRemoteEnvelope::Projection(message)) => {
                                        if event_tx.send(message).is_err() {
                                            break;
                                        }
                                    }
                                    Ok(BrainRemoteEnvelope::Reply(reply)) => {
                                        if let Some(response) = pending.remove(&reply.request_id()) {
                                            let result = match reply {
                                                BrainRemoteReply::Error { code, message, .. } => {
                                                    Err(format!("{code}: {message}"))
                                                }
                                                reply => Ok(reply),
                                            };
                                            let _ = response.send(result);
                                        }
                                    }
                                    _ => break,
                                }
                            }
                            tokio_tungstenite::tungstenite::Message::Ping(payload) => {
                                if socket
                                    .send(tokio_tungstenite::tungstenite::Message::Pong(payload))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            tokio_tungstenite::tungstenite::Message::Close(_) => break,
                            _ => {}
                        }
                    }
                }
            }
            for (_, response) in pending {
                let _ = response.send(Err("remote Brain connection closed".into()));
            }
            let mut active = connection.lock().await;
            if active
                .as_ref()
                .is_some_and(|current| current.id == connection_id)
            {
                *active = None;
            }
        });
        Ok(event_rx)
    }

    async fn send_remote_command(
        &self,
        kind: crate::ipc::brain_codec::BrainRemoteCommandKind,
    ) -> Result<crate::ipc::brain_codec::BrainRemoteReply> {
        let durable = !matches!(
            &kind,
            crate::ipc::brain_codec::BrainRemoteCommandKind::Acknowledge(_)
                | crate::ipc::brain_codec::BrainRemoteCommandKind::Detach
        );
        let handle = if durable {
            Some(self.prepare_mutation(&kind).await?)
        } else {
            None
        };
        self.send_remote_command_with_handle(kind, handle.as_ref())
            .await
    }

    async fn prepare_mutation(
        &self,
        kind: &crate::ipc::brain_codec::BrainRemoteCommandKind,
    ) -> Result<BrainMutationHandle> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        // A fresh canonical snapshot avoids deriving the next precondition
        // from asynchronously delivered projection ordering.
        let snapshot = self.snapshot().await?;
        Ok(BrainMutationHandle {
            idempotency_key: uuid::Uuid::new_v4(),
            brain_id: snapshot.brain_id,
            attachment_id: attachment.attachment_id,
            expected_revision: snapshot.revision,
            environment_generation: snapshot.environment.generation,
            command_sha256: crate::ipc::brain_codec::brain_remote_command_fingerprint(kind)?,
        })
    }

    async fn send_remote_command_with_handle(
        &self,
        kind: crate::ipc::brain_codec::BrainRemoteCommandKind,
        handle: Option<&BrainMutationHandle>,
    ) -> Result<crate::ipc::brain_codec::BrainRemoteReply> {
        let connection = self
            .connection
            .lock()
            .await
            .clone()
            .context("remote Brain event stream is not connected")?;
        let durable = !matches!(
            &kind,
            crate::ipc::brain_codec::BrainRemoteCommandKind::Acknowledge(_)
                | crate::ipc::brain_codec::BrainRemoteCommandKind::Detach
        );
        anyhow::ensure!(
            durable == handle.is_some(),
            "durable and connection-lifecycle commands require distinct retry semantics"
        );
        let mutation = match handle {
            Some(handle) => {
                let attachment = self
                    .attachment
                    .as_ref()
                    .context("client is not attached to a Brain")?;
                anyhow::ensure!(
                    handle.attachment_id == attachment.attachment_id,
                    "Brain mutation handle belongs to a different attachment"
                );
                anyhow::ensure!(
                    handle.command_sha256
                        == crate::ipc::brain_codec::brain_remote_command_fingerprint(&kind)?,
                    "Brain mutation handle was reused with a different command"
                );
                Some(crate::ipc::brain_codec::BrainRemoteMutation {
                    brain_id: handle.brain_id,
                    expected_revision: handle.expected_revision,
                    environment_generation: handle.environment_generation,
                    idempotency_key: handle.idempotency_key,
                })
            }
            None => None,
        };
        let (response_tx, response_rx) = oneshot::channel();
        if connection
            .commands
            .send(RemoteBrainRequest {
                mutation,
                kind,
                response: response_tx,
            })
            .is_err()
        {
            anyhow::bail!("remote Brain connection closed");
        }
        let response = match response_rx.await {
            Ok(response) => response.map_err(anyhow::Error::msg),
            Err(_) => Err(anyhow::anyhow!(
                "remote Brain connection closed before replying"
            )),
        };
        response
    }

    async fn ensure_credential(&self, subject: &str, role: AttachmentRole) -> Result<()> {
        self.ensure_credential_with_scopes(
            subject,
            role,
            Some(super::credential::default_participant_scopes(role)),
        )
        .await
    }

    async fn ensure_credential_with_scopes(
        &self,
        subject: &str,
        role: AttachmentRole,
        requested_scopes: Option<
            std::collections::BTreeSet<super::credential::BrainCredentialScope>,
        >,
    ) -> Result<()> {
        let now_ms = unix_epoch_millis();
        let mut credential = self.credential.lock().await;
        let minimum_expiry = match &self.bootstrap {
            RemoteBrainBootstrap::Password(_) => now_ms.saturating_add(60_000),
            RemoteBrainBootstrap::Invitation { .. } => now_ms,
        };
        if credential.as_ref().is_some_and(|credential| {
            credential.claims.subject == subject
                && credential.claims.role == role
                && credential.claims.brain == self.target.brain
                && credential.claims.attachment_id.is_none()
                && credential.claims.connection_id.is_none()
                && credential.claims.expires_ms > minimum_expiry
                && requested_scopes
                    .as_ref()
                    .is_none_or(|required| required.is_subset(&credential.claims.scopes))
        }) {
            return Ok(());
        }

        #[derive(Serialize)]
        struct Issue<'a> {
            subject: &'a str,
            role: AttachmentRole,
            scopes: Option<std::collections::BTreeSet<super::credential::BrainCredentialScope>>,
        }
        let issued = match &self.bootstrap {
            RemoteBrainBootstrap::Password(password) => {
                let issued = self
                    .http
                    .post(self.target.credentials_url())
                    .bearer_auth(password)
                    .json(&Issue {
                        subject,
                        role,
                        scopes: requested_scopes.clone(),
                    })
                    .send()
                    .await
                    .context("could not reach Brain credential issuer")?
                    .error_for_status()
                    .context("Brain credential request rejected")?
                    .json::<IssuedBrainCredential>()
                    .await
                    .context("invalid Brain credential response")?;
                RemoteBrainCredential {
                    token: issued.token,
                    claims: issued.claims,
                }
            }
            RemoteBrainBootstrap::Invitation { .. } => {
                anyhow::bail!("redeem the Brain invitation before requesting attachment authority")
            }
        };
        if issued.claims.subject != subject
            || issued.claims.role != role
            || issued.claims.brain != self.target.brain
            || requested_scopes
                .as_ref()
                .is_some_and(|required| !required.is_subset(&issued.claims.scopes))
        {
            anyhow::bail!("Brain credential issuer returned the wrong participant audience");
        }
        *credential = Some(issued);
        Ok(())
    }

    /// Archive an inactive Brain with an explicitly elevated administrative
    /// credential. Ordinary driver credentials never receive this scope.
    pub async fn archive(&self, subject: &str) -> Result<Option<String>> {
        let scopes = [super::credential::BrainCredentialScope::EnvironmentAdmin]
            .into_iter()
            .collect();
        self.ensure_credential_with_scopes(subject, AttachmentRole::Driver, Some(scopes))
            .await?;
        let token = self
            .credential
            .lock()
            .await
            .as_ref()
            .map(|credential| credential.token.clone())
            .context("administrative Brain credential was not retained")?;
        let body = self
            .http
            .delete(self.target.http_url())
            .bearer_auth(token)
            .send()
            .await
            .context("could not reach Brain archive endpoint")?
            .error_for_status()
            .context("Brain archive rejected")?
            .json::<serde_json::Value>()
            .await
            .context("invalid Brain archive response")?;
        Ok(body["archived_to"].as_str().map(str::to_owned))
    }

    async fn authorized_token(&self) -> Result<String> {
        let credential = self.credential.lock().await;
        let credential = credential
            .as_ref()
            .context("client has not bootstrapped a scoped Brain credential")?;
        if credential.claims.expires_ms <= unix_epoch_millis() {
            anyhow::bail!("scoped Brain credential expired; reconnect the attachment");
        }
        Ok(credential.token.clone())
    }

    async fn delegation_authorization(
        &self,
    ) -> Result<(String, Option<super::credential::BrainCredentialClaims>)> {
        match &self.bootstrap {
            RemoteBrainBootstrap::Password(password) => {
                anyhow::ensure!(
                    !self.target.secure && target_address_is_loopback(&self.target.address),
                    "Brain password bootstrap is restricted to a loopback address"
                );
                Ok((password.clone(), None))
            }
            RemoteBrainBootstrap::Invitation { .. } => {
                let credential = self.credential.lock().await;
                let credential = credential
                    .as_ref()
                    .context("redeem the Brain invitation before delegating authority")?;
                anyhow::ensure!(
                    credential.claims.brain == self.target.brain,
                    "Brain credential has a different Brain audience"
                );
                anyhow::ensure!(
                    credential.claims.attachment_id.is_none()
                        && credential.claims.connection_id.is_none(),
                    "attachment-bound credentials cannot delegate authority"
                );
                anyhow::ensure!(
                    credential
                        .claims
                        .permits(super::credential::BrainCredentialScope::BrainControl),
                    "Brain credential does not grant delegation control"
                );
                anyhow::ensure!(
                    credential.claims.expires_ms > unix_epoch_millis(),
                    "Brain credential has expired"
                );
                Ok((credential.token.clone(), Some(credential.claims.clone())))
            }
        }
    }

    fn bootstrap_password(&self) -> Result<&str> {
        match &self.bootstrap {
            RemoteBrainBootstrap::Password(password) => Ok(password),
            RemoteBrainBootstrap::Invitation { .. } => {
                anyhow::bail!("this operation requires the Brain owner's bootstrap credential")
            }
        }
    }
}

fn validate_remote_capabilities(
    capabilities: &RemoteBrainCapabilities,
    claims: &super::credential::BrainCredentialClaims,
    invited_node_public_key: Option<[u8; 32]>,
) -> Result<()> {
    anyhow::ensure!(
        capabilities.schema_version == 1,
        "unsupported Brain capability schema version {}",
        capabilities.schema_version
    );
    anyhow::ensure!(
        capabilities.brain_id == claims.brain_id
            && capabilities.brain == claims.brain
            && capabilities.environment.generation == claims.environment_generation,
        "Brain capability response has the wrong credential audience"
    );
    let node_public_key: [u8; 32] = hex::decode(&capabilities.node_public_key)
        .context("Brain capability response contains an invalid node identity")?
        .try_into()
        .map_err(|_| {
            anyhow::anyhow!("Brain capability response contains an invalid node identity")
        })?;
    if let Some(expected) = invited_node_public_key {
        anyhow::ensure!(
            node_public_key == expected,
            "Brain capability response came from a different invited node identity"
        );
    }
    Ok(())
}

#[derive(Clone)]
enum AttachedBrainTransport {
    Local(crate::ipc::IpcClient),
    Remote(RemoteBrainClient),
}

/// One client projection over the canonical Brain service. Local consoles use
/// Cap'n Proto on the daemon Unix socket; remote consoles retain scoped
/// credential bootstrap and the binary WebSocket adapter until remote
/// mutations move to the same Cap'n Proto request schema.
#[derive(Clone)]
pub struct AttachedBrainClient {
    pub target: RemoteBrainTarget,
    transport: AttachedBrainTransport,
    attachment: Option<BrainAttachment>,
}

impl AttachedBrainClient {
    pub fn local(target: RemoteBrainTarget, ipc: crate::ipc::IpcClient) -> Self {
        Self {
            target,
            transport: AttachedBrainTransport::Local(ipc),
            attachment: None,
        }
    }

    pub fn remote(client: RemoteBrainClient) -> Self {
        Self {
            target: client.target.clone(),
            transport: AttachedBrainTransport::Remote(client),
            attachment: None,
        }
    }

    pub fn attachment(&self) -> Option<&BrainAttachment> {
        self.attachment.as_ref()
    }

    pub async fn attach(
        &mut self,
        subject: &str,
        role: AttachmentRole,
        attachment_id: Option<AttachmentId>,
    ) -> Result<BrainAttachment> {
        let attachment = match &mut self.transport {
            AttachedBrainTransport::Local(ipc) => {
                ipc.brain_attach(&self.target.brain, subject, role, attachment_id)
                    .await?
            }
            AttachedBrainTransport::Remote(client) => {
                client.attach(subject, role, attachment_id).await?
            }
        };
        self.attachment = Some(attachment.clone());
        Ok(attachment)
    }

    pub async fn attach_persistent(
        &mut self,
        subject: &str,
        role: AttachmentRole,
        client_slot: &str,
    ) -> Result<BrainAttachment> {
        let attachment = match &mut self.transport {
            AttachedBrainTransport::Remote(client) => {
                client.attach_persistent(subject, role, client_slot).await?
            }
            AttachedBrainTransport::Local(ipc) => {
                let snapshot = ipc.brain_snapshot(&self.target.brain).await?;
                let store = AttachmentIdentityStore::new(AttachmentIdentityStore::default_path()?);
                let attachment_id = store.find(snapshot.brain_id, client_slot, subject, role)?;
                let attachment = ipc
                    .brain_attach(&self.target.brain, subject, role, attachment_id)
                    .await?;
                if let Err(error) = store.save(AttachmentIdentityRecord {
                    brain_id: snapshot.brain_id,
                    client_slot: client_slot.to_string(),
                    subject: subject.to_string(),
                    role,
                    attachment_id: attachment.attachment_id,
                }) {
                    let _ = ipc.brain_detach(&self.target.brain, &attachment).await;
                    return Err(error.context("persist Brain attachment identity"));
                }
                attachment
            }
        };
        self.attachment = Some(attachment.clone());
        Ok(attachment)
    }

    pub async fn attach_invited_persistent(
        &mut self,
        subject: &str,
        client_slot: &str,
    ) -> Result<(AttachmentRole, BrainAttachment)> {
        let AttachedBrainTransport::Remote(client) = &mut self.transport else {
            anyhow::bail!("Brain invitations are redeemed through the remote transport");
        };
        let (role, attachment) = client
            .attach_invited_persistent(subject, client_slot)
            .await?;
        self.attachment = Some(attachment.clone());
        Ok((role, attachment))
    }

    pub async fn snapshot(&self) -> Result<BrainSnapshot> {
        match &self.transport {
            AttachedBrainTransport::Local(ipc) => ipc.brain_snapshot(&self.target.brain).await,
            AttachedBrainTransport::Remote(client) => client.snapshot().await,
        }
    }

    pub async fn push(&self, kind: BrainEventKind) -> Result<()> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        match &self.transport {
            AttachedBrainTransport::Local(ipc) => {
                ipc.brain_submit(&self.target.brain, attachment, kind)
                    .await?;
                Ok(())
            }
            AttachedBrainTransport::Remote(client) => client.push(kind).await,
        }
    }

    pub async fn start_speculative(&self, prompt: String) -> Result<super::store::BrainRun> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        match &self.transport {
            AttachedBrainTransport::Local(ipc) => {
                ipc.brain_start_speculative(&self.target.brain, attachment, prompt)
                    .await
            }
            AttachedBrainTransport::Remote(client) => client.start_speculative(prompt).await,
        }
    }

    pub async fn cancel_run(&self, run_id: super::store::RunId) -> Result<super::store::BrainRun> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        match &self.transport {
            AttachedBrainTransport::Local(ipc) => {
                ipc.brain_cancel_run(&self.target.brain, attachment, run_id)
                    .await
            }
            AttachedBrainTransport::Remote(client) => client.cancel_run(run_id).await,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_schedule(
        &self,
        language: super::store::ProgramLanguage,
        source: String,
        grant_ceiling: crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: super::store::BrainScheduleDeliveryPolicy,
    ) -> Result<super::store::BrainSchedule> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        match &self.transport {
            AttachedBrainTransport::Local(ipc) => {
                ipc.brain_create_schedule(
                    &self.target.brain,
                    attachment,
                    language,
                    &source,
                    &grant_ceiling,
                    next_due_ms,
                    interval_ms,
                    &delivery_policy,
                )
                .await
            }
            AttachedBrainTransport::Remote(client) => {
                client
                    .create_schedule(
                        language,
                        source,
                        grant_ceiling,
                        next_due_ms,
                        interval_ms,
                        delivery_policy,
                    )
                    .await
            }
        }
    }

    pub async fn inspect_schedule(
        &self,
        schedule_id: super::store::ScheduleId,
    ) -> Result<Option<super::store::BrainSchedule>> {
        match &self.transport {
            AttachedBrainTransport::Local(ipc) => {
                ipc.brain_inspect_schedule(&self.target.brain, schedule_id)
                    .await
            }
            AttachedBrainTransport::Remote(client) => Ok(client
                .snapshot()
                .await?
                .schedules
                .into_iter()
                .find(|schedule| schedule.schedule_id == schedule_id)),
        }
    }

    pub async fn cancel_schedule(&self, schedule_id: super::store::ScheduleId) -> Result<bool> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        match &self.transport {
            AttachedBrainTransport::Local(ipc) => {
                ipc.brain_cancel_schedule(&self.target.brain, attachment, schedule_id)
                    .await
            }
            AttachedBrainTransport::Remote(client) => client.cancel_schedule(schedule_id).await,
        }
    }

    pub async fn schedule_initialization(
        &self,
        next_due_ms: u64,
    ) -> Result<super::store::BrainSchedule> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        match &self.transport {
            AttachedBrainTransport::Local(ipc) => {
                ipc.brain_schedule_initialization(&self.target.brain, attachment, next_due_ms)
                    .await
            }
            AttachedBrainTransport::Remote(client) => {
                client.schedule_initialization(next_due_ms).await
            }
        }
    }

    pub async fn acknowledge(&mut self, seq: u64) -> Result<()> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        let updated = match &mut self.transport {
            AttachedBrainTransport::Local(ipc) => {
                ipc.brain_acknowledge(&self.target.brain, attachment, seq)
                    .await?
            }
            AttachedBrainTransport::Remote(client) => {
                client.acknowledge(seq).await?;
                client
                    .attachment()
                    .cloned()
                    .context("remote Brain acknowledgement lost its attachment")?
            }
        };
        self.attachment = Some(updated);
        Ok(())
    }

    pub async fn disconnect(&self) -> Result<()> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        match &self.transport {
            AttachedBrainTransport::Local(ipc) => {
                ipc.brain_detach(&self.target.brain, attachment).await
            }
            AttachedBrainTransport::Remote(client) => client.disconnect().await,
        }
    }

    pub async fn watch(&self) -> Result<mpsc::UnboundedReceiver<BrainWireMessage>> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        match &self.transport {
            AttachedBrainTransport::Remote(client) => client.watch().await,
            AttachedBrainTransport::Local(ipc) => {
                let mut source = ipc.brain_watch(&self.target.brain, attachment).await?;
                let (tx, rx) = mpsc::unbounded_channel();
                tokio::task::spawn_local(async move {
                    while let Some(message) = source.recv().await {
                        match message {
                            Ok(message) => {
                                if tx.send(message).is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
                Ok(rx)
            }
        }
    }

    /// Connect while retaining a transport failure as data. The home-console
    /// supervisor uses this to distinguish its event watch from runner health.
    pub async fn watch_with_errors(
        &self,
    ) -> Result<mpsc::UnboundedReceiver<Result<BrainWireMessage>>> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        match &self.transport {
            AttachedBrainTransport::Remote(client) => {
                let mut source = client.watch().await?;
                let (tx, rx) = mpsc::unbounded_channel();
                tokio::spawn(async move {
                    while let Some(message) = source.recv().await {
                        if tx.send(Ok(message)).is_err() {
                            break;
                        }
                    }
                });
                Ok(rx)
            }
            AttachedBrainTransport::Local(ipc) => {
                ipc.brain_watch(&self.target.brain, attachment).await
            }
        }
    }
}

fn unix_epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ensure_supervisor_live_fixture() {
        static READY: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        READY.get_or_init(|| {
            let proof = crate::brain::isolated_test_proof().unwrap();
            let brain_listener = proof.duplicate_brain_listener().unwrap();
            let daemon_listener = proof.duplicate_daemon_listener().unwrap();
            brain_listener.set_nonblocking(true).unwrap();
            daemon_listener.set_nonblocking(true).unwrap();
            let state_root = proof.home.join(".finch/live-endpoint-fixture");
            std::fs::create_dir_all(&state_root).unwrap();
            let authority = super::super::credential::BrainCredentialAuthority::ephemeral([91; 32]);
            let state = Arc::new(
                crate::server::AgentServer::for_supervised_brain_http_test(
                    "supervisor.local",
                    &state_root,
                    authority,
                )
                .unwrap(),
            );
            let ipc_state = Arc::clone(&state);
            let ipc_path = proof.ipc_socket.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime
                    .block_on(crate::ipc::start_ipc_server(
                        ipc_state,
                        tokio_util::sync::CancellationToken::new(),
                    ))
                    .unwrap();
            });
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let brain_listener = tokio::net::TcpListener::from_std(brain_listener).unwrap();
                    let daemon_listener =
                        tokio::net::TcpListener::from_std(daemon_listener).unwrap();
                    let brain = axum::serve(
                        brain_listener,
                        crate::server::handlers::create_remote_brain_router(Arc::clone(&state))
                            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
                    );
                    let daemon = axum::serve(
                        daemon_listener,
                        crate::server::handlers::create_router(state).into_make_service(),
                    );
                    ready_tx.send(()).unwrap();
                    let _ = tokio::join!(brain, daemon);
                });
            });
            ready_rx.recv().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !ipc_path.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "supervised IPC fixture did not bind"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
    }

    fn isolated_live_brain_target(brain: &str) -> RemoteBrainTarget {
        ensure_supervisor_live_fixture();
        let proof = crate::brain::isolated_test_proof().unwrap();
        let address = std::env::var("FINCH_TEST_BRAIN_ADDR")
            .expect("FINCH_TEST_BRAIN_ADDR must name the owned ephemeral Brain listener");
        assert_eq!(address, proof.brain_addr);
        let socket: std::net::SocketAddr = address.parse().expect("invalid test Brain address");
        assert!(socket.ip().is_loopback() && socket.port() != 0);
        assert_ne!(socket.port(), DEFAULT_BRAIN_PORT);
        let mut target = RemoteBrainTarget::parse(&format!("{brain}@{address}")).unwrap();
        target.secure = false;
        target
    }

    fn isolated_live_daemon_address() -> String {
        ensure_supervisor_live_fixture();
        let proof = crate::brain::isolated_test_proof().unwrap();
        let address = std::env::var("FINCH_TEST_DAEMON_ADDR")
            .expect("FINCH_TEST_DAEMON_ADDR must name the owned ephemeral daemon");
        assert_eq!(address, proof.daemon_addr);
        let socket: std::net::SocketAddr = address.parse().expect("invalid test daemon address");
        assert!(socket.ip().is_loopback() && socket.port() != 0);
        assert_ne!(address, crate::config::constants::DEFAULT_DAEMON_ADDR);
        address
    }

    fn isolated_live_password() -> String {
        crate::brain::isolated_test_proof().unwrap();
        std::env::var("FINCH_TEST_BRAIN_PASSWORD")
            .expect("FINCH_TEST_BRAIN_PASSWORD must match the isolated daemon fixture")
    }

    async fn connect_isolated_live_ipc() -> crate::ipc::IpcClient {
        let proof = crate::brain::isolated_test_proof().unwrap();
        let path = std::env::var_os("FINCH_TEST_IPC_SOCKET")
            .map(std::path::PathBuf::from)
            .expect("FINCH_TEST_IPC_SOCKET must name the owned daemon socket");
        assert_eq!(path, proof.ipc_socket);
        #[cfg(unix)]
        let before = crate::brain::validate_isolated_test_socket(&proof, &path).unwrap();
        let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
        #[cfg(unix)]
        crate::brain::authenticate_isolated_test_peer(&stream).unwrap();
        let client = crate::ipc::IpcClient::from_stream(stream).await.unwrap();
        #[cfg(unix)]
        {
            let after = crate::brain::validate_isolated_test_socket(&proof, &path).unwrap();
            assert_eq!(
                before, after,
                "test IPC socket identity changed during connect"
            );
        }
        client
    }

    #[test]
    fn issued_intervals_allow_skew_and_use_endpoint_defaults() {
        let now = 1_000_000;
        let skew = super::super::credential::MAX_SIGNED_CLOCK_SKEW_MS;
        assert!(issued_interval_is_valid(
            now + skew,
            now + skew + 1_000,
            Some(1_000),
            15 * 60 * 1_000,
            now,
        ));
        assert!(!issued_interval_is_valid(
            now + skew + 1,
            now + skew + 1_000,
            Some(1_000),
            15 * 60 * 1_000,
            now,
        ));
        assert!(issued_interval_is_valid(
            now,
            now + 8 * 60 * 60 * 1_000,
            None,
            8 * 60 * 60 * 1_000,
            now,
        ));
        assert!(!issued_interval_is_valid(
            now,
            now + 8 * 60 * 60 * 1_000 + 1,
            None,
            8 * 60 * 60 * 1_000,
            now,
        ));
        assert!(issued_interval_is_valid(
            now,
            now + 15 * 60 * 1_000,
            None,
            15 * 60 * 1_000,
            now,
        ));
        assert!(!issued_interval_is_valid(
            now,
            now + 15 * 60 * 1_000 + 1,
            None,
            15 * 60 * 1_000,
            now,
        ));
        assert!(!issued_interval_is_valid(
            now,
            now,
            None,
            15 * 60 * 1_000,
            now,
        ));
    }

    #[test]
    fn mutation_handle_round_trips_for_process_restart_and_distinguishes_identical_calls() {
        let base = BrainMutationHandle {
            idempotency_key: uuid::Uuid::new_v4(),
            brain_id: BrainId(uuid::Uuid::new_v4()),
            attachment_id: AttachmentId(uuid::Uuid::new_v4()),
            expected_revision: 17,
            environment_generation: 4,
            command_sha256: "same-command".into(),
        };
        let restored: BrainMutationHandle =
            serde_json::from_str(&serde_json::to_string(&base).unwrap()).unwrap();
        assert_eq!(restored, base);
        let concurrent = BrainMutationHandle {
            idempotency_key: uuid::Uuid::new_v4(),
            ..base.clone()
        };
        assert_ne!(concurrent.idempotency_key, base.idempotency_key);
        assert_eq!(concurrent.command_sha256, base.command_sha256);
    }

    #[test]
    fn target_defaults_to_daemon_port_and_keeps_mdns_name() {
        let target = RemoteBrainTarget::parse("finch@workstation.local").unwrap();
        assert_eq!(target.display_name(), "finch@workstation.local");
        assert_eq!(target.address, "workstation.local:11436");
        assert!(target.secure);
    }

    #[test]
    fn target_accepts_an_explicit_port() {
        let target = RemoteBrainTarget::parse("review@10.0.0.4:9000").unwrap();
        assert_eq!(target.machine, "10.0.0.4");
        assert_eq!(target.address, "10.0.0.4:9000");
    }

    #[test]
    fn invitation_recipient_uses_certificate_hostname_and_tls_listener_port() {
        let target = RemoteBrainTarget::invitation_recipient(
            "copper-brook-a1752c",
            "Shammahs-MacBook-Air.local",
            "0.0.0.0:11436",
        )
        .unwrap();

        assert_eq!(
            target.command_target(),
            "copper-brook-a1752c@Shammahs-MacBook-Air.local:11436"
        );
        assert_eq!(target.machine, "Shammahs-MacBook-Air.local");
        assert!(target.secure);
    }

    #[test]
    fn invitation_recipient_normalizes_bare_certificate_hostname_for_mdns() {
        let target =
            RemoteBrainTarget::invitation_recipient("review", "workstation", "[::]:19436").unwrap();
        assert_eq!(target.command_target(), "review@workstation.local:19436");
    }

    #[test]
    fn invitation_recipient_never_publishes_bind_or_loopback_ip_as_host() {
        assert!(
            RemoteBrainTarget::invitation_recipient("review", "127.0.0.1", "0.0.0.0:11436")
                .is_err()
        );
        assert!(RemoteBrainTarget::invitation_recipient(
            "review",
            "workstation.local",
            "0.0.0.0:0"
        )
        .is_err());
    }

    #[test]
    fn bare_name_can_resolve_through_the_local_daemon() {
        let target = RemoteBrainTarget::local("review", "http://127.0.0.1:32123").unwrap();
        assert_eq!(target.brain, "review");
        assert_eq!(target.address, "127.0.0.1:32123");
        assert!(!target.secure);
    }

    #[test]
    fn target_rejects_ambiguous_or_unsafe_values() {
        assert!(RemoteBrainTarget::parse("brain-only").is_err());
        assert!(RemoteBrainTarget::parse("../brain@host").is_err());
        assert!(RemoteBrainTarget::parse("brain@host/path").is_err());
    }

    fn capability_test_claims(
        brain_id: BrainId,
    ) -> super::super::credential::BrainCredentialClaims {
        super::super::credential::BrainCredentialClaims {
            version: 1,
            credential_id: uuid::Uuid::new_v4(),
            issuer: "fixture.local".into(),
            subject: "alice@laptop.local".into(),
            brain_id,
            brain: "shared".into(),
            environment_generation: 7,
            role: AttachmentRole::Observer,
            scopes: super::super::credential::default_participant_scopes(AttachmentRole::Observer),
            attachment_id: None,
            connection_id: None,
            delegation_chain: Vec::new(),
            issued_ms: 0,
            expires_ms: u64::MAX,
        }
    }

    fn capability_test_response(
        brain_id: BrainId,
        node_public_key: [u8; 32],
    ) -> RemoteBrainCapabilities {
        RemoteBrainCapabilities {
            schema_version: 1,
            brain_id,
            brain: "shared".into(),
            environment: BrainEnvironment {
                machine: "fixture.local".into(),
                workspace: "/workspace".into(),
                generation: 7,
            },
            node_public_key: hex::encode(node_public_key),
            node: crate::node::NodeCapabilities {
                ram_gb: 16,
                local_model: Some("fixture-model".into()),
                has_teacher_api: true,
                version: "test".into(),
                os: "test".into(),
            },
        }
    }

    #[test]
    fn remote_capabilities_are_credential_audience_bound_and_node_pinned() {
        let brain_id = BrainId(uuid::Uuid::new_v4());
        let claims = capability_test_claims(brain_id);
        let capabilities = capability_test_response(brain_id, [61; 32]);
        validate_remote_capabilities(&capabilities, &claims, Some([61; 32])).unwrap();

        let mut wrong_brain = capabilities.clone();
        wrong_brain.brain_id = BrainId(uuid::Uuid::new_v4());
        assert!(validate_remote_capabilities(&wrong_brain, &claims, Some([61; 32])).is_err());

        let mut wrong_generation = capabilities.clone();
        wrong_generation.environment.generation += 1;
        assert!(validate_remote_capabilities(&wrong_generation, &claims, Some([61; 32])).is_err());
        assert!(validate_remote_capabilities(&capabilities, &claims, Some([62; 32])).is_err());
    }

    #[tokio::test]
    async fn capability_query_uses_the_scoped_credential() {
        use axum::{http::HeaderMap, routing::get, Json, Router};

        let brain_id = BrainId(uuid::Uuid::new_v4());
        let response = capability_test_response(brain_id, [63; 32]);
        let app = Router::new().route(
            "/v1/brains/named/shared/capabilities",
            get(move |headers: HeaderMap| {
                let response = response.clone();
                async move {
                    assert_eq!(
                        headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok()),
                        Some("Bearer scoped-token")
                    );
                    Json(response)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let target = RemoteBrainTarget {
            brain: "shared".into(),
            machine: "fixture.local".into(),
            address: address.to_string(),
            secure: false,
        };
        let client = RemoteBrainClient::new(target, "unused").unwrap();
        *client.credential.lock().await = Some(RemoteBrainCredential {
            token: "scoped-token".into(),
            claims: capability_test_claims(brain_id),
        });

        let capabilities = client.capabilities().await.unwrap();
        assert_eq!(capabilities.brain_id, brain_id);
        assert_eq!(
            capabilities.node.local_model.as_deref(),
            Some("fixture-model")
        );
        server.abort();
    }

    #[tokio::test]
    async fn invitation_client_issues_and_redeems_through_the_scoped_endpoints() {
        use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        #[derive(Clone)]
        struct Fixture {
            brain_id: BrainId,
            invitation: String,
            invitation_claims: super::super::credential::BrainInvitationClaims,
            issues: Arc<AtomicUsize>,
            redemptions: Arc<AtomicUsize>,
        }

        let brain_id = BrainId(uuid::Uuid::new_v4());
        let invitation_authority =
            super::super::credential::BrainCredentialAuthority::ephemeral([42; 32]);
        let now_ms = unix_epoch_millis();
        let (invitation, invitation_claims) = invitation_authority
            .issue_invitation(
                super::super::credential::BrainInvitationRequest {
                    issuer: "fixture.local".into(),
                    brain_id,
                    brain: "shared".into(),
                    environment_generation: 1,
                    role: AttachmentRole::Consultant,
                    scopes: super::super::credential::default_participant_scopes(
                        AttachmentRole::Consultant,
                    ),
                    delegation_chain: Vec::new(),
                    ttl_ms: 60_000,
                },
                now_ms,
            )
            .unwrap();
        let fixture = Fixture {
            brain_id,
            invitation,
            invitation_claims,
            issues: Arc::new(AtomicUsize::new(0)),
            redemptions: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route(
                "/v1/brains/named/shared/invitations",
                post(
                    |State(fixture): State<Fixture>, headers: HeaderMap| async move {
                        assert_eq!(
                            headers
                                .get(axum::http::header::AUTHORIZATION)
                                .unwrap(),
                            "Bearer owner-secret"
                        );
                        fixture.issues.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({
                            "invitation": fixture.invitation,
                            "claims": fixture.invitation_claims,
                        }))
                    },
                ),
            )
            .route(
                "/v1/brains/invitations/redeem",
                post(
                    |State(fixture): State<Fixture>, Json(body): Json<serde_json::Value>| async move {
                        assert_eq!(body["invitation"], fixture.invitation);
                        assert_eq!(body["subject"], "alice@laptop.local");
                        fixture.redemptions.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({
                            "token": "ordinary-scoped-credential",
                            "claims": {
                                "version": 1,
                                "credential_id": uuid::Uuid::new_v4(),
                                "issuer": "fixture.local",
                                "subject": "alice@laptop.local",
                                "brain_id": fixture.brain_id,
                                "brain": "shared",
                                "environment_generation": 1,
                                "role": "consultant",
                                "scopes": ["brain:read", "brain:attach", "brain:detach", "brain:submit"],
                                "delegation_chain": [],
                                "issued_ms": 1,
                                "expires_ms": u64::MAX
                            }
                        }))
                    },
                ),
            )
            .with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let target = RemoteBrainTarget {
            brain: "shared".into(),
            machine: "fixture.local".into(),
            address: address.to_string(),
            secure: false,
        };

        let owner = RemoteBrainClient::new(target.clone(), "owner-secret").unwrap();
        let (invitation, claims) = owner
            .issue_invitation(AttachmentRole::Consultant, Some(60_000))
            .await
            .unwrap();
        assert_eq!(invitation, fixture.invitation);
        assert_eq!(claims.role, AttachmentRole::Consultant);

        let guest = RemoteBrainClient::new_with_invitation(target, invitation).unwrap();
        assert_eq!(
            guest.redeem_invitation("alice@laptop.local").await.unwrap(),
            AttachmentRole::Consultant
        );
        assert_eq!(
            guest.redeem_invitation("alice@laptop.local").await.unwrap(),
            AttachmentRole::Consultant
        );
        assert_eq!(fixture.issues.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.redemptions.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn invitation_pins_the_https_redemption_endpoint() {
        use axum::{routing::post, Json, Router};

        #[derive(Deserialize)]
        struct RedeemRequest {
            invitation: String,
            subject: String,
        }

        let secret = [41; 32];
        let authority = super::super::credential::BrainCredentialAuthority::ephemeral(secret);
        let brain_id = BrainId(uuid::Uuid::new_v4());
        let (invitation, invitation_claims) = authority
            .issue_invitation(
                super::super::credential::BrainInvitationRequest {
                    issuer: "fixture.local".into(),
                    brain_id,
                    brain: "shared".into(),
                    environment_generation: 1,
                    role: AttachmentRole::Consultant,
                    scopes: super::super::credential::default_participant_scopes(
                        AttachmentRole::Consultant,
                    ),
                    delegation_chain: Vec::new(),
                    ttl_ms: 60_000,
                },
                unix_epoch_millis(),
            )
            .unwrap();
        let expected_invitation = invitation.clone();
        let app = Router::new().route(
            "/v1/brains/invitations/redeem",
            post(move |Json(request): Json<RedeemRequest>| {
                let expected_invitation = expected_invitation.clone();
                async move {
                    assert_eq!(request.invitation, expected_invitation);
                    Json(serde_json::json!({
                        "token": "scoped-token",
                        "claims": {
                            "version": 1,
                            "credential_id": uuid::Uuid::new_v4(),
                            "issuer": "fixture.local",
                            "subject": request.subject,
                            "brain_id": brain_id,
                            "brain": "shared",
                            "environment_generation": 1,
                            "role": "consultant",
                            "scopes": ["brain:read", "brain:attach", "brain:detach", "brain:submit"],
                            "delegation_chain": [],
                            "issued_ms": 1,
                            "expires_ms": u64::MAX
                        }
                    }))
                }
            }),
        );
        let node = crate::node::identity::NodeSigningIdentity::from_secret(secret);
        let tls =
            crate::node::tls::NodeTlsIdentity::from_signing_identity(&node, "localhost").unwrap();
        let invitation_certificate =
            super::super::credential::invitation_tls_certificate_der(&invitation_claims).unwrap();
        crate::node::tls::install_server_crypto_provider();
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_der(
            vec![invitation_certificate],
            tls.private_key_der().to_vec(),
        )
        .await
        .unwrap();
        let handle = axum_server::Handle::new();
        let server = tokio::spawn(
            axum_server::bind_rustls(
                "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap(),
                tls_config,
            )
            .handle(handle.clone())
            .serve(app.into_make_service()),
        );
        let address = handle.listening().await.unwrap();
        let target = RemoteBrainTarget {
            brain: "shared".into(),
            machine: "localhost".into(),
            address: format!("localhost:{}", address.port()),
            secure: true,
        };
        let client = RemoteBrainClient::new_with_invitation(target, invitation).unwrap();

        client.probe_invitation_endpoint().await.unwrap();

        assert_eq!(
            client
                .redeem_invitation("alice@laptop.local")
                .await
                .unwrap(),
            AttachmentRole::Consultant
        );
        server.abort();
    }

    #[tokio::test]
    async fn invitation_redemption_never_follows_a_cross_host_redirect() {
        use axum::{response::Redirect, routing::post, Router};

        let redirected_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attacker_count = redirected_requests.clone();
        let attacker = Router::new().route(
            "/stolen",
            post(move || {
                let attacker_count = attacker_count.clone();
                async move {
                    attacker_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    "unexpected redirect"
                }
            }),
        );
        let attacker_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let attacker_address = attacker_listener.local_addr().unwrap();
        let attacker_server =
            tokio::spawn(async move { axum::serve(attacker_listener, attacker).await.unwrap() });

        let secret = [43; 32];
        let authority = super::super::credential::BrainCredentialAuthority::ephemeral(secret);
        let (invitation, invitation_claims) = authority
            .issue_invitation(
                super::super::credential::BrainInvitationRequest {
                    issuer: "fixture.local".into(),
                    brain_id: BrainId(uuid::Uuid::new_v4()),
                    brain: "shared".into(),
                    environment_generation: 1,
                    role: AttachmentRole::Observer,
                    scopes: super::super::credential::default_participant_scopes(
                        AttachmentRole::Observer,
                    ),
                    delegation_chain: Vec::new(),
                    ttl_ms: 60_000,
                },
                unix_epoch_millis(),
            )
            .unwrap();
        let redirect_url = format!("http://{attacker_address}/stolen");
        let app = Router::new().route(
            "/v1/brains/invitations/redeem",
            post(move || {
                let redirect_url = redirect_url.clone();
                async move { Redirect::temporary(&redirect_url) }
            }),
        );
        let node = crate::node::identity::NodeSigningIdentity::from_secret(secret);
        let tls =
            crate::node::tls::NodeTlsIdentity::from_signing_identity(&node, "localhost").unwrap();
        let invitation_certificate =
            super::super::credential::invitation_tls_certificate_der(&invitation_claims).unwrap();
        crate::node::tls::install_server_crypto_provider();
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_der(
            vec![invitation_certificate],
            tls.private_key_der().to_vec(),
        )
        .await
        .unwrap();
        let handle = axum_server::Handle::new();
        let server = tokio::spawn(
            axum_server::bind_rustls(
                "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap(),
                tls_config,
            )
            .handle(handle.clone())
            .serve(app.into_make_service()),
        );
        let address = handle.listening().await.unwrap();
        let client = RemoteBrainClient::new_with_invitation(
            RemoteBrainTarget {
                brain: "shared".into(),
                machine: "localhost".into(),
                address: format!("localhost:{}", address.port()),
                secure: true,
            },
            invitation,
        )
        .unwrap();

        let error = client
            .redeem_invitation("alice@laptop.local")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("invalid Brain invitation redemption response"));
        assert_eq!(
            redirected_requests.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the invitation bearer must never be replayed to a redirect target"
        );
        server.abort();
        attacker_server.abort();
    }

    #[tokio::test]
    async fn invitation_rejects_an_alternate_certificate_for_the_same_hostname() {
        use axum::{routing::post, Json, Router};

        let invited_secret = [51; 32];
        let authority =
            super::super::credential::BrainCredentialAuthority::ephemeral(invited_secret);
        let brain_id = BrainId(uuid::Uuid::new_v4());
        let (invitation, _) = authority
            .issue_invitation(
                super::super::credential::BrainInvitationRequest {
                    issuer: "real.local".into(),
                    brain_id,
                    brain: "shared".into(),
                    environment_generation: 1,
                    role: AttachmentRole::Observer,
                    scopes: super::super::credential::default_participant_scopes(
                        AttachmentRole::Observer,
                    ),
                    delegation_chain: Vec::new(),
                    ttl_ms: 60_000,
                },
                unix_epoch_millis(),
            )
            .unwrap();

        let attacker = crate::node::identity::NodeSigningIdentity::from_secret([52; 32]);
        let attacker_tls =
            crate::node::tls::NodeTlsIdentity::from_signing_identity(&attacker, "localhost")
                .unwrap();
        crate::node::tls::install_server_crypto_provider();
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_der(
            vec![attacker_tls.certificate_der().to_vec()],
            attacker_tls.private_key_der().to_vec(),
        )
        .await
        .unwrap();
        let app = Router::new().route(
            "/v1/brains/invitations/redeem",
            post(|| async { Json(serde_json::json!({ "unexpected": true })) }),
        );
        let handle = axum_server::Handle::new();
        let server = tokio::spawn(
            axum_server::bind_rustls(
                "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap(),
                tls_config,
            )
            .handle(handle.clone())
            .serve(app.into_make_service()),
        );
        let address = handle.listening().await.unwrap();
        let target = RemoteBrainTarget {
            brain: "shared".into(),
            machine: "localhost".into(),
            address: format!("localhost:{}", address.port()),
            secure: true,
        };
        let client = RemoteBrainClient::new_with_invitation(target, invitation).unwrap();

        let error = client
            .redeem_invitation("alice@laptop.local")
            .await
            .unwrap_err();
        let detail = format!("{error:#}");
        assert!(
            detail.contains("certificate") || detail.contains("UnknownIssuer"),
            "unexpected TLS error: {detail}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn restricted_production_router_enforces_delegation_and_archive_scopes() {
        use std::collections::BTreeSet;
        use std::sync::Arc;

        use super::super::credential::{
            default_participant_scopes, BrainCredentialRequest, BrainCredentialScope,
            BrainInvitationRequest,
        };

        assert!(RemoteBrainClient::new(
            RemoteBrainTarget {
                brain: "shared".into(),
                machine: "remote.example".into(),
                address: "192.0.2.1:11436".into(),
                secure: false,
            },
            "must-not-be-sent",
        )
        .is_err());

        let temp = tempfile::tempdir().unwrap();
        let authority = super::super::credential::BrainCredentialAuthority::ephemeral([82; 32]);
        let state = Arc::new(
            crate::server::AgentServer::for_brain_http_test(
                "fixture.local",
                temp.path(),
                authority.clone(),
            )
            .unwrap(),
        );
        let snapshot = state.brain_store().snapshot("shared").unwrap();
        let now = unix_epoch_millis();
        let controller_scopes = [
            BrainCredentialScope::BrainRead,
            BrainCredentialScope::BrainAttach,
            BrainCredentialScope::BrainDetach,
            BrainCredentialScope::BrainControl,
            BrainCredentialScope::EnvironmentAdmin,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let (controller_invitation, _) = authority
            .issue_invitation(
                BrainInvitationRequest {
                    issuer: "fixture.local".into(),
                    brain_id: snapshot.brain_id,
                    brain: "shared".into(),
                    environment_generation: snapshot.environment.generation,
                    role: AttachmentRole::Driver,
                    scopes: controller_scopes,
                    delegation_chain: Vec::new(),
                    ttl_ms: 120_000,
                },
                now,
            )
            .unwrap();

        let tls = authority.invitation_tls_identity();
        crate::node::tls::install_server_crypto_provider();
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_der(
            vec![tls.certificate_der().to_vec()],
            tls.private_key_der().to_vec(),
        )
        .await
        .unwrap();
        let app = crate::server::handlers::create_remote_brain_router(state);
        let handle = axum_server::Handle::new();
        let server = tokio::spawn(
            axum_server::bind_rustls(
                "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap(),
                tls_config,
            )
            .handle(handle.clone())
            .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>()),
        );
        let address = handle.listening().await.unwrap();
        let target = RemoteBrainTarget {
            brain: "shared".into(),
            machine: "localhost".into(),
            address: format!("localhost:{}", address.port()),
            secure: true,
        };

        let controller =
            RemoteBrainClient::new_with_invitation(target.clone(), controller_invitation.clone())
                .unwrap();
        controller.redeem_invitation("controller").await.unwrap();
        let parent = controller
            .credential
            .lock()
            .await
            .as_ref()
            .unwrap()
            .claims
            .clone();
        let read = [BrainCredentialScope::BrainRead]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let (child_token, child) = controller
            .issue_credential(
                "reader",
                AttachmentRole::Observer,
                read.clone(),
                Some(10_000),
            )
            .await
            .unwrap();
        assert_eq!(child.delegation_chain, vec![parent.credential_id]);
        controller
            .revoke_delegated_credential(&child_token)
            .await
            .unwrap();
        assert!(authority.verify(&child_token, unix_epoch_millis()).is_err());
        let (invitation_token, invitation) = controller
            .issue_invitation_with_scopes(
                AttachmentRole::Observer,
                Some(default_participant_scopes(AttachmentRole::Observer)),
                Some(10_000),
            )
            .await
            .unwrap();
        assert_eq!(invitation.delegation_chain, vec![parent.credential_id]);
        let (_, default_invitation) = controller
            .issue_invitation(AttachmentRole::Observer, Some(10_000))
            .await
            .unwrap();
        assert_eq!(
            default_invitation.scopes,
            default_participant_scopes(AttachmentRole::Observer)
        );
        assert!(default_invitation.expires_ms - default_invitation.issued_ms <= 10_000);

        let (before_redemption, _) = controller
            .issue_invitation(AttachmentRole::Observer, Some(10_000))
            .await
            .unwrap();
        controller
            .revoke_delegated_invitation(&before_redemption)
            .await
            .unwrap();
        let revoked_before =
            RemoteBrainClient::new_with_invitation(target.clone(), before_redemption).unwrap();
        assert!(revoked_before
            .redeem_invitation("revoked-before")
            .await
            .is_err());

        let (after_redemption, _) = controller
            .issue_invitation(AttachmentRole::Observer, Some(10_000))
            .await
            .unwrap();
        let redeemed =
            RemoteBrainClient::new_with_invitation(target.clone(), after_redemption.clone())
                .unwrap();
        redeemed.redeem_invitation("revoked-after").await.unwrap();
        let redeemed_token = redeemed
            .credential
            .lock()
            .await
            .as_ref()
            .unwrap()
            .token
            .clone();
        controller
            .revoke_delegated_invitation(&after_redemption)
            .await
            .unwrap();
        assert!(authority
            .verify(&redeemed_token, unix_epoch_millis())
            .is_err());

        let control = [BrainCredentialScope::BrainControl]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let (sibling_token, sibling_claims) = controller
            .issue_credential(
                "sibling-controller",
                AttachmentRole::Driver,
                control.clone(),
                Some(20_000),
            )
            .await
            .unwrap();
        let (other_token, other_claims) = controller
            .issue_credential(
                "other-controller",
                AttachmentRole::Driver,
                control,
                Some(20_000),
            )
            .await
            .unwrap();
        let sibling =
            RemoteBrainClient::new_with_invitation(target.clone(), controller_invitation.clone())
                .unwrap();
        *sibling.credential.lock().await = Some(RemoteBrainCredential {
            token: sibling_token.clone(),
            claims: sibling_claims.clone(),
        });
        assert_eq!(
            sibling
                .http
                .delete(
                    sibling
                        .target
                        .delegated_credential_url(other_claims.credential_id)
                )
                .bearer_auth(&sibling_token)
                .json(&serde_json::json!({"credential": other_token.clone()}))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        assert_eq!(
            sibling
                .http
                .delete(
                    sibling
                        .target
                        .delegated_credential_url(invitation.invitation_id),
                )
                .bearer_auth(&sibling_token)
                .json(&serde_json::json!({"invitation": invitation_token}))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        let parent_token = controller
            .credential
            .lock()
            .await
            .as_ref()
            .unwrap()
            .token
            .clone();
        assert_eq!(
            sibling
                .http
                .delete(
                    sibling
                        .target
                        .delegated_credential_url(parent.credential_id)
                )
                .bearer_auth(&sibling_token)
                .json(&serde_json::json!({"credential": parent_token}))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        let controller_invitation_claims = super::super::credential::verify_portable_invitation(
            &controller_invitation,
            unix_epoch_millis(),
        )
        .unwrap()
        .0;
        assert_eq!(
            sibling
                .http
                .delete(
                    sibling
                        .target
                        .delegated_credential_url(controller_invitation_claims.invitation_id),
                )
                .bearer_auth(&sibling_token)
                .json(&serde_json::json!({"invitation": controller_invitation.clone()}))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );

        let bound_parent_token = authority
            .issue(
                BrainCredentialRequest {
                    issuer: "fixture.local".into(),
                    subject: "bound-admin".into(),
                    brain_id: snapshot.brain_id,
                    brain: "shared".into(),
                    environment_generation: snapshot.environment.generation,
                    role: AttachmentRole::Driver,
                    scopes: [
                        BrainCredentialScope::BrainAttach,
                        BrainCredentialScope::BrainControl,
                        BrainCredentialScope::EnvironmentAdmin,
                    ]
                    .into_iter()
                    .collect(),
                    delegation_chain: Vec::new(),
                    ttl_ms: 60_000,
                },
                now,
            )
            .unwrap();
        let bound_parent = authority.verify(&bound_parent_token, now).unwrap();
        let (bound_token, bound_claims) = authority
            .bind_attachment(
                &bound_parent,
                AttachmentId(uuid::Uuid::new_v4()),
                super::super::store::ConnectionId(uuid::Uuid::new_v4()),
                now,
            )
            .unwrap();
        assert!(bound_claims.permits(BrainCredentialScope::BrainControl));
        assert!(bound_claims.permits(BrainCredentialScope::EnvironmentAdmin));
        for (method, url, body) in [
            (
                reqwest::Method::POST,
                controller.target.credentials_url(),
                serde_json::json!({
                    "subject": "forbidden-child",
                    "role": AttachmentRole::Observer,
                    "scopes": [BrainCredentialScope::BrainRead],
                    "ttl_ms": 1_000,
                }),
            ),
            (
                reqwest::Method::POST,
                controller.target.invitations_url(),
                serde_json::json!({
                    "role": AttachmentRole::Observer,
                    "ttl_ms": 1_000,
                }),
            ),
            (
                reqwest::Method::DELETE,
                controller
                    .target
                    .delegated_credential_url(other_claims.credential_id),
                serde_json::json!({"credential": other_token}),
            ),
            (
                reqwest::Method::DELETE,
                controller.target.http_url(),
                serde_json::json!({}),
            ),
        ] {
            assert_eq!(
                controller
                    .http
                    .request(method, url)
                    .bearer_auth(&bound_token)
                    .json(&body)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                reqwest::StatusCode::FORBIDDEN
            );
        }

        let escalation = [BrainCredentialScope::EnvironmentAdmin]
            .into_iter()
            .collect();
        assert!(controller
            .issue_credential(
                "observer-admin",
                AttachmentRole::Observer,
                escalation,
                Some(10_000),
            )
            .await
            .is_err());
        assert!(controller
            .issue_credential(
                "too-long",
                AttachmentRole::Observer,
                read.clone(),
                Some(300_000),
            )
            .await
            .is_err());

        let limited_scopes = [
            BrainCredentialScope::BrainRead,
            BrainCredentialScope::BrainAttach,
        ]
        .into_iter()
        .collect();
        let (limited_invitation, _) = authority
            .issue_invitation(
                BrainInvitationRequest {
                    issuer: "fixture.local".into(),
                    brain_id: snapshot.brain_id,
                    brain: "shared".into(),
                    environment_generation: snapshot.environment.generation,
                    role: AttachmentRole::Driver,
                    scopes: limited_scopes,
                    delegation_chain: Vec::new(),
                    ttl_ms: 60_000,
                },
                now,
            )
            .unwrap();
        let limited =
            RemoteBrainClient::new_with_invitation(target.clone(), limited_invitation).unwrap();
        limited.redeem_invitation("limited").await.unwrap();
        assert!(limited
            .issue_invitation(AttachmentRole::Observer, Some(10_000))
            .await
            .is_err());
        let limited_token = limited
            .credential
            .lock()
            .await
            .as_ref()
            .unwrap()
            .token
            .clone();
        assert_eq!(
            limited
                .http
                .post(limited.target.invitations_url())
                .bearer_auth(limited_token.clone())
                .json(&serde_json::json!({
                    "role": AttachmentRole::Observer,
                    "ttl_ms": 10_000,
                }))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        assert_eq!(
            limited
                .http
                .delete(limited.target.http_url())
                .bearer_auth(limited_token)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );

        for (brain_id, generation, label) in [
            (
                BrainId(uuid::Uuid::new_v4()),
                snapshot.environment.generation,
                "wrong-brain",
            ),
            (
                snapshot.brain_id,
                snapshot.environment.generation + 1,
                "wrong-generation",
            ),
        ] {
            let token = authority
                .issue(
                    BrainCredentialRequest {
                        issuer: "fixture.local".into(),
                        subject: label.into(),
                        brain_id,
                        brain: "shared".into(),
                        environment_generation: generation,
                        role: AttachmentRole::Driver,
                        scopes: [BrainCredentialScope::BrainControl].into_iter().collect(),
                        delegation_chain: Vec::new(),
                        ttl_ms: 60_000,
                    },
                    now,
                )
                .unwrap();
            let claims = authority.verify(&token, now).unwrap();
            let hostile = RemoteBrainClient::new_with_invitation(
                target.clone(),
                controller_invitation.clone(),
            )
            .unwrap();
            *hostile.credential.lock().await = Some(RemoteBrainCredential { token, claims });
            assert!(hostile
                .issue_credential(label, AttachmentRole::Observer, read.clone(), Some(10_000))
                .await
                .is_err());
        }

        let expired_token = authority
            .issue(
                BrainCredentialRequest {
                    issuer: "fixture.local".into(),
                    subject: "expired".into(),
                    brain_id: snapshot.brain_id,
                    brain: "shared".into(),
                    environment_generation: snapshot.environment.generation,
                    role: AttachmentRole::Driver,
                    scopes: [BrainCredentialScope::BrainControl].into_iter().collect(),
                    delegation_chain: Vec::new(),
                    ttl_ms: 1,
                },
                now.saturating_sub(10),
            )
            .unwrap();
        let expired_claims = authority
            .verify(&expired_token, now.saturating_sub(10))
            .unwrap();
        let expired =
            RemoteBrainClient::new_with_invitation(target.clone(), controller_invitation.clone())
                .unwrap();
        *expired.credential.lock().await = Some(RemoteBrainCredential {
            token: expired_token,
            claims: expired_claims,
        });
        assert!(expired
            .issue_credential("expired-child", AttachmentRole::Observer, read, Some(1))
            .await
            .is_err());

        let ancestor_token = authority
            .issue(
                BrainCredentialRequest {
                    issuer: "fixture.local".into(),
                    subject: "ancestor".into(),
                    brain_id: snapshot.brain_id,
                    brain: "shared".into(),
                    environment_generation: snapshot.environment.generation,
                    role: AttachmentRole::Driver,
                    scopes: [BrainCredentialScope::BrainControl].into_iter().collect(),
                    delegation_chain: Vec::new(),
                    ttl_ms: 60_000,
                },
                now,
            )
            .unwrap();
        let ancestor = authority.verify(&ancestor_token, now).unwrap();
        let revoked_child_token = authority
            .issue(
                BrainCredentialRequest {
                    issuer: "fixture.local".into(),
                    subject: "revoked-child".into(),
                    brain_id: snapshot.brain_id,
                    brain: "shared".into(),
                    environment_generation: snapshot.environment.generation,
                    role: AttachmentRole::Driver,
                    scopes: [BrainCredentialScope::BrainControl].into_iter().collect(),
                    delegation_chain: vec![ancestor.credential_id],
                    ttl_ms: 30_000,
                },
                now,
            )
            .unwrap();
        let revoked_child_claims = authority.verify(&revoked_child_token, now).unwrap();
        let revoked_child =
            RemoteBrainClient::new_with_invitation(target, controller_invitation).unwrap();
        *revoked_child.credential.lock().await = Some(RemoteBrainCredential {
            token: revoked_child_token,
            claims: revoked_child_claims,
        });
        authority.revoke(ancestor.credential_id).unwrap();
        let read = [BrainCredentialScope::BrainRead].into_iter().collect();
        assert!(revoked_child
            .issue_credential("descendant", AttachmentRole::Observer, read, Some(1_000))
            .await
            .is_err());

        controller.archive("controller").await.unwrap();
        server.abort();
    }

    #[test]
    #[ignore = "requires an owned listener at FINCH_TEST_BRAIN_ADDR"]
    fn live_remote_creation_is_explicit_and_environment_owned() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let brain = format!("codex-create-{}", &uuid::Uuid::new_v4().to_string()[..8]);
            let target = isolated_live_brain_target(&brain);
            let client = RemoteBrainClient::new(target, isolated_live_password()).unwrap();
            let created = client.create().await.unwrap();
            assert_eq!(created.name, brain);
            assert_eq!(created.revision, 0);
            assert!(created.events.is_empty());
            assert!(client.create().await.is_err());
            client.archive("codex-create@localhost").await.unwrap();
        });
    }

    #[tokio::test]
    #[ignore = "requires an owned listener at FINCH_TEST_BRAIN_ADDR"]
    async fn live_invitation_issues_redeems_attaches_and_cannot_be_replayed() {
        let brain = format!("codex-invite-live-{}", uuid::Uuid::new_v4());
        let target = isolated_live_brain_target(&brain);
        let owner = RemoteBrainClient::new(target.clone(), isolated_live_password()).unwrap();
        owner.create().await.unwrap();
        let (invitation, _) = owner
            .issue_invitation(AttachmentRole::Observer, Some(60_000))
            .await
            .unwrap();

        let mut guest =
            RemoteBrainClient::new_with_invitation(target.clone(), invitation.clone()).unwrap();
        let (role, _) = guest
            .attach_invited_persistent("invited-observer@localhost", "invite-live")
            .await
            .unwrap();
        assert_eq!(role, AttachmentRole::Observer);
        let mut events = guest.watch().await.unwrap();
        assert!(matches!(
            events.recv().await.unwrap(),
            BrainWireMessage::Snapshot { .. }
        ));

        let replay = RemoteBrainClient::new_with_invitation(target, invitation).unwrap();
        assert!(replay
            .redeem_invitation("different-subject@localhost")
            .await
            .is_err());
        guest.disconnect().await.unwrap();
        owner.archive("invite-owner@localhost").await.unwrap();
    }

    #[tokio::test]
    async fn remote_binary_session_correlates_mutations_while_streaming_events() {
        use crate::brain::store::{BrainEvent, ConnectionId};
        use crate::ipc::brain_codec::{
            BrainRemoteCommandKind, BrainRemoteEnvelope, BrainRemoteReply,
        };
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let brain_id = BrainId(uuid::Uuid::new_v4());
        let attachment = BrainAttachment {
            attachment_id: AttachmentId(uuid::Uuid::new_v4()),
            subject: "alice@laptop.local".into(),
            role: AttachmentRole::Driver,
            acknowledged_seq: 0,
            connected: true,
            connection_id: Some(ConnectionId(uuid::Uuid::new_v4())),
        };
        let streamed = BrainEvent {
            schema_version: 2,
            brain_id,
            seq: 1,
            environment_generation: 1,
            sender: "bob@desktop.local".into(),
            created_ms: 10,
            run_id: None,
            mutation: None,
            kind: BrainEventKind::Prompt {
                text: "hello from another console".into(),
            },
        };
        let fixture_attachment = attachment.clone();
        let fixture_event = streamed.clone();
        let fixture = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let projection = BrainRemoteEnvelope::Projection(BrainWireMessage::Event {
                event: fixture_event.clone(),
            });
            socket
                .send(Message::Binary(
                    crate::ipc::brain_codec::encode_brain_remote_envelope(&projection)
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();

            let submit = socket.next().await.unwrap().unwrap().into_data();
            let BrainRemoteEnvelope::Command(submit) =
                crate::ipc::brain_codec::decode_brain_remote_envelope(&submit).unwrap()
            else {
                panic!("expected submit command")
            };
            assert!(matches!(
                submit.kind,
                BrainRemoteCommandKind::Submit(BrainEventKind::Prompt { ref text })
                    if text == "inspect it"
            ));
            let mutation = submit
                .mutation
                .as_ref()
                .expect("submit is a durable mutation");
            assert_eq!(mutation.brain_id, brain_id);
            assert_eq!(mutation.expected_revision, 1);
            assert_eq!(mutation.environment_generation, 1);
            let reply = BrainRemoteEnvelope::Reply(BrainRemoteReply::Submitted {
                request_id: submit.request_id,
                accepted: fixture_event,
                run: None,
                result: None,
            });
            socket
                .send(Message::Binary(
                    crate::ipc::brain_codec::encode_brain_remote_envelope(&reply)
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();

            let acknowledge = socket.next().await.unwrap().unwrap().into_data();
            let BrainRemoteEnvelope::Command(acknowledge) =
                crate::ipc::brain_codec::decode_brain_remote_envelope(&acknowledge).unwrap()
            else {
                panic!("expected acknowledge command")
            };
            assert_eq!(acknowledge.kind, BrainRemoteCommandKind::Acknowledge(1));
            assert!(acknowledge.mutation.is_none());
            let mut acknowledged = fixture_attachment;
            acknowledged.acknowledged_seq = 1;
            let reply = BrainRemoteEnvelope::Reply(BrainRemoteReply::Acknowledged {
                request_id: acknowledge.request_id,
                attachment: acknowledged,
            });
            socket
                .send(Message::Binary(
                    crate::ipc::brain_codec::encode_brain_remote_envelope(&reply)
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();

            let request_handoff = socket.next().await.unwrap().unwrap().into_data();
            let BrainRemoteEnvelope::Command(request_handoff) =
                crate::ipc::brain_codec::decode_brain_remote_envelope(&request_handoff).unwrap()
            else {
                panic!("expected runner handoff request")
            };
            let BrainRemoteCommandKind::RequestRunnerHandoff {
                target_subject,
                expected_lease_id,
                environment_generation,
                ttl_ms,
            } = request_handoff.kind
            else {
                panic!("expected runner handoff request")
            };
            assert_eq!(target_subject, "runner-b@box.local");
            assert_eq!(environment_generation, 1);
            assert_eq!(ttl_ms, 30_000);
            let handoff = crate::brain::store::BrainRunnerHandoff {
                handoff_id: crate::brain::store::RunnerHandoffId(uuid::Uuid::new_v4()),
                from_lease_id: expected_lease_id,
                requested_by: "alice@laptop.local".into(),
                target_subject,
                environment_generation,
                requested_ms: 10,
                expires_ms: 20,
            };
            let reply = BrainRemoteEnvelope::Reply(BrainRemoteReply::HandoffRequested {
                request_id: request_handoff.request_id,
                handoff: handoff.clone(),
            });
            socket
                .send(Message::Binary(
                    crate::ipc::brain_codec::encode_brain_remote_envelope(&reply)
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();

            let cancel_handoff = socket.next().await.unwrap().unwrap().into_data();
            let BrainRemoteEnvelope::Command(cancel_handoff) =
                crate::ipc::brain_codec::decode_brain_remote_envelope(&cancel_handoff).unwrap()
            else {
                panic!("expected runner handoff cancellation")
            };
            assert_eq!(
                cancel_handoff.kind,
                BrainRemoteCommandKind::CancelRunnerHandoff(handoff.handoff_id)
            );
            let reply = BrainRemoteEnvelope::Reply(BrainRemoteReply::HandoffCancelled {
                request_id: cancel_handoff.request_id,
            });
            socket
                .send(Message::Binary(
                    crate::ipc::brain_codec::encode_brain_remote_envelope(&reply)
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();

            let detach = socket.next().await.unwrap().unwrap().into_data();
            let BrainRemoteEnvelope::Command(detach) =
                crate::ipc::brain_codec::decode_brain_remote_envelope(&detach).unwrap()
            else {
                panic!("expected detach command")
            };
            assert_eq!(detach.kind, BrainRemoteCommandKind::Detach);
            let reply = BrainRemoteEnvelope::Reply(BrainRemoteReply::Detached {
                request_id: detach.request_id,
            });
            socket
                .send(Message::Binary(
                    crate::ipc::brain_codec::encode_brain_remote_envelope(&reply)
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
            socket.close(None).await.unwrap();
        });

        let target = RemoteBrainTarget {
            brain: "shared".into(),
            machine: "fixture".into(),
            address: address.to_string(),
            secure: false,
        };
        let mut client = RemoteBrainClient::new(target, "unused").unwrap();
        client.attachment = Some(attachment.clone());
        *client.credential.lock().await = Some(RemoteBrainCredential {
            token: "scoped-token".into(),
            claims: crate::brain::credential::BrainCredentialClaims {
                version: 1,
                credential_id: uuid::Uuid::new_v4(),
                issuer: "fixture".into(),
                subject: "alice@laptop.local".into(),
                brain_id,
                brain: "shared".into(),
                environment_generation: 1,
                role: AttachmentRole::Driver,
                scopes: super::super::credential::default_participant_scopes(
                    AttachmentRole::Driver,
                ),
                attachment_id: Some(attachment.attachment_id),
                connection_id: attachment.connection_id,
                delegation_chain: Vec::new(),
                issued_ms: 0,
                expires_ms: u64::MAX,
            },
        });

        let mut events = client.watch().await.unwrap();
        assert_eq!(
            events.recv().await.unwrap(),
            BrainWireMessage::Event { event: streamed }
        );
        let submit_kind = BrainEventKind::Prompt {
            text: "inspect it".into(),
        };
        let submit_handle = BrainMutationHandle {
            idempotency_key: uuid::Uuid::new_v4(),
            brain_id,
            attachment_id: attachment.attachment_id,
            expected_revision: 1,
            environment_generation: 1,
            command_sha256: crate::ipc::brain_codec::brain_remote_command_fingerprint(
                &BrainRemoteCommandKind::Submit(submit_kind.clone()),
            )
            .unwrap(),
        };
        client
            .push_with_handle(submit_kind, &submit_handle)
            .await
            .unwrap();
        client.acknowledge(1).await.unwrap();
        assert_eq!(client.attachment().unwrap().acknowledged_seq, 1);
        let lease_id = crate::brain::store::RunnerLeaseId(uuid::Uuid::new_v4());
        let handoff_kind = BrainRemoteCommandKind::RequestRunnerHandoff {
            target_subject: "runner-b@box.local".into(),
            expected_lease_id: lease_id,
            environment_generation: 1,
            ttl_ms: 30_000,
        };
        let handoff_handle = BrainMutationHandle {
            idempotency_key: uuid::Uuid::new_v4(),
            brain_id,
            attachment_id: attachment.attachment_id,
            expected_revision: 1,
            environment_generation: 1,
            command_sha256: crate::ipc::brain_codec::brain_remote_command_fingerprint(
                &handoff_kind,
            )
            .unwrap(),
        };
        let handoff = client
            .request_runner_handoff_with_handle(
                "runner-b@box.local",
                lease_id,
                1,
                30_000,
                &handoff_handle,
            )
            .await
            .unwrap();
        let cancel_kind = BrainRemoteCommandKind::CancelRunnerHandoff(handoff.handoff_id);
        let cancel_handle = BrainMutationHandle {
            idempotency_key: uuid::Uuid::new_v4(),
            brain_id,
            attachment_id: attachment.attachment_id,
            expected_revision: 1,
            environment_generation: 1,
            command_sha256: crate::ipc::brain_codec::brain_remote_command_fingerprint(&cancel_kind)
                .unwrap(),
        };
        client
            .cancel_runner_handoff_with_handle(handoff.handoff_id, &cancel_handle)
            .await
            .unwrap();
        client.disconnect().await.unwrap();
        fixture.await.unwrap();
    }

    #[tokio::test]
    async fn remote_mutation_retry_preserves_idempotency_key_across_reconnect() {
        use crate::brain::store::{BrainEvent, ConnectionId};
        use crate::ipc::brain_codec::{BrainRemoteEnvelope, BrainRemoteReply};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let brain_id = BrainId(uuid::Uuid::new_v4());
        let attachment = BrainAttachment {
            attachment_id: AttachmentId(uuid::Uuid::new_v4()),
            subject: "alice@laptop.local".into(),
            role: AttachmentRole::Driver,
            acknowledged_seq: 0,
            connected: true,
            connection_id: Some(ConnectionId(uuid::Uuid::new_v4())),
        };
        let projected = BrainEvent {
            schema_version: 13,
            brain_id,
            seq: 1,
            environment_generation: 1,
            sender: "daemon".into(),
            created_ms: 10,
            run_id: None,
            mutation: None,
            kind: BrainEventKind::ParticipantMessage {
                text: "ready".into(),
            },
        };
        let fixture_event = projected.clone();
        let fixture = tokio::spawn(async move {
            let mut original_key = None;
            for attempt in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let projection = BrainRemoteEnvelope::Projection(BrainWireMessage::Event {
                    event: fixture_event.clone(),
                });
                socket
                    .send(Message::Binary(
                        crate::ipc::brain_codec::encode_brain_remote_envelope(&projection)
                            .unwrap()
                            .into(),
                    ))
                    .await
                    .unwrap();
                let bytes = socket.next().await.unwrap().unwrap().into_data();
                let BrainRemoteEnvelope::Command(command) =
                    crate::ipc::brain_codec::decode_brain_remote_envelope(&bytes).unwrap()
                else {
                    panic!("expected retried mutation")
                };
                let key = command
                    .mutation
                    .as_ref()
                    .expect("prompt submission has mutation metadata")
                    .idempotency_key;
                if let Some(original) = original_key {
                    assert_eq!(key, original);
                } else {
                    original_key = Some(key);
                }
                if attempt == 0 {
                    socket.close(None).await.unwrap();
                    continue;
                }
                let reply = BrainRemoteEnvelope::Reply(BrainRemoteReply::Submitted {
                    request_id: command.request_id,
                    accepted: fixture_event.clone(),
                    run: None,
                    result: None,
                });
                socket
                    .send(Message::Binary(
                        crate::ipc::brain_codec::encode_brain_remote_envelope(&reply)
                            .unwrap()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }
        });

        let target = RemoteBrainTarget {
            brain: "shared".into(),
            machine: "fixture".into(),
            address: address.to_string(),
            secure: false,
        };
        let mut client = RemoteBrainClient::new(target, "unused").unwrap();
        client.attachment = Some(attachment.clone());
        *client.credential.lock().await = Some(RemoteBrainCredential {
            token: "scoped-token".into(),
            claims: crate::brain::credential::BrainCredentialClaims {
                version: 1,
                credential_id: uuid::Uuid::new_v4(),
                issuer: "fixture".into(),
                subject: attachment.subject.clone(),
                brain_id,
                brain: "shared".into(),
                environment_generation: 1,
                role: attachment.role,
                scopes: super::super::credential::default_participant_scopes(attachment.role),
                attachment_id: Some(attachment.attachment_id),
                connection_id: attachment.connection_id,
                delegation_chain: Vec::new(),
                issued_ms: 0,
                expires_ms: u64::MAX,
            },
        });

        let mut first_events = client.watch().await.unwrap();
        assert_eq!(
            first_events.recv().await,
            Some(BrainWireMessage::Event {
                event: projected.clone()
            })
        );
        let kind =
            crate::ipc::brain_codec::BrainRemoteCommandKind::Submit(BrainEventKind::Prompt {
                text: "once".into(),
            });
        let handle = BrainMutationHandle {
            idempotency_key: uuid::Uuid::new_v4(),
            brain_id,
            attachment_id: attachment.attachment_id,
            expected_revision: projected.seq,
            environment_generation: projected.environment_generation,
            command_sha256: crate::ipc::brain_codec::brain_remote_command_fingerprint(&kind)
                .unwrap(),
        };
        assert!(client
            .send_remote_command_with_handle(kind.clone(), Some(&handle))
            .await
            .is_err());
        while client.connection.lock().await.is_some() {
            tokio::task::yield_now().await;
        }
        drop(first_events);
        let mut second_events = client.watch().await.unwrap();
        assert!(second_events.recv().await.is_some());
        client
            .send_remote_command_with_handle(kind, Some(&handle))
            .await
            .unwrap();
        fixture.await.unwrap();
    }

    #[tokio::test]
    async fn production_server_deduplicates_lost_replies_across_daemon_restarts() {
        use crate::brain::store::{
            BrainEventKind, BrainRunKind, BrainScheduleDeliveryPolicy, ProgramLanguage,
        };

        async fn start(
            root: &std::path::Path,
            credentials: crate::brain::credential::BrainCredentialAuthority,
            environment_generation: u64,
        ) -> (
            RemoteBrainTarget,
            tokio::task::JoinHandle<()>,
            mpsc::UnboundedReceiver<crate::server::RunnerRequest>,
            super::super::store::BrainRunnerLease,
            crate::server::BrainLifecycleService,
        ) {
            let store = crate::brain::store::BrainStore::with_test_environment_generation(
                "box.local",
                Some(root.to_path_buf()),
                environment_generation,
            );
            store.snapshot("shared").unwrap();
            let server = std::sync::Arc::new(
                crate::server::AgentServer::for_brain_protocol_test(
                    store,
                    credentials,
                    "test-password".into(),
                    root,
                )
                .unwrap(),
            );
            let lifecycle = crate::server::BrainLifecycleService::from_server(&server);
            let environment = lifecycle.snapshot("shared").unwrap().environment;
            let lease = match lifecycle.snapshot("shared").unwrap().runner_lease {
                Some(lease)
                    if lease.environment_generation == environment.generation
                        && lease.expires_ms > unix_epoch_millis() =>
                {
                    lease
                }
                stale => {
                    if let Some(stale) = stale {
                        lifecycle.release_runner("shared", stale.lease_id).unwrap();
                    }
                    lifecycle
                        .acquire_runner("shared", "runner", &environment, None, 60_000)
                        .unwrap()
                }
            };
            let (runner_tx, runner_rx) = mpsc::unbounded_channel();
            lifecycle.register_test_runner("shared", lease.lease_id, runner_tx);
            let app = crate::server::handlers::create_router(server);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                .unwrap();
            });
            (
                RemoteBrainTarget {
                    brain: "shared".into(),
                    machine: "box.local".into(),
                    address: address.to_string(),
                    secure: false,
                },
                task,
                runner_rx,
                lease,
                lifecycle,
            )
        }

        async fn attach(
            target: RemoteBrainTarget,
            attachment_id: Option<AttachmentId>,
        ) -> (
            RemoteBrainClient,
            mpsc::UnboundedReceiver<BrainWireMessage>,
            BrainAttachment,
        ) {
            let mut client = RemoteBrainClient::new(target, "test-password").unwrap();
            client
                .authorize_runner_handoff_control("alice", AttachmentRole::Driver)
                .await
                .unwrap();
            let attachment = client
                .attach("alice", AttachmentRole::Driver, attachment_id)
                .await
                .unwrap();
            let mut events = client.watch().await.unwrap();
            assert!(matches!(
                events.recv().await,
                Some(BrainWireMessage::Snapshot { .. })
            ));
            (client, events, attachment)
        }

        let temp = tempfile::tempdir().unwrap();
        let credentials = crate::brain::credential::BrainCredentialAuthority::ephemeral([83; 32]);
        let (target, daemon, mut runner_rx, _, _) =
            start(temp.path(), credentials.clone(), 1).await;
        let (client, events, attachment) = attach(target, None).await;
        let source_program = "(emit \"exactly once\")";
        let program = BrainEventKind::Program {
            language: ProgramLanguage::Lisp,
            source: source_program.into(),
        };
        let handle = client.prepare_push_mutation(&program).await.unwrap();
        let effect_execution_id = uuid::Uuid::new_v4();
        tokio::spawn(async move {
            let crate::server::RunnerRequest::Program(request) = runner_rx.recv().await.unwrap()
            else {
                panic!("expected runner Program request")
            };
            let runtime = crate::runtime::ProgramRuntime::new();
            let outcome = runtime
                .submit_typed_only(crate::runtime::ProgramSubmission {
                    language: crate::programs::ProgramLanguage::Lisp,
                    source_id: Some("remote-idempotency".into()),
                    // The fixture runner supplies an authoritative checkpoint and
                    // the acknowledged effect journal separately, as a real
                    // frontend runner does after executing the submitted effect.
                    source: "(define (checkpoint) : int 1)".into(),
                    intent: "remote idempotency fixture".into(),
                    effect: crate::programs::ExecutionEffect::Pure,
                    declared_capabilities: Vec::new(),
                    manifest_generation: runtime.manifest_generation(),
                    expected_revision: Some(runtime.revision()),
                    budget: None,
                })
                .await
                .unwrap();
            let checkpoint = runtime
                .revision_history()
                .unwrap()
                .into_iter()
                .find(|snapshot| snapshot.revision == outcome.output_revision)
                .and_then(|snapshot| snapshot.checkpoint)
                .unwrap();
            request
                .response_tx
                .send(Ok(crate::server::RunnerProgramResult {
                    output: "completed exactly once".into(),
                    runtime_revision: outcome.output_revision,
                    checkpoint,
                    effect_journal: vec![crate::server::RunnerEffectRecord {
                        execution_id: effect_execution_id,
                        entry: crate::vm::EffectJournalEntry {
                            effect: crate::vm::VmSideEffect {
                                protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
                                sequence: 0,
                                requirement: crate::vm::CapabilityRequirement {
                                    capability: crate::vm::CapabilityKind::SessionEmit,
                                    selector: crate::vm::ResourceSelector::None,
                                },
                                event: crate::vm::HostSideEffect::Emit {
                                    text: "exactly once".into(),
                                },
                                output: Vec::new(),
                                origin: crate::vm::SourceOrigin::generated("remote-idempotency"),
                            },
                            state: crate::vm::EffectJournalState::Acknowledged {
                                values: Vec::new(),
                            },
                        },
                    }],
                }))
                .unwrap();
        });
        crate::server::handlers::drop_next_remote_brain_reply_after_commit();
        assert!(client
            .push_with_handle(program.clone(), &handle)
            .await
            .is_err());
        drop(events);
        daemon.abort();
        let _ = daemon.await;

        let (target, daemon, _runner_rx, lease, _) =
            start(temp.path(), credentials.clone(), 1).await;
        let (client, events, rebound) = attach(target, Some(attachment.attachment_id)).await;
        assert_eq!(rebound.attachment_id, handle.attachment_id);
        let reply = client
            .send_remote_command_with_handle(
                crate::ipc::brain_codec::BrainRemoteCommandKind::Submit(program.clone()),
                Some(&handle),
            )
            .await
            .unwrap();
        let crate::ipc::brain_codec::BrainRemoteReply::Submitted {
            result: Some(result),
            run: Some(completed_run),
            ..
        } = reply
        else {
            panic!("terminal Program replay omitted its result")
        };
        assert!(
            matches!(result.kind, BrainEventKind::Result { ref output, .. }
            if output == "completed exactly once")
        );
        assert_eq!(
            completed_run.status,
            super::super::store::BrainRunStatus::Completed
        );
        let snapshot = client.snapshot().await.unwrap();
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| {
                    event
                        .mutation
                        .as_ref()
                        .is_some_and(|receipt| receipt.mutation_id == handle.idempotency_key)
                })
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(event.kind,
            BrainEventKind::Program { ref source, .. } if source == source_program))
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(&event.kind,
            BrainEventKind::EffectRecorded { execution_id, .. }
                if *execution_id == effect_execution_id))
                .count(),
            0,
            "runner summaries cannot forge schema-15 audit provenance"
        );
        assert_eq!(
            snapshot
                .runs
                .iter()
                .filter(|run| run.kind == BrainRunKind::Interactive
                    && run.request_seq
                        == snapshot
                            .events
                            .iter()
                            .find(|event| {
                                event.mutation.as_ref().is_some_and(|receipt| {
                                    receipt.mutation_id == handle.idempotency_key
                                })
                            })
                            .unwrap()
                            .seq)
                .count(),
            1
        );

        assert!(client
            .push_with_handle(
                BrainEventKind::Program {
                    language: ProgramLanguage::Lisp,
                    source: "conflict".into(),
                },
                &handle,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("different command"));
        let mut stale = handle.clone();
        stale.expected_revision += 1;
        assert!(client.push_with_handle(program, &stale).await.is_err());

        let environment_generation = snapshot.environment.generation;
        let handoff_handle = client
            .prepare_runner_handoff_mutation(
                "runner-b",
                lease.lease_id,
                environment_generation,
                30_000,
            )
            .await
            .unwrap();
        crate::server::handlers::drop_next_remote_brain_reply_after_commit();
        assert!(client
            .request_runner_handoff_with_handle(
                "runner-b",
                lease.lease_id,
                environment_generation,
                30_000,
                &handoff_handle,
            )
            .await
            .is_err());
        drop(events);
        daemon.abort();
        let _ = daemon.await;

        let (target, daemon, _runner_rx, _, _) = start(temp.path(), credentials.clone(), 2).await;
        let (client, events, _) = attach(target, Some(attachment.attachment_id)).await;
        let handoff = client
            .request_runner_handoff_with_handle(
                "runner-b",
                lease.lease_id,
                environment_generation,
                30_000,
                &handoff_handle,
            )
            .await
            .unwrap();
        let mut stale_handoff = handoff_handle.clone();
        stale_handoff.expected_revision += 1;
        assert!(client
            .request_runner_handoff_with_handle(
                "runner-b",
                lease.lease_id,
                environment_generation,
                30_000,
                &stale_handoff,
            )
            .await
            .is_err());
        assert!(client
            .request_runner_handoff_with_handle(
                "runner-c",
                lease.lease_id,
                environment_generation,
                30_000,
                &handoff_handle,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("different command"));
        let cancel_handle = client
            .prepare_cancel_runner_handoff_mutation(handoff.handoff_id)
            .await
            .unwrap();
        crate::server::handlers::drop_next_remote_brain_reply_after_commit();
        assert!(client
            .cancel_runner_handoff_with_handle(handoff.handoff_id, &cancel_handle,)
            .await
            .is_err());
        drop(events);
        daemon.abort();
        let _ = daemon.await;

        let (target, daemon, _runner_rx, _, _) = start(temp.path(), credentials.clone(), 2).await;
        let (client, events, _) = attach(target, Some(attachment.attachment_id)).await;
        client
            .cancel_runner_handoff_with_handle(handoff.handoff_id, &cancel_handle)
            .await
            .unwrap();
        let mut stale_cancel = cancel_handle.clone();
        stale_cancel.environment_generation += 1;
        assert!(client
            .cancel_runner_handoff_with_handle(handoff.handoff_id, &stale_cancel,)
            .await
            .is_err());
        assert!(client
            .cancel_runner_handoff_with_handle(
                super::super::store::RunnerHandoffId(uuid::Uuid::new_v4()),
                &cancel_handle,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("different command"));
        let handoff_snapshot = client.snapshot().await.unwrap();
        assert!(handoff_snapshot.runner_handoff.is_none());
        for mutation_id in [
            handoff_handle.idempotency_key,
            cancel_handle.idempotency_key,
        ] {
            assert_eq!(
                handoff_snapshot
                    .events
                    .iter()
                    .filter(|event| {
                        event
                            .mutation
                            .as_ref()
                            .is_some_and(|receipt| receipt.mutation_id == mutation_id)
                    })
                    .count(),
                1
            );
        }

        let source = "(define (scheduled) : int 1)".to_string();
        let ceiling = crate::vm::EffectSet::default();
        let schedule_handle = client
            .prepare_create_schedule_mutation(
                ProgramLanguage::Lisp,
                &source,
                &ceiling,
                50_000,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .await
            .unwrap();
        crate::server::handlers::drop_next_remote_brain_reply_after_commit();
        assert!(client
            .create_schedule_with_handle(
                ProgramLanguage::Lisp,
                source.clone(),
                ceiling.clone(),
                50_000,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
                &schedule_handle,
            )
            .await
            .is_err());
        drop(events);
        daemon.abort();
        let _ = daemon.await;

        let (target, daemon, _runner_rx, _, _) = start(temp.path(), credentials.clone(), 2).await;
        let (client, _events, _) = attach(target, Some(attachment.attachment_id)).await;
        let schedule = client
            .create_schedule_with_handle(
                ProgramLanguage::Lisp,
                source,
                ceiling,
                50_000,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
                &schedule_handle,
            )
            .await
            .unwrap();
        let snapshot = client.snapshot().await.unwrap();
        assert_eq!(
            snapshot
                .schedules
                .iter()
                .filter(|item| { item.schedule_id == schedule.schedule_id })
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| {
                    event.mutation.as_ref().is_some_and(|receipt| {
                        receipt.mutation_id == schedule_handle.idempotency_key
                    })
                })
                .count(),
            1
        );
        let cancel_schedule_handle = client
            .prepare_cancel_schedule_mutation(schedule.schedule_id)
            .await
            .unwrap();
        crate::server::handlers::drop_next_remote_brain_reply_after_commit();
        assert!(client
            .cancel_schedule_with_handle(schedule.schedule_id, &cancel_schedule_handle,)
            .await
            .is_err());
        daemon.abort();
        let _ = daemon.await;

        let (target, daemon, mut runner_rx, _, lifecycle) =
            start(temp.path(), credentials.clone(), 2).await;
        let (client, events, current_attachment) =
            attach(target, Some(attachment.attachment_id)).await;
        assert!(client
            .cancel_schedule_with_handle(schedule.schedule_id, &cancel_schedule_handle,)
            .await
            .unwrap());

        let cancel_request = lifecycle
            .push_test_event(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "cancel remotely".into(),
                },
            )
            .unwrap();
        let cancellable = lifecycle
            .start_run_with_parent(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                cancel_request.seq,
                current_attachment.attachment_id,
                super::super::store::BrainRunStatus::Running,
                None,
            )
            .unwrap();
        let cancel_run_handle = client
            .prepare_cancel_run_mutation(cancellable.run_id)
            .await
            .unwrap();
        let cancellable_run_id = cancellable.run_id;
        tokio::spawn(async move {
            let crate::server::RunnerRequest::Cancel(request) = runner_rx.recv().await.unwrap()
            else {
                panic!("expected real runner cancellation")
            };
            assert_eq!(request.run_id, cancellable_run_id);
            request.response_tx.send(Ok(true)).unwrap();
        });
        crate::server::handlers::drop_next_remote_brain_reply_after_commit();
        assert!(client
            .cancel_run_with_handle(cancellable.run_id, &cancel_run_handle,)
            .await
            .is_err());
        drop(events);
        daemon.abort();
        let _ = daemon.await;

        let (target, daemon, mut runner_rx, _, lifecycle) =
            start(temp.path(), credentials.clone(), 2).await;
        let (client, events, current_attachment) =
            attach(target, Some(attachment.attachment_id)).await;
        let client = std::sync::Arc::new(client);
        assert_eq!(
            client
                .cancel_run_with_handle(cancellable.run_id, &cancel_run_handle,)
                .await
                .unwrap()
                .status,
            super::super::store::BrainRunStatus::Cancelled
        );

        let initialization_handle = client
            .prepare_schedule_initialization_mutation(75_000)
            .await
            .unwrap();
        let initialization = client
            .schedule_initialization_with_handle(75_000, &initialization_handle)
            .await
            .unwrap();
        assert!(initialization.module_identity.is_some());

        let concurrent_source = "(define (concurrent) : int 2)".to_string();
        let concurrent_ceiling = crate::vm::EffectSet::default();
        let concurrent_handle = client
            .prepare_create_schedule_mutation(
                ProgramLanguage::Lisp,
                &concurrent_source,
                &concurrent_ceiling,
                90_000,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .await
            .unwrap();
        let first = client.create_schedule_with_handle(
            ProgramLanguage::Lisp,
            concurrent_source.clone(),
            concurrent_ceiling.clone(),
            90_000,
            None,
            BrainScheduleDeliveryPolicy::Coalesce,
            &concurrent_handle,
        );
        let second = client.create_schedule_with_handle(
            ProgramLanguage::Lisp,
            concurrent_source,
            concurrent_ceiling,
            90_000,
            None,
            BrainScheduleDeliveryPolicy::Coalesce,
            &concurrent_handle,
        );
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.unwrap().schedule_id, second.unwrap().schedule_id);

        // A Prompt holds the Brain turn lane until its runner returns. A live
        // remote approval must therefore use the narrower approval mutation
        // lane, including when the exact same decision arrives concurrently.
        let live_prompt = BrainEventKind::Prompt {
            text: "wait for a remote approval".into(),
        };
        let live_prompt_handle = client.prepare_push_mutation(&live_prompt).await.unwrap();
        let (approval_ready_tx, approval_ready_rx) = tokio::sync::oneshot::channel();
        let live_lifecycle = lifecycle.clone();
        let live_runner = tokio::spawn(async move {
            let request = loop {
                match runner_rx.recv().await.unwrap() {
                    crate::server::RunnerRequest::Turn(request) => break request,
                    crate::server::RunnerRequest::ProjectMemory(projection) => {
                        projection.response_tx.send(Ok(0)).unwrap();
                    }
                    other => panic!("expected live Prompt turn, got {other:?}"),
                }
            };
            let approval_id = "live-remote-approval";
            let audience = request.approval_audience.clone();
            let registration = live_lifecycle
                .register_test_approval(request.request_seq, approval_id, audience.clone())
                .unwrap();
            live_lifecycle
                .push_test_event(
                    "shared",
                    "runner",
                    BrainEventKind::ApprovalRequested {
                        request_seq: request.request_seq,
                        approval_id: approval_id.into(),
                        approval_kind: "vm_capability".into(),
                        subject: "FileRead".into(),
                        audience: Some(audience.clone()),
                        detail: serde_json::json!({"path": "README.md"}),
                    },
                )
                .unwrap();
            approval_ready_tx
                .send((request.request_seq, audience.clone()))
                .unwrap();
            let decision = registration.wait().await.unwrap();
            let runtime = crate::runtime::ProgramRuntime::new();
            let outcome = runtime
                .submit_typed_only(crate::runtime::ProgramSubmission {
                    language: crate::programs::ProgramLanguage::Lisp,
                    source_id: Some("live-remote-approval".into()),
                    source: "(define (approved) : int 1)".into(),
                    intent: "finish approved remote Prompt".into(),
                    effect: crate::programs::ExecutionEffect::Pure,
                    declared_capabilities: Vec::new(),
                    manifest_generation: runtime.manifest_generation(),
                    expected_revision: Some(runtime.revision()),
                    budget: None,
                })
                .await
                .unwrap();
            let checkpoint = runtime
                .revision_history()
                .unwrap()
                .into_iter()
                .find(|snapshot| snapshot.revision == outcome.output_revision)
                .and_then(|snapshot| snapshot.checkpoint)
                .unwrap();
            request
                .response_tx
                .send(Ok(crate::server::RunnerTurnResult {
                    source: "(define (approved) : int 1)".into(),
                    language: ProgramLanguage::Lisp,
                    output: "approved remotely".into(),
                    continuation_messages: Vec::new(),
                    invocation_metadata: None,
                    turn_events: vec![
                        crate::server::RunnerTurnEvent::ApprovalRequested {
                            approval_id: approval_id.into(),
                            approval_kind: "vm_capability".into(),
                            subject: "FileRead".into(),
                            audience,
                            detail: serde_json::json!({"path": "README.md"}),
                        },
                        crate::server::RunnerTurnEvent::ApprovalDecided {
                            approval_id: approval_id.into(),
                            decision,
                        },
                    ],
                    runtime_revision: outcome.output_revision,
                    checkpoint,
                    effect_journal: Vec::new(),
                    commit_ack: None,
                }))
                .unwrap();
        });
        let prompt_client = client.clone();
        let prompt_handle = live_prompt_handle.clone();
        let mut prompt_submission = tokio::spawn(async move {
            prompt_client
                .push_with_handle(live_prompt, &prompt_handle)
                .await
        });
        let (request_seq, _) = tokio::select! {
            ready = approval_ready_rx => ready.unwrap(),
            ended = &mut prompt_submission => {
                panic!("live Prompt ended before requesting approval: {ended:?}")
            }
        };
        let approval_submission = async {
            let decision = BrainEventKind::ApprovalDecided {
                request_seq,
                approval_id: "live-remote-approval".into(),
                decision: serde_json::json!({"choice": "approve_once"}),
            };
            let handle = client.prepare_push_mutation(&decision).await.unwrap();
            let first = client.push_with_handle(decision.clone(), &handle);
            let second = client.push_with_handle(decision, &handle);
            let (first, second) = tokio::join!(first, second);
            assert_eq!(first.unwrap(), second.unwrap());
            handle
        };
        let (prompt_result, live_decision_handle) =
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::join!(prompt_submission, approval_submission)
            })
            .await
            .expect("remote approval deadlocked behind its originating Prompt");
        prompt_result.unwrap().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), live_runner)
            .await
            .expect("live runner did not finish after approval")
            .unwrap();
        let live_snapshot = client.snapshot().await.unwrap();
        assert_eq!(
            live_snapshot
                .events
                .iter()
                .filter(|event| matches!(
                    &event.kind, BrainEventKind::ApprovalDecided { approval_id, .. }
                        if approval_id == "live-remote-approval"
                ))
                .count(),
            1
        );
        assert_eq!(
            live_snapshot
                .events
                .iter()
                .filter(|event| event.mutation.as_ref().is_some_and(
                    |receipt| receipt.mutation_id == live_decision_handle.idempotency_key
                ))
                .count(),
            1
        );

        let before_approval = lifecycle
            .push_test_event(
                "shared",
                "provider",
                BrainEventKind::ParticipantMessage {
                    text: "approval fixture".into(),
                },
            )
            .unwrap();
        let approval_id = "remote-replay-approval";
        let approval_audience = crate::brain::store::BrainApprovalAudience {
            brain_id: client.snapshot().await.unwrap().brain_id,
            brain: "shared".into(),
            attachment_id: current_attachment.attachment_id,
            subject: current_attachment.subject.clone(),
            role: current_attachment.role,
            environment_generation: client.snapshot().await.unwrap().environment.generation,
        };
        lifecycle
            .push_test_event(
                "shared",
                "provider",
                BrainEventKind::ApprovalRequested {
                    request_seq: before_approval.seq,
                    approval_id: approval_id.into(),
                    approval_kind: "effect".into(),
                    subject: "fixture effect".into(),
                    audience: Some(approval_audience.clone()),
                    detail: serde_json::json!({"capability": "fixture"}),
                },
            )
            .unwrap();
        let _approval = lifecycle
            .register_test_approval(before_approval.seq, approval_id, approval_audience)
            .unwrap();
        let decision = BrainEventKind::ApprovalDecided {
            request_seq: before_approval.seq,
            approval_id: approval_id.into(),
            decision: serde_json::json!({"choice": "approve_once"}),
        };
        let decision_handle = client.prepare_push_mutation(&decision).await.unwrap();
        crate::server::handlers::drop_next_remote_brain_reply_after_commit();
        assert!(client
            .push_with_handle(decision.clone(), &decision_handle)
            .await
            .is_err());
        drop(events);
        daemon.abort();
        let _ = daemon.await;

        let (target, daemon, _runner_rx, _, _) = start(temp.path(), credentials, 2).await;
        let (client, _events, _) = attach(target, Some(attachment.attachment_id)).await;
        client
            .push_with_handle(decision, &decision_handle)
            .await
            .unwrap();
        let snapshot = client.snapshot().await.unwrap();
        for mutation_id in [
            cancel_schedule_handle.idempotency_key,
            cancel_run_handle.idempotency_key,
            initialization_handle.idempotency_key,
            concurrent_handle.idempotency_key,
            decision_handle.idempotency_key,
        ] {
            assert_eq!(
                snapshot
                    .events
                    .iter()
                    .filter(|event| event
                        .mutation
                        .as_ref()
                        .is_some_and(|receipt| receipt.mutation_id == mutation_id))
                    .count(),
                1
            );
        }
        daemon.abort();
        let _ = daemon.await;
    }

    #[tokio::test]
    async fn remote_initialization_client_uses_narrowed_websocket_authority() {
        use axum::{
            extract::{Path, Query, State, WebSocketUpgrade},
            http::{HeaderMap, StatusCode},
            response::{IntoResponse, Response},
            routing::get,
            Router,
        };
        use futures::StreamExt;

        #[derive(Clone)]
        struct Fixture {
            lifecycle: crate::server::BrainLifecycleService,
            credentials: super::super::credential::BrainCredentialAuthority,
        }

        #[derive(Deserialize)]
        struct Connection {
            attachment_id: uuid::Uuid,
            connection_id: uuid::Uuid,
        }

        async fn snapshot_route(
            State(fixture): State<Fixture>,
            Path(name): Path<String>,
        ) -> axum::Json<BrainSnapshot> {
            axum::Json(fixture.lifecycle.snapshot(&name).unwrap())
        }

        async fn websocket(
            State(fixture): State<Fixture>,
            headers: HeaderMap,
            Path(name): Path<String>,
            Query(connection): Query<Connection>,
            ws: WebSocketUpgrade,
        ) -> Response {
            let attachment_id = AttachmentId(connection.attachment_id);
            let connection_id = crate::brain::store::ConnectionId(connection.connection_id);
            let claims = match crate::server::handlers::authorize_pending_remote_attachment(
                &fixture.lifecycle,
                &fixture.credentials,
                &headers,
                &name,
                attachment_id,
                connection_id,
            ) {
                Ok(claims) => claims,
                Err(response) => return response,
            };
            let Ok(watch) = fixture.lifecycle.watch(&name, attachment_id, connection_id) else {
                return StatusCode::CONFLICT.into_response();
            };
            let initial = watch.snapshot;
            let lifecycle = fixture.lifecycle.clone();
            ws.on_upgrade(move |mut socket| async move {
                let envelope = crate::ipc::brain_codec::BrainRemoteEnvelope::Projection(
                    BrainWireMessage::Snapshot { brain: initial },
                );
                socket
                    .send(axum::extract::ws::Message::Binary(
                        crate::ipc::brain_codec::encode_brain_remote_envelope(&envelope)
                            .unwrap()
                            .into(),
                    ))
                    .await
                    .unwrap();
                while let Some(Ok(axum::extract::ws::Message::Binary(bytes))) = socket.next().await
                {
                    let Ok(crate::ipc::brain_codec::BrainRemoteEnvelope::Command(command)) =
                        crate::ipc::brain_codec::decode_brain_remote_envelope(&bytes)
                    else {
                        break;
                    };
                    let request_id = command.request_id;
                    let reply = match command.kind {
                        crate::ipc::brain_codec::BrainRemoteCommandKind::ScheduleInitialization {
                            next_due_ms,
                        } => crate::server::handlers::execute_authorized_remote_initialization(
                            &lifecycle,
                            &claims,
                            &name,
                            attachment_id,
                            connection_id,
                            request_id,
                            next_due_ms,
                            None,
                        ),
                        _ => break,
                    };
                    let envelope = crate::ipc::brain_codec::BrainRemoteEnvelope::Reply(reply);
                    socket
                        .send(axum::extract::ws::Message::Binary(
                            crate::ipc::brain_codec::encode_brain_remote_envelope(&envelope)
                                .unwrap()
                                .into(),
                        ))
                        .await
                        .unwrap();
                }
            })
            .into_response()
        }

        let store = crate::brain::store::BrainStore::with_root("box.local", None);
        let lifecycle = crate::server::BrainLifecycleService::new(
            store,
            crate::server::BrainRunnerBroker::default(),
            crate::server::BrainApprovalBroker::default(),
        );
        let credentials = super::super::credential::BrainCredentialAuthority::ephemeral([71; 32]);
        let snapshot = lifecycle.snapshot("shared").unwrap();
        let now_ms = unix_epoch_millis();
        let bind = |attachment: &BrainAttachment| {
            let role = attachment.role;
            let parent_token = credentials
                .issue(
                    super::super::credential::BrainCredentialRequest {
                        issuer: "box.local".into(),
                        subject: attachment.subject.clone(),
                        brain_id: snapshot.brain_id,
                        brain: "shared".into(),
                        environment_generation: snapshot.environment.generation,
                        role,
                        scopes: super::super::credential::default_participant_scopes(role),
                        delegation_chain: Vec::new(),
                        ttl_ms: 60_000,
                    },
                    now_ms,
                )
                .unwrap();
            let parent = credentials.verify(&parent_token, now_ms).unwrap();
            credentials
                .bind_attachment(
                    &parent,
                    attachment.attachment_id,
                    attachment.connection_id.unwrap(),
                    now_ms,
                )
                .unwrap()
        };
        let driver = lifecycle
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let sibling = lifecycle
            .attach("shared", "mallory", AttachmentRole::Driver, None)
            .unwrap();
        let consultant = lifecycle
            .attach("shared", "bob", AttachmentRole::Consultant, None)
            .unwrap();
        let (driver_token, driver_claims) = bind(&driver);
        let (sibling_token, sibling_claims) = bind(&sibling);
        let (consultant_token, consultant_claims) = bind(&consultant);

        let app = Router::new()
            .route("/v1/brains/named/:name", get(snapshot_route))
            .route("/v1/brains/named/:name/ws", get(websocket))
            .with_state(Fixture {
                lifecycle,
                credentials,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let target = RemoteBrainTarget {
            brain: "shared".into(),
            machine: "box.local".into(),
            address: address.to_string(),
            secure: false,
        };
        let make_client =
            |attachment: BrainAttachment,
             token: String,
             claims: super::super::credential::BrainCredentialClaims| {
                let mut client = RemoteBrainClient::new(target.clone(), "unused").unwrap();
                client.attachment = Some(attachment);
                (client, RemoteBrainCredential { token, claims })
            };

        let (stale, stale_credential) = make_client(driver.clone(), sibling_token, sibling_claims);
        *stale.credential.lock().await = Some(stale_credential);
        assert!(stale.watch().await.is_err());

        let (driver_client, driver_credential) = make_client(driver, driver_token, driver_claims);
        *driver_client.credential.lock().await = Some(driver_credential);
        let mut driver_events = driver_client.watch().await.unwrap();
        assert!(matches!(
            driver_events.recv().await.unwrap(),
            BrainWireMessage::Snapshot { .. }
        ));
        assert!(
            driver_client
                .schedule_initialization(1_000)
                .await
                .unwrap()
                .active
        );

        let (consultant_client, consultant_credential) =
            make_client(consultant, consultant_token, consultant_claims);
        *consultant_client.credential.lock().await = Some(consultant_credential);
        let mut consultant_events = consultant_client.watch().await.unwrap();
        assert!(matches!(
            consultant_events.recv().await.unwrap(),
            BrainWireMessage::Snapshot { .. }
        ));
        assert!(consultant_client
            .schedule_initialization(2_000)
            .await
            .is_err());
        server.abort();
    }

    #[tokio::test]
    #[ignore = "requires an owned listener at FINCH_TEST_BRAIN_ADDR"]
    async fn live_remote_binary_session_attaches_submits_acknowledges_and_detaches() {
        let brain = format!("codex-remote-binary-smoke-{}", uuid::Uuid::new_v4());
        let target = isolated_live_brain_target(&brain);
        let mut client = RemoteBrainClient::new(target, isolated_live_password()).unwrap();

        client
            .attach("codex-smoke@localhost", AttachmentRole::Driver, None)
            .await
            .unwrap();
        let mut events = client.watch().await.unwrap();
        let initial = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(initial, BrainWireMessage::Snapshot { .. }));

        client
            .push(BrainEventKind::Prompt {
                text: "remote binary lifecycle smoke".into(),
            })
            .await
            .unwrap();
        let prompt_seq = loop {
            let message = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
                .await
                .unwrap()
                .unwrap();
            if let BrainWireMessage::Event { event } = message {
                if matches!(event.kind, BrainEventKind::Prompt { .. }) {
                    break event.seq;
                }
            }
        };
        client.acknowledge(prompt_seq).await.unwrap();
        assert_eq!(client.attachment().unwrap().acknowledged_seq, prompt_seq);
        client.disconnect().await.unwrap();

        let detached = loop {
            let Some(message) =
                tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
                    .await
                    .unwrap()
            else {
                panic!("remote stream closed before projecting detach")
            };
            if let BrainWireMessage::Event { event } = message {
                if matches!(event.kind, BrainEventKind::ClientDetached { .. }) {
                    break true;
                }
            }
        };
        assert!(detached);

        let mut never_watched = RemoteBrainClient::new(client.target.clone(), "loopback").unwrap();
        never_watched
            .attach(
                "codex-pending-cleanup@localhost",
                AttachmentRole::Observer,
                None,
            )
            .await
            .unwrap();
        never_watched.disconnect().await.unwrap();

        client.archive("codex-smoke@localhost").await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires an owned listener at FINCH_TEST_BRAIN_ADDR"]
    async fn live_remote_attachment_credential_cannot_claim_a_sibling_connection() {
        let brain = format!(
            "remote-auth-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        let target = isolated_live_brain_target(&brain);
        let subject = "same-subject@localhost";
        let mut first = RemoteBrainClient::new(target.clone(), isolated_live_password()).unwrap();
        let first_attachment = first
            .attach(subject, AttachmentRole::Driver, None)
            .await
            .unwrap();
        let mut second = RemoteBrainClient::new(target, isolated_live_password()).unwrap();
        second
            .attach(subject, AttachmentRole::Driver, None)
            .await
            .unwrap();

        let mut forged = second.clone();
        forged.attachment = Some(first_attachment);
        assert!(forged.watch().await.is_err());

        let mut first_events = first.watch().await.unwrap();
        let initial = tokio::time::timeout(std::time::Duration::from_secs(5), first_events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(initial, BrainWireMessage::Snapshot { .. }));

        first.disconnect().await.unwrap();
        second.disconnect().await.unwrap();
        first.archive(subject).await.unwrap();
    }

    #[test]
    #[ignore = "requires explicitly owned IPC and HTTP endpoints"]
    fn live_local_and_remote_transports_produce_equivalent_lifecycle() {
        use crate::brain::store::{BrainRunKind, BrainRunStatus};
        use crate::ipc::brain_codec::{BrainRemoteCommandKind, BrainRemoteReply};

        fn lifecycle(snapshot: &BrainSnapshot) -> Vec<&'static str> {
            snapshot
                .events
                .iter()
                .filter_map(|event| match event.kind {
                    BrainEventKind::ClientAttached { .. } => Some("attached"),
                    BrainEventKind::Prompt { .. } => Some("prompt"),
                    BrainEventKind::SpeculativePrompt { .. } => Some("speculative-prompt"),
                    BrainEventKind::RunStarted { .. } => Some("run-started"),
                    BrainEventKind::ClientDetached { .. } => Some("detached"),
                    _ => None,
                })
                .collect()
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local_set = tokio::task::LocalSet::new();
        runtime.block_on(local_set.run_until(async {
            let suffix = uuid::Uuid::new_v4();
            let local_brain = format!("codex-conformance-local-{suffix}");
            let remote_brain = format!("codex-conformance-remote-{suffix}");

            let ipc = connect_isolated_live_ipc().await;
            let local_attachment = ipc
                .brain_attach(
                    &local_brain,
                    "conformance@localhost",
                    AttachmentRole::Driver,
                    None,
                )
                .await
                .unwrap();
            let mut local_events = ipc
                .brain_watch(&local_brain, &local_attachment)
                .await
                .unwrap();
            let local_initial = local_events.recv().await.unwrap().unwrap();
            assert!(matches!(local_initial, BrainWireMessage::Snapshot { .. }));
            let local_outcome = ipc
                .brain_submit(
                    &local_brain,
                    &local_attachment,
                    BrainEventKind::Prompt {
                        text: "same lifecycle".into(),
                    },
                )
                .await
                .unwrap();
            let local_ack = ipc
                .brain_acknowledge(&local_brain, &local_attachment, local_outcome.accepted.seq)
                .await
                .unwrap();
            let local_speculative = ipc
                .brain_start_speculative(
                    &local_brain,
                    &local_ack,
                    "same speculative lifecycle".into(),
                )
                .await
                .unwrap();
            ipc.brain_detach(&local_brain, &local_ack).await.unwrap();
            let local_snapshot = ipc.brain_snapshot(&local_brain).await.unwrap();

            let daemon_address = isolated_live_daemon_address();
            let owner_password = isolated_live_password();
            let target = RemoteBrainTarget::local(&remote_brain, &daemon_address).unwrap();
            let owner = RemoteBrainClient::new(target.clone(), owner_password.clone()).unwrap();
            owner.create().await.unwrap();
            let (invitation, _) = owner
                .issue_invitation(AttachmentRole::Driver, Some(60_000))
                .await
                .unwrap();
            let mut remote = RemoteBrainClient::new_with_invitation(target, invitation).unwrap();
            remote
                .attach_invited_persistent("conformance@localhost", "conformance-live")
                .await
                .unwrap();
            let mut remote_events = remote.watch().await.unwrap();
            assert!(matches!(
                remote_events.recv().await.unwrap(),
                BrainWireMessage::Snapshot { .. }
            ));
            let remote_outcome = remote
                .send_remote_command(BrainRemoteCommandKind::Submit(BrainEventKind::Prompt {
                    text: "same lifecycle".into(),
                }))
                .await
                .unwrap();
            let BrainRemoteReply::Submitted {
                accepted: remote_accepted,
                run: remote_run,
                result: remote_result,
                ..
            } = remote_outcome
            else {
                panic!("remote transport returned a non-submission outcome")
            };
            remote.acknowledge(remote_accepted.seq).await.unwrap();
            let remote_speculative = remote
                .start_speculative("same speculative lifecycle".into())
                .await
                .unwrap();
            remote.disconnect().await.unwrap();
            let remote_snapshot = remote.snapshot().await.unwrap();

            assert_eq!(local_outcome.accepted.kind, remote_accepted.kind);
            assert_eq!(local_outcome.result, remote_result);
            let local_run = local_outcome.run.unwrap();
            let remote_run = remote_run.unwrap();
            assert_eq!(local_run.kind, BrainRunKind::Interactive);
            assert_eq!(local_run.kind, remote_run.kind);
            assert_eq!(local_run.status, BrainRunStatus::QueuedForEnvironment);
            assert_eq!(local_run.status, remote_run.status);
            assert_eq!(local_speculative.kind, BrainRunKind::Speculative);
            assert_eq!(local_speculative.kind, remote_speculative.kind);
            assert_eq!(local_speculative.status, remote_speculative.status);
            assert_eq!(
                local_run.request_seq - local_snapshot.events[0].seq,
                remote_run.request_seq - remote_snapshot.events[0].seq
            );
            assert_eq!(lifecycle(&local_snapshot), lifecycle(&remote_snapshot));

            drop(local_events);
            drop(remote_events);
            owner.archive("conformance@localhost").await.unwrap();
            let local_target = RemoteBrainTarget::local(&local_brain, &daemon_address).unwrap();
            RemoteBrainClient::new(local_target, owner_password)
                .unwrap()
                .archive("conformance@localhost")
                .await
                .unwrap();
        }));
    }

    #[test]
    #[ignore = "requires explicitly owned IPC and HTTP endpoints"]
    fn live_addressed_handoff_moves_program_dispatch_to_the_target_runner() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local_set = tokio::task::LocalSet::new();
        runtime.block_on(local_set.run_until(async {
            let brain = format!("codex-handoff-live-{}", uuid::Uuid::new_v4());
            let source_subject = "codex-source/frontend-live";
            let target_subject = "codex-target/frontend-live";
            let ipc = connect_isolated_live_ipc().await;
            let snapshot = ipc.brain_snapshot(&brain).await.unwrap();

            ipc.brain_claim_runner_identity(source_subject)
                .await
                .unwrap();
            ipc.brain_claim_runner_identity(target_subject)
                .await
                .unwrap();

            let source_lease = ipc
                .brain_acquire_runner(&brain, source_subject, &snapshot.environment, None, 120_000)
                .await
                .unwrap();
            let (source_tx, mut source_rx) = tokio::sync::mpsc::unbounded_channel();
            ipc.register_brain_runner(&brain, source_lease.lease_id, source_tx)
                .await
                .unwrap();

            let daemon_address = isolated_live_daemon_address();
            let target = RemoteBrainTarget::local(&brain, &daemon_address).unwrap();
            let password = isolated_live_password();
            let mut controller = RemoteBrainClient::new(target, password).unwrap();
            controller
                .authorize_runner_handoff_control("codex-control@localhost", AttachmentRole::Driver)
                .await
                .unwrap();
            controller
                .attach("codex-control@localhost", AttachmentRole::Driver, None)
                .await
                .unwrap();
            let _events = controller.watch().await.unwrap();
            let handoff = controller
                .request_runner_handoff(
                    target_subject,
                    source_lease.lease_id,
                    snapshot.environment.generation,
                    120_000,
                )
                .await
                .unwrap();

            let controller_credential_id = controller
                .credential
                .lock()
                .await
                .as_ref()
                .unwrap()
                .claims
                .credential_id;
            controller
                .http
                .delete(format!(
                    "{}://{}/v1/brains/credentials/{controller_credential_id}",
                    controller.target.http_scheme(),
                    controller.target.address,
                ))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            let revoked = controller
                .cancel_runner_handoff(handoff.handoff_id)
                .await
                .unwrap_err()
                .to_string();
            assert!(
                revoked.contains("revoked")
                    || revoked.contains("unauthorized")
                    || revoked.contains("connection closed")
                    || revoked.contains("no longer authorizes"),
                "unexpected revocation error: {revoked}"
            );

            let target_lease = ipc
                .brain_accept_runner_handoff(
                    &brain,
                    target_subject,
                    handoff.handoff_id,
                    &snapshot.environment,
                    120_000,
                )
                .await
                .unwrap();
            let (target_tx, mut target_rx) = tokio::sync::mpsc::unbounded_channel();
            let bootstrap = ipc
                .register_brain_runner(&brain, target_lease.lease_id, target_tx)
                .await
                .unwrap();

            let mut submitter =
                RemoteBrainClient::new(controller.target.clone(), "loopback").unwrap();
            submitter
                .attach("codex-submit@localhost", AttachmentRole::Driver, None)
                .await
                .unwrap();
            let _submit_events = submitter.watch().await.unwrap();
            let submitting_client = submitter.clone();
            let submission = tokio::task::spawn_local(async move {
                submitting_client
                    .push(BrainEventKind::Program {
                        language: crate::brain::store::ProgramLanguage::Lisp,
                        source: "(say \"handoff-live\")".into(),
                    })
                    .await
            });
            let request = tokio::time::timeout(std::time::Duration::from_secs(5), target_rx.recv())
                .await
                .unwrap()
                .expect("target runner callback closed");
            let crate::cli::repl_event::ReplEvent::NamedBrainProgramRequested(request) = request
            else {
                panic!("target callback received the wrong frontend event")
            };
            assert_eq!(request.brain, brain);
            assert_eq!(request.source, "(say \"handoff-live\")");
            request
                .response_tx
                .send(Ok(crate::server::RunnerProgramResult {
                    output: "handoff-live".into(),
                    runtime_revision: bootstrap.runtime_revision,
                    checkpoint: bootstrap.checkpoint,
                    effect_journal: Vec::new(),
                }))
                .unwrap();
            submission.await.unwrap().unwrap();

            match tokio::time::timeout(std::time::Duration::from_millis(100), source_rx.recv())
                .await
            {
                Err(_) | Ok(None) => {}
                Ok(Some(event)) => panic!("stale source runner received {event:?}"),
            }
            let final_snapshot = ipc.brain_snapshot(&brain).await.unwrap();
            assert_eq!(
                final_snapshot
                    .runner_lease
                    .as_ref()
                    .map(|lease| lease.subject.as_str()),
                Some(target_subject)
            );
            assert!(final_snapshot.events.iter().any(|event| matches!(
                &event.kind,
                BrainEventKind::Result {
                    output,
                    error: None,
                    ..
                } if output == "handoff-live"
            )));

            ipc.brain_release_runner(&brain, target_lease.lease_id)
                .await
                .unwrap();
            submitter.disconnect().await.unwrap();
            submitter.archive("codex-submit@localhost").await.unwrap();
        }));
    }

    #[tokio::test]
    async fn cloned_client_reuses_a_live_scoped_credential() {
        let client = RemoteBrainClient::new(
            RemoteBrainTarget::local("shared", "http://127.0.0.1:32123").unwrap(),
            "bootstrap-secret",
        )
        .unwrap();
        *client.credential.lock().await = Some(RemoteBrainCredential {
            token: "scoped-token".into(),
            claims: crate::brain::credential::BrainCredentialClaims {
                version: 1,
                credential_id: uuid::Uuid::new_v4(),
                issuer: "box.local".into(),
                subject: "alice@laptop.local".into(),
                brain_id: BrainId(uuid::Uuid::new_v4()),
                brain: "shared".into(),
                environment_generation: 1,
                role: AttachmentRole::Driver,
                scopes: super::super::credential::default_participant_scopes(
                    AttachmentRole::Driver,
                ),
                attachment_id: None,
                connection_id: None,
                delegation_chain: Vec::new(),
                issued_ms: 0,
                expires_ms: u64::MAX,
            },
        });

        assert_eq!(client.authorized_token().await.unwrap(), "scoped-token");
        assert_eq!(
            client.clone().authorized_token().await.unwrap(),
            "scoped-token"
        );
    }

    #[tokio::test]
    async fn ordinary_operations_require_bootstrapping_first() {
        let client = RemoteBrainClient::new(
            RemoteBrainTarget::local("shared", "http://127.0.0.1:32123").unwrap(),
            "bootstrap-secret",
        )
        .unwrap();
        let error = client.snapshot().await.unwrap_err();
        assert!(error.to_string().contains("scoped Brain credential"));
    }

    #[test]
    fn attachment_identity_survives_restart_and_is_scoped_to_console() {
        let temp = tempfile::tempdir().unwrap();
        let store = AttachmentIdentityStore::new(temp.path().join("attachments.json"));
        let brain_id = BrainId(uuid::Uuid::new_v4());
        let attachment_id = AttachmentId(uuid::Uuid::new_v4());
        store
            .save(AttachmentIdentityRecord {
                brain_id,
                client_slot: "home-brain".into(),
                subject: "alice@box.local".into(),
                role: AttachmentRole::Driver,
                attachment_id,
            })
            .unwrap();

        let restarted = AttachmentIdentityStore::new(temp.path().join("attachments.json"));
        assert_eq!(
            restarted
                .find(
                    brain_id,
                    "home-brain",
                    "alice@box.local",
                    AttachmentRole::Driver,
                )
                .unwrap(),
            Some(attachment_id)
        );
        assert_eq!(
            restarted
                .find(
                    brain_id,
                    "other-console",
                    "alice@box.local",
                    AttachmentRole::Driver,
                )
                .unwrap(),
            None
        );
    }

    #[test]
    fn corrupt_attachment_identity_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("attachments.json");
        std::fs::write(&path, b"not json").unwrap();
        let error = AttachmentIdentityStore::new(path).load().unwrap_err();
        assert!(error.to_string().contains("parse"));
    }

    #[test]
    fn attachment_identity_updates_preserve_other_console_slots() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("attachments.json");
        let brain_id = BrainId(uuid::Uuid::new_v4());
        let first_id = AttachmentId(uuid::Uuid::new_v4());
        let second_id = AttachmentId(uuid::Uuid::new_v4());
        for (client_slot, attachment_id) in [("first", first_id), ("second", second_id)] {
            AttachmentIdentityStore::new(path.clone())
                .save(AttachmentIdentityRecord {
                    brain_id,
                    client_slot: client_slot.into(),
                    subject: "alice@box.local".into(),
                    role: AttachmentRole::Driver,
                    attachment_id,
                })
                .unwrap();
        }

        let store = AttachmentIdentityStore::new(path);
        assert_eq!(
            store
                .find(brain_id, "first", "alice@box.local", AttachmentRole::Driver,)
                .unwrap(),
            Some(first_id)
        );
        assert_eq!(
            store
                .find(
                    brain_id,
                    "second",
                    "alice@box.local",
                    AttachmentRole::Driver,
                )
                .unwrap(),
            Some(second_id)
        );
    }
}
