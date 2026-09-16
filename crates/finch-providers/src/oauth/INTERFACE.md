# oauth — public interface

Generated from [`crates/finch-providers/src/oauth/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-providers/src/oauth/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// A provider-issued authorization code plus the correlated verifier.
pub struct AuthorizationCodeGrant { … }
/// Result of an initial RFC 8628 device authorization request.
pub struct DeviceAuthorization { … }
impl DeviceAuthorization {
    /// Create locally-issued pending device authority whose expiry cannot be restarted by delaying or cloning the completion call.
    pub fn issued(device_code: String, user_code: String, verification_uri: String, verification_uri_complete: Option<String>, expires_in: Duration, interval: Duration) -> Result<Self>;
}
/// Provider interpretation of one device polling response.
pub enum DevicePoll { Pending, SlowDown, Denied, Expired, Tokens, AuthorizationCode }
/// Private 0700 directory containing atomic 0600 token records on Unix.
pub struct FileOAuthCredentialStore { … }
impl FileOAuthCredentialStore {
    /// Read without creating the credential root, lock file, or any other filesystem object.
    pub fn load_existing(&self, reference: &str) -> Result<Option<OAuthTokenRecord>>;
    /// Create a store rooted at `root` without touching the filesystem.
    pub fn new(root: PathBuf) -> Self;
}
/// Provider-neutral OAuth state machine.
pub struct OAuthClient<D, S> { … }
impl OAuthClient {
    /// Start a device authorization without persisting transient codes.
    pub async fn begin_device_authorization(&self) -> Result<DeviceAuthorization>;
    /// Start device authorization with caller-owned cancellation authority.
    pub async fn begin_device_authorization_cancellable(&self, cancel: CancellationToken) -> Result<DeviceAuthorization>;
    /// Correlate an exact loopback callback, exchange its code, and persist it.
    pub async fn finish_browser_authorization(&self, reference: &str, pending: PendingBrowserAuthorization, callback_url: &str, cancel: CancellationToken) -> Result<ProviderCredential>;
    /// Poll until terminal state, persist exactly one validated account, and return its #174 metadata.
    pub async fn finish_device_authorization(&self, reference: &str, pending: &DeviceAuthorization, cancel: CancellationToken) -> Result<ProviderCredential>;
    pub async fn finish_device_authorization_commit(&self, reference: &str, pending: &DeviceAuthorization, cancel: CancellationToken) -> Result<OAuthCredentialCommit>;
    /// Refresh with a crash marker and generation-checked token rotation.
    pub async fn refresh(&self, reference: &str, cancel: CancellationToken) -> Result<ProviderCredential>;
    /// Revoke remotely, then retain a durable local tombstone.
    pub async fn revoke(&self, reference: &str, cancel: CancellationToken) -> Result<ProviderCredential>;
    /// Begin browser authorization using state, nonce, and S256 PKCE.
    pub fn begin_browser_authorization(&self, redirect_uri: &str, lifetime: Duration) -> Result<PendingBrowserAuthorization>;
    pub fn new(dialect: Arc<D>, store: Arc<S>) -> Result<Self>;
    /// Reject any conflicting local record before a device authorization request is allowed to leave the process.
    pub fn preflight_reauthentication(&self, reference: &str) -> Result<()>;
    /// Resolve a crash-interrupted mutation without trusting or transmitting its possibly rotated tokens.
    pub fn recover_interrupted_as_revoked(&self, reference: &str) -> Result<ProviderCredential>;
    /// Locally tombstone an exact bound credential without transmitting it.
    pub fn tombstone_local_generation(&self, reference: &str, expected_generation: &str) -> Result<ProviderCredential>;
    /// Validate an active record before projecting it into public config.
    pub fn validate_active_reuse(&self, record: &OAuthTokenRecord) -> Result<()>;
    /// Validate a loaded record's complete immutable dialect authority.
    pub fn validate_existing_binding(&self, record: &OAuthTokenRecord) -> Result<()>;
    /// Construct with injected HTTP/clock/sleeper ports.
    pub fn with_ports(dialect: Arc<D>, store: Arc<S>, ports: ProviderPorts) -> Result<Self>;
}
/// Generation-bound result of one successful credential CAS.
pub struct OAuthCredentialCommit { … }
/// Secret-free marker for a validated OAuth grant that could not be committed.
pub enum OAuthCredentialPersistenceError { Commit }
/// Typed terminal device-authorization outcomes used by interactive recovery.
pub enum OAuthDeviceAuthorizationError { Cancelled, Expired, Denied }
/// Stable, provider-owned OAuth protocol description.
pub struct OAuthDialectDescriptor { … }
impl OAuthDialectDescriptor {
    /// Validate all static authority before any request is constructed.
    pub fn validate(&self) -> Result<()>;
}
/// An HTTP form request selected entirely by a provider dialect.
pub struct OAuthHttpRequest { … }
/// Audited request encoding chosen by a dialect.
pub enum OAuthRequestBody { Form, Json }
/// Provider-validated OAuth token record.
pub struct OAuthTokenRecord { … }
impl OAuthTokenRecord {
    /// Construct #174 metadata without copying secret material into config.
    pub fn provider_credential(&self, name: &str) -> ProviderCredential;
}
/// Pending browser authorization state.
pub struct PendingBrowserAuthorization { … }
/// Local #174 resolver backed by the same OAuth persistence used by login, refresh, recovery, and logout.
pub struct StoredOAuthCredentialResolver<S> { … }
impl StoredOAuthCredentialResolver {
    pub fn new(store: Arc<S>, descriptor: &OAuthDialectDescriptor) -> Result<Self>;
}
/// Correlation authority passed to provider-specific token validation.
pub enum TokenValidationContext { Device, Browser, Refresh }
```

## Traits

```rust
/// Crash-safe token persistence boundary with generation compare-and-swap.
pub trait OAuthCredentialStore: Send + Sync {
    fn load(&self, reference: &str) -> Result<Option<OAuthTokenRecord>>;
    fn compare_and_swap(&self, reference: &str, expected_generation: Option<&str>, replacement: &OAuthTokenRecord) -> Result<()>;
}
/// Strict provider dialect boundary.
pub trait OAuthDialect: Send + Sync {
    fn descriptor(&self) -> &OAuthDialectDescriptor;
    fn preflight(&self) -> Result<()>;
    fn device_authorization_request(&self) -> Result<OAuthHttpRequest>;
    fn parse_device_authorization(&self, status: StatusCode, body: Value) -> Result<DeviceAuthorization>;
    fn parse_device_authorization_response(&self, status: StatusCode, body: &[u8]) -> Result<DeviceAuthorization>;
    fn device_poll_request(&self, pending: &DeviceAuthorization) -> Result<OAuthHttpRequest>;
    fn parse_device_poll(&self, status: StatusCode, body: Value) -> Result<DevicePoll>;
    fn parse_device_poll_response(&self, status: StatusCode, body: &[u8]) -> Result<DevicePoll>;
    fn authorization_code_request(&self, grant: &AuthorizationCodeGrant) -> Result<OAuthHttpRequest>;
    fn refresh_request(&self, refresh_token: &str) -> Result<OAuthHttpRequest>;
    fn revoke_request(&self, token: &str) -> Result<OAuthHttpRequest>;
}
```

## Functions

```rust
pub fn validate_reference(reference: &str) -> Result<()> { … }
pub(crate) fn validate_secret_field(value: &str, label: &str) -> Result<()> { … }
```

## Constants

```rust
pub(crate) const MAX_AUTH_BODY_BYTES: usize = 64 * 1024;
```
