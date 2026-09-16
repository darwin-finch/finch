//! Exact xAI issuer/JWKS authority for the pinned SuperGrok compatibility dialect.
//!
//! This module implements only ES256 compact JWS. Discovery and key retrieval
//! are pinned to exact paths and origins; neither token headers nor discovery
//! responses can substitute an authority.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, TimeDelta, Utc};
use futures::StreamExt;
use reqwest::{Client, StatusCode, Url};
use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::grok_oauth::{
    GrokTokenVerifier, VerifiedGrokClaims, GROK_REQUIRED_TOKEN_ISSUER, XAI_AUTH_ORIGIN,
    XAI_PUBLIC_CLIENT_ID,
};

const DISCOVERY_PATH: &str = "/.well-known/openid-configuration";
const JWKS_PATH: &str = "/.well-known/jwks.json";
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
const MAX_TOKEN_BYTES: usize = 32 * 1024;
const DEFAULT_CACHE_LIFETIME: Duration = Duration::from_secs(5 * 60);
const MAX_CACHE_LIFETIME: Duration = Duration::from_secs(60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CLOCK_SKEW_SECONDS: i64 = 60;
const MAX_SIGNED_TOKEN_AGE_SECONDS: i64 = 24 * 60 * 60;

/// Bounded, single-flight verifier for the exact pinned xAI issuer.
pub struct GrokJwksVerifier {
    issuer: String,
    discovery_url: Url,
    jwks_url: Url,
    client_id: String,
    http: Client,
    cache: Mutex<KeyCache>,
    generation: AtomicU64,
}

impl std::fmt::Debug for GrokJwksVerifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrokJwksVerifier")
            .field("issuer", &self.issuer)
            .field("discovery_url", &self.discovery_url)
            .field("jwks_url", &self.jwks_url)
            .field("client_id", &self.client_id)
            .field("cache", &"[REDACTED KEY CACHE]")
            .finish()
    }
}

#[derive(Default)]
struct KeyCache {
    keys: BTreeMap<String, Arc<VerifiedEcKey>>,
    expires_at: Option<tokio::time::Instant>,
    generation: u64,
}

struct VerifiedEcKey {
    uncompressed: Vec<u8>,
}

impl GrokJwksVerifier {
    /// Construct the production verifier for the exact pinned xAI authority.
    pub fn production() -> Result<Self> {
        Self::new(
            XAI_AUTH_ORIGIN,
            GROK_REQUIRED_TOKEN_ISSUER,
            XAI_PUBLIC_CLIENT_ID,
            false,
            REQUEST_TIMEOUT,
        )
    }

    fn new(
        authority_origin: &str,
        expected_issuer: &str,
        client_id: &str,
        allow_insecure_loopback: bool,
        timeout: Duration,
    ) -> Result<Self> {
        let authority_url = exact_issuer(authority_origin, allow_insecure_loopback)?;
        let issuer_url = exact_issuer(expected_issuer, false)?;
        let discovery_url = authority_url
            .join(DISCOVERY_PATH)
            .context("xAI discovery authority is invalid")?;
        let jwks_url = authority_url
            .join(JWKS_PATH)
            .context("xAI JWKS authority is invalid")?;
        if client_id.trim().is_empty() || timeout.is_zero() {
            bail!("xAI token verifier authority is incomplete");
        }
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(timeout)
            .build()
            .context("Failed to construct bounded xAI JWKS client")?;
        Ok(Self {
            issuer: issuer_url.as_str().trim_end_matches('/').to_string(),
            discovery_url,
            jwks_url,
            client_id: client_id.to_string(),
            http,
            cache: Mutex::new(KeyCache::default()),
            generation: AtomicU64::new(0),
        })
    }

    #[cfg(test)]
    pub fn for_test(
        authority_origin: &str,
        expected_issuer: &str,
        client_id: &str,
    ) -> Result<Self> {
        Self::new(
            authority_origin,
            expected_issuer,
            client_id,
            true,
            REQUEST_TIMEOUT,
        )
    }

