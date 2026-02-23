// Offline Ed25519 license key validation for Finch commercial licensing.
//
// Key format: FINCH-<base64url(JSON payload)>.<base64url(Ed25519 signature over payload bytes)>
//
// Payload JSON:
//   {"sub":"user@example.com","name":"Jane Doe","tier":"commercial",
//    "iss":"2026-01-15","exp":"2027-01-15"}
//
// Keys are validated entirely offline using the embedded public key.
// The private key stays server-side and is never embedded in the binary.

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey, Verifier};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Embedded public key
// ---------------------------------------------------------------------------

/// Ed25519 public key (compressed, 32 bytes).
///
/// Ed25519 public key — d192687b60094f0a6ec1e24a9a1ffe1bf0892c594cafdc940235eac502d804ca
/// Private key stored in 1Password: "Finch License Signing Key (Ed25519)" (Employee vault)
/// Local copy: ~/.finch/license_private.pem
const PUBLIC_KEY_BYTES: &[u8; 32] = &[
    0xd1, 0x92, 0x68, 0x7b, 0x60, 0x09, 0x4f, 0x0a,
    0x6e, 0xc1, 0xe2, 0x4a, 0x9a, 0x1f, 0xfe, 0x1b,
    0xf0, 0x89, 0x2c, 0x59, 0x4c, 0xaf, 0xdc, 0x94,
    0x02, 0x35, 0xea, 0xc5, 0x02, 0xd8, 0x04, 0xca,
];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The JSON payload embedded inside a FINCH-... license key.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LicensePayload {
    /// Email address of the licensee
    sub: String,
    /// Display name of the licensee
    name: String,
    /// License tier ("commercial")
    tier: String,
    /// Issue date (ISO 8601: YYYY-MM-DD)
    iss: String,
    /// Expiry date (ISO 8601: YYYY-MM-DD)
    exp: String,
}

