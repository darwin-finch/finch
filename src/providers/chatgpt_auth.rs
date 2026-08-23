//! Native ChatGPT device authorization for Finch.
//!
//! This intentionally lives behind a small module boundary: ChatGPT device
//! authorization is distinct from OpenAI API-key authentication and its HTTP
//! contract is not part of the public OpenAI API reference.

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_AUTH_BASE_URL: &str = "https://auth.openai.com";
const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEVICE_VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
const DEVICE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatGptTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: String,
    pub expires_at: u64,
}

impl ChatGptTokens {
    fn needs_refresh(&self) -> bool {
        self.expires_at <= unix_time().saturating_add(60)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct UserCodeResponse {
    device_auth_id: String,
    user_code: String,
    #[serde(default = "default_poll_interval")]
    interval: u64,
}

fn default_poll_interval() -> u64 {
    5
}

#[derive(Debug, Clone)]
pub struct PendingDeviceLogin {
    pub user_code: String,
    pub verification_url: String,
    device_auth_id: String,
    poll_interval: Duration,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
    id_token: Option<String>,
}

fn default_expires_in() -> u64 {
    3600
}

#[derive(Clone)]
pub struct ChatGptAuth {
    client: Client,
    auth_base_url: String,
    client_id: String,
    token_path: PathBuf,
}

impl ChatGptAuth {
    pub fn new() -> Result<Self> {
        let token_path = dirs::home_dir()
            .context("Cannot determine home directory for ChatGPT credentials")?
            .join(".finch")
            .join("auth")
            .join("chatgpt.json");
        Self::with_options(DEFAULT_AUTH_BASE_URL, DEFAULT_CLIENT_ID, token_path)
    }

    fn with_options(
        auth_base_url: impl Into<String>,
        client_id: impl Into<String>,
        token_path: PathBuf,
    ) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .context("Failed to create ChatGPT authentication client")?,
            auth_base_url: auth_base_url.into().trim_end_matches('/').to_string(),
            client_id: client_id.into(),
            token_path,
        })
    }

    pub async fn begin_device_login(&self) -> Result<PendingDeviceLogin> {
        let response = self
            .client
            .post(format!(
                "{}/api/accounts/deviceauth/usercode",
                self.auth_base_url
            ))
            .json(&serde_json::json!({ "client_id": self.client_id }))
            .send()
            .await
            .context("Failed to request a ChatGPT device code")?;

        if !response.status().is_success() {
            bail!(
                "ChatGPT device authorization is unavailable (HTTP {}): {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }
        let code: UserCodeResponse = response
            .json()
            .await
            .context("Invalid ChatGPT device-code response")?;
        Ok(PendingDeviceLogin {
            user_code: code.user_code,
            verification_url: DEVICE_VERIFICATION_URL.to_string(),
            device_auth_id: code.device_auth_id,
            poll_interval: Duration::from_secs(code.interval.max(1)),
        })
    }

    pub async fn finish_device_login(&self, pending: &PendingDeviceLogin) -> Result<ChatGptTokens> {
        let deadline = tokio::time::Instant::now() + DEVICE_TIMEOUT;
        loop {
            if tokio::time::Instant::now() >= deadline {
                bail!("ChatGPT device authorization timed out after 15 minutes");
            }
            let response = self
                .client
                .post(format!(
                    "{}/api/accounts/deviceauth/token",
                    self.auth_base_url
                ))
                .json(&serde_json::json!({
                    "device_auth_id": pending.device_auth_id,
                    "user_code": pending.user_code,
                }))
                .send()
                .await
                .context("Failed while waiting for ChatGPT device authorization")?;

            if response.status().is_success() {
                let device_token: DeviceTokenResponse = response
                    .json()
                    .await
                    .context("Invalid ChatGPT device authorization response")?;
                let tokens = self.exchange_authorization_code(device_token).await?;
                self.save(&tokens)?;
                return Ok(tokens);
            }

            if matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::FORBIDDEN
            ) {
                tokio::time::sleep(pending.poll_interval).await;
                continue;
            }

            bail!(
                "ChatGPT device authorization failed (HTTP {}): {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }
    }

    async fn exchange_authorization_code(
        &self,
        device_token: DeviceTokenResponse,
    ) -> Result<ChatGptTokens> {
        let body = form_body(&[
            ("grant_type", "authorization_code"),
            ("code", &device_token.authorization_code),
            ("client_id", &self.client_id),
            (
                "redirect_uri",
                "https://auth.openai.com/deviceauth/callback",
            ),
            ("code_verifier", &device_token.code_verifier),
        ]);
        let response = self
            .client
            .post(format!("{}/oauth/token", self.auth_base_url))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .context("Failed to exchange ChatGPT authorization code")?;
        self.decode_token_response(response, None).await
    }

    pub async fn tokens(&self) -> Result<ChatGptTokens> {
        let current = self.load()?;
        if !current.needs_refresh() {
            return Ok(current);
        }
        let body = form_body(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &current.refresh_token),
            ("client_id", &self.client_id),
        ]);
        let response = self
            .client
            .post(format!("{}/oauth/token", self.auth_base_url))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .context("Failed to refresh ChatGPT credentials")?;
        let refreshed = self.decode_token_response(response, Some(&current)).await?;
        self.save(&refreshed)?;
        Ok(refreshed)
    }

    async fn decode_token_response(
        &self,
        response: reqwest::Response,
        previous: Option<&ChatGptTokens>,
    ) -> Result<ChatGptTokens> {
        if !response.status().is_success() {
            bail!(
                "ChatGPT token request failed (HTTP {}): {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }
        let response: OAuthTokenResponse = response
            .json()
            .await
            .context("Invalid ChatGPT token response")?;
        let account_id = account_id_from_jwt(&response.access_token)
            .or_else(|| response.id_token.as_deref().and_then(account_id_from_jwt))
            .or_else(|| previous.map(|tokens| tokens.account_id.clone()))
            .context("ChatGPT token did not contain an account identifier")?;
        let refresh_token = response
            .refresh_token
            .or_else(|| previous.map(|tokens| tokens.refresh_token.clone()))
            .context("ChatGPT token response did not contain a refresh token")?;
        Ok(ChatGptTokens {
            access_token: response.access_token,
            refresh_token,
            account_id,
            expires_at: unix_time().saturating_add(response.expires_in),
        })
    }

    fn load(&self) -> Result<ChatGptTokens> {
        let bytes = std::fs::read(&self.token_path).with_context(|| {
            format!(
                "No Finch ChatGPT login found at {}; run `finch auth login chatgpt`",
                self.token_path.display()
            )
        })?;
        serde_json::from_slice(&bytes).context("Finch ChatGPT credential file is invalid")
    }

    fn save(&self, tokens: &ChatGptTokens) -> Result<()> {
        let parent = self
            .token_path
            .parent()
            .context("ChatGPT credential path has no parent")?;
        std::fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(tokens)?;
        write_private_file(&self.token_path, &bytes)
    }
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn form_body(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn account_id_from_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .or_else(|| claims.get("chatgpt_account_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("Credential path has no parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(bytes)?;
    temp.as_file_mut().sync_all()?;
    temp.persist(path)
        .map_err(|error| error.error)
        .context("Failed to save ChatGPT credentials")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jwt(claims: serde_json::Value) -> String {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        format!("header.{payload}.signature")
    }

    #[test]
    fn extracts_nested_chatgpt_account_id() {
        let token = jwt(json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct-123" }
        }));
        assert_eq!(account_id_from_jwt(&token).as_deref(), Some("acct-123"));
    }

    #[test]
    fn form_encoding_does_not_corrupt_refresh_tokens() {
        assert_eq!(
            form_body(&[("refresh_token", "a+b/c=")]),
            "refresh_token=a%2Bb%2Fc%3D"
        );
    }

    #[test]
    fn saves_credentials_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth").join("chatgpt.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private_file(&path, b"secret").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn device_flow_exchanges_and_persists_tokens() {
        let mut server = mockito::Server::new_async().await;
        let access = jwt(json!({ "chatgpt_account_id": "acct-test" }));
        let _user_code = server
            .mock("POST", "/api/accounts/deviceauth/usercode")
            .match_body(mockito::Matcher::PartialJson(
                json!({"client_id": "client"}),
            ))
            .with_status(200)
            .with_body(
                json!({
                    "device_auth_id": "device",
                    "user_code": "ABCD-EFGH",
                    "interval": 1
                })
                .to_string(),
            )
            .create_async()
            .await;
        let _poll = server
            .mock("POST", "/api/accounts/deviceauth/token")
            .with_status(200)
            .with_body(
                json!({
                    "authorization_code": "authorization",
                    "code_verifier": "verifier"
                })
                .to_string(),
            )
            .create_async()
            .await;
        let _exchange = server
            .mock("POST", "/oauth/token")
            .match_body(mockito::Matcher::Regex(
                "grant_type=authorization_code.*code=authorization.*code_verifier=verifier".into(),
            ))
            .with_status(200)
            .with_body(
                json!({
                    "access_token": access,
                    "refresh_token": "refresh",
                    "expires_in": 3600
                })
                .to_string(),
            )
            .create_async()
            .await;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chatgpt.json");
        let auth = ChatGptAuth::with_options(server.url(), "client", path.clone()).unwrap();
        let pending = auth.begin_device_login().await.unwrap();
        assert_eq!(pending.user_code, "ABCD-EFGH");
        let tokens = auth.finish_device_login(&pending).await.unwrap();
        assert_eq!(tokens.account_id, "acct-test");
        assert_eq!(
            serde_json::from_slice::<ChatGptTokens>(&std::fs::read(path).unwrap()).unwrap(),
            tokens
        );
    }
}
