use super::*;
use crate::config::EndpointFamily;
use anyhow::{bail, Result};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::Response;
use axum::routing::post;
use axum::Router;
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

#[derive(Default)]
struct MemoryStore(Mutex<BTreeMap<String, OAuthTokenRecord>>);

impl OAuthCredentialStore for MemoryStore {
    fn load(&self, reference: &str) -> Result<Option<OAuthTokenRecord>> {
        Ok(self.0.lock().unwrap().get(reference).cloned())
    }

    fn compare_and_swap(
        &self,
        reference: &str,
        expected_generation: Option<&str>,
        replacement: &OAuthTokenRecord,
    ) -> Result<()> {
        let mut records = self.0.lock().unwrap();
        if records
            .get(reference)
            .map(|record| record.generation.as_str())
            != expected_generation
        {
            bail!("generation mismatch");
        }
        records.insert(reference.into(), replacement.clone());
        Ok(())
    }
}

struct FakeReply {
    status: StatusCode,
    body: Value,
    delay: Duration,
}

#[derive(Default)]
struct FakeState {
    replies: Mutex<BTreeMap<String, VecDeque<FakeReply>>>,
    requests: Mutex<Vec<(String, String)>>,
}

struct FakeServer {
    origin: String,
    state: Arc<FakeState>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(FakeState::default());
        let app = Router::new()
            .fallback(post(fake_handler))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            origin,
            state,
            task,
        }
    }

    fn push(&self, path: &str, status: StatusCode, body: Value) {
        self.push_delayed(path, status, body, Duration::ZERO);
    }

    fn push_delayed(&self, path: &str, status: StatusCode, body: Value, delay: Duration) {
        self.state
            .replies
            .lock()
            .unwrap()
            .entry(path.into())
            .or_default()
            .push_back(FakeReply {
                status,
                body,
                delay,
            });
    }

    fn request_count(&self, path: &str) -> usize {
        self.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(actual, _)| actual == path)
            .count()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[test]
fn zero_and_tiny_device_intervals_are_clamped_to_a_safe_floor() {
    assert_eq!(RFC8628_DEFAULT_POLL_INTERVAL, Duration::from_secs(5));
    assert_eq!(bounded_poll_interval(Duration::ZERO), MIN_POLL_INTERVAL);
    assert_eq!(
        bounded_poll_interval(Duration::from_nanos(1)),
        MIN_POLL_INTERVAL
    );
    assert_eq!(
        bounded_poll_interval(MAX_POLL_INTERVAL + Duration::from_secs(1)),
        MAX_POLL_INTERVAL
    );
}

async fn fake_handler(State(state): State<Arc<FakeState>>, request: Request) -> Response<Body> {
    let path = request.uri().path().to_string();
    let bytes = axum::body::to_bytes(request.into_body(), MAX_AUTH_BODY_BYTES + 1)
        .await
        .unwrap();
    state
        .requests
        .lock()
        .unwrap()
        .push((path.clone(), String::from_utf8_lossy(&bytes).into_owned()));
    let reply = state
        .replies
        .lock()
        .unwrap()
        .get_mut(&path)
        .and_then(VecDeque::pop_front)
        .unwrap_or(FakeReply {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: json!({"error": "unexpected_request"}),
            delay: Duration::ZERO,
        });
    tokio::time::sleep(reply.delay).await;
    Response::builder()
        .status(reply.status.as_u16())
        .header("content-type", "application/json")
        .body(Body::from(reply.body.to_string()))
        .unwrap()
}

#[derive(Clone)]
struct SyntheticDialect {
    descriptor: OAuthDialectDescriptor,
    prefix: &'static str,
}