/// Decoded license information returned by [`validate_key`].
#[derive(Debug, Clone)]
pub struct ParsedLicense {
    pub name: String,
    pub email: String,
    pub expires_at: chrono::NaiveDate,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Validate a `FINCH-...` license key against the embedded public key.
///
/// Returns [`ParsedLicense`] on success.
/// Returns `Err` for:
/// - malformed key (bad prefix, missing `.`, invalid base64)
/// - invalid Ed25519 signature
/// - expired key
/// - malformed JSON payload or date fields
pub fn validate_key(key: &str) -> Result<ParsedLicense> {
    let vk = VerifyingKey::from_bytes(PUBLIC_KEY_BYTES)
        .context("Embedded public key is invalid — this is a build configuration error")?;
    validate_key_with_vk(key, &vk)
}

/// Validate a `FINCH-...` license key with an explicitly supplied [`VerifyingKey`].
///
/// This is the inner implementation used by both [`validate_key`] (production)
/// and the test suite (test keypair).
pub fn validate_key_with_vk(key: &str, vk: &VerifyingKey) -> Result<ParsedLicense> {
    // 1. Strip FINCH- prefix
    let without_prefix = key
        .strip_prefix("FINCH-")
        .ok_or_else(|| anyhow::anyhow!("Invalid key format: must start with FINCH-"))?;

    // 2. Split on first '.' to get payload_b64 and sig_b64
    let dot = without_prefix.find('.').ok_or_else(|| {
        anyhow::anyhow!("Invalid key format: missing '.' separator between payload and signature")
    })?;
    let (payload_b64, rest) = without_prefix.split_at(dot);
    let sig_b64 = &rest[1..]; // skip the dot

    // 3. Decode both from base64url (returns Err, not panic, for invalid input)
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .context("Invalid key: payload section is not valid base64url")?;
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .context("Invalid key: signature section is not valid base64url")?;

    // 4. Verify Ed25519 signature over the raw payload bytes
    let sig_array: &[u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid signature: expected 64 bytes, got {}", sig_bytes.len()))?;
    let signature = Signature::from_bytes(sig_array);
    vk.verify(&payload_bytes, &signature)
        .map_err(|_| anyhow::anyhow!("Invalid signature: key signature verification failed"))?;

    // 5. Parse payload JSON
    let payload: LicensePayload = serde_json::from_slice(&payload_bytes)
        .context("Invalid key: payload JSON is malformed")?;

    // 6. Check expiry against today's date
    let expires_at = chrono::NaiveDate::parse_from_str(&payload.exp, "%Y-%m-%d")
        .context("Invalid key: expiry date is not in YYYY-MM-DD format")?;
    let today = chrono::Local::now().date_naive();
    if today > expires_at {
        bail!(
            "License key has expired (expired: {}). Renew at https://polar.sh/darwin-finch",
            payload.exp
        );
    }

    Ok(ParsedLicense {
        name: payload.name,
        email: payload.sub,
        expires_at,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// Golden key signed with the real production private key.
    /// Payload: ci-test@finch.internal / "CI Test" / exp 2099-01-01
    /// Regenerate with: python3 scripts/issue_license.py ci-test@finch.internal "CI Test"
    ///   --key ~/.finch/license_private.pem --years 73
    /// (or see scripts/issue_license.py for exact invocation)
    const GOLDEN_KEY: &str = "FINCH-eyJzdWIiOiJjaS10ZXN0QGZpbmNoLmludGVybmFsIiwibmFtZSI6IkNJIFRlc3QiLCJ0aWVyIjoiY29tbWVyY2lhbCIsImlzcyI6IjIwMjYtMDEtMDEiLCJleHAiOiIyMDk5LTAxLTAxIn0.d6cVaWf1rhk4zJrwrcpLOC9SfjKhSLjCaq-HY4Zh6HuKmCqQXFUFisPeHt7sF3c3CEdToI78hXNfF03DOrcRDw";

    /// Build a well-formed FINCH-... key signed with `signing_key`.
    fn make_test_key(signing_key: &SigningKey, payload: &LicensePayload) -> String {
        let payload_json = serde_json::to_vec(payload).unwrap();
        let payload_b64 = URL_SAFE_NO_PAD.encode(&payload_json);
        let sig = signing_key.sign(&payload_json);
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig.to_bytes());
        format!("FINCH-{}.{}", payload_b64, sig_b64)
    }

    fn future_payload() -> LicensePayload {
        let next_year = chrono::Local::now().date_naive() + chrono::Duration::days(365);
        LicensePayload {
            sub: "test@example.com".to_string(),
            name: "Test User".to_string(),
            tier: "commercial".to_string(),
            iss: "2026-01-01".to_string(),
            exp: next_year.format("%Y-%m-%d").to_string(),
        }
    }

    #[test]
    fn test_validate_key_valid() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let vk = sk.verifying_key();
        let payload = future_payload();
        let key = make_test_key(&sk, &payload);

        let result = validate_key_with_vk(&key, &vk);
        assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());

        let parsed = result.unwrap();
        assert_eq!(parsed.email, "test@example.com");
        assert_eq!(parsed.name, "Test User");
    }

    #[test]
    fn test_validate_key_expired() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let vk = sk.verifying_key();
        let payload = LicensePayload {
            sub: "test@example.com".to_string(),
            name: "Test User".to_string(),
            tier: "commercial".to_string(),
            iss: "2020-01-01".to_string(),
            exp: "2020-01-01".to_string(), // clearly in the past
        };
        let key = make_test_key(&sk, &payload);

        let result = validate_key_with_vk(&key, &vk);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string().to_lowercase();
        assert!(msg.contains("expired"), "Expected 'expired' in: {}", msg);
    }

    #[test]
    fn test_validate_key_tampered_signature() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let vk = sk.verifying_key();
        let key = make_test_key(&sk, &future_payload());