    async fn verify_compact(
        &self,
        token: &str,
        cancel: &CancellationToken,
    ) -> Result<SignedClaims> {
        if token.is_empty()
            || token.len() > MAX_TOKEN_BYTES
            || token.chars().any(char::is_whitespace)
        {
            bail!("xAI signed token has an invalid size or encoding");
        }
        let mut segments = token.split('.');
        let encoded_header = segments.next().unwrap_or_default();
        let encoded_claims = segments.next().unwrap_or_default();
        let encoded_signature = segments.next().unwrap_or_default();
        if encoded_header.is_empty()
            || encoded_claims.is_empty()
            || encoded_signature.is_empty()
            || segments.next().is_some()
        {
            bail!("xAI signed token is not a compact three-part JWS");
        }

        let header_bytes = decode_segment(encoded_header, "header")?;
        reject_duplicate_json_fields(&header_bytes, "signed token header")?;
        let header_value: Value = serde_json::from_slice(&header_bytes)
            .context("xAI signed token header is malformed")?;
        let header: JwsHeader = serde_json::from_value(header_value.clone())
            .context("xAI signed token header is incompatible")?;
        reject_header_authority_substitution(&header_value)?;
        if header.alg != "ES256" || header.kid.trim().is_empty() || header.kid.len() > 256 {
            bail!("xAI signed token algorithm or key identifier is unsupported");
        }
        if header.typ.as_deref().is_some_and(|typ| typ != "JWT") {
            bail!("xAI signed token type is unsupported");
        }

        let signature = decode_segment(encoded_signature, "signature")?;
        if signature.len() != 64 {
            bail!("xAI signed token ES256 signature is invalid");
        }
        let key = self.key_for(&header.kid, cancel).await?;
        let signing_input = format!("{encoded_header}.{encoded_claims}");
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &key.uncompressed)
            .verify(signing_input.as_bytes(), &signature)
            .map_err(|_| anyhow::anyhow!("xAI signed token signature is invalid"))?;