impl SyntheticDialect {
    fn new(origin: &str, prefix: &'static str) -> Self {
        let scopes = if prefix == "alpha" {
            BTreeSet::from(["alpha.read".into()])
        } else {
            BTreeSet::from(["beta.execute".into(), "beta.identity".into()])
        };
        Self {
            descriptor: OAuthDialectDescriptor {
                dialect_id: format!("synthetic_{prefix}"),
                protocol_revision: format!("{prefix}-v1"),
                provider: CredentialProvider::ChatgptSubscription,
                credential_kind: CredentialKind::OauthDevice,
                browser_credential_kind: Some(CredentialKind::OauthBrowserPkce),
                issuer: "openai-chatgpt".into(),
                audience: AudienceBinding::standard(EndpointFamily::ChatgptSubscription),
                client_id: format!("{prefix}-client"),
                scopes,
                device_authorization_endpoint: format!("{origin}/{prefix}/device"),
                device_token_endpoint: format!("{origin}/{prefix}/poll"),
                authorization_endpoint: format!("{origin}/{prefix}/authorize"),
                token_endpoint: format!("{origin}/{prefix}/token"),
                revocation_endpoint: format!("{origin}/{prefix}/revoke"),
                allowed_origins: BTreeSet::from([origin.into()]),
                allowed_user_authorization_origins: BTreeSet::from(
                    ["https://login.example".into()],
                ),
                allow_insecure_loopback: true,
            },
            prefix,
        }
    }

    fn request(&self, suffix: &str, body: Value) -> OAuthHttpRequest {
        OAuthHttpRequest {
            endpoint: format!(
                "{}/{}/{}",
                self.descriptor.allowed_origins.first().unwrap(),
                self.prefix,
                suffix
            ),
            body: if self.prefix == "alpha" {
                OAuthRequestBody::Json(body)
            } else {
                OAuthRequestBody::Form(vec![("payload".into(), body.to_string())])
            },
        }
    }
}

impl OAuthDialect for SyntheticDialect {
    fn descriptor(&self) -> &OAuthDialectDescriptor {
        &self.descriptor
    }

    fn device_authorization_request(&self) -> Result<OAuthHttpRequest> {
        Ok(self.request("device", json!({"client": self.descriptor.client_id})))
    }

    fn parse_device_authorization(
        &self,
        status: StatusCode,
        body: Value,
    ) -> Result<DeviceAuthorization> {
        if !status.is_success() {
            bail!("synthetic device failure");
        }
        let code_field = format!("{}_device", self.prefix);
        let user_field = format!("{}_user", self.prefix);
        Ok(DeviceAuthorization {
            device_code: body[&code_field].as_str().unwrap().into(),
            user_code: body[&user_field].as_str().unwrap().into(),
            verification_uri: body
                .get("verification_uri")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("https://login.example/{}/verify", self.prefix)),
            verification_uri_complete: None,
            expires_in: Duration::from_secs(2),
            interval: Duration::from_millis(10),
        })
    }

    fn device_poll_request(&self, pending: &DeviceAuthorization) -> Result<OAuthHttpRequest> {
        Ok(self.request("poll", json!({"device": pending.device_code})))
    }

    fn parse_device_poll(&self, status: StatusCode, body: Value) -> Result<DevicePoll> {
        if status.is_success() {
            return Ok(DevicePoll::Tokens(body));
        }
        match body.get("state").and_then(Value::as_str) {
            Some("pending") => Ok(DevicePoll::Pending),
            Some("slower") => Ok(DevicePoll::SlowDown),
            Some("denied") => Ok(DevicePoll::Denied),
            Some("expired") => Ok(DevicePoll::Expired),
            _ => bail!("synthetic poll contract changed"),
        }
    }

    fn authorization_code_request(
        &self,
        grant: &AuthorizationCodeGrant,
    ) -> Result<OAuthHttpRequest> {
        Ok(self.request(
            "token",
            json!({"code": grant.code, "verifier": grant.verifier}),
        ))
    }

    fn refresh_request(&self, refresh_token: &str) -> Result<OAuthHttpRequest> {
        Ok(self.request("token", json!({"refresh": refresh_token})))
    }

    fn revoke_request(&self, token: &str) -> Result<OAuthHttpRequest> {
        Ok(self.request("revoke", json!({"token": token})))
    }

    fn validate_tokens(
        &self,
        status: StatusCode,
        body: Value,
        previous: Option<&OAuthTokenRecord>,
        context: &TokenValidationContext,
    ) -> Result<OAuthTokenRecord> {
        if !status.is_success() {
            bail!("synthetic token failure");
        }
        let token_field = format!("{}_access", self.prefix);
        let account_field = format!("{}_account", self.prefix);
        if let TokenValidationContext::Browser { expected_nonce, .. } = context {
            if body.get("nonce").and_then(Value::as_str) != Some(expected_nonce) {
                bail!("synthetic nonce mismatch");
            }
        }
        let account = body[&account_field]
            .as_str()
            .unwrap_or("account")
            .to_string();
        if previous.is_some_and(|previous| previous.account != account) {
            bail!("synthetic account changed");
        }
        Ok(OAuthTokenRecord {
            dialect_id: self.descriptor.dialect_id.clone(),
            protocol_revision: self.descriptor.protocol_revision.clone(),
            provider: self.descriptor.provider,
            kind: if matches!(context, TokenValidationContext::Browser { .. }) {
                self.descriptor.browser_credential_kind.unwrap()
            } else {
                self.descriptor.credential_kind
            },
            issuer: self.descriptor.issuer.clone(),
            audience: self.descriptor.audience.clone(),
            client_id: self.descriptor.client_id.clone(),
            account,
            tenant: None,
            project: None,
            scopes: self.descriptor.scopes.clone(),
            access_token: body[&token_field].as_str().unwrap().into(),
            refresh_token: Some(
                body.get("refresh")
                    .and_then(Value::as_str)
                    .unwrap_or("refresh-one")
                    .into(),
            ),
            id_token: None,
            expires_at: Utc::now() + TimeDelta::hours(1),
            generation: random_secret(24),
            revoked: false,
            mutation_pending: false,
        })
    }
}

