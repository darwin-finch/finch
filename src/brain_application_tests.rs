//! Root application integration tests for the Brain boundary.

use crate::brain::test_support::{
    ephemeral_credential_authority, unix_epoch_millis as test_unix_epoch_millis,
    verify_portable_invitation,
};
use crate::brain::*;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::mpsc;

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
        let authority = ephemeral_credential_authority([91; 32]);
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
                .block_on(crate::server::start_ipc_server(
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
                let daemon_listener = tokio::net::TcpListener::from_std(daemon_listener).unwrap();
                let brain = axum::serve(
                    brain_listener,
                    crate::server::create_remote_brain_router(Arc::clone(&state))
                        .into_make_service_with_connect_info::<std::net::SocketAddr>(),
                );
                let daemon = axum::serve(
                    daemon_listener,
                    crate::server::create_router(state).into_make_service(),
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
    target.test_set_insecure();
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
    assert_ne!(address, crate::config::DEFAULT_DAEMON_ADDR);
    address
}

fn isolated_live_password() -> String {
    crate::brain::isolated_test_proof().unwrap();
    std::env::var("FINCH_TEST_BRAIN_PASSWORD")
        .expect("FINCH_TEST_BRAIN_PASSWORD must match the isolated daemon fixture")
}

async fn connect_isolated_live_ipc() -> crate::client::IpcClient {
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
    let client = crate::client::IpcClient::from_stream(stream).await.unwrap();
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

#[tokio::test]
async fn restricted_production_router_enforces_delegation_and_archive_scopes() {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use {
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
    let authority = ephemeral_credential_authority([82; 32]);
    let state = Arc::new(
        crate::server::AgentServer::for_brain_http_test(
            "fixture.local",
            temp.path(),
            authority.clone(),
        )
        .unwrap(),
    );
    let snapshot = state.brain_store().snapshot("shared").unwrap();
    let now = test_unix_epoch_millis();
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
    let tls_config =
        axum_server::tls_rustls::RustlsConfig::from_config(tls.rustls_server_config().unwrap());
    let app = crate::server::create_remote_brain_router(state);
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
    let parent = controller.test_credential_claims().await.unwrap();
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
    assert!(authority
        .verify(&child_token, test_unix_epoch_millis())
        .is_err());
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
        RemoteBrainClient::new_with_invitation(target.clone(), after_redemption.clone()).unwrap();
    redeemed.redeem_invitation("revoked-after").await.unwrap();
    let redeemed_token = redeemed.test_credential_token().await.unwrap();
    controller
        .revoke_delegated_invitation(&after_redemption)
        .await
        .unwrap();
    assert!(authority
        .verify(&redeemed_token, test_unix_epoch_millis())
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
    sibling
        .test_set_credential(sibling_token.clone(), sibling_claims.clone())
        .await;
    assert_eq!(
        sibling
            .test_http_client()
            .delete(
                sibling
                    .target
                    .test_delegated_credential_url(other_claims.credential_id)
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
            .test_http_client()
            .delete(
                sibling
                    .target
                    .test_delegated_credential_url(invitation.invitation_id),
            )
            .bearer_auth(&sibling_token)
            .json(&serde_json::json!({"invitation": invitation_token}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let parent_token = controller.test_credential_token().await.unwrap();
    assert_eq!(
        sibling
            .test_http_client()
            .delete(
                sibling
                    .target
                    .test_delegated_credential_url(parent.credential_id)
            )
            .bearer_auth(&sibling_token)
            .json(&serde_json::json!({"credential": parent_token}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let controller_invitation_claims =
        verify_portable_invitation(&controller_invitation, test_unix_epoch_millis())
            .unwrap()
            .0;
    assert_eq!(
        sibling
            .test_http_client()
            .delete(
                sibling
                    .target
                    .test_delegated_credential_url(controller_invitation_claims.invitation_id),
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
            crate::brain::ConnectionId(uuid::Uuid::new_v4()),
            now,
        )
        .unwrap();
    assert!(bound_claims.permits(BrainCredentialScope::BrainControl));
    assert!(bound_claims.permits(BrainCredentialScope::EnvironmentAdmin));
    for (method, url, body) in [
        (
            reqwest::Method::POST,
            controller.target.test_credentials_url(),
            serde_json::json!({
                "subject": "forbidden-child",
                "role": AttachmentRole::Observer,
                "scopes": [BrainCredentialScope::BrainRead],
                "ttl_ms": 1_000,
            }),
        ),
        (
            reqwest::Method::POST,
            controller.target.test_invitations_url(),
            serde_json::json!({
                "role": AttachmentRole::Observer,
                "ttl_ms": 1_000,
            }),
        ),
        (
            reqwest::Method::DELETE,
            controller
                .target
                .test_delegated_credential_url(other_claims.credential_id),
            serde_json::json!({"credential": other_token}),
        ),
        (
            reqwest::Method::DELETE,
            controller.target.test_http_url(),
            serde_json::json!({}),
        ),
    ] {
        assert_eq!(
            controller
                .test_http_client()
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
    let limited_token = limited.test_credential_token().await.unwrap();
    assert_eq!(
        limited
            .test_http_client()
            .post(limited.target.test_invitations_url())
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
            .test_http_client()
            .delete(limited.target.test_http_url())
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
        let hostile =
            RemoteBrainClient::new_with_invitation(target.clone(), controller_invitation.clone())
                .unwrap();
        hostile.test_set_credential(token, claims).await;
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
    expired
        .test_set_credential(expired_token, expired_claims)
        .await;
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
    revoked_child
        .test_set_credential(revoked_child_token, revoked_child_claims)
        .await;
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
async fn production_server_deduplicates_lost_replies_across_daemon_restarts() {
    use crate::brain::{
        BrainEventKind, BrainRunKind, BrainScheduleDeliveryPolicy, ProgramLanguage,
    };

    async fn start(
        root: &std::path::Path,
        credentials: BrainCredentialAuthority,
        environment_generation: u64,
    ) -> (
        RemoteBrainTarget,
        tokio::task::JoinHandle<()>,
        mpsc::UnboundedReceiver<crate::server::RunnerRequest>,
        crate::brain::BrainRunnerLease,
        crate::server::BrainLifecycleService,
    ) {
        let store = crate::brain::BrainStore::with_test_environment_generation(
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
                    && lease.expires_ms > test_unix_epoch_millis() =>
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
        let app = crate::server::create_router(server);
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
    let credentials = ephemeral_credential_authority([83; 32]);
    let (target, daemon, mut runner_rx, _, _) = start(temp.path(), credentials.clone(), 1).await;
    let (client, events, attachment) = attach(target, None).await;
    let source_program = "(emit \"exactly once\")";
    let program = BrainEventKind::Program {
        language: ProgramLanguage::Lisp,
        source: source_program.into(),
    };
    let handle = client.prepare_push_mutation(&program).await.unwrap();
    let effect_execution_id = uuid::Uuid::new_v4();
    tokio::spawn(async move {
        let crate::server::RunnerRequest::Program(request) = runner_rx.recv().await.unwrap() else {
            panic!("expected runner Program request")
        };
        let runtime = crate::runtime::ProgramRuntime::new();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: finch_programs::ProgramLanguage::Lisp,
                source_id: Some("remote-idempotency".into()),
                // The fixture runner supplies an authoritative checkpoint and
                // the acknowledged effect journal separately, as a real
                // frontend runner does after executing the submitted effect.
                source: "(define (checkpoint) : int 1)".into(),
                intent: "remote idempotency fixture".into(),
                effect: finch_programs::ExecutionEffect::Pure,
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
                        state: crate::vm::EffectJournalState::Acknowledged { values: Vec::new() },
                    },
                }],
            }))
            .unwrap();
    });
    crate::server::drop_next_remote_brain_reply_after_commit();
    assert!(client
        .push_with_handle(program.clone(), &handle)
        .await
        .is_err());
    drop(events);
    daemon.abort();
    let _ = daemon.await;

    let (target, daemon, _runner_rx, lease, _) = start(temp.path(), credentials.clone(), 1).await;
    let (client, events, rebound) = attach(target, Some(attachment.attachment_id)).await;
    assert_eq!(rebound.attachment_id, handle.attachment_id);
    let reply = client
        .test_send_remote_command_with_handle(
            crate::brain::test_support::BrainRemoteCommandKind::Submit(program.clone()),
            Some(&handle),
        )
        .await
        .unwrap();
    let crate::brain::test_support::BrainRemoteReply::Submitted {
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
        crate::brain::BrainRunStatus::Completed
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
        .prepare_runner_handoff_mutation("runner-b", lease.lease_id, environment_generation, 30_000)
        .await
        .unwrap();
    crate::server::drop_next_remote_brain_reply_after_commit();
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
    crate::server::drop_next_remote_brain_reply_after_commit();
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
            crate::brain::RunnerHandoffId(uuid::Uuid::new_v4()),
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
    crate::server::drop_next_remote_brain_reply_after_commit();
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
                event
                    .mutation
                    .as_ref()
                    .is_some_and(|receipt| receipt.mutation_id == schedule_handle.idempotency_key)
            })
            .count(),
        1
    );
    let cancel_schedule_handle = client
        .prepare_cancel_schedule_mutation(schedule.schedule_id)
        .await
        .unwrap();
    crate::server::drop_next_remote_brain_reply_after_commit();
    assert!(client
        .cancel_schedule_with_handle(schedule.schedule_id, &cancel_schedule_handle,)
        .await
        .is_err());
    daemon.abort();
    let _ = daemon.await;

    let (target, daemon, mut runner_rx, _, lifecycle) =
        start(temp.path(), credentials.clone(), 2).await;
    let (client, events, current_attachment) = attach(target, Some(attachment.attachment_id)).await;
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
                attached_mentions: Vec::new(),
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
            crate::brain::BrainRunStatus::Running,
            None,
        )
        .unwrap();
    let cancel_run_handle = client
        .prepare_cancel_run_mutation(cancellable.run_id)
        .await
        .unwrap();
    let cancellable_run_id = cancellable.run_id;
    tokio::spawn(async move {
        let crate::server::RunnerRequest::Cancel(request) = runner_rx.recv().await.unwrap() else {
            panic!("expected real runner cancellation")
        };
        assert_eq!(request.run_id, cancellable_run_id);
        request.response_tx.send(Ok(true)).unwrap();
    });
    crate::server::drop_next_remote_brain_reply_after_commit();
    assert!(client
        .cancel_run_with_handle(cancellable.run_id, &cancel_run_handle,)
        .await
        .is_err());
    drop(events);
    daemon.abort();
    let _ = daemon.await;

    let (target, daemon, mut runner_rx, _, lifecycle) =
        start(temp.path(), credentials.clone(), 2).await;
    let (client, events, current_attachment) = attach(target, Some(attachment.attachment_id)).await;
    let client = std::sync::Arc::new(client);
    assert_eq!(
        client
            .cancel_run_with_handle(cancellable.run_id, &cancel_run_handle,)
            .await
            .unwrap()
            .status,
        crate::brain::BrainRunStatus::Cancelled
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
        attached_mentions: Vec::new(),
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
                language: finch_programs::ProgramLanguage::Lisp,
                source_id: Some("live-remote-approval".into()),
                source: "(define (approved) : int 1)".into(),
                intent: "finish approved remote Prompt".into(),
                effect: finch_programs::ExecutionEffect::Pure,
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
            .filter(|event| event
                .mutation
                .as_ref()
                .is_some_and(|receipt| receipt.mutation_id == live_decision_handle.idempotency_key))
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
    let approval_audience = crate::brain::BrainApprovalAudience {
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
    crate::server::drop_next_remote_brain_reply_after_commit();
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
        credentials: BrainCredentialAuthority,
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
        let connection_id = crate::brain::ConnectionId(connection.connection_id);
        let claims = match crate::server::authorize_pending_remote_attachment(
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
            let envelope = crate::brain::test_support::BrainRemoteEnvelope::Projection(
                BrainWireMessage::Snapshot { brain: initial },
            );
            socket
                .send(axum::extract::ws::Message::Binary(
                    crate::brain::test_support::encode_brain_remote_envelope(&envelope)
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
            while let Some(Ok(axum::extract::ws::Message::Binary(bytes))) = socket.next().await {
                let Ok(crate::brain::test_support::BrainRemoteEnvelope::Command(command)) =
                    crate::brain::test_support::decode_brain_remote_envelope(&bytes)
                else {
                    break;
                };
                let request_id = command.request_id;
                let reply = match command.kind {
                    crate::brain::test_support::BrainRemoteCommandKind::ScheduleInitialization {
                        next_due_ms,
                    } => crate::server::execute_authorized_remote_initialization(
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
                let envelope = crate::brain::test_support::BrainRemoteEnvelope::Reply(reply);
                socket
                    .send(axum::extract::ws::Message::Binary(
                        crate::brain::test_support::encode_brain_remote_envelope(&envelope)
                            .unwrap()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }
        })
        .into_response()
    }

    let store = crate::brain::BrainStore::with_root("box.local", None);
    let lifecycle = crate::server::BrainLifecycleService::new(
        store,
        crate::server::BrainRunnerBroker::default(),
        crate::server::BrainApprovalBroker::default(),
    );
    let credentials = ephemeral_credential_authority([71; 32]);
    let snapshot = lifecycle.snapshot("shared").unwrap();
    let now_ms = test_unix_epoch_millis();
    let bind = |attachment: &BrainAttachment| {
        let role = attachment.role;
        let parent_token = credentials
            .issue(
                BrainCredentialRequest {
                    issuer: "box.local".into(),
                    subject: attachment.subject.clone(),
                    brain_id: snapshot.brain_id,
                    brain: "shared".into(),
                    environment_generation: snapshot.environment.generation,
                    role,
                    scopes: default_participant_scopes(role),
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
    let make_client = |attachment: BrainAttachment,
                       token: String,
                       claims: BrainCredentialClaims| {
        RemoteBrainClient::test_with_credential(target.clone(), attachment, token, claims).unwrap()
    };

    let stale = make_client(driver.clone(), sibling_token, sibling_claims);
    assert!(stale.watch().await.is_err());

    let driver_client = make_client(driver, driver_token, driver_claims);
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

    let consultant_client = make_client(consultant, consultant_token, consultant_claims);
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
            attached_mentions: Vec::new(),
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
        let Some(message) = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
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
    forged.test_set_attachment(first_attachment);
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
    use crate::brain::test_support::{BrainRemoteCommandKind, BrainRemoteReply};
    use crate::brain::{BrainRunKind, BrainRunStatus};

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
                    attached_mentions: Vec::new(),
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
            .test_send_remote_command(BrainRemoteCommandKind::Submit(BrainEventKind::Prompt {
                text: "same lifecycle".into(),
                attached_mentions: Vec::new(),
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
            .test_credential_claims()
            .await
            .unwrap()
            .credential_id;
        controller
            .test_http_client()
            .delete(format!(
                "{}://{}/v1/brains/credentials/{controller_credential_id}",
                controller.target.test_http_scheme(),
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

        let mut submitter = RemoteBrainClient::new(controller.target.clone(), "loopback").unwrap();
        submitter
            .attach("codex-submit@localhost", AttachmentRole::Driver, None)
            .await
            .unwrap();
        let _submit_events = submitter.watch().await.unwrap();
        let submitting_client = submitter.clone();
        let submission = tokio::task::spawn_local(async move {
            submitting_client
                .push(BrainEventKind::Program {
                    language: crate::brain::ProgramLanguage::Lisp,
                    source: "(say \"handoff-live\")".into(),
                })
                .await
        });
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), target_rx.recv())
            .await
            .unwrap()
            .expect("target runner callback closed");
        let crate::cli::ReplEvent::NamedBrainProgramRequested(request) = request else {
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

        match tokio::time::timeout(std::time::Duration::from_millis(100), source_rx.recv()).await {
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
