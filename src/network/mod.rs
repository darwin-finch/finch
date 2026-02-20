// Network module — Lotus Network device registration and membership.
//
// Each finch instance is a device on the Lotus Network. Devices can:
//   - Exist standalone (no account required)
//   - Join the Lotus Network anonymously (just a UUID, no signup)
//   - Link to a user account (multiple devices per account)
//
// The device UUID is deterministic (UUID v5) so reinstalling finch on
// the same machine gives the same device ID.

pub mod client;
pub mod membership;

pub use client::LotusClient;
pub use membership::{DeviceMembership, MembershipStatus};