fn device_body(prefix: &str) -> Value {
    json!({
        (format!("{prefix}_device")): "device-secret",
        (format!("{prefix}_user")): "ABCD-EFGH"
    })
}

fn token_body(prefix: &str, account: &str, refresh: &str) -> Value {
    json!({
        (format!("{prefix}_access")): format!("{prefix}-access-secret"),
        (format!("{prefix}_account")): account,
        "refresh": refresh
    })
}

#[tokio::test]
async fn two_synthetic_dialects_share_device_core_without_provider_hard_coding() {
    for prefix in ["alpha", "beta"] {
        let server = FakeServer::start().await;
        server.push(
            &format!("/{prefix}/device"),
            StatusCode::OK,
            device_body(prefix),
        );
        server.push(
            &format!("/{prefix}/poll"),
            StatusCode::BAD_REQUEST,
            json!({"state": "pending"}),
        );
        server.push(
            &format!("/{prefix}/poll"),
            StatusCode::TOO_MANY_REQUESTS,
            json!({"state": "slower"}),
        );
        server.push(
            &format!("/{prefix}/poll"),
            StatusCode::OK,
            token_body(prefix, "account-one", "refresh-one"),
        );
        let store = Arc::new(MemoryStore::default());
        let client = OAuthClient::new(
            Arc::new(SyntheticDialect::new(&server.origin, prefix)),
            store.clone(),
        )
        .unwrap();
        let pending = client.begin_device_authorization().await.unwrap();
        let credential = client
            .finish_device_authorization("chatgpt:shared", &pending, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(credential.account.as_deref(), Some("account-one"));
        assert_eq!(credential.scopes, client.dialect.descriptor.scopes);
        assert_eq!(server.request_count(&format!("/{prefix}/poll")), 3);
        assert_eq!(store.0.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn device_denial_expiry_cancellation_and_timeout_are_terminal_without_persistence() {
    for state in ["denied", "expired"] {
        let server = FakeServer::start().await;
        let prefix = "alpha";
        server.push(
            &format!("/{prefix}/device"),
            StatusCode::OK,
            device_body(prefix),
        );
        server.push(
            &format!("/{prefix}/poll"),
            StatusCode::BAD_REQUEST,
            json!({"state": state}),
        );
        let store = Arc::new(MemoryStore::default());
        let client = OAuthClient::new(
            Arc::new(SyntheticDialect::new(&server.origin, prefix)),
            store.clone(),
        )
        .unwrap();
        let pending = client.begin_device_authorization().await.unwrap();
        let error = client
            .finish_device_authorization("chatgpt:test", &pending, CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(state));
        assert!(store.0.lock().unwrap().is_empty());
    }

    let server = FakeServer::start().await;
    server.push("/alpha/device", StatusCode::OK, device_body("alpha"));
    let store = Arc::new(MemoryStore::default());
    let client = OAuthClient::new(
        Arc::new(SyntheticDialect::new(&server.origin, "alpha")),
        store.clone(),
    )
    .unwrap();
    let pending = client.begin_device_authorization().await.unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(client
        .finish_device_authorization("chatgpt:test", &pending, cancel)
        .await
        .unwrap_err()
        .to_string()
        .contains("cancelled"));
    assert_eq!(server.request_count("/alpha/poll"), 0);

    server.push_delayed(
        "/alpha/poll",
        StatusCode::OK,
        token_body("alpha", "account", "refresh"),
        Duration::from_millis(100),
    );
    let timed = OAuthClient::new(
        Arc::new(SyntheticDialect::new(&server.origin, "alpha")),
        store.clone(),
    )
    .unwrap()
    .with_timeout(Duration::from_millis(10));
    assert!(timed
        .finish_device_authorization("chatgpt:timeout", &pending, CancellationToken::new())
        .await
        .unwrap_err()
        .to_string()
        .contains("timed out"));
    assert!(store.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn device_response_cannot_substitute_user_verification_origin_or_persist() {
    let server = FakeServer::start().await;
    let mut hostile = device_body("alpha");
    hostile["verification_uri"] = Value::String("https://evil.example/steal-code".into());
    server.push("/alpha/device", StatusCode::OK, hostile);
    let store = Arc::new(MemoryStore::default());
    let client = OAuthClient::new(
        Arc::new(SyntheticDialect::new(&server.origin, "alpha")),
        store.clone(),
    )
    .unwrap();
    let error = client
        .begin_device_authorization()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("allowed user origin"));
    assert!(store.0.lock().unwrap().is_empty());
    assert_eq!(server.request_count("/alpha/poll"), 0);
}

#[tokio::test]
async fn browser_pkce_state_nonce_and_redirect_are_correlated_before_persistence() {
    let server = FakeServer::start().await;
    let store = Arc::new(MemoryStore::default());
    let client = OAuthClient::new(
        Arc::new(SyntheticDialect::new(&server.origin, "alpha")),
        store.clone(),
    )
    .unwrap();
    let pending = client
        .begin_browser_authorization("http://127.0.0.1:12345/callback", Duration::from_secs(60))
        .unwrap();
    let state = pending.state.clone();
    server.push(
        "/alpha/token",
        StatusCode::OK,
        json!({
            "alpha_access": "secret-access",
            "alpha_account": "account-one",
            "refresh": "secret-refresh",
            "nonce": "wrong"
        }),
    );
    let callback = format!("http://127.0.0.1:12345/callback?code=secret-code&state={state}");
    assert!(client
        .finish_browser_authorization("chatgpt:browser", pending, &callback)
        .await
        .unwrap_err()
        .to_string()
        .contains("nonce mismatch"));
    assert!(store.0.lock().unwrap().is_empty());

    let pending = client
        .begin_browser_authorization("http://127.0.0.1:12345/callback", Duration::from_secs(60))
        .unwrap();
    let state = pending.state.clone();
    let nonce = pending.nonce.clone();
    server.push(
        "/alpha/token",
        StatusCode::OK,
        json!({
            "alpha_access": "secret-access",
            "alpha_account": "account-one",
            "refresh": "secret-refresh",
            "nonce": nonce
        }),
    );
    let callback = format!("http://127.0.0.1:12345/callback?code=secret-code&state={state}");
    client
        .finish_browser_authorization("chatgpt:browser", pending, &callback)
        .await
        .unwrap();
    assert_eq!(store.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn refresh_rotation_is_generation_checked_and_interruption_has_tombstone_recovery() {
    let server = FakeServer::start().await;
    let dialect = Arc::new(SyntheticDialect::new(&server.origin, "beta"));
    let store = Arc::new(MemoryStore::default());
    let mut initial = dialect
        .validate_tokens(
            StatusCode::OK,
            token_body("beta", "account-one", "refresh-old"),
            None,
            &TokenValidationContext::Device,
        )
        .unwrap();
    initial.generation = "generation-old".into();
    store
        .0
        .lock()
        .unwrap()
        .insert("chatgpt:work".into(), initial);
    server.push(
        "/beta/token",
        StatusCode::OK,
        token_body("beta", "account-one", "refresh-rotated"),
    );
    let client = OAuthClient::new(dialect, store.clone()).unwrap();
    client.refresh("chatgpt:work").await.unwrap();
    assert_eq!(
        store.0.lock().unwrap()["chatgpt:work"]
            .refresh_token
            .as_deref(),
        Some("refresh-rotated")
    );

    let mut interrupted = store.0.lock().unwrap()["chatgpt:work"].clone();
    interrupted.generation = "interrupted".into();
    interrupted.mutation_pending = true;
    store
        .0
        .lock()
        .unwrap()
        .insert("chatgpt:work".into(), interrupted);
    let error = client
        .refresh("chatgpt:work")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("credential recovery"));
    let tombstone = client
        .recover_interrupted_as_revoked("chatgpt:work")
        .unwrap();
    assert_eq!(tombstone.lifecycle, CredentialLifecycle::Revoked);
    let stored = &store.0.lock().unwrap()["chatgpt:work"];
    assert!(stored.revoked && stored.access_token.is_empty() && stored.refresh_token.is_none());
}

#[tokio::test]
async fn expired_refresh_revoke_crash_and_same_name_reauthentication_are_durable() {
    let server = FakeServer::start().await;
    let dialect = Arc::new(SyntheticDialect::new(&server.origin, "alpha"));
    let store = Arc::new(MemoryStore::default());
    let mut expired = dialect
        .validate_tokens(
            StatusCode::OK,
            token_body("alpha", "account-one", "refresh-one"),
            None,
            &TokenValidationContext::Device,
        )
        .unwrap();
    expired.expires_at = Utc::now() - TimeDelta::minutes(1);
    store
        .0
        .lock()
        .unwrap()
        .insert("chatgpt:work".into(), expired);
    server.push(
        "/alpha/token",
        StatusCode::OK,
        token_body("alpha", "account-one", "refresh-two"),
    );
    let client = OAuthClient::new(dialect.clone(), store.clone()).unwrap();
    client.refresh("chatgpt:work").await.unwrap();
    assert_eq!(server.request_count("/alpha/token"), 1);

    server.push_delayed(
        "/alpha/revoke",
        StatusCode::OK,
        json!({}),
        Duration::from_millis(100),
    );
    let timed = OAuthClient::new(dialect.clone(), store.clone())
        .unwrap()
        .with_timeout(Duration::from_millis(10));
    assert!(timed.revoke("chatgpt:work").await.is_err());
    assert!(store.0.lock().unwrap()["chatgpt:work"].mutation_pending);
    timed
        .recover_interrupted_as_revoked("chatgpt:work")
        .unwrap();
    assert!(store.0.lock().unwrap()["chatgpt:work"].revoked);

    server.push(
        "/alpha/poll",
        StatusCode::OK,
        token_body("alpha", "account-one", "refresh-three"),
    );
    let pending = DeviceAuthorization {
        device_code: "device-secret".into(),
        user_code: "ABCD-EFGH".into(),
        verification_uri: "https://login.example/alpha/verify".into(),
        verification_uri_complete: None,
        expires_in: Duration::from_secs(2),
        interval: Duration::ZERO,
    };
    client
        .finish_device_authorization("chatgpt:work", &pending, CancellationToken::new())
        .await
        .unwrap();
    let replacement = &store.0.lock().unwrap()["chatgpt:work"];
    assert!(!replacement.revoked && !replacement.mutation_pending);
    assert_eq!(replacement.refresh_token.as_deref(), Some("refresh-three"));

    server.push("/alpha/revoke", StatusCode::OK, json!({}));
    client.revoke("chatgpt:work").await.unwrap();
    let tombstone = &store.0.lock().unwrap()["chatgpt:work"];
    assert!(tombstone.revoked && !tombstone.mutation_pending);
    assert!(tombstone.access_token.is_empty() && tombstone.refresh_token.is_none());
}

#[test]
fn debug_and_errors_redact_transient_and_persisted_secrets() {
    let pending = DeviceAuthorization {
        device_code: "device-secret".into(),
        user_code: "user-secret".into(),
        verification_uri: "https://login.example/device".into(),
        verification_uri_complete: Some("https://login.example/device?code=user-secret".into()),
        expires_in: Duration::from_secs(60),
        interval: Duration::from_secs(1),
    };
    let debug = format!("{pending:?}");
    assert!(!debug.contains("device-secret"));
    assert!(!debug.contains("user-secret"));
    let context = TokenValidationContext::Browser {
        expected_nonce: "nonce-secret".into(),
        redirect_uri: "http://127.0.0.1/callback".into(),
    };
    assert!(!format!("{context:?}").contains("nonce-secret"));
}

#[test]
fn dialect_can_fail_closed_when_browser_public_client_contract_is_unsupported() {
    let mut dialect = SyntheticDialect::new("http://127.0.0.1:12345", "alpha");
    dialect.descriptor.browser_credential_kind = None;
    let client = OAuthClient::new(Arc::new(dialect), Arc::new(MemoryStore::default())).unwrap();
    let error = client
        .begin_browser_authorization("http://127.0.0.1:12346/callback", Duration::from_secs(60))
        .unwrap_err()
        .to_string();
    assert!(error.contains("unsupported by this provider dialect revision"));
}

#[test]
fn saved_oauth_tokens_project_through_174_binding_and_resolve_only_exact_account() {
    let dialect = SyntheticDialect::new("http://127.0.0.1:12345", "alpha");
    let record = dialect
        .validate_tokens(
            StatusCode::OK,
            token_body("alpha", "account-one", "refresh-one"),
            None,
            &TokenValidationContext::Device,
        )
        .unwrap();
    let credential = record.provider_credential("chatgpt:work");
    let binding = crate::config::CredentialBinding {
        credential_ref: "chatgpt:work".into(),
        audience: Some(AudienceBinding::standard(
            EndpointFamily::ChatgptSubscription,
        )),
        tenant: None,
        project: None,
        account: Some("account-one".into()),
        required_scopes: dialect.descriptor.scopes.clone(),
    };
    crate::config::credential::validate_binding(
        CredentialProvider::ChatgptSubscription,
        None,
        &binding,
        &credential,
        Utc::now(),
    )
    .unwrap();
    let store = Arc::new(MemoryStore::default());
    store
        .0
        .lock()
        .unwrap()
        .insert("chatgpt:work".into(), record);
    let resolver = StoredOAuthCredentialResolver::new(store.clone(), &dialect.descriptor).unwrap();
    assert_eq!(
        resolver.resolve(&credential).unwrap().credential_name,
        "chatgpt:work"
    );
    let mut wrong = credential.clone();
    wrong.account = Some("account-other".into());
    let error = resolver.resolve(&wrong).unwrap_err().to_string();
    assert!(error.contains("metadata or lifecycle"));
    assert!(!error.contains("alpha-access-secret"));
}

#[tokio::test]
async fn foreign_dialect_cannot_resolve_revoke_or_recover_a_stored_token() {
    let server = FakeServer::start().await;
    let alpha = SyntheticDialect::new(&server.origin, "alpha");
    let beta = SyntheticDialect::new(&server.origin, "beta");
    let mut record = alpha
        .validate_tokens(
            StatusCode::OK,
            token_body("alpha", "account-one", "refresh-one"),
            None,
            &TokenValidationContext::Device,
        )
        .unwrap();
    let credential = record.provider_credential("chatgpt:work");
    let store = Arc::new(MemoryStore::default());
    store
        .0
        .lock()
        .unwrap()
        .insert("chatgpt:work".into(), record.clone());

    let resolver = StoredOAuthCredentialResolver::new(store.clone(), &beta.descriptor).unwrap();
    assert!(resolver.resolve(&credential).is_err());
    let client = OAuthClient::new(Arc::new(beta), store.clone()).unwrap();
    assert!(client.revoke("chatgpt:work").await.is_err());
    assert_eq!(server.request_count("/beta/revoke"), 0);
    assert_eq!(
        store.0.lock().unwrap()["chatgpt:work"].generation,
        record.generation
    );

    record.mutation_pending = true;
    store
        .0
        .lock()
        .unwrap()
        .insert("chatgpt:work".into(), record.clone());
    assert!(client
        .recover_interrupted_as_revoked("chatgpt:work")
        .is_err());
    let unchanged = &store.0.lock().unwrap()["chatgpt:work"];
    assert!(unchanged.mutation_pending && !unchanged.revoked);
}
