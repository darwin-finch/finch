//! TLS material derived from the persistent cryptographic node identity.
//!
//! Invitations authenticate the Ed25519 node key. Reusing that key in the
//! node's self-signed certificate means there is only one remote identity to
//! pin, rather than an unrelated invitation key and TLS key.

use anyhow::{Context, Result};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};

use super::identity::NodeSigningIdentity;

#[derive(Clone)]
pub struct NodeTlsIdentity {
    certificate_der: Vec<u8>,
    private_key_der: Vec<u8>,
}

impl NodeTlsIdentity {
    pub fn from_signing_identity(identity: &NodeSigningIdentity, hostname: &str) -> Result<Self> {
        Self::from_signing_identity_with_params(identity, hostname, None)
    }

    #[cfg(test)]
    pub(crate) fn from_signing_identity_with_validity(
        identity: &NodeSigningIdentity,
        hostname: &str,
        not_before: (i32, u8, u8),
        not_after: (i32, u8, u8),
    ) -> Result<Self> {
        Self::from_signing_identity_with_params(identity, hostname, Some((not_before, not_after)))
    }

    fn from_signing_identity_with_params(
        identity: &NodeSigningIdentity,
        hostname: &str,
        validity: Option<((i32, u8, u8), (i32, u8, u8))>,
    ) -> Result<Self> {
        let private_key_der = identity.private_key_pkcs8_der()?;
        let key_pair = KeyPair::try_from(private_key_der.as_slice())
            .context("load node signing key for TLS")?;
        anyhow::ensure!(
            key_pair.public_key_raw() == identity.public_key_bytes(),
            "TLS certificate key does not match the node signing identity"
        );

        let mut names = vec!["localhost".to_string(), "finch.invalid".to_string()];
        let hostname = hostname.trim();
        if !hostname.is_empty() {
            names.push(hostname.to_string());
            if !hostname.contains('.') {
                names.push(format!("{hostname}.local"));
            }
        }
        names.sort();
        names.dedup();

        let mut params = CertificateParams::new(names).context("create node TLS certificate")?;
        if let Some((not_before, not_after)) = validity {
            params.not_before = rcgen::date_time_ymd(not_before.0, not_before.1, not_before.2);
            params.not_after = rcgen::date_time_ymd(not_after.0, not_after.1, not_after.2);
        }
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, "Finch Brain node");
        params.distinguished_name = distinguished_name;
        let certificate = params
            .self_signed(&key_pair)
            .context("sign node TLS certificate")?;

        Ok(Self {
            certificate_der: certificate.der().to_vec(),
            private_key_der,
        })
    }

    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }

    pub(crate) fn private_key_der(&self) -> &[u8] {
        &self.private_key_der
    }
}

fn signature_algorithms_match(
    left: rustls::crypto::WebPkiSupportedAlgorithms,
    right: rustls::crypto::WebPkiSupportedAlgorithms,
) -> bool {
    let all_match = left.all.len() == right.all.len()
        && left
            .all
            .iter()
            .zip(right.all)
            .all(|(left, right)| std::ptr::eq(*left, *right));
    let mappings_match = left.mapping.len() == right.mapping.len()
        && left.mapping.iter().zip(right.mapping).all(
            |((left_scheme, left_algorithms), (right_scheme, right_algorithms))| {
                left_scheme == right_scheme
                    && left_algorithms.len() == right_algorithms.len()
                    && left_algorithms
                        .iter()
                        .zip(*right_algorithms)
                        .all(|(left, right)| std::ptr::eq(*left, *right))
            },
        );
    all_match && mappings_match
}

fn ring_provider_mismatch(provider: &rustls::crypto::CryptoProvider) -> Option<&'static str> {
    let ring = rustls::crypto::ring::default_provider();
    if provider.cipher_suites != ring.cipher_suites {
        return Some("its cipher suites differ from Finch's ring provider");
    }
    if provider.kx_groups.len() != ring.kx_groups.len()
        || !provider
            .kx_groups
            .iter()
            .zip(ring.kx_groups)
            .all(|(left, right)| std::ptr::eq(*left, right))
    {
        return Some("its key-exchange groups differ from Finch's ring provider");
    }
    if !signature_algorithms_match(
        provider.signature_verification_algorithms,
        ring.signature_verification_algorithms,
    ) {
        return Some("its signature algorithms differ from Finch's ring provider");
    }
    if !std::ptr::eq(provider.secure_random, ring.secure_random) {
        return Some("its secure-random implementation differs from Finch's ring provider");
    }
    if !std::ptr::eq(provider.key_provider, ring.key_provider) {
        return Some("its key provider differs from Finch's ring provider");
    }
    None
}

#[cfg(test)]
fn provider_is_ring(provider: &rustls::crypto::CryptoProvider) -> bool {
    ring_provider_mismatch(provider).is_none()
}

fn require_ring_provider(provider: &rustls::crypto::CryptoProvider, conflict: &str) -> Result<()> {
    if let Some(mismatch) = ring_provider_mismatch(provider) {
        anyhow::bail!(
            "Finch requires Rustls's exact ring crypto provider, but {conflict}: {mismatch}"
        );
    }
    Ok(())
}