        let claims_bytes = decode_segment(encoded_claims, "claims")?;
        reject_duplicate_json_fields(&claims_bytes, "signed token claims")?;
        let mut claims: SignedClaims = serde_json::from_slice(&claims_bytes)
            .context("xAI signed token claims are malformed")?;
        claims.validate_signed_authority(&self.issuer)?;
        Ok(claims)
    }

    async fn key_for(&self, kid: &str, cancel: &CancellationToken) -> Result<Arc<VerifiedEcKey>> {
        let observed_generation = self.generation.load(Ordering::Acquire);
        {
            let cache = self.cache.lock().await;
            if cache
                .expires_at
                .is_some_and(|expiry| expiry > tokio::time::Instant::now())
            {
                if let Some(key) = cache.keys.get(kid) {
                    return Ok(key.clone());
                }
            }
        }

        let mut cache = self.cache.lock().await;
        if cache.generation != observed_generation
            && cache
                .expires_at
                .is_some_and(|expiry| expiry > tokio::time::Instant::now())
        {
            return cache
                .keys
                .get(kid)
                .cloned()
                .context("xAI JWKS rotation did not contain the signed token key");
        }
        let (keys, lifetime) = self.fetch_keys(cancel).await?;
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        cache.keys = keys;
        cache.expires_at = Some(tokio::time::Instant::now() + lifetime);
        cache.generation = generation;
        cache
            .keys
            .get(kid)
            .cloned()
            .context("xAI signed token key identifier is absent from the pinned JWKS")
    }

    async fn fetch_keys(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(BTreeMap<String, Arc<VerifiedEcKey>>, Duration)> {
        let (_, discovery_bytes) = self.fetch_document(&self.discovery_url, cancel).await?;
        reject_duplicate_json_fields(&discovery_bytes, "discovery document")?;
        let discovery: DiscoveryDocument = serde_json::from_slice(&discovery_bytes)
            .context("xAI discovery document is malformed")?;
        if discovery.issuer != self.issuer || discovery.jwks_uri != self.jwks_url.as_str() {
            bail!("xAI discovery document changed issuer or JWKS authority");
        }

        let (cache_control, jwks_bytes) = self.fetch_document(&self.jwks_url, cancel).await?;
        reject_duplicate_json_fields(&jwks_bytes, "JWKS document")?;
        let document: JwksDocument =
            serde_json::from_slice(&jwks_bytes).context("xAI JWKS document is malformed")?;
        if document.keys.is_empty() || document.keys.len() > 128 {
            bail!("xAI JWKS contains an invalid number of keys");
        }
        let mut keys = BTreeMap::new();
        for key in document.keys {
            if key.kty != "EC"
                || key.crv != "P-256"
                || key.key_use.as_deref().is_some_and(|value| value != "sig")
                || key.alg.as_deref().is_some_and(|value| value != "ES256")
            {
                bail!("xAI JWKS contains a key outside the pinned ES256 signing contract");
            }
            if key.kid.trim().is_empty() || key.kid.len() > 256 || keys.contains_key(&key.kid) {
                bail!("xAI JWKS contains a missing, duplicate, or ambiguous key identifier");
            }
            let x = decode_key_component(&key.x, "x")?;
            let y = decode_key_component(&key.y, "y")?;
            if x.len() != 32 || y.len() != 32 {
                bail!("xAI JWKS P-256 coordinates are invalid");
            }
            let mut uncompressed = Vec::with_capacity(65);
            uncompressed.push(0x04);
            uncompressed.extend_from_slice(&x);
            uncompressed.extend_from_slice(&y);
            keys.insert(key.kid, Arc::new(VerifiedEcKey { uncompressed }));
        }
        Ok((keys, cache_lifetime(cache_control.as_deref())))
    }

    async fn fetch_document(
        &self,
        url: &Url,
        cancel: &CancellationToken,
    ) -> Result<(Option<String>, Vec<u8>)> {
        let response = tokio::select! {
            _ = cancel.cancelled() => bail!("xAI token verification was cancelled"),
            response = self.http.get(url.clone()).send() => response.context("xAI verification authority is unavailable")?,
        };
        if response.status() != StatusCode::OK {
            bail!(
                "xAI verification authority returned HTTP {}",
                response.status()
            );
        }
        if response.url() != url {
            bail!("xAI verification authority redirected outside its pinned URL");
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_DOCUMENT_BYTES as u64)
        {
            bail!("xAI verification document exceeded the size limit");
        }
        let cache_control = response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => bail!("xAI token verification was cancelled"),
                next = stream.next() => next,
            };
            let Some(chunk) = next else { break };
            let chunk = chunk.context("xAI verification document body failed")?;
            if body.len().saturating_add(chunk.len()) > MAX_DOCUMENT_BYTES {
                bail!("xAI verification document exceeded the size limit");
            }
            body.extend_from_slice(&chunk);
        }
        Ok((cache_control, body))
    }
}

#[async_trait]
impl GrokTokenVerifier for GrokJwksVerifier {
    fn preflight(&self) -> Result<()> {
        exact_issuer(&self.issuer, false)?;
        if self.discovery_url.path() != DISCOVERY_PATH
            || self.jwks_url.path() != JWKS_PATH
            || self.discovery_url.origin() != self.jwks_url.origin()
            || self.client_id.trim().is_empty()
        {
            bail!("xAI token verification authority is incompatible");
        }
        Ok(())
    }

