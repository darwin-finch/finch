//! Exact OpenAI issuer/JWKS authority for the pinned ChatGPT compatibility dialect.
//!
//! This module deliberately implements only RS256 compact JWS. Discovery and
//! key retrieval are pinned to exact paths and origins; neither token headers
//! nor discovery responses can substitute an authority.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, TimeDelta, Utc};
use futures::StreamExt;
use reqwest::{Client, StatusCode, Url};
use ring::signature::{RsaPublicKeyComponents, RSA_PKCS1_2048_8192_SHA256};
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::chatgpt_oauth::{
    OpenAiTokenVerifier, VerifiedOpenAiClaims, OPENAI_AUTH_ORIGIN, OPENAI_PUBLIC_CLIENT_ID,
    REQUIRED_TOKEN_ISSUER,
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

/// Bounded, single-flight verifier for the exact pinned OpenAI issuer.
pub struct OpenAiJwksVerifier {
    issuer: String,
    discovery_url: Url,
    jwks_url: Url,
    client_id: String,
    http: Client,
    cache: Mutex<KeyCache>,
    generation: AtomicU64,
}

impl std::fmt::Debug for OpenAiJwksVerifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiJwksVerifier")
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
    keys: BTreeMap<String, Arc<VerifiedRsaKey>>,
    expires_at: Option<tokio::time::Instant>,
    generation: u64,
}

struct VerifiedRsaKey {
    modulus: Vec<u8>,
    exponent: Vec<u8>,
}

impl OpenAiJwksVerifier {
    /// Construct the production verifier for the exact pinned OpenAI authority.
    pub fn production() -> Result<Self> {
        Self::new(
            OPENAI_AUTH_ORIGIN,
            REQUIRED_TOKEN_ISSUER,
            OPENAI_PUBLIC_CLIENT_ID,
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
            .context("OpenAI discovery authority is invalid")?;
        let jwks_url = authority_url
            .join(JWKS_PATH)
            .context("OpenAI JWKS authority is invalid")?;
        if client_id.trim().is_empty() || timeout.is_zero() {
            bail!("OpenAI token verifier authority is incomplete");
        }
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(timeout)
            .build()
            .context("Failed to construct bounded OpenAI JWKS client")?;
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
    pub(crate) fn for_test(
        authority_origin: &str,
        expected_issuer: &str,
        client_id: &str,
        timeout: Duration,
    ) -> Result<Self> {
        Self::new(authority_origin, expected_issuer, client_id, true, timeout)
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
            bail!("OpenAI signed token has an invalid size or encoding");
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
            bail!("OpenAI signed token is not a compact three-part JWS");
        }

        let header_bytes = decode_segment(encoded_header, "header")?;
        reject_duplicate_json_fields(&header_bytes, "signed token header")?;
        let header_value: Value = serde_json::from_slice(&header_bytes)
            .context("OpenAI signed token header is malformed")?;
        let header: JwsHeader = serde_json::from_value(header_value.clone())
            .context("OpenAI signed token header is incompatible")?;
        reject_header_authority_substitution(&header_value)?;
        if header.alg != "RS256" || header.kid.trim().is_empty() || header.kid.len() > 256 {
            bail!("OpenAI signed token algorithm or key identifier is unsupported");
        }
        if header.typ.as_deref().is_some_and(|typ| typ != "JWT") {
            bail!("OpenAI signed token type is unsupported");
        }

        let signature = decode_segment(encoded_signature, "signature")?;
        let key = self.key_for(&header.kid, cancel).await?;
        let signing_input = format!("{encoded_header}.{encoded_claims}");
        RsaPublicKeyComponents {
            n: &key.modulus,
            e: &key.exponent,
        }
        .verify(
            &RSA_PKCS1_2048_8192_SHA256,
            signing_input.as_bytes(),
            &signature,
        )
        .map_err(|_| anyhow::anyhow!("OpenAI signed token signature is invalid"))?;

        let claims_bytes = decode_segment(encoded_claims, "claims")?;
        reject_duplicate_json_fields(&claims_bytes, "signed token claims")?;
        let mut claims: SignedClaims = serde_json::from_slice(&claims_bytes)
            .context("OpenAI signed token claims are malformed")?;
        claims.validate_signed_authority(&self.issuer)?;
        Ok(claims)
    }

    async fn key_for(&self, kid: &str, cancel: &CancellationToken) -> Result<Arc<VerifiedRsaKey>> {
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
                .context("OpenAI JWKS rotation did not contain the signed token key");
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
            .context("OpenAI signed token key identifier is absent from the pinned JWKS")
    }

    async fn fetch_keys(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(BTreeMap<String, Arc<VerifiedRsaKey>>, Duration)> {
        let (_, discovery_bytes) = self.fetch_document(&self.discovery_url, cancel).await?;
        reject_duplicate_json_fields(&discovery_bytes, "discovery document")?;
        let discovery: DiscoveryDocument = serde_json::from_slice(&discovery_bytes)
            .context("OpenAI discovery document is malformed")?;
        if discovery.issuer != self.issuer || discovery.jwks_uri != self.jwks_url.as_str() {
            bail!("OpenAI discovery document changed issuer or JWKS authority");
        }

        let (cache_control, jwks_bytes) = self.fetch_document(&self.jwks_url, cancel).await?;
        reject_duplicate_json_fields(&jwks_bytes, "JWKS document")?;
        let document: JwksDocument =
            serde_json::from_slice(&jwks_bytes).context("OpenAI JWKS document is malformed")?;
        if document.keys.is_empty() || document.keys.len() > 128 {
            bail!("OpenAI JWKS contains an invalid number of keys");
        }
        let mut keys = BTreeMap::new();
        for key in document.keys {
            if key.kty != "RSA" || key.key_use.as_deref() != Some("sig") || key.alg != "RS256" {
                bail!("OpenAI JWKS contains a key outside the pinned RS256 signing contract");
            }
            if key.kid.trim().is_empty() || key.kid.len() > 256 || keys.contains_key(&key.kid) {
                bail!("OpenAI JWKS contains a missing, duplicate, or ambiguous key identifier");
            }
            let modulus = decode_key_component(&key.n, "modulus")?;
            let exponent = decode_key_component(&key.e, "exponent")?;
            if modulus.len() < 256
                || modulus.len() > 1024
                || exponent.is_empty()
                || exponent.len() > 8
            {
                bail!("OpenAI JWKS RSA key strength or exponent is invalid");
            }
            keys.insert(key.kid, Arc::new(VerifiedRsaKey { modulus, exponent }));
        }
        Ok((keys, cache_lifetime(cache_control.as_deref())))
    }

    async fn fetch_document(
        &self,
        url: &Url,
        cancel: &CancellationToken,
    ) -> Result<(Option<String>, Vec<u8>)> {
        let response = tokio::select! {
            _ = cancel.cancelled() => bail!("OpenAI token verification was cancelled"),
            response = self.http.get(url.clone()).send() => response.context("OpenAI verification authority is unavailable")?,
        };
        if response.status() != StatusCode::OK {
            bail!(
                "OpenAI verification authority returned HTTP {}",
                response.status()
            );
        }
        if response.url() != url {
            bail!("OpenAI verification authority redirected outside its pinned URL");
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_DOCUMENT_BYTES as u64)
        {
            bail!("OpenAI verification document exceeded the size limit");
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
                _ = cancel.cancelled() => bail!("OpenAI token verification was cancelled"),
                next = stream.next() => next,
            };
            let Some(chunk) = next else { break };
            let chunk = chunk.context("OpenAI verification document body failed")?;
            if body.len().saturating_add(chunk.len()) > MAX_DOCUMENT_BYTES {
                bail!("OpenAI verification document exceeded the size limit");
            }
            body.extend_from_slice(&chunk);
        }
        Ok((cache_control, body))
    }
}

#[async_trait]
impl OpenAiTokenVerifier for OpenAiJwksVerifier {
    fn preflight(&self) -> Result<()> {
        exact_issuer(&self.issuer, false)?;
        if self.discovery_url.path() != DISCOVERY_PATH
            || self.jwks_url.path() != JWKS_PATH
            || self.discovery_url.origin() != self.jwks_url.origin()
            || self.client_id.trim().is_empty()
        {
            bail!("OpenAI token verification authority is incompatible");
        }
        Ok(())
    }

