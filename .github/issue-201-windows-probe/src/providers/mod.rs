//! Compatibility names for the former Finch `providers` path includes.
//!
//! ChatGPT OAuth and OpenAI JWKS now live in `finch-providers`.

pub use finch_providers::{
    OpenAiChatGptOAuthDialect, OpenAiJwksVerifier, OpenAiTokenVerifier,
};