/// Install Finch's process-wide Rustls provider, or reject an incompatible
/// provider that another TLS consumer installed first.
///
/// Finch enables only ring in its resolved TLS graph. Every TLS entry point
/// calls this before constructing a Rustls client or server config so another
/// library cannot make initialization order choose a provider implicitly.
///
/// This replaces the former `install_server_crypto_provider() -> ()` API.
/// Callers must propagate this function's result because a provider conflict
/// makes continued TLS configuration unsafe.
pub fn install_crypto_provider() -> Result<()> {
    if let Some(installed) = rustls::crypto::CryptoProvider::get_default() {
        return require_ring_provider(installed, "another provider is already installed");
    }

    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_ok()
    {
        return Ok(());
    }

    let installed = rustls::crypto::CryptoProvider::get_default()
        .context("Rustls crypto-provider installation raced without selecting a provider")?;
    require_ring_provider(installed, "another provider won initialization")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_is_stable_and_uses_the_node_key() {
        let node = NodeSigningIdentity::from_secret([17; 32]);
        let first = NodeTlsIdentity::from_signing_identity(&node, "workstation").unwrap();
        let second = NodeTlsIdentity::from_signing_identity(&node, "workstation").unwrap();

        assert_eq!(first.certificate_der(), second.certificate_der());
        assert_eq!(first.private_key_der(), second.private_key_der());

        let key_pair = KeyPair::try_from(first.private_key_der()).unwrap();
        assert_eq!(key_pair.public_key_raw(), node.public_key_bytes());
    }

    #[test]
    fn certificate_changes_with_the_node_identity() {
        let first = NodeSigningIdentity::from_secret([17; 32]);
        let second = NodeSigningIdentity::from_secret([18; 32]);

        assert_ne!(
            NodeTlsIdentity::from_signing_identity(&first, "workstation")
                .unwrap()
                .certificate_der(),
            NodeTlsIdentity::from_signing_identity(&second, "workstation")
                .unwrap()
                .certificate_der(),
        );
    }

    #[test]
    fn crypto_provider_installation_is_idempotent_and_ring_backed() {
        install_crypto_provider().expect("Finch must install the ring provider");
        let first = rustls::crypto::CryptoProvider::get_default()
            .expect("Rustls must retain Finch's installed provider");
        assert!(
            provider_is_ring(first),
            "Finch must never silently select a non-ring provider"
        );

        install_crypto_provider().expect("reinstalling Finch's ring provider must be idempotent");
        let second = rustls::crypto::CryptoProvider::get_default()
            .expect("Rustls must retain Finch's installed provider");
        assert!(
            std::ptr::eq(first.as_ref(), second.as_ref()),
            "idempotent initialization must preserve the process-wide provider"
        );
    }

    fn assert_provider_rejected(provider: &rustls::crypto::CryptoProvider, diagnostic: &str) {
        let error = require_ring_provider(provider, "the modified test provider was selected")
            .expect_err("Finch must reject providers that are not its exact ring provider");
        assert!(
            error.to_string().contains(diagnostic),
            "provider conflict diagnostic {:?} must name the mismatched component {diagnostic:?}",
            error.to_string()
        );
    }

    #[test]
    fn crypto_provider_classifier_rejects_every_modified_ring_component() {
        #[derive(Debug)]
        struct NonRingRandom;

        impl rustls::crypto::SecureRandom for NonRingRandom {
            fn fill(
                &self,
                _buffer: &mut [u8],
            ) -> std::result::Result<(), rustls::crypto::GetRandomFailed> {
                Err(rustls::crypto::GetRandomFailed)
            }
        }

        #[derive(Debug)]
        struct NonRingKeyProvider;

        impl rustls::crypto::KeyProvider for NonRingKeyProvider {
            fn load_private_key(
                &self,
                _key_der: rustls::pki_types::PrivateKeyDer<'static>,
            ) -> std::result::Result<std::sync::Arc<dyn rustls::sign::SigningKey>, rustls::Error>
            {
                Err(rustls::Error::General(
                    "non-ring test key provider".to_string(),
                ))
            }
        }

        static NON_RING_RANDOM: NonRingRandom = NonRingRandom;
        static NON_RING_KEY_PROVIDER: NonRingKeyProvider = NonRingKeyProvider;
        let ring = rustls::crypto::ring::default_provider();

        let mut modified = ring.clone();
        modified.cipher_suites.clear();
        assert_provider_rejected(&modified, "cipher suites");

        let mut modified = ring.clone();
        modified.kx_groups.clear();
        assert_provider_rejected(&modified, "key-exchange groups");

        let mut modified = ring.clone();
        modified.signature_verification_algorithms.all =
            &ring.signature_verification_algorithms.all[1..];
        assert_provider_rejected(&modified, "signature algorithms");

        let mut modified = ring.clone();
        modified.signature_verification_algorithms.mapping =
            &ring.signature_verification_algorithms.mapping[1..];
        assert_provider_rejected(&modified, "signature algorithms");

        let mut modified = ring.clone();
        modified.secure_random = &NON_RING_RANDOM;
        assert_provider_rejected(&modified, "secure-random implementation");

        let mut modified = ring.clone();
        modified.key_provider = &NON_RING_KEY_PROVIDER;
        assert_provider_rejected(&modified, "key provider");
    }
}
