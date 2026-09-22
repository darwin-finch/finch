//! Compatibility facade for provider-neutral OAuth.
//!
//! Implementation lives in the private `oauth` module of `finch-providers`,
//! re-exported flat from that crate's facade.

pub use finch_providers::{
    validate_reference, AuthorizationCodeGrant, DeviceAuthorization, DevicePoll,
    FileOAuthCredentialStore, OAuthClient, OAuthCredentialCommit, OAuthCredentialPersistenceError,
    OAuthCredentialStore, OAuthDeviceAuthorizationError, OAuthDialect, OAuthDialectDescriptor,
    OAuthHttpRequest, OAuthRequestBody, OAuthTokenRecord, PendingBrowserAuthorization,
    StoredOAuthCredentialResolver, TokenValidationContext,
};
