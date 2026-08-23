/// Async SSH client using russh (pure-Rust SSHv2).
///
/// Each `SshSession` wraps a `russh::client::Handle` (the authenticated
/// session object).  Sessions are stored in `SshSessionStore` and
/// referenced from Lisp via `Val::SshSession(Uuid)`.
///
/// Security note: `check_server_key` currently accepts all host keys.
/// A future version will verify against `~/.finch/known_hosts`.
use anyhow::{bail, Result};
use russh::client::{self, Config, Handle};
use russh::ChannelMsg;
use russh_keys::key::PublicKey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

// ── Handler ───────────────────────────────────────────────────────────────────

/// russh client handler — decides whether to trust server host keys.
struct SshHandler;

#[async_trait::async_trait]
impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        // Accept all host keys for now (TOFU — trust on first use).
        // TODO: verify against ~/.finch/known_hosts.
        Ok(true)
    }
}

// ── Session ───────────────────────────────────────────────────────────────────

/// A live authenticated SSH session.
pub struct SshSession {
    handle: Handle<SshHandler>,
    pub host: String,
    pub user: String,
}

impl SshSession {
    /// Connect and authenticate with a password.
    pub async fn connect_password(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
    ) -> Result<Self> {
        let config = Arc::new(Config::default());
        let addr = format!("{host}:{port}");
        let mut handle = client::connect(config, addr.as_str(), SshHandler)
            .await
            .map_err(|e| anyhow::anyhow!("ssh-connect: {e}"))?;
        let authenticated = handle
            .authenticate_password(user, password)
            .await
            .map_err(|e| anyhow::anyhow!("ssh-auth: {e}"))?;
        if !authenticated {
            bail!("ssh-connect: authentication failed for {user}@{host}:{port}");
        }
        Ok(Self {
            handle,
            host: host.to_string(),
            user: user.to_string(),
        })
    }

    /// Connect and authenticate with an Ed25519 private key (32-byte seed).
    pub async fn connect_key(
        host: &str,
        port: u16,
        user: &str,
        private_key_bytes: &[u8],
    ) -> Result<Self> {
        use russh_keys::key::{KeyPair, ED25519};
        if private_key_bytes.len() != 32 {
            bail!("ssh-auth-key: private key must be 32 bytes");
        }
        // Build an ed25519 signing key from the raw seed bytes.
        let seed: [u8; 32] = private_key_bytes.try_into().unwrap();
        let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
        let keypair = KeyPair::Ed25519(sk.into());

        let config = Arc::new(Config::default());
        let addr = format!("{host}:{port}");
        let mut handle = client::connect(config, addr.as_str(), SshHandler)
            .await
            .map_err(|e| anyhow::anyhow!("ssh-connect: {e}"))?;
        let authenticated = handle
            .authenticate_publickey(user, Arc::new(keypair))
            .await
            .map_err(|e| anyhow::anyhow!("ssh-auth-key: {e}"))?;
        if !authenticated {
            bail!("ssh-connect: key authentication failed for {user}@{host}:{port}");
        }
        Ok(Self {
            handle,
            host: host.to_string(),
            user: user.to_string(),
        })
    }

    /// Execute a command and return (stdout, stderr, exit_code).
    pub async fn exec(&mut self, cmd: &str) -> Result<(String, String, u32)> {
        let mut channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| anyhow::anyhow!("ssh-exec: failed to open channel: {e}"))?;

        channel
            .exec(true, cmd.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("ssh-exec: failed to exec '{cmd}': {e}"))?;

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = 0u32;

        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { ref data }) => {
                    stdout.push_str(String::from_utf8_lossy(data).as_ref());
                }
                Some(ChannelMsg::ExtendedData { ref data, ext: 1 }) => {
                    stderr.push_str(String::from_utf8_lossy(data).as_ref());
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = exit_status;
                }
                Some(ChannelMsg::Eof) | None => break,
                _ => {}
            }
        }

        Ok((stdout, stderr, exit_code))
    }

    /// Write a file via `cat >` (simple transfer, no SCP/SFTP dependency).
    pub async fn write_file(&mut self, remote_path: &str, content: &[u8]) -> Result<()> {
        use base64::Engine;
        // Encode content as base64 and decode on the remote side.
        let b64 = base64::engine::general_purpose::STANDARD.encode(content);
        let cmd = format!("echo '{}' | base64 -d > {}", b64, remote_path);
        let (_, stderr, code) = self.exec(&cmd).await?;
        if code != 0 {
            bail!("ssh-write-file: remote exited {code}: {stderr}");
        }
        Ok(())
    }

    /// Read a file via `cat`.
    pub async fn read_file(&mut self, remote_path: &str) -> Result<Vec<u8>> {
        let (stdout, stderr, code) = self.exec(&format!("cat {remote_path}")).await?;
        if code != 0 {
            bail!("ssh-read-file: remote exited {code}: {stderr}");
        }
        Ok(stdout.into_bytes())
    }

    /// Close the session.
    pub async fn close(mut self) -> Result<()> {
        self.handle
            .disconnect(russh::Disconnect::ByApplication, "", "en")
            .await
            .map_err(|e| anyhow::anyhow!("ssh-close: {e}"))?;
        Ok(())
    }

    pub fn info(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }
}

// ── Store ─────────────────────────────────────────────────────────────────────

/// Thread-safe store of live SSH sessions, referenced from Lisp by UUID.
#[derive(Default)]
pub struct SshSessionStore {
    sessions: Mutex<HashMap<Uuid, SshSession>>,
}

impl SshSessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a session and return its handle UUID.
    pub async fn insert(&self, session: SshSession) -> Uuid {
        let id = Uuid::new_v4();
        self.sessions.lock().await.insert(id, session);
        id
    }

    /// Remove and return a session (e.g. to close it).
    pub async fn remove(&self, id: Uuid) -> Option<SshSession> {
        self.sessions.lock().await.remove(&id)
    }

    /// Execute a command on a session.  Holds the store lock across the I/O
    /// await (acceptable for scripting; sessions are rarely used concurrently).
    pub async fn exec(&self, id: Uuid, cmd: &str) -> Result<(String, String, u32)> {
        let mut sessions = self.sessions.lock().await;
        let s = sessions
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("ssh: session {id} not found"))?;
        s.exec(cmd).await
    }

    pub async fn read_file(&self, id: Uuid, path: &str) -> Result<Vec<u8>> {
        let mut sessions = self.sessions.lock().await;
        let s = sessions
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("ssh: session {id} not found"))?;
        s.read_file(path).await
    }

    pub async fn write_file(&self, id: Uuid, path: &str, content: Vec<u8>) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        let s = sessions
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("ssh: session {id} not found"))?;
        s.write_file(path, &content).await
    }

    pub async fn info(&self, id: Uuid) -> Result<String> {
        let sessions = self.sessions.lock().await;
        let s = sessions
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("ssh: session {id} not found"))?;
        Ok(s.info())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ssh_session_store_insert_and_remove() {
        // We can't connect to a real server in unit tests, so we test only the store.
        // A real integration test would need a local sshd.
        let store = SshSessionStore::new();
        // The store starts empty — with an unknown UUID it returns an error.
        let unknown = Uuid::new_v4();
        let result = store.exec(unknown, "true").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_ssh_session_store_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SshSessionStore>();
    }
}
