//! CI-only compilation probe for the extracted OAuth and credential crate.
//!
//! The Finch binary still contains Unix-only IPC modules, so whole-crate
//! Windows failure cannot prove that the fail-closed non-Unix OAuth store
//! compiles. This probe depends on `finch-providers`, which now owns that
//! store and the credential types.

pub use finch_providers::oauth::FileOAuthCredentialStore;
pub use finch_providers::{
    AudienceBinding, CredentialKind, CredentialProvider, ProviderCredential,
};

/// Touch the fail-closed store constructor so Windows CI compiles the
/// non-Unix persistence branch.
pub fn store() -> FileOAuthCredentialStore {
    FileOAuthCredentialStore::new(std::path::PathBuf::from("oauth-probe"))
}
