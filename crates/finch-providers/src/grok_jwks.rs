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
    GrokAuthStageError, GrokTokenVerifier, VerifiedGrokClaims, GROK_REQUIRED_TOKEN_ISSUER,
    XAI_AUTH_ORIGIN, XAI_PUBLIC_CLIENT_ID,
};

const DISCOVERY_PATH: &str = "/.well-known/openid-configuration";
const JWKS_PATH: &str = "/.well-known/jwks.json";
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
const MAX_TOKEN_BYTES: usize = 32 * 1024;
const DEFAULT_CACHE_LIFETIME: Duration = Duration::from_secs(5 * 60);
const MAX_CACHE_LIFETIME: Duration = Duration::from_secs(60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CLOCK_SKEW_SECONDS: i64 = 300;
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
    pub(crate) fn production() -> Result<Self> {
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
        let (keys, lifetime) = self
            .fetch_keys(cancel)
            .await
            .context(GrokAuthStageError::JwksTransport)?;
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
                || key.crv.as_deref() != Some("P-256")
                || key.key_use.as_deref().is_some_and(|value| value != "sig")
                || key.alg.as_deref().is_some_and(|value| value != "ES256")
            {
                continue;
            }
            if key.kid.trim().is_empty() || key.kid.len() > 256 || keys.contains_key(&key.kid) {
                continue;
            }
            let Some(x_val) = key.x else { continue };
            let Some(y_val) = key.y else { continue };
            let Ok(x) = decode_key_component(&x_val, "x") else { continue };
            let Ok(y) = decode_key_component(&y_val, "y") else { continue };
            if x.len() != 32 || y.len() != 32 {
                continue;
            }
            let mut uncompressed = Vec::with_capacity(65);
            uncompressed.push(0x04);
            uncompressed.extend_from_slice(&x);
            uncompressed.extend_from_slice(&y);
            keys.insert(key.kid, Arc::new(VerifiedEcKey { uncompressed }));
        }
        if keys.is_empty() {
            bail!("xAI JWKS contains no supported keys");
        }
        Ok((keys, cache_lifetime(cache_control.as_deref())))
    }

    async fn fetch_document(
        &self,
        url: &Url,
        cancel: &CancellationToken,
    ) -> Result<(Option<String>, Vec<u8>)> {
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(crate::oauth::OAuthDeviceAuthorizationError::Cancelled.into()),
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
                _ = cancel.cancelled() => return Err(crate::oauth::OAuthDeviceAuthorizationError::Cancelled.into()),
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
        let identity = match id_token {
            Some(id_token) => {
                crate::oauth::validate_secret_field(id_token, "identity token")?;
                let identity = self
                    .verify_compact(id_token, cancel)
                    .await
                    .map_err(mark_identity_error)?;
                validate_client_binding(&identity, &self.client_id)?;
                identity
            }
            None => self
                .verify_compact(access_token, cancel)
                .await
                .map_err(mark_identity_error)?,
        };

        let signed_access = if id_token.is_some() && is_compact_jws_candidate(access_token) {
            let claims = self
                .verify_compact(access_token, cancel)
                .await
                .map_err(mark_identity_error)?;
            validate_client_binding(&claims, &self.client_id)?;
            if claims.subject != identity.subject {
                return Err(GrokAuthStageError::IdentitySignature.into());
            }
            Some(claims)
        } else if id_token.is_none() {
            validate_client_binding(&identity, &self.client_id)?;
            None
        } else {
            None
        };

        let authority = signed_access.as_ref().unwrap_or(&identity);
        let account_id = authority
            .principal_id
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| identity.subject.clone());
        validate_signed_public_claim(&account_id, "account")
            .context(GrokAuthStageError::AccountEntitlement)?;
        if let Some(principal_type) = authority.principal_type.as_deref() {
            validate_signed_public_claim(principal_type, "principal type")
                .context(GrokAuthStageError::AccountEntitlement)?;
        }
        let principal_type = authority.principal_type.clone();
        Ok(VerifiedGrokClaims {
            issuer: authority.issuer.clone(),
            audiences: authority.audiences.clone(),
            authorized_party: authority.authorized_party.clone(),
            subject: identity.subject.clone(),
            account_id,
            principal_type,
            nonce: identity.nonce.clone(),
            expires_at: authority.expires_at,
            not_before: authority.not_before,
        })
    }
}

