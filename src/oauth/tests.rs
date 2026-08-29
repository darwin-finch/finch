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
    body: String,
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
        Self::from_listener(listener)
    }

    async fn start_ipv6() -> Self {
        let listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
        Self::from_listener(listener)
    }

    fn from_listener(listener: tokio::net::TcpListener) -> Self {
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
                body: body.to_string(),
                delay,
            });
    }

    fn push_raw(&self, path: &str, status: StatusCode, body: String) {
        self.state
            .replies
            .lock()
            .unwrap()
            .entry(path.into())
            .or_default()
            .push_back(FakeReply {
                status,
                body,
                delay: Duration::ZERO,
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
            body: json!({"error": "unexpected_request"}).to_string(),
            delay: Duration::ZERO,
        });
    tokio::time::sleep(reply.delay).await;
    Response::builder()
        .status(reply.status.as_u16())
        .header("content-type", "application/json")
        .body(Body::from(reply.body))
        .unwrap()
}

#[derive(Clone)]
struct SyntheticDialect {
    descriptor: OAuthDialectDescriptor,
    prefix: &'static str,
    refresh_kind_override: Option<CredentialKind>,
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
            refresh_kind_override: None,
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

#[async_trait::async_trait]
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
        DeviceAuthorization::issued(
            body[&code_field].as_str().unwrap().into(),
            body[&user_field].as_str().unwrap().into(),
            body.get("verification_uri")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("https://login.example/{}/verify", self.prefix)),
            None,
            Duration::from_secs(2),
            Duration::from_millis(10),
        )
    }

    fn device_poll_request(&self, pending: &DeviceAuthorization) -> Result<OAuthHttpRequest> {
        Ok(self.request("poll", json!({"device": pending.device_code})))
    }

    fn parse_device_poll(&self, status: StatusCode, body: Value) -> Result<DevicePoll> {
        if let Some(code) = body.get("authorization_code").and_then(Value::as_str) {
            return Ok(DevicePoll::AuthorizationCode(AuthorizationCodeGrant {
                code: code.into(),
                verifier: "device-verifier".into(),
                redirect_uri: "http://127.0.0.1/device".into(),
            }));
        }
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

    async fn validate_tokens(
        &self,
        status: StatusCode,
        body: Value,
        previous: Option<&OAuthTokenRecord>,
        context: &TokenValidationContext,
        _cancel: &CancellationToken,
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
            kind: match context {
                TokenValidationContext::Browser { .. } => {
                    self.descriptor.browser_credential_kind.unwrap()
                }
                TokenValidationContext::Refresh => self
                    .refresh_kind_override
                    .unwrap_or_else(|| previous.unwrap().kind),
                TokenValidationContext::Device => self.descriptor.credential_kind,
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
        assert!(client
            .finish_device_authorization("chatgpt:shared", &pending, CancellationToken::new())
            .await
            .is_err());
        assert_eq!(server.request_count(&format!("/{prefix}/poll")), 3);
    }
}

#[tokio::test]
async fn bracketed_ipv6_origin_validates_descriptor_and_outbound_endpoint_exactly_once() {
    let server = FakeServer::start_ipv6().await;
    server.push("/alpha/device", StatusCode::OK, device_body("alpha"));
    let dialect = Arc::new(SyntheticDialect::new(&server.origin, "alpha"));
    dialect.descriptor().validate().unwrap();
    assert_eq!(
        origin(&Url::parse(&dialect.descriptor().device_authorization_endpoint).unwrap()).unwrap(),
        server.origin
    );
    let client = OAuthClient::new(dialect, Arc::new(MemoryStore::default())).unwrap();
    let pending = client.begin_device_authorization().await.unwrap();
    assert_eq!(pending.user_code, "ABCD-EFGH");
    assert_eq!(server.request_count("/alpha/device"), 1);
    assert_eq!(server.request_count("/alpha/poll"), 0);
}

#[tokio::test]
async fn device_deadline_and_shared_completion_claim_cannot_be_restarted_or_duplicated() {
    let server = FakeServer::start().await;
    let dialect = Arc::new(SyntheticDialect::new(&server.origin, "alpha"));
    let store = Arc::new(MemoryStore::default());
    let client = OAuthClient::new(dialect.clone(), store.clone()).unwrap();

    let expired = DeviceAuthorization::issued(
        "expired-device".into(),
        "ABCD-EFGH".into(),
        "https://login.example/alpha/verify".into(),
        None,
        Duration::from_millis(20),
        Duration::ZERO,
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(client
        .finish_device_authorization("chatgpt:expired-start", &expired, CancellationToken::new())
        .await
        .unwrap_err()
        .to_string()
        .contains("expired"));
    assert_eq!(server.request_count("/alpha/poll"), 0);
    assert!(store.0.lock().unwrap().is_empty());

    server.push_delayed(
        "/alpha/poll",
        StatusCode::OK,
        token_body("alpha", "account-one", "refresh-one"),
        Duration::from_millis(30),
    );
    let pending = DeviceAuthorization::issued(
        "shared-device".into(),
        "IJKL-MNOP".into(),
        "https://login.example/alpha/verify".into(),
        None,
        Duration::from_secs(2),
        Duration::ZERO,
    )
    .unwrap();
    let duplicate = pending.clone();
    let first = client.finish_device_authorization(
        "chatgpt:single-consumer",
        &pending,
        CancellationToken::new(),
    );
    let second = client.finish_device_authorization(
        "chatgpt:single-consumer",
        &duplicate,
        CancellationToken::new(),
    );
    let (first, second) = tokio::join!(first, second);
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let rejected = first.err().or_else(|| second.err()).unwrap().to_string();
    assert!(rejected.contains("already claimed"));
    assert_eq!(server.request_count("/alpha/poll"), 1);
    assert_eq!(server.request_count("/alpha/token"), 0);
    assert_eq!(store.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn both_synthetic_dialects_share_refresh_and_revoke_lifecycle_without_fallback() {
    for prefix in ["alpha", "beta"] {
        let server = FakeServer::start().await;
        let dialect = Arc::new(SyntheticDialect::new(&server.origin, prefix));
        let store = Arc::new(MemoryStore::default());
        let record = dialect
            .validate_tokens(
                StatusCode::OK,
                token_body(prefix, "account-one", "refresh-one"),
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        store
            .0
            .lock()
            .unwrap()
            .insert("chatgpt:account".into(), record);
        server.push(
            &format!("/{prefix}/token"),
            StatusCode::OK,
            token_body(prefix, "account-one", "refresh-two"),
        );
        server.push(&format!("/{prefix}/revoke"), StatusCode::OK, json!({}));
        let client = OAuthClient::new(dialect, store.clone()).unwrap();
        client
            .refresh("chatgpt:account", CancellationToken::new())
            .await
            .unwrap();
        client
            .revoke("chatgpt:account", CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(server.request_count(&format!("/{prefix}/token")), 1);
        assert_eq!(server.request_count(&format!("/{prefix}/revoke")), 1);
        assert!(store.0.lock().unwrap()["chatgpt:account"].revoked);
    }
}

#[tokio::test]
async fn refresh_preserves_browser_kind_and_empty_revocation_success_commits_tombstone() {
    let server = FakeServer::start().await;
    let dialect = Arc::new(SyntheticDialect::new(&server.origin, "alpha"));
    let store = Arc::new(MemoryStore::default());
    let browser = dialect
        .validate_tokens(
            StatusCode::OK,
            json!({
                "alpha_access": "browser-access",
                "alpha_account": "account-one",
                "refresh": "browser-refresh",
                "nonce": "browser-nonce"
            }),
            None,
            &TokenValidationContext::Browser {
                expected_nonce: "browser-nonce".into(),
                redirect_uri: "http://127.0.0.1/callback".into(),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(browser.kind, CredentialKind::OauthBrowserPkce);
    store
        .0
        .lock()
        .unwrap()
        .insert("chatgpt:browser".into(), browser);
    server.push(
        "/alpha/token",
        StatusCode::OK,
        token_body("alpha", "account-one", "browser-refresh-two"),
    );
    let client = OAuthClient::new(dialect, store.clone()).unwrap();
    client
        .refresh("chatgpt:browser", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        store.0.lock().unwrap()["chatgpt:browser"].kind,
        CredentialKind::OauthBrowserPkce
    );

    server.push_raw("/alpha/revoke", StatusCode::NO_CONTENT, String::new());
    client
        .revoke("chatgpt:browser", CancellationToken::new())
        .await
        .unwrap();
    let tombstone = store.0.lock().unwrap()["chatgpt:browser"].clone();
    assert!(tombstone.revoked && !tombstone.mutation_pending);
    assert!(tombstone.access_token.is_empty() && tombstone.refresh_token.is_none());
    assert_eq!(server.request_count("/alpha/revoke"), 1);

    let hostile_server = FakeServer::start().await;
    let mut switching = SyntheticDialect::new(&hostile_server.origin, "alpha");
    switching.refresh_kind_override = Some(CredentialKind::OauthDevice);
    let browser = switching
        .validate_tokens(
            StatusCode::OK,
            json!({
                "alpha_access": "browser-access",
                "alpha_account": "account-one",
                "refresh": "browser-refresh",
                "nonce": "browser-nonce"
            }),
            None,
            &TokenValidationContext::Browser {
                expected_nonce: "browser-nonce".into(),
                redirect_uri: "http://127.0.0.1/callback".into(),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let hostile_store = Arc::new(MemoryStore::default());
    hostile_store
        .0
        .lock()
        .unwrap()
        .insert("chatgpt:kind-switch".into(), browser);
    hostile_server.push(
        "/alpha/token",
        StatusCode::OK,
        token_body("alpha", "account-one", "browser-refresh-two"),
    );
    let hostile = OAuthClient::new(Arc::new(switching), hostile_store.clone()).unwrap();
    assert!(hostile
        .refresh("chatgpt:kind-switch", CancellationToken::new())
        .await
        .is_err());
    let pending = hostile_store.0.lock().unwrap()["chatgpt:kind-switch"].clone();
    assert!(pending.mutation_pending);
    assert_eq!(pending.kind, CredentialKind::OauthBrowserPkce);
    assert_eq!(hostile_server.request_count("/alpha/token"), 1);
}

#[tokio::test]
async fn malformed_and_oversized_oauth_responses_fail_before_follow_on_activity() {
    let hostile_bodies = [
        "{malformed-json".to_string(),
        json!({"padding": "x".repeat(MAX_AUTH_BODY_BYTES)}).to_string(),
    ];
    for body in hostile_bodies.clone() {
        let server = FakeServer::start().await;
        server.push_raw("/alpha/device", StatusCode::OK, body);
        let client = OAuthClient::new(
            Arc::new(SyntheticDialect::new(&server.origin, "alpha")),
            Arc::new(MemoryStore::default()),
        )
        .unwrap();
        assert!(client.begin_device_authorization().await.is_err());
        assert_eq!(server.request_count("/alpha/device"), 1);
        assert_eq!(server.request_count("/alpha/poll"), 0);
    }

    for body in hostile_bodies.clone() {
        let server = FakeServer::start().await;
        server.push_raw("/alpha/poll", StatusCode::OK, body);
        let store = Arc::new(MemoryStore::default());
        let client = OAuthClient::new(
            Arc::new(SyntheticDialect::new(&server.origin, "alpha")),
            store.clone(),
        )
        .unwrap();
        let pending = DeviceAuthorization::issued(
            "poll-device".into(),
            "ABCD-EFGH".into(),
            "https://login.example/alpha/verify".into(),
            None,
            Duration::from_secs(2),
            Duration::ZERO,
        )
        .unwrap();
        assert!(client
            .finish_device_authorization("chatgpt:poll", &pending, CancellationToken::new())
            .await
            .is_err());
        assert_eq!(server.request_count("/alpha/poll"), 1);
        assert_eq!(server.request_count("/alpha/token"), 0);
        assert!(store.0.lock().unwrap().is_empty());
    }

    for body in hostile_bodies.clone() {
        let server = FakeServer::start().await;
        server.push(
            "/alpha/poll",
            StatusCode::OK,
            json!({"authorization_code": "code-secret"}),
        );
        server.push_raw("/alpha/token", StatusCode::OK, body);
        let store = Arc::new(MemoryStore::default());
        let client = OAuthClient::new(
            Arc::new(SyntheticDialect::new(&server.origin, "alpha")),
            store.clone(),
        )
        .unwrap();
        let pending = DeviceAuthorization::issued(
            "code-device".into(),
            "IJKL-MNOP".into(),
            "https://login.example/alpha/verify".into(),
            None,
            Duration::from_secs(2),
            Duration::ZERO,
        )
        .unwrap();
        assert!(client
            .finish_device_authorization("chatgpt:code", &pending, CancellationToken::new())
            .await
            .is_err());
        assert_eq!(server.request_count("/alpha/poll"), 1);
        assert_eq!(server.request_count("/alpha/token"), 1);
        assert!(store.0.lock().unwrap().is_empty());
    }

    for body in hostile_bodies.clone() {
        let server = FakeServer::start().await;
        let dialect = Arc::new(SyntheticDialect::new(&server.origin, "alpha"));
        let store = Arc::new(MemoryStore::default());
        let initial = dialect
            .validate_tokens(
                StatusCode::OK,
                token_body("alpha", "account-one", "refresh-one"),
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        store
            .0
            .lock()
            .unwrap()
            .insert("chatgpt:refresh-hostile".into(), initial);
        server.push_raw("/alpha/token", StatusCode::OK, body);
        let client = OAuthClient::new(dialect, store.clone()).unwrap();
        assert!(client
            .refresh("chatgpt:refresh-hostile", CancellationToken::new())
            .await
            .is_err());
        assert_eq!(server.request_count("/alpha/token"), 1);
        assert!(store.0.lock().unwrap()["chatgpt:refresh-hostile"].mutation_pending);
    }

    for (status, body) in [
        (StatusCode::BAD_REQUEST, "{malformed-json".to_string()),
        (
            StatusCode::OK,
            json!({"padding": "x".repeat(MAX_AUTH_BODY_BYTES)}).to_string(),
        ),
    ] {
        let server = FakeServer::start().await;
        let dialect = Arc::new(SyntheticDialect::new(&server.origin, "alpha"));
        let store = Arc::new(MemoryStore::default());
        let initial = dialect
            .validate_tokens(
                StatusCode::OK,
                token_body("alpha", "account-one", "refresh-one"),
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        store
            .0
            .lock()
            .unwrap()
            .insert("chatgpt:revoke-hostile".into(), initial);
        server.push_raw("/alpha/revoke", status, body);
        let client = OAuthClient::new(dialect, store.clone()).unwrap();
        assert!(client
            .revoke("chatgpt:revoke-hostile", CancellationToken::new())
            .await
            .is_err());
        assert_eq!(server.request_count("/alpha/revoke"), 1);
        assert!(store.0.lock().unwrap()["chatgpt:revoke-hostile"].mutation_pending);
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
    let timeout_pending = DeviceAuthorization::issued(
        "timeout-device".into(),
        "IJKL-MNOP".into(),
        "https://login.example/alpha/verify".into(),
        None,
        Duration::from_secs(2),
        Duration::ZERO,
    )
    .unwrap();
    assert!(timed
        .finish_device_authorization(
            "chatgpt:timeout",
            &timeout_pending,
            CancellationToken::new()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("timed out"));
    assert!(store.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn delayed_device_poll_and_code_exchange_cannot_persist_after_terminal_state() {
    let server = FakeServer::start().await;
    let dialect = Arc::new(SyntheticDialect::new(&server.origin, "alpha"));
    let store = Arc::new(MemoryStore::default());
    let client = OAuthClient::new(dialect, store.clone()).unwrap();
    let pending = DeviceAuthorization::issued(
        "device-secret".into(),
        "ABCD-EFGH".into(),
        "https://login.example/alpha/verify".into(),
        None,
        Duration::from_millis(30),
        Duration::ZERO,
    )
    .unwrap();
    server.push_delayed(
        "/alpha/poll",
        StatusCode::OK,
        token_body("alpha", "account-one", "refresh-one"),
        Duration::from_millis(100),
    );
    assert!(client
        .finish_device_authorization("chatgpt:expired", &pending, CancellationToken::new())
        .await
        .unwrap_err()
        .to_string()
        .contains("expired"));
    assert!(store.0.lock().unwrap().is_empty());

    server.push(
        "/alpha/poll",
        StatusCode::OK,
        json!({"authorization_code": "code-secret"}),
    );
    server.push_delayed(
        "/alpha/token",
        StatusCode::OK,
        token_body("alpha", "account-one", "refresh-two"),
        Duration::from_millis(100),
    );
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        trigger.cancel();
    });
    let pending = DeviceAuthorization::issued(
        "device-secret".into(),
        "ABCD-EFGH".into(),
        "https://login.example/alpha/verify".into(),
        None,
        Duration::from_secs(2),
        Duration::ZERO,
    )
    .unwrap();
    assert!(client
        .finish_device_authorization("chatgpt:cancelled", &pending, cancel)
        .await
        .unwrap_err()
        .to_string()
        .contains("cancelled"));
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
        .finish_browser_authorization(
            "chatgpt:browser",
            pending,
            &callback,
            CancellationToken::new()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("nonce mismatch"));
    assert!(store.0.lock().unwrap().is_empty());

    for hostile_redirect in [
        "http://user@127.0.0.1:12345/callback",
        "http://user:pass@127.0.0.1:12345/callback",
        "http://127.0.0.1:12345/callback?fixed=value",
        "http://127.0.0.1:12345/callback#fragment",
    ] {
        assert!(client
            .begin_browser_authorization(hostile_redirect, Duration::from_secs(60))
            .is_err());
    }
    for hostile_callback in ["userinfo", "extra", "fragment", "path"] {
        let pending = client
            .begin_browser_authorization("http://127.0.0.1:12345/callback", Duration::from_secs(60))
            .unwrap();
        let state = pending.state.clone();
        let callback = match hostile_callback {
            "userinfo" => {
                format!("http://user@127.0.0.1:12345/callback?code=secret-code&state={state}")
            }
            "extra" => format!(
                "http://127.0.0.1:12345/callback?code=secret-code&state={state}&extra=value"
            ),
            "fragment" => {
                format!("http://127.0.0.1:12345/callback?code=secret-code&state={state}#fragment")
            }
            "path" => format!("http://127.0.0.1:12345/other?code=secret-code&state={state}"),
            _ => unreachable!(),
        };
        assert!(client
            .finish_browser_authorization(
                "chatgpt:hostile",
                pending,
                &callback,
                CancellationToken::new()
            )
            .await
            .is_err());
    }
    assert_eq!(server.request_count("/alpha/token"), 1);

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
        .finish_browser_authorization(
            "chatgpt:browser",
            pending,
            &callback,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(store.0.lock().unwrap().len(), 1);

    let pending = client
        .begin_browser_authorization("http://[::1]:12345/callback", Duration::from_secs(60))
        .unwrap();
    let state = pending.state.clone();
    let nonce = pending.nonce.clone();
    server.push(
        "/alpha/token",
        StatusCode::OK,
        json!({
            "alpha_access": "ipv6-access",
            "alpha_account": "account-one",
            "refresh": "ipv6-refresh",
            "nonce": nonce
        }),
    );
    let callback = format!("http://[::1]:12345/callback?code=secret-code&state={state}");
    client
        .finish_browser_authorization(
            "chatgpt:browser-ipv6",
            pending,
            &callback,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let pending = client
        .begin_browser_authorization("http://127.0.0.1:12345/callback", Duration::from_millis(30))
        .unwrap();
    let state = pending.state.clone();
    let nonce = pending.nonce.clone();
    server.push_delayed(
        "/alpha/token",
        StatusCode::OK,
        json!({
            "alpha_access": "late-access",
            "alpha_account": "account-one",
            "refresh": "late-refresh",
            "nonce": nonce
        }),
        Duration::from_millis(100),
    );
    let callback = format!("http://127.0.0.1:12345/callback?code=secret-code&state={state}");
    assert!(client
        .finish_browser_authorization(
            "chatgpt:browser-late",
            pending,
            &callback,
            CancellationToken::new()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("expired"));
    assert!(!store.0.lock().unwrap().contains_key("chatgpt:browser-late"));

    let pending = client
        .begin_browser_authorization("http://127.0.0.1:12345/callback", Duration::from_secs(2))
        .unwrap();
    let state = pending.state.clone();
    let nonce = pending.nonce.clone();
    server.push_delayed(
        "/alpha/token",
        StatusCode::OK,
        json!({
            "alpha_access": "cancelled-access",
            "alpha_account": "account-one",
            "refresh": "cancelled-refresh",
            "nonce": nonce
        }),
        Duration::from_millis(100),
    );
    let callback = format!("http://127.0.0.1:12345/callback?code=secret-code&state={state}");
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        trigger.cancel();
    });
    assert!(client
        .finish_browser_authorization("chatgpt:browser-cancelled", pending, &callback, cancel)
        .await
        .unwrap_err()
        .to_string()
        .contains("cancelled"));
    assert!(!store
        .0
        .lock()
        .unwrap()
        .contains_key("chatgpt:browser-cancelled"));
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
            &CancellationToken::new(),
        )
        .await
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
    client
        .refresh("chatgpt:work", CancellationToken::new())
        .await
        .unwrap();
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
        .refresh("chatgpt:work", CancellationToken::new())
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
async fn refresh_cancellation_before_replacement_persistence_keeps_recoverable_marker() {
    let server = FakeServer::start().await;
    let dialect = Arc::new(SyntheticDialect::new(&server.origin, "alpha"));
    let store = Arc::new(MemoryStore::default());
    let initial = dialect
        .validate_tokens(
            StatusCode::OK,
            token_body("alpha", "account-one", "refresh-old"),
            None,
            &TokenValidationContext::Device,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    store
        .0
        .lock()
        .unwrap()
        .insert("chatgpt:cancel-refresh".into(), initial);
    server.push_delayed(
        "/alpha/token",
        StatusCode::OK,
        token_body("alpha", "account-one", "refresh-new"),
        Duration::from_millis(100),
    );
    let client = OAuthClient::new(dialect, store.clone()).unwrap();
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        trigger.cancel();
    });
    assert!(client
        .refresh("chatgpt:cancel-refresh", cancel)
        .await
        .unwrap_err()
        .to_string()
        .contains("cancelled"));
    let stored = &store.0.lock().unwrap()["chatgpt:cancel-refresh"];
    assert!(stored.mutation_pending);
    assert_eq!(stored.refresh_token.as_deref(), Some("refresh-old"));
    assert!(!format!("{stored:?}").contains("refresh-old"));
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
            &CancellationToken::new(),
        )
        .await
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
    client
        .refresh("chatgpt:work", CancellationToken::new())
        .await
        .unwrap();
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
    assert!(timed
        .revoke("chatgpt:work", CancellationToken::new())
        .await
        .is_err());
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
    let pending = DeviceAuthorization::issued(
        "device-secret".into(),
        "ABCD-EFGH".into(),
        "https://login.example/alpha/verify".into(),
        None,
        Duration::from_secs(2),
        Duration::ZERO,
    )
    .unwrap();
    client
        .finish_device_authorization("chatgpt:work", &pending, CancellationToken::new())
        .await
        .unwrap();
    let replacement = store.0.lock().unwrap()["chatgpt:work"].clone();
    assert!(!replacement.revoked && !replacement.mutation_pending);
    assert_eq!(replacement.refresh_token.as_deref(), Some("refresh-three"));

    server.push("/alpha/revoke", StatusCode::OK, json!({}));
    client
        .revoke("chatgpt:work", CancellationToken::new())
        .await
        .unwrap();
    let tombstone = &store.0.lock().unwrap()["chatgpt:work"];
    assert!(tombstone.revoked && !tombstone.mutation_pending);
    assert!(tombstone.access_token.is_empty() && tombstone.refresh_token.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn file_store_reopens_pending_revoke_and_recovers_durable_tombstone() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("oauth");
    let server = FakeServer::start().await;
    let dialect = Arc::new(SyntheticDialect::new(&server.origin, "alpha"));
    let initial = dialect
        .validate_tokens(
            StatusCode::OK,
            token_body("alpha", "account-one", "refresh-one"),
            None,
            &TokenValidationContext::Device,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let store = Arc::new(file_store::FileOAuthCredentialStore::new(root.clone()));
    store
        .compare_and_swap("chatgpt:file", None, &initial)
        .unwrap();
    server.push_delayed(
        "/alpha/revoke",
        StatusCode::NO_CONTENT,
        json!({}),
        Duration::from_millis(100),
    );
    let client = OAuthClient::new(dialect.clone(), store.clone())
        .unwrap()
        .with_timeout(Duration::from_millis(10));
    assert!(client
        .revoke("chatgpt:file", CancellationToken::new())
        .await
        .is_err());
    assert!(
        store
            .load("chatgpt:file")
            .unwrap()
            .unwrap()
            .mutation_pending
    );
    drop(client);
    drop(store);

    let reopened = Arc::new(file_store::FileOAuthCredentialStore::new(root.clone()));
    let recovery = OAuthClient::new(dialect, reopened.clone()).unwrap();
    let metadata = recovery
        .recover_interrupted_as_revoked("chatgpt:file")
        .unwrap();
    assert_eq!(metadata.lifecycle, CredentialLifecycle::Revoked);
    drop(recovery);
    drop(reopened);

    let verified = file_store::FileOAuthCredentialStore::new(root)
        .load("chatgpt:file")
        .unwrap()
        .unwrap();
    assert!(verified.revoked && !verified.mutation_pending);
    assert!(verified.access_token.is_empty() && verified.refresh_token.is_none());
    assert_eq!(server.request_count("/alpha/revoke"), 1);
}

#[test]
fn debug_and_errors_redact_transient_and_persisted_secrets() {
    let pending = DeviceAuthorization::issued(
        "device-secret".into(),
        "user-secret".into(),
        "https://login.example/device".into(),
        Some("https://login.example/device?code=user-secret".into()),
        Duration::from_secs(60),
        Duration::from_secs(1),
    )
    .unwrap();
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

#[tokio::test]
async fn saved_oauth_tokens_project_through_174_binding_and_resolve_only_exact_account() {
    let dialect = SyntheticDialect::new("http://127.0.0.1:12345", "alpha");
    let record = dialect
        .validate_tokens(
            StatusCode::OK,
            token_body("alpha", "account-one", "refresh-one"),
            None,
            &TokenValidationContext::Device,
            &CancellationToken::new(),
        )
        .await
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
            &CancellationToken::new(),
        )
        .await
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
    assert!(client
        .revoke("chatgpt:work", CancellationToken::new())
        .await
        .is_err());
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
