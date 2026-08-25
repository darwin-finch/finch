//! Client for attaching a Finch TUI to a named brain on another daemon.

use anyhow::{Context, Result};
use fs2::FileExt;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::shared::{
    AttachmentId, AttachmentRole, BrainAttachment, BrainEventKind, BrainId, BrainSnapshot,
    BrainWireMessage,
};

pub const DEFAULT_BRAIN_PORT: u16 = 11435;
const ATTACHMENT_IDENTITIES_VERSION: u32 = 1;

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
}

impl RemoteBrainTarget {
    pub fn parse(value: &str) -> Result<Self> {
        let (brain, host) = value
            .trim()
            .split_once('@')
            .context("brain target must be NAME@MACHINE[:PORT]")?;
        super::shared::SharedBrainStore::validate_name(brain)?;
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
        })
    }

    pub fn display_name(&self) -> String {
        format!("{}@{}", self.brain, self.machine)
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
        Self::parse(&format!("{brain}@{address}"))
    }

    fn http_url(&self) -> String {
        format!("http://{}/v1/brains/named/{}", self.address, self.brain)
    }

    fn attachments_url(&self) -> String {
        format!("{}/attachments", self.http_url())
    }

    fn credentials_url(&self) -> String {
        format!("{}/credentials", self.http_url())
    }

    fn ws_url(&self, attachment: &BrainAttachment) -> Result<String> {
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        Ok(format!(
            "ws://{}/v1/brains/named/{}/ws?attachment_id={}&connection_id={}",
            self.address, self.brain, attachment.attachment_id.0, connection_id.0
        ))
    }
}

fn has_explicit_port(host: &str) -> bool {
    host.rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
}

#[derive(Clone)]
pub struct RemoteBrainClient {
    pub target: RemoteBrainTarget,
    bootstrap_password: String,
    credential: std::sync::Arc<tokio::sync::Mutex<Option<RemoteBrainCredential>>>,
    http: Client,
    attachment: Option<BrainAttachment>,
}

#[derive(Clone)]
struct RemoteBrainCredential {
    token: String,
    claims: super::credential::BrainCredentialClaims,
}

