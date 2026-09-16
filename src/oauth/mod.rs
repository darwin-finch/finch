//! Compatibility facade for provider-neutral OAuth.
//!
//! Implementation lives in `finch-providers::oauth`.

pub use finch_providers::oauth::{
    validate_reference, AuthorizationCodeGrant, DeviceAuthorization, DevicePoll,
    FileOAuthCredentialStore, OAuthClient, OAuthCredentialCommit, OAuthCredentialPersistenceError,
    OAuthCredentialStore, OAuthDeviceAuthorizationError, OAuthDialect, OAuthDialectDescriptor,
    OAuthHttpRequest, OAuthRequestBody, OAuthTokenRecord, PendingBrowserAuthorization,
    StoredOAuthCredentialResolver, TokenValidationContext,
};