    async fn verify(
        &self,
        id_token: Option<&str>,
        access_token: &str,
        cancel: &CancellationToken,
    ) -> Result<VerifiedGrokClaims> {
        crate::oauth::validate_secret_field(access_token, "access token")?;
        let identity = self.verify_compact(access_token, cancel).await?;
        if !identity.audiences.contains(&self.client_id)
            || (identity.audiences.len() > 1
                && identity.authorized_party.as_deref() != Some(self.client_id.as_str()))
            || identity
                .authorized_party
                .as_deref()
                .is_some_and(|party| party != self.client_id)
        {
            bail!("xAI access token audience does not match the pinned public client");
        }
        if let Some(id_token) = id_token {
            let id_claims = self.verify_compact(id_token, cancel).await?;
            if id_claims.subject != identity.subject {
                bail!("xAI identity token subject does not match the access token");
            }
        }
        let account_id = identity
            .principal_id
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| identity.subject.clone());
        validate_signed_public_claim(&account_id, "account")?;
        if let Some(principal_type) = identity.principal_type.as_deref() {
            validate_signed_public_claim(principal_type, "principal type")?;
        }
        Ok(VerifiedGrokClaims {
            issuer: identity.issuer,
            audiences: identity.audiences,
            authorized_party: identity.authorized_party,
            subject: identity.subject,
            account_id,
            principal_type: identity.principal_type,
            nonce: identity.nonce,
            expires_at: identity.expires_at,
            not_before: identity.not_before,
        })
    }
}

fn validate_signed_public_claim(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("xAI signed token {label} claim is invalid");
    }
    Ok(())
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
}

#[derive(Deserialize)]
struct JwksDocument {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    #[serde(rename = "use")]
    key_use: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    kid: String,
    crv: String,
    x: String,
    y: String,
}

#[derive(Deserialize)]
struct JwsHeader {
    alg: String,
    kid: String,
    #[serde(default)]
    typ: Option<String>,
}

#[derive(Deserialize)]
struct SignedClaims {
    #[serde(rename = "iss")]
    issuer: String,
    #[serde(rename = "aud", deserialize_with = "audiences")]
    audiences: BTreeSet<String>,
    #[serde(rename = "sub")]
    subject: String,
    #[serde(rename = "azp", default)]
    authorized_party: Option<String>,
    exp: i64,
    #[serde(default)]
    nbf: Option<i64>,
    iat: i64,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default, alias = "principalType")]
    principal_type: Option<String>,
    #[serde(default, alias = "principalId")]
    principal_id: Option<String>,
    #[serde(skip)]
    expires_at: DateTime<Utc>,
    #[serde(skip)]
    not_before: Option<DateTime<Utc>>,
}

impl SignedClaims {
    fn validate_signed_authority(&mut self, expected_issuer: &str) -> Result<()> {
        let now = Utc::now();
        self.expires_at = DateTime::from_timestamp(self.exp, 0)
            .context("xAI signed token expiration is invalid")?;
        self.not_before = self
            .nbf
            .map(|value| {
                DateTime::from_timestamp(value, 0)
                    .context("xAI signed token not-before time is invalid")
            })
            .transpose()?;
        let issued_at = DateTime::from_timestamp(self.iat, 0)
            .context("xAI signed token issued-at time is invalid")?;
        if self.issuer != expected_issuer
            || self.subject.is_empty()
            || self.subject.len() > 256
            || self.subject.chars().any(char::is_control)
            || self.audiences.is_empty()
            || self.expires_at <= now - TimeDelta::seconds(CLOCK_SKEW_SECONDS)
            || self.expires_at <= issued_at
            || self
                .not_before
                .is_some_and(|value| value > now + TimeDelta::seconds(CLOCK_SKEW_SECONDS))
            || issued_at > now + TimeDelta::seconds(CLOCK_SKEW_SECONDS)
            || issued_at < now - TimeDelta::seconds(MAX_SIGNED_TOKEN_AGE_SECONDS)
        {
            bail!("xAI signed token issuer, subject, audience, or lifetime is invalid");
        }
        Ok(())
    }
}