impl RemoteBrainClient {
    pub fn new(target: RemoteBrainTarget, password: impl Into<String>) -> Result<Self> {
        Ok(Self {
            target,
            bootstrap_password: password.into(),
            credential: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(180))
                .build()?,
            attachment: None,
        })
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
        #[derive(Serialize)]
        struct Attach<'a> {
            subject: &'a str,
            role: AttachmentRole,
            attachment_id: Option<AttachmentId>,
        }

        self.ensure_credential(subject, role).await?;
        let token = self.authorized_token().await?;
        let attachment = self
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
            .json::<BrainAttachment>()
            .await
            .context("invalid brain attachment")?;
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

    pub async fn push(&self, kind: BrainEventKind) -> Result<()> {
        #[derive(Serialize)]
        struct Push {
            attachment_id: AttachmentId,
            connection_id: super::shared::ConnectionId,
            #[serde(flatten)]
            kind: BrainEventKind,
        }

        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;

        self.http
            .post(self.target.http_url())
            .bearer_auth(self.authorized_token().await?)
            .json(&Push {
                attachment_id: attachment.attachment_id,
                connection_id,
                kind,
            })
            .send()
            .await
            .context("could not reach brain host")?
            .error_for_status()
            .context("brain push rejected")?;
        Ok(())
    }

    pub async fn acknowledge(&mut self, seq: u64) -> Result<()> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        let updated = self
            .http
            .post(format!(
                "{}/{}/ack",
                self.target.attachments_url(),
                attachment.attachment_id.0
            ))
            .bearer_auth(self.authorized_token().await?)
            .json(&serde_json::json!({
                "connection_id": connection_id,
                "seq": seq
            }))
            .send()
            .await
            .context("could not reach brain host")?
            .error_for_status()
            .context("brain acknowledgement rejected")?
            .json::<BrainAttachment>()
            .await
            .context("invalid brain acknowledgement")?;
        self.attachment = Some(updated);
        Ok(())
    }

    pub async fn disconnect(&self) -> Result<()> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        self.http
            .delete(format!(
                "{}/{}/connections/{}",
                self.target.attachments_url(),
                attachment.attachment_id.0,
                connection_id.0
            ))
            .bearer_auth(self.authorized_token().await?)
            .send()
            .await
            .context("could not reach brain host")?
            .error_for_status()
            .context("brain detach rejected")?;
        Ok(())
    }

    /// Connect to the brain's snapshot/live-event stream.
    pub async fn watch(&self) -> Result<mpsc::UnboundedReceiver<BrainWireMessage>> {
        let attachment = self
            .attachment
            .as_ref()
            .context("client is not attached to a Brain")?;
        let mut request = self.target.ws_url(attachment)?.into_client_request()?;
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            format!("Bearer {}", self.authorized_token().await?).parse()?,
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .context("could not open brain event stream")?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tx.closed() => {
                        let _ = socket.close(None).await;
                        break;
                    }
                    incoming = socket.next() => {
                        let Some(Ok(message)) = incoming else {
                            break;
                        };
                        if let tokio_tungstenite::tungstenite::Message::Binary(bytes) = message {
                            let Ok(message) = crate::ipc::brain_codec::decode_brain_wire_message(&bytes) else {
                                break;
                            };
                            if tx.send(message).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });
        Ok(rx)
    }

    async fn ensure_credential(&self, subject: &str, role: AttachmentRole) -> Result<()> {
        let now_ms = unix_epoch_millis();
        let mut credential = self.credential.lock().await;
        if credential.as_ref().is_some_and(|credential| {
            credential.claims.subject == subject
                && credential.claims.role == role
                && credential.claims.brain == self.target.brain
                && credential.claims.expires_ms > now_ms.saturating_add(60_000)
        }) {
            return Ok(());
        }

        #[derive(Serialize)]
        struct Issue<'a> {
            subject: &'a str,
            role: AttachmentRole,
        }
        #[derive(Deserialize)]
        struct Issued {
            token: String,
            claims: super::credential::BrainCredentialClaims,
        }

        let issued = self
            .http
            .post(self.target.credentials_url())
            .bearer_auth(&self.bootstrap_password)
            .json(&Issue { subject, role })
            .send()
            .await
            .context("could not reach Brain credential issuer")?
            .error_for_status()
            .context("Brain credential request rejected")?
            .json::<Issued>()
            .await
            .context("invalid Brain credential response")?;
        if issued.claims.subject != subject
            || issued.claims.role != role
            || issued.claims.brain != self.target.brain
        {
            anyhow::bail!("Brain credential issuer returned the wrong participant audience");
        }
        *credential = Some(RemoteBrainCredential {
            token: issued.token,
            claims: issued.claims,
        });
        Ok(())
    }

    async fn authorized_token(&self) -> Result<String> {
        let identity = self
            .credential
            .lock()
            .await
            .as_ref()
            .map(|credential| (credential.claims.subject.clone(), credential.claims.role))
            .context("client has not bootstrapped a scoped Brain credential")?;
        self.ensure_credential(&identity.0, identity.1).await?;
        self.credential
            .lock()
            .await
            .as_ref()
            .map(|credential| credential.token.clone())
            .context("Brain credential refresh did not produce a token")
    }
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
                let attachment_id =
                    store.find(snapshot.brain_id, client_slot, subject, role)?;
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
                ipc.brain_submit(&self.target.brain, attachment, kind).await?;
                Ok(())
            }
            AttachedBrainTransport::Remote(client) => client.push(kind).await,
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

    #[test]
    fn target_defaults_to_daemon_port_and_keeps_mdns_name() {
        let target = RemoteBrainTarget::parse("finch@workstation.local").unwrap();
        assert_eq!(target.display_name(), "finch@workstation.local");
        assert_eq!(target.address, "workstation.local:11435");
    }

    #[test]
    fn target_accepts_an_explicit_port() {
        let target = RemoteBrainTarget::parse("review@10.0.0.4:9000").unwrap();
        assert_eq!(target.machine, "10.0.0.4");
        assert_eq!(target.address, "10.0.0.4:9000");
    }

    #[test]
    fn bare_name_can_resolve_through_the_local_daemon() {
        let target = RemoteBrainTarget::local("review", "http://127.0.0.1:11435").unwrap();
        assert_eq!(target.brain, "review");
        assert_eq!(target.address, "127.0.0.1:11435");
    }

    #[test]
    fn target_rejects_ambiguous_or_unsafe_values() {
        assert!(RemoteBrainTarget::parse("brain-only").is_err());
        assert!(RemoteBrainTarget::parse("../brain@host").is_err());
        assert!(RemoteBrainTarget::parse("brain@host/path").is_err());
    }

    #[tokio::test]
    async fn cloned_client_reuses_a_live_scoped_credential() {
        let client = RemoteBrainClient::new(
            RemoteBrainTarget::parse("shared@box.local").unwrap(),
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
                scopes: [crate::brain::credential::BrainCredentialScope::BrainRead]
                    .into_iter()
                    .collect(),
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
            RemoteBrainTarget::parse("shared@box.local").unwrap(),
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
