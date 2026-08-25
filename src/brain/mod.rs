//! Canonical durable Brain state, credentials, and client transports.
//!
//! Speculative/background activity is represented by `BrainRun` records in
//! the named Brain service. There is deliberately no second client-local
//! "Brain session" or hidden context-injection path here.

pub mod credential;
pub mod names;
pub mod remote;
pub mod shared;
