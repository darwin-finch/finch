// Device membership — tracks this device's relationship to the Lotus Network.
//
// Three states:
//   Unregistered  — device has a UUID but hasn't contacted Lotus
//   Anonymous     — registered to Lotus, no user account
//   AccountMember — linked to a user account (has account_id + token)
//
// Persisted to ~/.finch/membership.json

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// This device's current relationship to the Lotus Network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MembershipStatus {
    /// Device has never contacted Lotus.
    Unregistered,
    /// Registered with Lotus, but not linked to a user account.
    Anonymous {
        /// Token issued by Lotus for anonymous device sessions.
        device_token: String,
    },
    /// Linked to a user account.
    AccountMember {
        /// Lotus user account ID.
        account_id: String,
        /// Auth token for this device.
        device_token: String,
        /// Display name for the account (optional).
        account_name: Option<String>,
    },
}

impl MembershipStatus {
    pub fn is_registered(&self) -> bool {
        !matches!(self, MembershipStatus::Unregistered)
    }

    pub fn device_token(&self) -> Option<&str> {
        match self {
            MembershipStatus::Anonymous { device_token } => Some(device_token),
            MembershipStatus::AccountMember { device_token, .. } => Some(device_token),
            _ => None,
        }
    }

    pub fn account_id(&self) -> Option<&str> {
        match self {
            MembershipStatus::AccountMember { account_id, .. } => Some(account_id),
            _ => None,
        }
    }
}

/// Full membership state for this device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceMembership {
    /// This device's stable UUID (UUID v5).
    pub device_id: Uuid,
    /// Current membership status.
    pub status: MembershipStatus,
    /// Lotus Network base URL (configurable for self-hosting).
    pub lotus_url: String,
}

impl DeviceMembership {
    const DEFAULT_LOTUS_URL: &'static str = "https://api.lotusnetwork.dev";

    /// Load from disk, or create a fresh unregistered membership.
    pub fn load_or_create(device_id: Uuid) -> Result<Self> {
        let path = Self::path()?;

        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read membership from {}", path.display()))?;
            let m: Self =
                serde_json::from_str(&raw).with_context(|| "Failed to parse membership JSON")?;
            return Ok(m);
        }

        let m = Self {
            device_id,
            status: MembershipStatus::Unregistered,
            lotus_url: Self::DEFAULT_LOTUS_URL.to_string(),
        };
        m.save()?;
        Ok(m)
    }

    /// Persist to disk.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create ~/.finch directory")?;
        }
        let json = serde_json::to_string_pretty(self).context("Failed to serialize membership")?;
        std::fs::write(&path, json)
            .with_context(|| format!("Failed to write membership to {}", path.display()))?;
        Ok(())
    }

    fn path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        Ok(home.join(".finch").join("membership.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id() -> Uuid {
        Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
    }

    #[test]
    fn test_unregistered_is_not_registered() {
        assert!(!MembershipStatus::Unregistered.is_registered());
    }

    #[test]
    fn test_anonymous_is_registered() {
        let s = MembershipStatus::Anonymous {
            device_token: "tok".to_string(),
        };
        assert!(s.is_registered());
        assert_eq!(s.device_token(), Some("tok"));
        assert!(s.account_id().is_none());
    }

    #[test]
    fn test_account_member_fields() {
        let s = MembershipStatus::AccountMember {
            account_id: "acc-1".to_string(),
            device_token: "tok-2".to_string(),
            account_name: Some("Alice".to_string()),
        };
        assert!(s.is_registered());
        assert_eq!(s.device_token(), Some("tok-2"));
        assert_eq!(s.account_id(), Some("acc-1"));
    }

    #[test]
    fn test_membership_status_serde_roundtrip_anonymous() {
        let s = MembershipStatus::Anonymous {
            device_token: "t".to_string(),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: MembershipStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn test_membership_status_serde_roundtrip_account_member() {
        let s = MembershipStatus::AccountMember {
            account_id: "a".to_string(),
            device_token: "t".to_string(),
            account_name: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: MembershipStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn test_device_membership_construction() {
        let id = make_id();
        let m = DeviceMembership {
            device_id: id,
            status: MembershipStatus::Unregistered,
            lotus_url: DeviceMembership::DEFAULT_LOTUS_URL.to_string(),
        };
        assert_eq!(m.device_id, id);
        assert!(!m.status.is_registered());
    }
}
