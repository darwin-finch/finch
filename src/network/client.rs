// Lotus Network API client.
//
// Handles device registration, account linking, and future sync operations.
// The API endpoints are stubs — they'll be wired to the real Lotus server
// once it's deployed.
//
// API contract (to be implemented in ~/repos/lotus-network):
//
//   POST /v1/devices/register
//     Body: { device_id, fingerprint, finch_version, os, capabilities }
//     Response: { device_token }
//
//   POST /v1/devices/join-account
//     Auth: Bearer <device_token>
//     Body: { invite_code }
//     Response: { account_id, account_name }
//
//   GET /v1/devices/me
//     Auth: Bearer <device_token>
//     Response: { device_id, account_id?, account_name?, registered_at }

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Client for the Lotus Network API.
pub struct LotusClient {
    base_url: String,
    http: Client,
}

/// Payload sent when registering a new device.
#[derive(Debug, Serialize)]
pub struct RegisterDeviceRequest {
    pub device_id: Uuid,
    /// Human-readable hostname / device fingerprint.
    pub fingerprint: String,
    pub finch_version: String,
    pub os: String,
}

/// Response from device registration.
#[derive(Debug, Deserialize)]
pub struct RegisterDeviceResponse {
    pub device_token: String,
}

/// Payload for joining an account via invite code.
#[derive(Debug, Serialize)]
pub struct JoinAccountRequest {
    pub invite_code: String,
}

/// Response from joining an account.
#[derive(Debug, Deserialize)]
pub struct JoinAccountResponse {
    pub account_id: String,
    pub account_name: Option<String>,
}

/// Response from GET /v1/devices/me
#[derive(Debug, Deserialize)]
pub struct DeviceInfo {
    pub device_id: Uuid,
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub registered_at: Option<String>,
}

impl LotusClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            base_url: base_url.into(),
            http,
        })
    }

    /// Register this device with the Lotus Network (anonymous — no account needed).
    /// Returns a device token for future API calls.
    pub async fn register_device(
        &self,
        req: RegisterDeviceRequest,
    ) -> Result<RegisterDeviceResponse> {
        let url = format!("{}/v1/devices/register", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&req)
            .send()
            .await
            .context("Failed to reach Lotus Network")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lotus API error {}: {}", status, body);
        }

        resp.json::<RegisterDeviceResponse>()
            .await
            .context("Failed to parse Lotus registration response")
    }

    /// Link this device to a user account via an invite code.
    /// The invite code is generated in the Lotus web app and ties the device
    /// to the user's account.
    pub async fn join_account(
        &self,
        device_token: &str,
        invite_code: &str,
    ) -> Result<JoinAccountResponse> {
        let url = format!("{}/v1/devices/join-account", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(device_token)
            .json(&JoinAccountRequest {
                invite_code: invite_code.to_string(),
            })
            .send()
            .await
            .context("Failed to reach Lotus Network")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lotus API error {}: {}", status, body);
        }

        resp.json::<JoinAccountResponse>()
            .await
            .context("Failed to parse join-account response")
    }

    /// Fetch this device's current status from Lotus.
    pub async fn device_info(&self, device_token: &str) -> Result<DeviceInfo> {
        let url = format!("{}/v1/devices/me", self.base_url);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(device_token)
            .send()
            .await
            .context("Failed to reach Lotus Network")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lotus API error {}: {}", status, body);
        }

        resp.json::<DeviceInfo>()
            .await
            .context("Failed to parse device info response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_builds() {
        let c = LotusClient::new("https://api.lotusnetwork.dev");
        assert!(c.is_ok());
    }

    #[test]
    fn test_register_request_serializes() {
        let req = RegisterDeviceRequest {
            device_id: Uuid::new_v4(),
            fingerprint: "my-laptop".to_string(),
            finch_version: "0.5.0".to_string(),
            os: "darwin".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("device_id"));
        assert!(json.contains("fingerprint"));
        assert!(json.contains("finch_version"));
    }

    #[test]
    fn test_join_account_request_serializes() {
        let req = JoinAccountRequest {
            invite_code: "ABC-123".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("ABC-123"));
    }

    #[test]
    fn test_device_info_deserializes() {
        let json = r#"{
            "device_id": "550e8400-e29b-41d4-a716-446655440000",
            "account_id": "acc-1",
            "account_name": "Alice",
            "registered_at": "2026-02-19T00:00:00Z"
        }"#;
        let info: DeviceInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.account_id.as_deref(), Some("acc-1"));
        assert_eq!(info.account_name.as_deref(), Some("Alice"));
    }

    #[test]
    fn test_device_info_deserializes_minimal() {
        let json = r#"{
            "device_id": "550e8400-e29b-41d4-a716-446655440000"
        }"#;
        let info: DeviceInfo = serde_json::from_str(json).unwrap();
        assert!(info.account_id.is_none());
        assert!(info.account_name.is_none());
    }
}