    async fn verify(
        &self,
        id_token: Option<&str>,
        access_token: &str,
        cancel: &CancellationToken,
    ) -> Result<VerifiedOpenAiClaims> {
        let id_token = id_token.context(
            "ChatGPT token response omitted the signed identity token required for account binding",
        )?;
        let identity = self.verify_compact(id_token, cancel).await?;
        if !identity.audiences.contains(&self.client_id)
            || (identity.audiences.len() > 1
                && identity.authorized_party.as_deref() != Some(self.client_id.as_str()))
            || identity
                .authorized_party
                .as_deref()
                .is_some_and(|party| party != self.client_id)
        {
            bail!("OpenAI identity token audience does not match the pinned public client");
        }
        crate::oauth::validate_secret_field(access_token, "access token")?;
        let identity_auth = identity
            .auth
            .context("OpenAI identity token omitted the namespaced ChatGPT account entitlement")?;
        for (value, label) in [
            (identity_auth.account_id.as_str(), "identity account"),
            (identity_auth.plan_type.as_str(), "identity plan"),
        ] {
            validate_signed_public_claim(value, label)?;
        }
        Ok(VerifiedOpenAiClaims {
            issuer: identity.issuer,
            audiences: identity.audiences,
            authorized_party: identity.authorized_party,
            subject: identity.subject,
            account_id: identity_auth.account_id,
            chatgpt_plan_type: identity_auth.plan_type,
            account_is_fedramp: identity_auth.account_is_fedramp,
            nonce: identity.nonce,
            expires_at: identity.expires_at,
            not_before: identity.not_before,
        })
    }
}

fn validate_signed_public_claim(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("OpenAI signed token {label} claim is invalid");
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
    alg: String,
    kid: String,
    n: String,
    e: String,
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
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<ChatGptAuthClaims>,
    #[serde(skip)]
    expires_at: DateTime<Utc>,
    #[serde(skip)]
    not_before: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct ChatGptAuthClaims {
    #[serde(rename = "chatgpt_account_id")]
    account_id: String,
    #[serde(rename = "chatgpt_plan_type")]
    plan_type: String,
    #[serde(rename = "chatgpt_account_is_fedramp", default)]
    account_is_fedramp: bool,
}

impl SignedClaims {
    fn validate_signed_authority(&mut self, expected_issuer: &str) -> Result<()> {
        let now = Utc::now();
        self.expires_at = DateTime::from_timestamp(self.exp, 0)
            .context("OpenAI signed token expiration is invalid")?;
        self.not_before = self
            .nbf
            .map(|value| {
                DateTime::from_timestamp(value, 0)
                    .context("OpenAI signed token not-before time is invalid")
            })
            .transpose()?;
        let issued_at = DateTime::from_timestamp(self.iat, 0)
            .context("OpenAI signed token issued-at time is invalid")?;
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
            bail!("OpenAI signed token issuer, subject, audience, or lifetime is invalid");
        }
        Ok(())
    }
}

fn exact_issuer(value: &str, allow_insecure_loopback: bool) -> Result<Url> {
    let url = Url::parse(value).context("OpenAI issuer must be an absolute URL")?;
    let loopback =
        allow_insecure_loopback && url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    if (url.scheme() != "https" && !loopback)
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.path() != "/" && !url.path().is_empty())
    {
        bail!("OpenAI issuer authority is not an exact HTTPS origin");
    }
    Ok(url)
}

