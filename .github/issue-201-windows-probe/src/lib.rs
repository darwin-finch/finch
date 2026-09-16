//! CI-only compile probe for ChatGPT OAuth dialect and OpenAI JWKS on Windows.
//!
//! Sources live in `finch-providers`. This probe compiles that crate rather
//! than `#[path]`-including Finch application files.

pub use finch_providers::oauth::FileOAuthCredentialStore;
pub use finch_providers::{
    OpenAiChatGptOAuthDialect, OpenAiJwksVerifier, OpenAiTokenVerifier,
    CHATGPT_OAUTH_PROTOCOL_REVISION,
};

mod providers;

/// Force the JWKS verifier and dialect constructors into the Windows compile.
pub fn chatgpt_auth_surface() {
    let _ = OpenAiJwksVerifier::production();
    let _ = OpenAiChatGptOAuthDialect::production();
    let _ = FileOAuthCredentialStore::new(std::path::PathBuf::from("oauth-probe"));
}