fn exact_issuer(value: &str, allow_insecure_loopback: bool) -> Result<Url> {
    let url = Url::parse(value).context("xAI issuer must be an absolute URL")?;
    let loopback =
        allow_insecure_loopback && url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    if (url.scheme() != "https" && !loopback)
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.path() != "/" && !url.path().is_empty())
    {
        bail!("xAI issuer authority is not an exact HTTPS origin");
    }
    Ok(url)
}

fn reject_header_authority_substitution(header: &Value) -> Result<()> {
    let object = header
        .as_object()
        .context("xAI signed token header must be an object")?;
    for forbidden in ["jku", "jwk", "x5u", "x5c", "crit"] {
        if object.contains_key(forbidden) {
            bail!("xAI signed token header attempted authority substitution");
        }
    }
    Ok(())
}

fn decode_segment(value: &str, name: &str) -> Result<Vec<u8>> {
    if value.len() > MAX_TOKEN_BYTES {
        bail!("xAI signed token {name} exceeded the size limit");
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .with_context(|| format!("xAI signed token {name} is not base64url"))
}

fn decode_key_component(value: &str, name: &str) -> Result<Vec<u8>> {
    if value.is_empty() || value.len() > 256 {
        bail!("xAI JWKS EC {name} is invalid");
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .with_context(|| format!("xAI JWKS EC {name} is not base64url"))
}

fn cache_lifetime(value: Option<&str>) -> Duration {
    value
        .into_iter()
        .flat_map(|header| header.split(','))
        .map(str::trim)
        .find_map(|directive| directive.strip_prefix("max-age=")?.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_CACHE_LIFETIME)
        .min(MAX_CACHE_LIFETIME)
}

fn reject_duplicate_json_fields(bytes: &[u8], label: &str) -> Result<()> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    NoDuplicateJson::deserialize(&mut deserializer)
        .with_context(|| format!("xAI {label} contains duplicate or malformed JSON fields"))?;
    deserializer
        .end()
        .with_context(|| format!("xAI {label} contains trailing JSON data"))?;
    Ok(())
}

struct NoDuplicateJson;

impl<'de> Deserialize<'de> for NoDuplicateJson {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateVisitor)
    }
}

struct NoDuplicateVisitor;

impl<'de> Visitor<'de> for NoDuplicateVisitor {
    type Value = NoDuplicateJson;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON without duplicate object fields")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut names = BTreeSet::new();
        while let Some(name) = map.next_key::<String>()? {
            if !names.insert(name) {
                return Err(serde::de::Error::custom("duplicate JSON object field"));
            }
            let _: NoDuplicateJson = map.next_value()?;
        }
        Ok(NoDuplicateJson)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<NoDuplicateJson>()?.is_some() {}
        Ok(NoDuplicateJson)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        NoDuplicateJson::deserialize(deserializer)
    }
}

fn audiences<'de, D>(deserializer: D) -> std::result::Result<BTreeSet<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    let values = match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(value) => vec![value],
        OneOrMany::Many(values) => values,
    };
    if values.is_empty() || values.len() > 32 || values.iter().any(|value| value.trim().is_empty())
    {
        return Err(serde::de::Error::custom("invalid signed token audience"));
    }
    Ok(values.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_verifier_pins_exact_auth_xai_authority() {
        let verifier = GrokJwksVerifier::production().unwrap();
        assert_eq!(verifier.issuer, "https://auth.x.ai");
        assert_eq!(
            verifier.discovery_url.as_str(),
            "https://auth.x.ai/.well-known/openid-configuration"
        );
        assert_eq!(
            verifier.jwks_url.as_str(),
            "https://auth.x.ai/.well-known/jwks.json"
        );
        assert_eq!(verifier.client_id, XAI_PUBLIC_CLIENT_ID);
        assert!(verifier.preflight().is_ok());
        let debug = format!("{verifier:?}");
        assert!(
            debug.contains("[REDACTED KEY CACHE]"),
            "JWKS Debug must not dump cached keys: {debug}"
        );
        assert!(
            !debug.contains("uncompressed"),
            "JWKS Debug must not dump key material: {debug}"
        );
    }
}
