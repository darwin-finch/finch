use finch::ssh::{HostKeyPolicy, SshSession};
use russh::keys::ssh_key::private::{Ed25519Keypair, KeypairData};
use russh::keys::ssh_key::{HashAlg, PrivateKey, PublicKey};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const HOST_KEY_SEED: [u8; 32] = [7; 32];
const USER_KEY_SEED: [u8; 32] = [9; 32];
const OTHER_KEY_SEED: [u8; 32] = [11; 32];

struct TestServer {
    addr: SocketAddr,
    host_key_fingerprint: String,
    successful_authentications: Arc<AtomicUsize>,
    commands: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl TestServer {
    async fn start() -> Self {
        let host_key = private_key_from_seed(&HOST_KEY_SEED);
        let host_key_fingerprint = host_key
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();
        let authorized_key = private_key_from_seed(&USER_KEY_SEED).public_key().clone();
        let successful_authentications = Arc::new(AtomicUsize::new(0));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_state = FixtureServer {
            authorized_key,
            successful_authentications: successful_authentications.clone(),
            commands: commands.clone(),
        };
        let config = Arc::new(server::Config {
            keys: vec![host_key],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let task = tokio::spawn(async move {
            let mut server = server_state;
            server.run_on_socket(config, &listener).await.unwrap();
        });

        Self {
            addr,
            host_key_fingerprint,
            successful_authentications,
            commands,
            task,
        }
    }

    fn policy(&self) -> HostKeyPolicy {
        HostKeyPolicy::pinned_sha256(&self.host_key_fingerprint).unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct FixtureServer {
    authorized_key: PublicKey,
    successful_authentications: Arc<AtomicUsize>,
    commands: Arc<Mutex<Vec<String>>>,
}

impl server::Server for FixtureServer {
    type Handler = FixtureHandler;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self::Handler {
        FixtureHandler {
            authorized_key: self.authorized_key.clone(),
            successful_authentications: self.successful_authentications.clone(),
            commands: self.commands.clone(),
        }
    }
}

struct FixtureHandler {
    authorized_key: PublicKey,
    successful_authentications: Arc<AtomicUsize>,
    commands: Arc<Mutex<Vec<String>>>,
}

impl server::Handler for FixtureHandler {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == "finch" && password == "correct horse" {
            self.successful_authentications
                .fetch_add(1, Ordering::SeqCst);
            return Ok(Auth::Accept);
        }
        Ok(Auth::reject())
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        if user == "finch" && public_key == &self.authorized_key {
            self.successful_authentications
                .fetch_add(1, Ordering::SeqCst);
            return Ok(Auth::Accept);
        }
        Ok(Auth::reject())
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8(command.to_vec())?;
        self.commands.lock().unwrap().push(command.clone());
        session.channel_success(channel)?;

        let exit_status = if command == "fixture-command" {
            session.data(channel, b"stdout-data".to_vec())?;
            session.extended_data(channel, 1, b"stderr-data".to_vec())?;
            23
        } else if command.starts_with("cat < ") {
            session.data(channel, b"file-data".to_vec())?;
            0
        } else {
            0
        };

        session.exit_status_request(channel, exit_status)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

#[tokio::test]
async fn test_ssh_password_auth_host_key_exec_close_and_reconnect() {
    let server = TestServer::start().await;

    let mut session = SshSession::connect_password_with_host_key_policy(
        "127.0.0.1",
        server.addr.port(),
        "finch",
        "correct horse",
        server.policy(),
    )
    .await
    .unwrap();
    let (stdout, stderr, exit_status) = session.exec("fixture-command").await.unwrap();
    assert_eq!(stdout, "stdout-data");
    assert_eq!(stderr, "stderr-data");
    assert_eq!(exit_status, 23);
    session.close().await.unwrap();

    let reconnected = SshSession::connect_password_with_host_key_policy(
        "127.0.0.1",
        server.addr.port(),
        "finch",
        "correct horse",
        server.policy(),
    )
    .await
    .unwrap();
    reconnected.close().await.unwrap();
    assert_eq!(server.successful_authentications.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_ssh_password_and_host_key_failures_are_rejected() {
    let server = TestServer::start().await;

    let auth_error = SshSession::connect_password_with_host_key_policy(
        "127.0.0.1",
        server.addr.port(),
        "finch",
        "wrong password",
        server.policy(),
    )
    .await
    .err()
    .expect("wrong password must be rejected");
    assert!(auth_error.to_string().contains("authentication failed"));

    let wrong_host_key = private_key_from_seed(&OTHER_KEY_SEED)
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();
    let host_key_error = SshSession::connect_password_with_host_key_policy(
        "127.0.0.1",
        server.addr.port(),
        "finch",
        "correct horse",
        HostKeyPolicy::pinned_sha256(&wrong_host_key).unwrap(),
    )
    .await
    .err()
    .expect("wrong host key must be rejected");
    assert!(host_key_error.to_string().starts_with("ssh-connect:"));
}

#[tokio::test]
async fn test_ssh_ed25519_auth_success_and_failure() {
    let server = TestServer::start().await;

    let session = SshSession::connect_key_with_host_key_policy(
        "127.0.0.1",
        server.addr.port(),
        "finch",
        &USER_KEY_SEED,
        server.policy(),
    )
    .await
    .unwrap();
    session.close().await.unwrap();

    let auth_error = SshSession::connect_key_with_host_key_policy(
        "127.0.0.1",
        server.addr.port(),
        "finch",
        &OTHER_KEY_SEED,
        server.policy(),
    )
    .await
    .err()
    .expect("wrong Ed25519 key must be rejected");
    assert!(auth_error.to_string().contains("key authentication failed"));

    let length_error = SshSession::connect_key("127.0.0.1", server.addr.port(), "finch", &[1; 31])
        .await
        .err()
        .expect("wrong Ed25519 seed length must be rejected");
    assert!(length_error.to_string().contains("must be 32 bytes"));
}

#[tokio::test]
async fn test_ssh_legacy_connect_policy_is_explicit_accept_any() {
    let server = TestServer::start().await;
    let session =
        SshSession::connect_password("127.0.0.1", server.addr.port(), "finch", "correct horse")
            .await
            .unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn test_ssh_file_helpers_quote_untrusted_paths() {
    let server = TestServer::start().await;
    let mut session = SshSession::connect_password_with_host_key_policy(
        "127.0.0.1",
        server.addr.port(),
        "finch",
        "correct horse",
        server.policy(),
    )
    .await
    .unwrap();
    let hostile_path = "dir/file'; touch /tmp/pwned; echo '";

    session.write_file(hostile_path, b"contents").await.unwrap();
    assert_eq!(session.read_file(hostile_path).await.unwrap(), b"file-data");
    session.close().await.unwrap();

    let commands = server.commands.lock().unwrap();
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0],
        "printf '%s' 'Y29udGVudHM=' | base64 -d > 'dir/file'\"'\"'; touch /tmp/pwned; echo '\"'\"''"
    );
    assert_eq!(
        commands[1],
        "cat < 'dir/file'\"'\"'; touch /tmp/pwned; echo '\"'\"''"
    );
}

#[tokio::test]
async fn test_ssh_malformed_oversized_packet_is_bounded_and_rejected() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream
            .write_all(b"SSH-2.0-finch-malformed-test\r\n")
            .await
            .unwrap();
        stream
            .write_all(&[0xff, 0xff, 0xff, 0xff, 4, 0, 0, 0])
            .await
            .unwrap();
    });

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        SshSession::connect_password("127.0.0.1", addr.port(), "finch", "correct horse"),
    )
    .await
    .expect("oversized packet handling must be bounded");
    assert!(result.is_err());
    server.await.unwrap();
}

fn private_key_from_seed(seed: &[u8; 32]) -> PrivateKey {
    PrivateKey::new(KeypairData::Ed25519(Ed25519Keypair::from_seed(seed)), "").unwrap()
}
