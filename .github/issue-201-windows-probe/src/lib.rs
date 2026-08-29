//! CI-only exact-source compile probe for the provider-neutral OAuth core and
//! OpenAI verifier on Windows, where Finch's unrelated IPC target is not yet
//! portable.

#[path = "../../../src/config/credential.rs"]
mod credential;

mod config {
    pub use crate::credential::*;
}

#[path = "../../../src/oauth/mod.rs"]
mod oauth;

mod providers;
