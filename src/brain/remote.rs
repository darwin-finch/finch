//! Client for attaching a Finch TUI to a named brain on another daemon.

use anyhow::{Context, Result};
use futures::StreamExt;
use reqwest::Client;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::shared::{
    AttachmentId, AttachmentRole, BrainAttachment, BrainEventKind, BrainSnapshot,
    BrainWireMessage,
};

pub const DEFAULT_BRAIN_PORT: u16 = 11435;

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
    password: String,
    http: Client,
    attachment: Option<BrainAttachment>,
}

impl RemoteBrainClient {
    pub fn new(target: RemoteBrainTarget, password: impl Into<String>) -> Result<Self> {
        Ok(Self {
            target,
            password: password.into(),
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

        let attachment = self
            .http
            .post(self.target.attachments_url())
            .bearer_auth(&self.password)
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

    pub async fn snapshot(&self) -> Result<BrainSnapshot> {
        self.http
            .get(self.target.http_url())
            .bearer_auth(&self.password)
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
            .bearer_auth(&self.password)
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
            .bearer_auth(&self.password)
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
            .bearer_auth(&self.password)
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
            format!("Bearer {}", self.password).parse()?,
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
                        if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                            if let Ok(message) = serde_json::from_str::<BrainWireMessage>(&text) {
                                if tx.send(message).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });
        Ok(rx)
    }
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
}