fn validate_client_binding(claims: &SignedClaims, client_id: &str) -> Result<()> {
    if !claims.audiences.contains(client_id)
        || (claims.audiences.len() > 1 && claims.authorized_party.as_deref() != Some(client_id))
        || claims
            .authorized_party
            .as_deref()
            .is_some_and(|party| party != client_id)
    {
        return Err(GrokAuthStageError::ClientBinding.into());
    }
    Ok(())
}

fn is_compact_jws_candidate(token: &str) -> bool {
    let mut segments = token.split('.');
    segments.next().is_some_and(|segment| !segment.is_empty())
        && segments.next().is_some_and(|segment| !segment.is_empty())
        && segments.next().is_some_and(|segment| !segment.is_empty())
        && segments.next().is_none()
}

fn mark_identity_error(error: anyhow::Error) -> anyhow::Error {
    if error
        .downcast_ref::<crate::oauth::OAuthDeviceAuthorizationError>()
        .is_some()
        || error.downcast_ref::<GrokAuthStageError>().is_some()
    {
        error
    } else {
        error.context(GrokAuthStageError::IdentitySignature)
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
    crv: Option<String>,
    x: Option<String>,
    y: Option<String>,
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
    use crate::credentials::CredentialLifecycle;
    use crate::grok_oauth::XaiGrokOAuthDialect;
    use crate::oauth::{OAuthDialect, TokenValidationContext};
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use serde_json::json;

    fn signing_fixture() -> (EcdsaKeyPair, Value) {
        let rng = SystemRandom::new();
        let key_document =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key_pair = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            key_document.as_ref(),
            &rng,
        )
        .unwrap();
        let public_key = key_pair.public_key().as_ref();
        assert_eq!(
            public_key.first(),
            Some(&0x04),
            "ES256 fixture key must use uncompressed SEC1 encoding"
        );
        let jwk = json!({
            "kty": "EC",
            "use": "sig",
            "alg": "ES256",
            "kid": "fixture-key",
            "crv": "P-256",
            "x": URL_SAFE_NO_PAD.encode(&public_key[1..33]),
            "y": URL_SAFE_NO_PAD.encode(&public_key[33..65]),
        });
        (key_pair, jwk)
    }

    fn signed_token_fixture(
        key_pair: &EcdsaKeyPair,
        audience: Value,
        authorized_party: Option<&str>,
        subject: &str,
        expires_in_seconds: i64,
        principal_id: Option<&str>,
    ) -> String {
        let header = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"alg":"ES256", "kid":"fixture-key", "typ":"JWT"})).unwrap(),
        );
        let now = Utc::now().timestamp();
        let mut claims = json!({
            "iss": GROK_REQUIRED_TOKEN_ISSUER,
            "aud": audience,
            "sub": subject,
            "iat": now,
            "exp": now + expires_in_seconds,
        });
        if let Some(authorized_party) = authorized_party {
            claims["azp"] = Value::String(authorized_party.into());
        }
        if let Some(principal_id) = principal_id {
            claims["principal_id"] = Value::String(principal_id.into());
        }
        let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signing_input = format!("{header}.{claims}");
        let signature = key_pair
            .sign(&SystemRandom::new(), signing_input.as_bytes())
            .unwrap();
        format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        )
    }

    fn signed_identity_fixture() -> (String, Value) {
        let (key_pair, jwk) = signing_fixture();
        (
            signed_token_fixture(
                &key_pair,
                json!(XAI_PUBLIC_CLIENT_ID),
                None,
                "acct-work",
                3600,
                None,
            ),
            jwk,
        )
    }

    async fn verification_server(jwk: Value) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let discovery = Arc::new(json!({
            "issuer": GROK_REQUIRED_TOKEN_ISSUER,
            "jwks_uri": format!("{origin}{JWKS_PATH}"),
        }));
        let keys = Arc::new(json!({"keys": [jwk]}));
        let app = axum::Router::new()
            .route(
                DISCOVERY_PATH,
                axum::routing::get({
                    let discovery = discovery.clone();
                    move || {
                        let discovery = discovery.clone();
                        async move { axum::Json(discovery.as_ref().clone()) }
                    }
                }),
            )
            .route(
                JWKS_PATH,
                axum::routing::get({
                    let keys = keys.clone();
                    move || {
                        let keys = keys.clone();
                        async move { axum::Json(keys.as_ref().clone()) }
                    }
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (origin, server)
    }

    #[tokio::test]
    async fn signed_identity_token_authorizes_opaque_access_token_without_trusting_access_bytes() {
        let (identity_token, jwk) = signed_identity_fixture();
        let (origin, server) = verification_server(jwk).await;
        let verifier = GrokJwksVerifier::new(
            &origin,
            GROK_REQUIRED_TOKEN_ISSUER,
            XAI_PUBLIC_CLIENT_ID,
            true,
            Duration::from_secs(2),
        )
        .unwrap();

        let verified = verifier
            .verify(
                Some(&identity_token),
                "opaque-access-token",
                &CancellationToken::new(),
            )
            .await
            .expect("a signed identity token must authorize a bounded opaque access bearer");
        server.abort();

        assert_eq!(verified.subject, "acct-work");
        assert_eq!(verified.account_id, "acct-work");
        assert_eq!(
            verified.audiences,
            BTreeSet::from([XAI_PUBLIC_CLIENT_ID.into()])
        );
    }

    #[tokio::test]
    async fn signed_access_token_requires_its_own_client_binding_and_subject_match() {
        let (key_pair, jwk) = signing_fixture();
        let identity = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "acct-work",
            3600,
            None,
        );
        let wrong_client_access = signed_token_fixture(
            &key_pair,
            json!("another-client"),
            None,
            "acct-work",
            900,
            Some("wrong-client-account"),
        );
        let wrong_subject_access = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "another-subject",
            900,
            None,
        );
        let (origin, server) = verification_server(jwk).await;
        let verifier = GrokJwksVerifier::new(
            &origin,
            GROK_REQUIRED_TOKEN_ISSUER,
            XAI_PUBLIC_CLIENT_ID,
            true,
            Duration::from_secs(2),
        )
        .unwrap();

        let wrong_client = verifier
            .verify(
                Some(&identity),
                &wrong_client_access,
                &CancellationToken::new(),
            )
            .await
            .expect_err("a signed access bearer for another client must fail closed");
        assert!(matches!(
            wrong_client.downcast_ref::<GrokAuthStageError>(),
            Some(GrokAuthStageError::ClientBinding)
        ));

        let wrong_subject = verifier
            .verify(
                Some(&identity),
                &wrong_subject_access,
                &CancellationToken::new(),
            )
            .await
            .expect_err("signed identity and access tokens must name the same subject");
        server.abort();
        assert!(matches!(
            wrong_subject.downcast_ref::<GrokAuthStageError>(),
            Some(GrokAuthStageError::IdentitySignature)
        ));
    }

    #[tokio::test]
    async fn signed_access_token_bounds_account_and_expiry_to_bearer_authority() {
        let (key_pair, jwk) = signing_fixture();
        let identity = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "acct-work",
            3600,
            Some("identity-account"),
        );
        let access = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "acct-work",
            300,
            Some("access-account"),
        );
        let (origin, server) = verification_server(jwk).await;
        let verifier = GrokJwksVerifier::new(
            &origin,
            GROK_REQUIRED_TOKEN_ISSUER,
            XAI_PUBLIC_CLIENT_ID,
            true,
            Duration::from_secs(2),
        )
        .unwrap();

        let before = Utc::now();
        let verified = verifier
            .verify(Some(&identity), &access, &CancellationToken::new())
            .await
            .expect("a correctly bound signed access bearer must verify");
        server.abort();

        assert_eq!(
            verified.account_id, "access-account",
            "stored account authority must come from the independently bound access bearer"
        );
        assert!(
            verified.expires_at <= before + TimeDelta::seconds(301),
            "stored lifetime must not outlive the shorter signed access bearer: {}",
            verified.expires_at
        );
    }

    #[tokio::test]
    async fn token_response_lifetime_bounds_real_opaque_and_signed_access_records() {
        let (key_pair, jwk) = signing_fixture();
        let identity = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "acct-work",
            3600,
            None,
        );
        let signed_access = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "acct-work",
            900,
            None,
        );
        let (origin, server) = verification_server(jwk).await;
        let verifier = Arc::new(
            GrokJwksVerifier::new(
                &origin,
                GROK_REQUIRED_TOKEN_ISSUER,
                XAI_PUBLIC_CLIENT_ID,
                true,
                Duration::from_secs(2),
            )
            .unwrap(),
        );
        let dialect = XaiGrokOAuthDialect::for_test(&origin, verifier).unwrap();

        let before = Utc::now();
        let opaque = dialect
            .validate_token_response(
                StatusCode::OK,
                &serde_json::to_vec(&json!({
                    "access_token": "opaque-access",
                    "id_token": identity,
                    "expires_in": 300,
                }))
                .unwrap(),
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .expect("signed identity must authorize a response-bounded opaque bearer");
        assert!(
            opaque.expires_at <= before + TimeDelta::seconds(301),
            "opaque bearer lifetime must be bounded by expires_in: {}",
            opaque.expires_at
        );

        let signed = dialect
            .validate_token_response(
                StatusCode::OK,
                &serde_json::to_vec(&json!({
                    "access_token": signed_access,
                    "id_token": opaque.id_token,
                    "expires_in": 60,
                }))
                .unwrap(),
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .expect("signed bearer must also honor a shorter response lifetime");
        server.abort();
        assert!(
            signed.expires_at <= Utc::now() + TimeDelta::seconds(60),
            "signed bearer lifetime must take the minimum response deadline: {}",
            signed.expires_at
        );
    }

    #[tokio::test]
    async fn production_verifier_dialect_refresh_carries_only_verified_identity_lineage() {
        let (key_pair, jwk) = signing_fixture();
        let identity = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "subject-A",
            3600,
            Some("acct-work"),
        );
        let substituted_subject_identity = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "subject-B",
            3600,
            Some("acct-work"),
        );
        let substituted_signed_access = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "subject-B",
            120,
            Some("acct-work"),
        );
        let different_account_identity = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "subject-A",
            3600,
            Some("acct-other"),
        );
        let valid_new_identity = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "subject-A",
            3600,
            Some("acct-work"),
        );
        let valid_signed_access = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "subject-A",
            120,
            Some("acct-work"),
        );
        let (origin, server) = verification_server(jwk).await;
        let verifier = Arc::new(
            GrokJwksVerifier::new(
                &origin,
                GROK_REQUIRED_TOKEN_ISSUER,
                XAI_PUBLIC_CLIENT_ID,
                true,
                Duration::from_secs(2),
            )
            .unwrap(),
        );
        let dialect = XaiGrokOAuthDialect::for_test(&origin, verifier).unwrap();
        let initial = dialect
            .validate_token_response(
                StatusCode::OK,
                &serde_json::to_vec(&json!({
                    "access_token": "initial-opaque-access",
                    "refresh_token": "refresh-authority",
                    "id_token": identity,
                    "expires_in": 300,
                }))
                .unwrap(),
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .expect("initial device response must establish verified refresh lineage");
        assert!(matches!(
            initial.provider_credential("grok-sub:fixture").lifecycle,
            CredentialLifecycle::Active {
                refreshable: true,
                ..
            }
        ));
        assert_eq!(initial.account, "acct-work");
        let retained_identity = initial.id_token.clone();
        let before = Utc::now();
        let refreshed = dialect
            .validate_token_response(
                StatusCode::OK,
                &serde_json::to_vec(&json!({
                    "access_token": "refreshed-opaque-access",
                    "expires_in": 120,
                }))
                .unwrap(),
                Some(&initial),
                &TokenValidationContext::Refresh,
                &CancellationToken::new(),
            )
            .await
            .expect("official opaque refresh response must retain prior verified identity");
        assert_eq!(refreshed.account, initial.account);
        assert_eq!(refreshed.id_token, retained_identity);
        assert_eq!(refreshed.refresh_token, initial.refresh_token);
        assert!(matches!(
            refreshed.provider_credential("grok-sub:fixture").lifecycle,
            CredentialLifecycle::Active {
                refreshable: true,
                ..
            }
        ));
        assert!(
            refreshed.expires_at <= before + TimeDelta::seconds(121),
            "refreshed bearer must be bounded by its own response lifetime: {}",
            refreshed.expires_at
        );

        // GSK-05 regression 1: same-account, substituted-subject new-ID/opaque-access refresh must be rejected.
        let subject_substitution = dialect
            .validate_token_response(
                StatusCode::OK,
                &serde_json::to_vec(&json!({
                    "access_token": "substituted-opaque-access",
                    "id_token": substituted_subject_identity,
                    "expires_in": 120,
                }))
                .unwrap(),
                Some(&initial),
                &TokenValidationContext::Refresh,
                &CancellationToken::new(),
            )
            .await
            .expect_err("refresh must reject same-account subject substitution in new ID token");
        assert!(matches!(
            subject_substitution.downcast_ref::<GrokAuthStageError>(),
            Some(GrokAuthStageError::AccountEntitlement)
        ));
        assert!(
            !format!("{subject_substitution:#}").contains("substituted-opaque-access"),
            "refresh identity diagnostics must remain secret-free: {subject_substitution:#}"
        );

        // GSK-05 regression 2: same-account, substituted-subject signed-access refresh shape must be rejected.
        let signed_access_substitution = dialect
            .validate_token_response(
                StatusCode::OK,
                &serde_json::to_vec(&json!({
                    "access_token": substituted_signed_access,
                    "expires_in": 120,
                }))
                .unwrap(),
                Some(&initial),
                &TokenValidationContext::Refresh,
                &CancellationToken::new(),
            )
            .await
            .expect_err(
                "refresh must reject same-account subject substitution in signed access token",
            );
        assert!(matches!(
            signed_access_substitution.downcast_ref::<GrokAuthStageError>(),
            Some(GrokAuthStageError::AccountEntitlement)
        ));

        // Different account must also be rejected.
        let different_account = dialect
            .validate_token_response(
                StatusCode::OK,
                &serde_json::to_vec(&json!({
                    "access_token": "different-account-access",
                    "id_token": different_account_identity,
                    "expires_in": 120,
                }))
                .unwrap(),
                Some(&initial),
                &TokenValidationContext::Refresh,
                &CancellationToken::new(),
            )
            .await
            .expect_err("refresh must reject a newly asserted different account identity");
        assert!(matches!(
            different_account.downcast_ref::<GrokAuthStageError>(),
            Some(GrokAuthStageError::AccountEntitlement)
        ));

        // Unprovable prior subject lineage must fail closed for identity-bearing refresh.
        let mut unprovable_previous = initial.clone();
        unprovable_previous.id_token = None;
        unprovable_previous.access_token = "purely-opaque-access".into();
        let unprovable_refresh = dialect
            .validate_token_response(
                StatusCode::OK,
                &serde_json::to_vec(&json!({
                    "access_token": "refreshed-opaque-access",
                    "id_token": valid_new_identity,
                    "expires_in": 120,
                }))
                .unwrap(),
                Some(&unprovable_previous),
                &TokenValidationContext::Refresh,
                &CancellationToken::new(),
            )
            .await
            .expect_err("refresh must fail closed when prior record cannot prove subject lineage");
        assert!(matches!(
            unprovable_refresh.downcast_ref::<GrokAuthStageError>(),
            Some(GrokAuthStageError::AccountEntitlement)
        ));

        // Valid refresh with matching subject and account must succeed.
        let matching_new_id = dialect
            .validate_token_response(
                StatusCode::OK,
                &serde_json::to_vec(&json!({
                    "access_token": "matching-opaque-access",
                    "id_token": valid_new_identity,
                    "expires_in": 120,
                }))
                .unwrap(),
                Some(&initial),
                &TokenValidationContext::Refresh,
                &CancellationToken::new(),
            )
            .await
            .expect("refresh must accept matching subject and account");
        assert_eq!(matching_new_id.account, initial.account);
        assert_eq!(
            matching_new_id.id_token.as_deref(),
            Some(valid_new_identity.as_str())
        );

        // Valid refresh with matching signed access token must succeed.
        let matching_signed = dialect
            .validate_token_response(
                StatusCode::OK,
                &serde_json::to_vec(&json!({
                    "access_token": valid_signed_access,
                    "expires_in": 120,
                }))
                .unwrap(),
                Some(&initial),
                &TokenValidationContext::Refresh,
                &CancellationToken::new(),
            )
            .await
            .expect("refresh must accept matching signed access token");
        assert_eq!(matching_signed.account, initial.account);

        server.abort();
    }

    #[tokio::test]
    async fn dialect_preserves_real_verifier_client_binding_stage_without_secret_copy() {
        let (key_pair, jwk) = signing_fixture();
        let identity = signed_token_fixture(
            &key_pair,
            json!(XAI_PUBLIC_CLIENT_ID),
            None,
            "acct-work",
            3600,
            None,
        );
        let access = signed_token_fixture(
            &key_pair,
            json!("wrong-client"),
            None,
            "acct-work",
            300,
            None,
        );
        let (origin, server) = verification_server(jwk).await;
        let verifier = GrokJwksVerifier::new(
            &origin,
            GROK_REQUIRED_TOKEN_ISSUER,
            XAI_PUBLIC_CLIENT_ID,
            true,
            Duration::from_secs(2),
        )
        .unwrap();
        let dialect = XaiGrokOAuthDialect::for_test(&origin, Arc::new(verifier)).unwrap();
        let marker = "refresh-secret-stage-sentinel";
        let body = serde_json::to_vec(&json!({
            "access_token": access,
            "id_token": identity,
            "refresh_token": marker,
        }))
        .unwrap();

        let error = dialect
            .validate_token_response(
                StatusCode::OK,
                &body,
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .expect_err("wrong-client signed access must retain its verifier stage");
        server.abort();

        assert!(matches!(
            error.downcast_ref::<GrokAuthStageError>(),
            Some(GrokAuthStageError::ClientBinding)
        ));
        assert!(
            !format!("{error:#}").contains(marker),
            "client-binding diagnostics must not reflect token response secrets: {error:#}"
        );
    }

    #[tokio::test]
    async fn dialect_preserves_real_jwks_transport_stage_without_secret_copy() {
        let (identity, jwk) = signed_identity_fixture();
        let (origin, server) = verification_server(jwk).await;
        let verifier = GrokJwksVerifier::new(
            &origin,
            GROK_REQUIRED_TOKEN_ISSUER,
            XAI_PUBLIC_CLIENT_ID,
            true,
            Duration::from_secs(1),
        )
        .unwrap();
        let dialect = XaiGrokOAuthDialect::for_test(&origin, Arc::new(verifier)).unwrap();
        server.abort();
        let _ = server.await;
        let marker = "opaque-access-stage-sentinel";
        let body = serde_json::to_vec(&json!({
            "access_token": marker,
            "id_token": identity,
        }))
        .unwrap();

        let error = dialect
            .validate_token_response(
                StatusCode::OK,
                &body,
                None,
                &TokenValidationContext::Device,
                &CancellationToken::new(),
            )
            .await
            .expect_err("unavailable pinned JWKS authority must retain its verifier stage");

        assert!(matches!(
            error.downcast_ref::<GrokAuthStageError>(),
            Some(GrokAuthStageError::JwksTransport)
        ));
        assert!(
            !format!("{error:#}").contains(marker),
            "JWKS diagnostics must not reflect access bearer bytes: {error:#}"
        );
    }

    #[tokio::test]
    async fn opaque_access_token_without_verified_identity_fails_closed_before_network() {
        let verifier = GrokJwksVerifier::new(
            "http://127.0.0.1:9",
            GROK_REQUIRED_TOKEN_ISSUER,
            XAI_PUBLIC_CLIENT_ID,
            true,
            Duration::from_millis(50),
        )
        .unwrap();
        let error = verifier
            .verify(None, "opaque-access-sentinel", &CancellationToken::new())
            .await
            .expect_err("an opaque bearer alone is not signed identity evidence");
        assert!(
            error.downcast_ref::<GrokAuthStageError>().is_some(),
            "unsigned identity rejection must retain a safe stage marker: {error:#}"
        );
        assert!(
            !format!("{error:#}").contains("opaque-access-sentinel"),
            "identity diagnostics must not reflect bearer bytes: {error:#}"
        );
    }

    #[tokio::test]
    async fn cancellation_during_jwks_authority_fetch_is_terminal_and_typed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requested = Arc::new(tokio::sync::Notify::new());
        let app = axum::Router::new().route(
            DISCOVERY_PATH,
            axum::routing::get({
                let requested = requested.clone();
                move || {
                    let requested = requested.clone();
                    async move {
                        requested.notify_one();
                        std::future::pending::<axum::Json<Value>>().await
                    }
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let verifier = GrokJwksVerifier::new(
            &origin,
            GROK_REQUIRED_TOKEN_ISSUER,
            XAI_PUBLIC_CLIENT_ID,
            true,
            Duration::from_secs(2),
        )
        .unwrap();
        let (identity_token, _) = signed_identity_fixture();
        let cancel = CancellationToken::new();
        let verify_cancel = cancel.clone();
        let verification = tokio::spawn(async move {
            verifier
                .verify(Some(&identity_token), "opaque-access-token", &verify_cancel)
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), requested.notified())
            .await
            .expect("the verifier must reach the local pinned discovery authority");
        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(1), verification)
            .await
            .expect("cancellation must stop a pending JWKS authority fetch")
            .unwrap()
            .expect_err("cancelled verification must not return identity claims");
        server.abort();
        assert!(
            error
                .downcast_ref::<crate::oauth::OAuthDeviceAuthorizationError>()
                .is_some(),
            "JWKS cancellation must retain the terminal cancellation type: {error:#}"
        );
    }

    #[test]
    fn client_binding_requires_exact_audience_and_multi_audience_azp() {
        let now = Utc::now();
        let claims = |audiences: BTreeSet<String>, authorized_party: Option<String>| SignedClaims {
            issuer: GROK_REQUIRED_TOKEN_ISSUER.into(),
            audiences,
            subject: "acct-work".into(),
            authorized_party,
            exp: (now + TimeDelta::hours(1)).timestamp(),
            nbf: None,
            iat: now.timestamp(),
            nonce: None,
            principal_type: None,
            principal_id: None,
            expires_at: now + TimeDelta::hours(1),
            not_before: None,
        };
        assert!(validate_client_binding(
            &claims(BTreeSet::from([XAI_PUBLIC_CLIENT_ID.into()]), None),
            XAI_PUBLIC_CLIENT_ID
        )
        .is_ok());
        let wrong = validate_client_binding(
            &claims(BTreeSet::from(["another-client".into()]), None),
            XAI_PUBLIC_CLIENT_ID,
        )
        .unwrap_err();
        assert!(matches!(
            wrong.downcast_ref::<GrokAuthStageError>(),
            Some(GrokAuthStageError::ClientBinding)
        ));
        assert!(validate_client_binding(
            &claims(
                BTreeSet::from([XAI_PUBLIC_CLIENT_ID.into(), "another-audience".into()]),
                None,
            ),
            XAI_PUBLIC_CLIENT_ID,
        )
        .is_err());
        assert!(validate_client_binding(
            &claims(
                BTreeSet::from([XAI_PUBLIC_CLIENT_ID.into(), "another-audience".into()]),
                Some(XAI_PUBLIC_CLIENT_ID.into()),
            ),
            XAI_PUBLIC_CLIENT_ID,
        )
        .is_ok());
    }

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
