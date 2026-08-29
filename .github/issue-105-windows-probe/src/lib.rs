//! CI-only compilation probe for Finch's exact OAuth and credential sources.
//!
//! The main Finch crate currently contains Unix-only IPC modules, so its
//! unrelated whole-crate Windows failure cannot demonstrate whether the OAuth
//! core correctly compiles its fail-closed non-Unix persistence branch.

#[path = "../../../src/config/credential.rs"]
mod credential;

mod config {
    pub use crate::credential::*;
}

#[path = "../../../src/oauth/mod.rs"]
mod oauth;
