# oauth — public interface

Generated from [`src/oauth/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/oauth/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// A provider-issued authorization code plus the correlated verifier.
pub struct AuthorizationCodeGrant { … }
/// Result of an initial RFC 8628 device authorization request.
pub struct DeviceAuthorization { … }
/// Provider interpretation of one device polling response.
pub enum DevicePoll { Pending, SlowDown, Denied, Expired, Tokens, AuthorizationCode }
/// Private 0700 directory containing atomic 0600 token records on Unix.
pub struct FileOAuthCredentialStore { … }
/// Provider-neutral OAuth state machine.
pub struct OAuthClient<D, S> { … }
/// Generation-bound result of one successful credential CAS.
pub struct OAuthCredentialCommit { … }
/// Secret-free marker for a validated OAuth grant that could not be committed.
pub enum OAuthCredentialPersistenceError { Commit }
/// Typed terminal device-authorization outcomes used by interactive recovery.
pub enum OAuthDeviceAuthorizationError { Cancelled, Expired, Denied }
/// Stable, provider-owned OAuth protocol description.
pub struct OAuthDialectDescriptor { … }
/// An HTTP form request selected entirely by a provider dialect.
pub struct OAuthHttpRequest { … }
/// Audited request encoding chosen by a dialect.
pub enum OAuthRequestBody { Form, Json }
/// Provider-validated OAuth token record.
pub struct OAuthTokenRecord { … }
/// Pending browser authorization state.
pub struct PendingBrowserAuthorization { … }
/// Local #174 resolver backed by the same OAuth persistence used by login, refresh, recovery, and logout.
pub struct StoredOAuthCredentialResolver<S> { … }
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
    async fn validate_tokens(&self, status: StatusCode, body: Value, previous: Option<&OAuthTokenRecord>, context: &TokenValidationContext, cancel: &CancellationToken) -> Result<OAuthTokenRecord>;
    async fn validate_token_response(&self, status: StatusCode, body: &[u8], previous: Option<&OAuthTokenRecord>, context: &TokenValidationContext, cancel: &CancellationToken) -> Result<OAuthTokenRecord>;
}
```

## Functions

```rust
pub fn validate_reference(reference: &str) -> Result<()> { … }
```