fn reject_header_authority_substitution(header: &Value) -> Result<()> {
    let object = header
        .as_object()
        .context("OpenAI signed token header must be an object")?;
    for forbidden in ["jku", "jwk", "x5u", "x5c", "crit"] {
        if object.contains_key(forbidden) {
            bail!("OpenAI signed token header attempted authority substitution");
        }
    }
    Ok(())
}

fn decode_segment(value: &str, name: &str) -> Result<Vec<u8>> {
    if value.len() > MAX_TOKEN_BYTES {
        bail!("OpenAI signed token {name} exceeded the size limit");
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .with_context(|| format!("OpenAI signed token {name} is not base64url"))
}

fn decode_key_component(value: &str, name: &str) -> Result<Vec<u8>> {
    if value.is_empty() || value.len() > 2048 {
        bail!("OpenAI JWKS RSA {name} is invalid");
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .with_context(|| format!("OpenAI JWKS RSA {name} is not base64url"))
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
        .with_context(|| format!("OpenAI {label} contains duplicate or malformed JSON fields"))?;
    deserializer
        .end()
        .with_context(|| format!("OpenAI {label} contains trailing JSON data"))?;
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
    use crate::oauth::{OAuthCredentialStore, OAuthDialect, TokenValidationContext};
    use crate::providers::chatgpt_oauth::OpenAiChatGptOAuthDialect;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::Response;
    use axum::http::StatusCode as AxumStatusCode;
    use axum::routing::{get, post};
    use axum::Router;
    use ring::rand::SystemRandom;
    use ring::signature::{KeyPair, RsaKeyPair, RSA_PKCS1_SHA256};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex as StdMutex;

    const TEST_PRIVATE_KEY_DER: &str = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCdAeQ874zqSDAW9XjZmHRYDQvVpFjAju0X3pKB1+42m2Amo28WbSErTp/dVlJtkX1rOfdTVcitvDLqPS9oBD5F+S78+oCyEHaJn89H/jK74si47+8ulnI25WMQNdQ3C5ADy/BpDVU+8wBOWIktGz7mJtOByfWCGSE72OKRQq+cpqYMlyn2D1bCTZoER6MoantQtPdqa40tvmLJig3Z9c2fDN7oqHyRF53EPtSa0OoRu6f2ayTngrwoLE+emVVl8dZHaOrJIaDTMVFux9T3lY4XOxszIWxztdIKyHe08hH8jqWqhJHa6weYmcB2cuTtZrTEAsQhF5wW9xODaSOMLhSNAgMBAAECggEAAda1VQ9bH51DzukGBspVxng0pMZdcbfax/ZH0fR06jfMmvc8BE+33Tl4/s8VfQoApYJSxquRA5PaJssbpIS0M/6UkcrfOfaeZMM12rp73p5rylqo+usxIDp0fAqdVx2wDJNVV+2bi3auELzRsnEIvgpDXNhAI0tnC7vg/2GAC/4U7l66SSX8BQdLbGDlZrq2IVpG29KckBUrsl+uyFJRhJ3EsmdOpjpAWv6882/0ea1Svi4Uy8kvEAF3Qwq2k+0UjOfJZls9MS1JSXpQQARvdeNv4QJmIjnCMo30naCKZmXIFVDrM1xUuVUURAGlu0rSv+qQzV7pJ2Pf3BvLj41pgQKBgQDMnMtDNf8DFndwuKDE0ZAF3dMyS34vqS33jqI7qj930ra0BOvECCgjqGpwIY7xmxYN1U6hZDy+5uFeGthhjpgJbQq0pWFUxob6Xw2dcuEu/D2K/Kqi9YhVq2ZZZ6nT3ddMvJmdXRA+KuoidcX+8dA//gJBnA5K9K/9Ds+XNmOXwQKBgQDEcGkeU+c+fIjLLTEy2jwRsh7n8ZMC5OHg9WaEomuBro4ZfNXi4lwvz7+6+LSTBSPgrJgaJ7hymgH5LwpLL1r5uGZyzFCKnkRz01I9aXVWfewhKIdquRXUU1MNQ2PrFiZtE5u7MctqT+YeuX3J0WASy+aWOGpfNZ06JqoAoNlPzQKBgG0MY4g+jtqmbqG0xHog9hEqWBTGB0p/b/AwJGaIJatGsfjfZofjkQDwEUoRmI1LikV1GaMKORXFFveAdzIHPSBI7Ru5yFXWOLnXTvpK75iK9oHMh2SyVybRYorjpK813DkZiwVDRBTd6krTWeK2Hbb9OVaeRT/NiL3l1t1QL2QBAoGBAIFlkrjZh//PRMShdkELJHp7nIQoyzAi2O+4dtlzq+F2vD/pzXJwrU0JSkC9RyV5Q1LiHidMduF2tUoRRHSWMxU/9Kw2De/hpTGuyAOQDiz1Ma/95IXWeZytbo3UEGNw6cr8GZ9Lg7T6AJnIkiV4+BIpojDd5KPmyzTc9ysGyV8ZAoGAfvFL5BicPTVwwB85n+PdGtl6mMjjHdDyS/O8B9BsESA1sbVJ48Os1NiUT+bjZktTWhjScUKCNCMzAop5RbzANwkdzb0rs8CrrFENYjncMjLnsLFlc/qL8FkpiCFeOxX05iy5kvxbm8nJOTspoCDzQywEKKZGSrx0+Z6vF4bkTWg=";
    const TEST_MODULUS: &str = "nQHkPO-M6kgwFvV42Zh0WA0L1aRYwI7tF96SgdfuNptgJqNvFm0hK06f3VZSbZF9azn3U1XIrbwy6j0vaAQ-Rfku_PqAshB2iZ_PR_4yu-LIuO_vLpZyNuVjEDXUNwuQA8vwaQ1VPvMATliJLRs-5ibTgcn1ghkhO9jikUKvnKamDJcp9g9Wwk2aBEejKGp7ULT3amuNLb5iyYoN2fXNnwze6Kh8kRedxD7UmtDqEbun9msk54K8KCxPnplVZfHWR2jqySGg0zFRbsfU95WOFzsbMyFsc7XSCsh3tPIR_I6lqoSR2usHmJnAdnLk7Wa0xALEIRecFvcTg2kjjC4UjQ";

    #[derive(Clone)]
    struct FixtureState {
        origin: String,
        discovery_issuer_override: Arc<StdMutex<Option<String>>>,
        discovery_jwks_override: Arc<StdMutex<Option<String>>>,
        jwks: Arc<StdMutex<VecDeque<Value>>>,
        discovery_count: Arc<AtomicUsize>,
        jwks_count: Arc<AtomicUsize>,
        token_identity_valid: Arc<AtomicBool>,
        delay: Duration,
        redirect_jwks: bool,
        oversized: bool,
    }

    struct FixtureServer {
        origin: String,
        state: FixtureState,
    }

    impl FixtureServer {
        async fn start(jwks: Vec<Value>) -> Self {
            Self::start_with(jwks, Duration::ZERO, false, false).await
        }

        async fn start_with(
            jwks: Vec<Value>,
            delay: Duration,
            redirect_jwks: bool,
            oversized: bool,
        ) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let state = FixtureState {
                origin: origin.clone(),
                discovery_issuer_override: Arc::new(StdMutex::new(None)),
                discovery_jwks_override: Arc::new(StdMutex::new(None)),
                jwks: Arc::new(StdMutex::new(jwks.into())),
                discovery_count: Arc::new(AtomicUsize::new(0)),
                jwks_count: Arc::new(AtomicUsize::new(0)),
                token_identity_valid: Arc::new(AtomicBool::new(true)),
                delay,
                redirect_jwks,
                oversized,
            };
            let router = Router::new()
                .route(DISCOVERY_PATH, get(discovery))
                .route(JWKS_PATH, get(jwks_document))
                .route("/api/accounts/deviceauth/usercode", post(device_start))
                .route("/api/accounts/deviceauth/token", post(device_poll))
                .route("/oauth/token", post(token_exchange))
                .route("/attacker", get(|| async { "{}" }))
                .with_state(state.clone());
            tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            Self { origin, state }
        }
    }

    async fn discovery(State(state): State<FixtureState>) -> Response<Body> {
        state.discovery_count.fetch_add(1, AtomicOrdering::SeqCst);
        tokio::time::sleep(state.delay).await;
        let jwks_uri = state
            .discovery_jwks_override
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| format!("{}{}", state.origin, JWKS_PATH));
        let issuer = state
            .discovery_issuer_override
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| REQUIRED_TOKEN_ISSUER.to_string());
        json_response(json!({
            "issuer": issuer,
            "jwks_uri": jwks_uri,
        }))
    }

    async fn jwks_document(State(state): State<FixtureState>) -> Response<Body> {
        state.jwks_count.fetch_add(1, AtomicOrdering::SeqCst);
        tokio::time::sleep(state.delay).await;
        if state.redirect_jwks {
            return Response::builder()
                .status(AxumStatusCode::FOUND)
                .header("location", "/attacker")
                .body(Body::empty())
                .unwrap();
        }
        if state.oversized {
            return Response::builder()
                .status(AxumStatusCode::OK)
                .body(Body::from(vec![b'x'; MAX_DOCUMENT_BYTES + 1]))
                .unwrap();
        }
        let mut documents = state.jwks.lock().unwrap();
        let document = if documents.len() > 1 {
            documents.pop_front().unwrap()
        } else {
            documents
                .front()
                .cloned()
                .unwrap_or_else(|| json!({"keys": []}))
        };
        Response::builder()
            .status(AxumStatusCode::OK)
            .header("content-type", "application/json")
            .header("cache-control", "max-age=300")
            .body(Body::from(document.to_string()))
            .unwrap()
    }

    async fn device_start() -> Response<Body> {
        json_response(json!({
            "device_auth_id": "device-fixture",
            "user_code": "SAFE-FIXTURE",
            "interval": "0",
        }))
    }

    async fn device_poll() -> Response<Body> {
        let verifier = "fixture-verifier";
        json_response(json!({
            "authorization_code": "fixture-authorization",
            "code_verifier": verifier,
            "code_challenge": URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        }))
    }

    async fn token_exchange(State(state): State<FixtureState>) -> Response<Body> {
        let (identity, _access) = claims("acct-live-shape", required_scopes());
        let mut id_token = signed("key-one", &identity);
        if !state.token_identity_valid.load(AtomicOrdering::SeqCst) {
            id_token.push('x');
        }
        json_response(json!({
            "id_token": id_token,
            "access_token": "opaque-access-bearer",
            "refresh_token": "opaque-refresh-bearer",
        }))
    }

    fn json_response(value: Value) -> Response<Body> {
        Response::builder()
            .status(AxumStatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap()
    }

    fn jwks(kid: &str) -> Value {
        json!({"keys": [{
            "kty": "RSA", "use": "sig", "alg": "RS256", "kid": kid,
            "n": TEST_MODULUS, "e": "AQAB"
        }]})
    }

    fn signed(kid: &str, claims: &Value) -> String {
        let header = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&json!({"alg":"RS256", "kid":kid, "typ":"JWT"})).unwrap());
        let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        let input = format!("{header}.{claims}");
        let der = base64::engine::general_purpose::STANDARD
            .decode(TEST_PRIVATE_KEY_DER)
            .unwrap();
        let key = RsaKeyPair::from_pkcs8(&der).unwrap();
        let mut signature = vec![0; key.public().modulus_len()];
        key.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            input.as_bytes(),
            &mut signature,
        )
        .unwrap();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }

    fn claims(account: &str, scopes: &str) -> (Value, Value) {
        let now = Utc::now().timestamp();
        let auth = json!({
            "chatgpt_account_id": account,
            "chatgpt_plan_type": "plus",
            "chatgpt_account_is_fedramp": false,
        });
        let identity = json!({
            "iss": REQUIRED_TOKEN_ISSUER,
            "aud": OPENAI_PUBLIC_CLIENT_ID,
            "sub": "user-123",
            "iat": now,
            "nbf": now - 1,
            "exp": now + 3600,
            "nonce": "nonce-123",
            "https://api.openai.com/auth": auth,
        });
        let access = json!({
            "iss": REQUIRED_TOKEN_ISSUER,
            "aud": OPENAI_CODEX_ACCESS_TOKEN_AUDIENCE,
            "sub": "user-123",
            "iat": now,
            "nbf": now - 1,
            "exp": now + 1800,
            "scope": scopes,
            "https://api.openai.com/auth": auth,
        });
        (identity, access)
    }

    fn required_scopes() -> &'static str {
        "openid profile email offline_access api.connectors.read api.connectors.invoke"
    }

    #[tokio::test]
    async fn signed_identity_and_opaque_access_fixture_binds_account_nonce_and_time() {
        let server = FixtureServer::start(vec![jwks("key-one")]).await;
        let verifier = Arc::new(
            OpenAiJwksVerifier::for_test(
                &server.origin,
                REQUIRED_TOKEN_ISSUER,
                OPENAI_PUBLIC_CLIENT_ID,
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let dialect = OpenAiChatGptOAuthDialect::for_test(&server.origin, verifier).unwrap();
        let (identity, _access) = claims("acct-one", required_scopes());
        let record = dialect
            .validate_tokens(
                StatusCode::OK,
                json!({
                    "id_token": signed("key-one", &identity),
                    "access_token": "opaque-access-bearer",
                    "refresh_token": "refresh-secret",
                }),
                None,
                &TokenValidationContext::Browser {
                    expected_nonce: "nonce-123".into(),
                    redirect_uri: "http://127.0.0.1/callback".into(),
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(record.account, "acct-one");
        assert_eq!(
            record.expires_at.timestamp(),
            identity["exp"].as_i64().unwrap()
        );
        assert_eq!(server.state.discovery_count.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(server.state.jwks_count.load(AtomicOrdering::SeqCst), 1);
        let debug = format!("{record:?}{:?}", dialect.descriptor());
        assert!(!debug.contains("refresh-secret"));
    }

    #[tokio::test]
    async fn device_browser_success_with_opaque_access_persists_exact_named_credential() {
        let server = FixtureServer::start(vec![jwks("key-one")]).await;
        let verifier = Arc::new(
            OpenAiJwksVerifier::for_test(
                &server.origin,
                REQUIRED_TOKEN_ISSUER,
                OPENAI_PUBLIC_CLIENT_ID,
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let dialect =
            Arc::new(OpenAiChatGptOAuthDialect::for_test(&server.origin, verifier).unwrap());
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::oauth::file_store::FileOAuthCredentialStore::new(
            directory.path().join("oauth"),
        ));
        let client = crate::oauth::OAuthClient::new(dialect, store.clone()).unwrap();
        let pending = client.begin_device_authorization().await.unwrap();
        let credential = client
            .finish_device_authorization("chatgpt:live-shape", &pending, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(credential.name, "chatgpt:live-shape");
        assert_eq!(credential.account.as_deref(), Some("acct-live-shape"));
        let persisted = store.load("chatgpt:live-shape").unwrap().unwrap();
        assert_eq!(persisted.account, "acct-live-shape");
        assert_eq!(persisted.access_token, "opaque-access-bearer");
        assert!(store.load("chatgpt:other").unwrap().is_none());
    }

    #[tokio::test]
    async fn device_invalid_signed_identity_is_stage_classified_without_persistence() {
        let server = FixtureServer::start(vec![jwks("key-one")]).await;
        server
            .state
            .token_identity_valid
            .store(false, AtomicOrdering::SeqCst);
        let verifier = Arc::new(
            OpenAiJwksVerifier::for_test(
                &server.origin,
                REQUIRED_TOKEN_ISSUER,
                OPENAI_PUBLIC_CLIENT_ID,
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let dialect =
            Arc::new(OpenAiChatGptOAuthDialect::for_test(&server.origin, verifier).unwrap());
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::oauth::file_store::FileOAuthCredentialStore::new(
            directory.path().join("oauth"),
        ));
        let client = crate::oauth::OAuthClient::new(dialect, store.clone()).unwrap();
        let pending = client.begin_device_authorization().await.unwrap();
        let error = client
            .finish_device_authorization(
                "chatgpt:invalid-identity",
                &pending,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();

        assert!(error.chain().any(|source| source
            .downcast_ref::<crate::providers::chatgpt_oauth::ChatGptAuthStageError>(
        ) == Some(
            &crate::providers::chatgpt_oauth::ChatGptAuthStageError::IdentityVerification
        )));
        assert!(store.load("chatgpt:invalid-identity").unwrap().is_none());
        let rendered = format!("{error:#}");
        for secret in ["opaque-access-bearer", "opaque-refresh-bearer"] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
    }

    #[tokio::test]
    async fn signed_claim_and_signature_hostile_matrix_fails_before_persistence() {
        for defect in [
            "issuer",
            "trailing-issuer",
            "audience",
            "multi-audience-without-azp",
            "multi-audience-wrong-azp",
            "account",
            "scope",
            "nonce",
            "expired",
            "future",
            "missing-iat",
            "stale-iat",
            "signature",
        ] {
            let server = FixtureServer::start(vec![jwks("key-one")]).await;
            let verifier = Arc::new(
                OpenAiJwksVerifier::for_test(
                    &server.origin,
                    REQUIRED_TOKEN_ISSUER,
                    OPENAI_PUBLIC_CLIENT_ID,
                    Duration::from_secs(1),
                )
                .unwrap(),
            );
            let dialect = OpenAiChatGptOAuthDialect::for_test(&server.origin, verifier).unwrap();
            let (mut identity, _access) = claims("acct-one", required_scopes());
            let mut expected_nonce = "nonce-123";
            let mut token_scope = required_scopes();
            match defect {
                "issuer" => identity["iss"] = json!("https://evil.example"),
                "trailing-issuer" => identity["iss"] = json!("https://auth.openai.com/"),
                "audience" => identity["aud"] = json!("other-client"),
                "multi-audience-without-azp" => {
                    identity["aud"] = json!([OPENAI_PUBLIC_CLIENT_ID, "other-client"])
                }
                "multi-audience-wrong-azp" => {
                    identity["aud"] = json!([OPENAI_PUBLIC_CLIENT_ID, "other-client"]);
                    identity["azp"] = json!("other-client");
                }
                "account" => {
                    identity["https://api.openai.com/auth"]["chatgpt_account_id"] = json!("")
                }
                "scope" => token_scope = "openid",
                "nonce" => expected_nonce = "other-nonce",
                "expired" => identity["exp"] = json!(Utc::now().timestamp() - 120),
                "future" => identity["nbf"] = json!(Utc::now().timestamp() + 120),
                "missing-iat" => {
                    identity.as_object_mut().unwrap().remove("iat");
                }
                "stale-iat" => identity["iat"] = json!(Utc::now().timestamp() - 25 * 60 * 60),
                "signature" => {}
                _ => unreachable!(),
            }
            let id_token = signed("key-one", &identity);
            let access_token = "opaque-access-bearer".to_string();
            if defect == "signature" {
                let separator = id_token.find('.').unwrap();
                let mut corrupted = id_token.clone().into_bytes();
                corrupted[separator.saturating_sub(1)] ^= 1;
                let corrupted = String::from_utf8(corrupted).unwrap();
                let error = dialect
                    .validate_tokens(
                        StatusCode::OK,
                        json!({
                            "id_token": corrupted,
                            "access_token": access_token,
                            "refresh_token": "refresh-secret",
                            "scope": token_scope,
                        }),
                        None,
                        &TokenValidationContext::Browser {
                            expected_nonce: expected_nonce.into(),
                            redirect_uri: "http://127.0.0.1/callback".into(),
                        },
                        &CancellationToken::new(),
                    )
                    .await
                    .unwrap_err()
                    .to_string();
                assert!(!error.contains("refresh-secret"), "{defect}: {error}");
                continue;
            }
            let error = dialect
                .validate_tokens(
                    StatusCode::OK,
                    json!({
                        "id_token": id_token,
                        "access_token": access_token,
                        "refresh_token": "refresh-secret",
                        "scope": token_scope,
                    }),
                    None,
                    &TokenValidationContext::Browser {
                        expected_nonce: expected_nonce.into(),
                        redirect_uri: "http://127.0.0.1/callback".into(),
                    },
                    &CancellationToken::new(),
                )
                .await
                .unwrap_err()
                .to_string();
            assert!(!error.contains("refresh-secret"), "{defect}: {error}");
        }

        let server = FixtureServer::start(vec![jwks("key-one")]).await;
        let verifier = OpenAiJwksVerifier::for_test(
            &server.origin,
            REQUIRED_TOKEN_ISSUER,
            OPENAI_PUBLIC_CLIENT_ID,
            Duration::from_secs(1),
        )
        .unwrap();
        let (mut identity, access) = claims("acct-one", required_scopes());
        identity["aud"] = json!([OPENAI_PUBLIC_CLIENT_ID, "other-client"]);
        identity["azp"] = json!(OPENAI_PUBLIC_CLIENT_ID);
        verifier
            .verify(
                Some(&signed("key-one", &identity)),
                &signed("key-one", &access),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn alg_key_type_use_and_kid_confusion_matrix_is_rejected() {
        let (identity, access) = claims("acct-one", required_scopes());
        for (defect, document, token) in [
            (
                "wrong-kty",
                json!({"keys":[{"kty":"EC","use":"sig","alg":"RS256","kid":"key-one","n":TEST_MODULUS,"e":"AQAB"}]}),
                signed("key-one", &identity),
            ),
            (
                "wrong-use",
                json!({"keys":[{"kty":"RSA","use":"enc","alg":"RS256","kid":"key-one","n":TEST_MODULUS,"e":"AQAB"}]}),
                signed("key-one", &identity),
            ),
            (
                "wrong-alg",
                json!({"keys":[{"kty":"RSA","use":"sig","alg":"RS512","kid":"key-one","n":TEST_MODULUS,"e":"AQAB"}]}),
                signed("key-one", &identity),
            ),
            (
                "missing-kid",
                json!({"keys":[{"kty":"RSA","use":"sig","alg":"RS256","kid":"","n":TEST_MODULUS,"e":"AQAB"}]}),
                signed("key-one", &identity),
            ),
            (
                "duplicate-kid",
                json!({"keys":[
                    {"kty":"RSA","use":"sig","alg":"RS256","kid":"key-one","n":TEST_MODULUS,"e":"AQAB"},
                    {"kty":"RSA","use":"sig","alg":"RS256","kid":"key-one","n":TEST_MODULUS,"e":"AQAB"}
                ]}),
                signed("key-one", &identity),
            ),
        ] {
            let server = FixtureServer::start(vec![document]).await;
            let verifier = OpenAiJwksVerifier::for_test(
                &server.origin,
                REQUIRED_TOKEN_ISSUER,
                OPENAI_PUBLIC_CLIENT_ID,
                Duration::from_secs(1),
            )
            .unwrap();
            assert!(
                verifier
                    .verify(
                        Some(&token),
                        &signed("key-one", &access),
                        &CancellationToken::new()
                    )
                    .await
                    .is_err(),
                "{defect}"
            );
        }

        let server = FixtureServer::start(vec![jwks("key-one")]).await;
        let verifier = OpenAiJwksVerifier::for_test(
            &server.origin,
            REQUIRED_TOKEN_ISSUER,
            OPENAI_PUBLIC_CLIENT_ID,
            Duration::from_secs(1),
        )
        .unwrap();
        for token in [
            format!(
                "{}.{}.x",
                URL_SAFE_NO_PAD.encode(br#"{"alg":"none","kid":"key-one"}"#),
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&identity).unwrap())
            ),
            format!(
                "{}.{}.x",
                URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","kid":"key-one"}"#),
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&identity).unwrap())
            ),
        ] {
            assert!(verifier
                .verify(
                    Some(&token),
                    &signed("key-one", &access),
                    &CancellationToken::new()
                )
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn key_rotation_and_concurrent_cache_miss_are_single_flight() {
        let server = FixtureServer::start_with(
            vec![jwks("key-one"), jwks("key-two")],
            Duration::from_millis(20),
            false,
            false,
        )
        .await;
        let verifier = Arc::new(
            OpenAiJwksVerifier::for_test(
                &server.origin,
                REQUIRED_TOKEN_ISSUER,
                OPENAI_PUBLIC_CLIENT_ID,
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let (identity, access) = claims("acct-one", required_scopes());
        let first_id = signed("key-one", &identity);
        let first_access = signed("key-one", &access);
        let left_cancel = CancellationToken::new();
        let right_cancel = CancellationToken::new();
        let (left, right) = tokio::join!(
            verifier.verify(Some(&first_id), &first_access, &left_cancel),
            verifier.verify(Some(&first_id), &first_access, &right_cancel),
        );
        left.unwrap();
        right.unwrap();
        assert_eq!(server.state.jwks_count.load(AtomicOrdering::SeqCst), 1);

        verifier
            .verify(
                Some(&signed("key-two", &identity)),
                &signed("key-two", &access),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(server.state.jwks_count.load(AtomicOrdering::SeqCst), 2);

        {
            let mut cache = verifier.cache.lock().await;
            cache.expires_at = Some(tokio::time::Instant::now());
        }
        verifier
            .verify(
                Some(&signed("key-two", &identity)),
                &signed("key-two", &access),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(server.state.jwks_count.load(AtomicOrdering::SeqCst), 3);
        assert_eq!(cache_lifetime(Some("public, max-age=0")), Duration::ZERO);
    }

    #[tokio::test]
    async fn malformed_oversized_redirect_timeout_and_cancel_are_bounded_and_redacted() {
        let (identity, access) = claims("acct-one", required_scopes());
        for (defect, delay, redirect, oversized) in [
            ("redirect", Duration::ZERO, true, false),
            ("oversized", Duration::ZERO, false, true),
            ("timeout", Duration::from_millis(80), false, false),
        ] {
            let server =
                FixtureServer::start_with(vec![jwks("key-one")], delay, redirect, oversized).await;
            let verifier = OpenAiJwksVerifier::for_test(
                &server.origin,
                REQUIRED_TOKEN_ISSUER,
                OPENAI_PUBLIC_CLIENT_ID,
                Duration::from_millis(20),
            )
            .unwrap();
            let error = verifier
                .verify(
                    Some(&signed("key-one", &identity)),
                    &signed("key-one", &access),
                    &CancellationToken::new(),
                )
                .await
                .unwrap_err()
                .to_string();
            assert!(!error.contains("acct-one"), "{defect}: {error}");
        }

        let server = FixtureServer::start_with(
            vec![jwks("key-one")],
            Duration::from_millis(100),
            false,
            false,
        )
        .await;
        let verifier = OpenAiJwksVerifier::for_test(
            &server.origin,
            REQUIRED_TOKEN_ISSUER,
            OPENAI_PUBLIC_CLIENT_ID,
            Duration::from_secs(1),
        )
        .unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(verifier
            .verify(
                Some(&signed("key-one", &identity)),
                &signed("key-one", &access),
                &cancel,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
        assert_eq!(server.state.jwks_count.load(AtomicOrdering::SeqCst), 0);

        for malformed in ["", "a.b", "a.b.c.d", "eyJhbGciOiJub25lIn0.e30.x"] {
            assert!(verifier
                .verify(Some(malformed), malformed, &CancellationToken::new())
                .await
                .is_err());
        }

        for duplicate in [
            br#"{"kid":"one","kid":"two"}"#.as_slice(),
            br#"{"keys":[{"kid":"one","kid":"two"}]}"#.as_slice(),
            br#"{"iss":"one","nested":{"aud":"a","aud":"b"}}"#.as_slice(),
        ] {
            assert!(reject_duplicate_json_fields(duplicate, "hostile fixture").is_err());
        }
    }

    #[tokio::test]
    async fn discovery_cannot_proxy_or_substitute_the_pinned_jwks_authority() {
        let issuer_server = FixtureServer::start(vec![jwks("key-one")]).await;
        *issuer_server
            .state
            .discovery_issuer_override
            .lock()
            .unwrap() = Some(format!("{REQUIRED_TOKEN_ISSUER}/"));
        let issuer_verifier = OpenAiJwksVerifier::for_test(
            &issuer_server.origin,
            REQUIRED_TOKEN_ISSUER,
            OPENAI_PUBLIC_CLIENT_ID,
            Duration::from_secs(1),
        )
        .unwrap();
        let (identity, access) = claims("acct-one", required_scopes());
        assert!(issuer_verifier
            .verify(
                Some(&signed("key-one", &identity)),
                &signed("key-one", &access),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("changed issuer or JWKS authority"));
        assert_eq!(
            issuer_server.state.jwks_count.load(AtomicOrdering::SeqCst),
            0
        );

        let attacker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let attacker_url = format!("http://{}/jwks", attacker.local_addr().unwrap());
        let server = FixtureServer::start(vec![jwks("key-one")]).await;
        *server.state.discovery_jwks_override.lock().unwrap() = Some(attacker_url);
        let verifier = OpenAiJwksVerifier::for_test(
            &server.origin,
            REQUIRED_TOKEN_ISSUER,
            OPENAI_PUBLIC_CLIENT_ID,
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(verifier
            .verify(
                Some(&signed("key-one", &identity)),
                &signed("key-one", &access),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("changed issuer or JWKS authority"));
        assert_eq!(server.state.jwks_count.load(AtomicOrdering::SeqCst), 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), attacker.accept())
                .await
                .is_err()
        );
    }
}