        // Replace the signature portion with 64 zero bytes (valid length, wrong value)
        let dot = key.rfind('.').unwrap();
        let tampered = format!(
            "{}.{}",
            &key[..dot],
            URL_SAFE_NO_PAD.encode([0u8; 64])
        );

        let result = validate_key_with_vk(&tampered, &vk);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string().to_lowercase();
        assert!(msg.contains("signature"), "Expected 'signature' in: {}", msg);
    }

    #[test]
    fn test_validate_key_malformed_base64() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let vk = sk.verifying_key();
        // The "!@#" characters are not valid base64url
        let result = validate_key_with_vk("FINCH-notbase64!@#.something", &vk);
        assert!(result.is_err(), "Expected Err for malformed base64");
    }

    #[test]
    fn test_validate_key_missing_dot() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let vk = sk.verifying_key();
        let result = validate_key_with_vk("FINCH-nodothere", &vk);
        assert!(result.is_err(), "Expected Err for missing dot separator");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("'.'") || msg.contains("separator"),
            "Expected separator mention in: {}",
            msg
        );
    }

    /// Tests the full production path: PUBLIC_KEY_BYTES → validate_key() → ParsedLicense.
    /// Uses a key pre-signed with the real Ed25519 private key; expires 2099-01-01.
    #[test]
    fn test_validate_key_production_path_golden_key() {
        let result = validate_key(GOLDEN_KEY);
        assert!(result.is_ok(), "Golden key should validate: {:?}", result.err());
        let parsed = result.unwrap();
        assert_eq!(parsed.email, "ci-test@finch.internal");
        assert_eq!(parsed.name, "CI Test");
        assert_eq!(
            parsed.expires_at,
            chrono::NaiveDate::from_ymd_opt(2099, 1, 1).unwrap()
        );
    }

    /// Ensure a key signed with a *different* (test) private key is rejected by validate_key().
    #[test]
    fn test_validate_key_rejects_wrong_signer() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let key = make_test_key(&sk, &future_payload());
        let result = validate_key(&key);
        assert!(result.is_err(), "Key from wrong signer must be rejected by production validate_key");
    }

    #[test]
    fn test_license_config_default() {
        use crate::config::LicenseType;
        let config = crate::config::LicenseConfig::default();
        assert_eq!(
            config.license_type,
            LicenseType::Noncommercial,
            "Default license type must be Noncommercial"
        );
        assert!(config.key.is_none());
        assert!(config.licensee_name.is_none());
    }

    #[test]
    fn test_license_config_toml_round_trip() {
        use crate::config::{LicenseConfig, LicenseType};
        let original = LicenseConfig {
            key: Some("FINCH-testkey.testsig".to_string()),
            license_type: LicenseType::Commercial,
            verified_at: Some("2026-01-15".to_string()),
            expires_at: Some("2027-01-15".to_string()),
            licensee_name: Some("Jane Doe".to_string()),
            notice_suppress_until: None,
        };
        let toml_str = toml::to_string(&original).unwrap();
        let decoded: LicenseConfig = toml::from_str(&toml_str).unwrap();

        assert_eq!(decoded.license_type, LicenseType::Commercial);
        assert_eq!(decoded.key.as_deref(), Some("FINCH-testkey.testsig"));
        assert_eq!(decoded.licensee_name.as_deref(), Some("Jane Doe"));
        assert_eq!(decoded.expires_at.as_deref(), Some("2027-01-15"));
    }

    #[test]
    fn test_config_missing_license_section() {
        use crate::config::LicenseType;
        // Simulate a TOML snippet that has no [license] section
        #[derive(serde::Deserialize)]
        struct MinimalConfig {
            #[serde(default)]
            license: crate::config::LicenseConfig,
        }
        let toml_str = "streaming_enabled = true\ntui_enabled = true\n";
        let parsed: MinimalConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            parsed.license.license_type,
            LicenseType::Noncommercial,
            "Missing [license] section must deserialize to Noncommercial"
        );
    }
}
