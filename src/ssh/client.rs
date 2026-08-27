/// Async SSH client using russh (pure-Rust SSHv2).
///
/// Each `SshSession` wraps a `russh::client::Handle` (the authenticated
/// session object).  Sessions are stored in `SshSessionStore` and
/// referenced by host adapters through an opaque session identifier.
///
use anyhow::{bail, Result};
use russh::client::{self, Config, Handle};
use russh::keys::ssh_key::{Fingerprint, HashAlg, PublicKey};
use russh::ChannelMsg;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

// ── Handler ───────────────────────────────────────────────────────────────────

/// Host-key verification policy for a new SSH connection.
///
/// Prefer [`HostKeyPolicy::pinned_sha256`]. The insecure policy exists only to
/// preserve the behavior of Finch's original `connect_*` methods until their
/// callers can provide a host-key pin. It is accept-all, not trust on first use:
/// it neither remembers the first key nor detects a later key change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostKeyPolicy(HostKeyVerification);

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostKeyVerification {
    InsecureAcceptAny,
    PinnedSha256(Fingerprint),
}

impl HostKeyPolicy {
    /// Accept every server host key without verification.
    ///
    /// # Security
    ///
    /// This permits man-in-the-middle attacks. It is provided for compatibility
    /// with the pre-0.60 Finch SSH API and is not a TOFU policy.
    pub fn insecure_accept_any() -> Self {
        Self(HostKeyVerification::InsecureAcceptAny)
    }

    /// Require an exact OpenSSH SHA-256 host-key fingerprint.
    ///
    /// The expected format is `SHA256:<unpadded-base64>`, as printed by
    /// `ssh-keygen -l -E sha256`.
    pub fn pinned_sha256(fingerprint: &str) -> Result<Self> {
        let fingerprint = fingerprint
            .parse::<Fingerprint>()
            .map_err(|e| anyhow::anyhow!("ssh-host-key: invalid SHA-256 fingerprint: {e}"))?;
        if !fingerprint.is_sha256() {
            bail!("ssh-host-key: expected a SHA-256 fingerprint");
        }
        Ok(Self(HostKeyVerification::PinnedSha256(fingerprint)))
    }
}

/// russh client handler — decides whether to trust server host keys.
struct SshHandler {
    host_key_policy: HostKeyPolicy,
}

impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(match &self.host_key_policy.0 {
            HostKeyVerification::InsecureAcceptAny => true,
            HostKeyVerification::PinnedSha256(expected) => {
                server_public_key.fingerprint(HashAlg::Sha256) == *expected
            }
        })
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
    ///
    /// # Security
    ///
    /// This compatibility method accepts every server host key. New callers
    /// should use [`Self::connect_password_with_host_key_policy`] with a pin.
    pub async fn connect_password(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
    ) -> Result<Self> {
        Self::connect_password_with_host_key_policy(
            host,
            port,
            user,
            password,
            HostKeyPolicy::insecure_accept_any(),
        )
        .await
    }

    /// Connect with password authentication and an explicit host-key policy.
    pub async fn connect_password_with_host_key_policy(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        host_key_policy: HostKeyPolicy,
    ) -> Result<Self> {
        let config = Arc::new(Config::default());
        let addr = format!("{host}:{port}");
        let mut handle = client::connect(config, addr.as_str(), SshHandler { host_key_policy })
            .await
            .map_err(|e| anyhow::anyhow!("ssh-connect: {e}"))?;
        let authenticated = handle
            .authenticate_password(user, password)
            .await
            .map_err(|e| anyhow::anyhow!("ssh-auth: {e}"))?;
        if !authenticated.success() {
            bail!("ssh-connect: authentication failed for {user}@{host}:{port}");
        }
        Ok(Self {
            handle,
            host: host.to_string(),
            user: user.to_string(),
        })
    }

    /// Connect and authenticate with an Ed25519 private key (32-byte seed).
    ///
    /// # Security
    ///
    /// This compatibility method accepts every server host key. New callers
    /// should use [`Self::connect_key_with_host_key_policy`] with a pin.
    pub async fn connect_key(
        host: &str,
        port: u16,
        user: &str,
        private_key_bytes: &[u8],
    ) -> Result<Self> {
        Self::connect_key_with_host_key_policy(
            host,
            port,
            user,
            private_key_bytes,
            HostKeyPolicy::insecure_accept_any(),
        )
        .await
    }

    /// Connect with Ed25519 authentication and an explicit host-key policy.
    pub async fn connect_key_with_host_key_policy(
        host: &str,
        port: u16,
        user: &str,
        private_key_bytes: &[u8],
        host_key_policy: HostKeyPolicy,
    ) -> Result<Self> {
        use russh::keys::ssh_key::private::{Ed25519Keypair, KeypairData};
        use russh::keys::{PrivateKey, PrivateKeyWithHashAlg};

        if private_key_bytes.len() != 32 {
            bail!("ssh-auth-key: private key must be 32 bytes");
        }
        let seed: [u8; 32] = private_key_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("ssh-auth-key: private key must be 32 bytes"))?;
        let keypair = Ed25519Keypair::from_seed(&seed);
        let private_key = PrivateKey::new(KeypairData::Ed25519(keypair), "")
            .map_err(|e| anyhow::anyhow!("ssh-auth-key: invalid Ed25519 private key: {e}"))?;
        let private_key = PrivateKeyWithHashAlg::new(Arc::new(private_key), None);

        let config = Arc::new(Config::default());
        let addr = format!("{host}:{port}");
        let mut handle = client::connect(config, addr.as_str(), SshHandler { host_key_policy })
            .await
            .map_err(|e| anyhow::anyhow!("ssh-connect: {e}"))?;
        let authenticated = handle
            .authenticate_publickey(user, private_key)
            .await
            .map_err(|e| anyhow::anyhow!("ssh-auth-key: {e}"))?;
        if !authenticated.success() {
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
                Some(ChannelMsg::Eof) => {}
                Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
        }

        Ok((stdout, stderr, exit_code))
    }

    /// Write a file through the remote user's shell (no SCP/SFTP dependency).
    ///
    /// The remote shell must implement POSIX single-quote parsing and provide
    /// `base64 -d`. The path is quoted as one shell word, but this remains a
    /// shell command: it follows redirections and symlinks and is not a safe
    /// filesystem capability primitive.
    pub async fn write_file(&mut self, remote_path: &str, content: &[u8]) -> Result<()> {
        use base64::Engine;
        let remote_path = quote_posix_shell_word(remote_path)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(content);
        let cmd = format!("printf '%s' '{b64}' | base64 -d > {remote_path}");
        let (_, stderr, code) = self.exec(&cmd).await?;
        if code != 0 {
            bail!("ssh-write-file: remote exited {code}: {stderr}");
        }
        Ok(())
    }

    /// Read a file through the remote user's POSIX-compatible shell.
    ///
    /// The path is quoted as one shell word. Redirections and symlinks still
    /// have normal remote-shell semantics; this is not a safe filesystem
    /// capability primitive.
    pub async fn read_file(&mut self, remote_path: &str) -> Result<Vec<u8>> {
        let remote_path = quote_posix_shell_word(remote_path)?;
        let (stdout, stderr, code) = self.exec(&format!("cat < {remote_path}")).await?;
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

fn quote_posix_shell_word(value: &str) -> Result<String> {
    if value.contains('\0') {
        bail!("ssh-path: remote path contains a NUL byte");
    }
    Ok(format!("'{}'", value.replace('\'', "'\"'\"'")))
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

    #[test]
    fn test_quote_posix_shell_word_blocks_path_injection() {
        assert_eq!(
            quote_posix_shell_word("a'; touch /tmp/pwned; echo '").unwrap(),
            "'a'\"'\"'; touch /tmp/pwned; echo '\"'\"''"
        );
        assert!(quote_posix_shell_word("bad\0path").is_err());
    }
}
