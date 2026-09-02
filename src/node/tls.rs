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

fn provider_is_ring(provider: &rustls::crypto::CryptoProvider) -> bool {
    let ring = rustls::crypto::ring::default_provider();
    std::ptr::eq(provider.secure_random, ring.secure_random)
        && std::ptr::eq(provider.key_provider, ring.key_provider)
}

fn require_ring_provider(provider: &rustls::crypto::CryptoProvider, conflict: &str) -> Result<()> {
    anyhow::ensure!(
        provider_is_ring(provider),
        "Finch requires Rustls's ring crypto provider, but {conflict}"
    );
    Ok(())
}

/// Install Finch's process-wide Rustls provider, or reject an incompatible
/// provider that another TLS consumer installed first.
///
/// Finch enables only ring in its resolved TLS graph. Every TLS entry point
/// calls this before constructing a Rustls client or server config so another
/// library cannot make initialization order choose a provider implicitly.
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

/// Compatibility entry point for callers that initialized only Finch's TLS
/// server. New code should install the provider before any Rustls client or
/// server configuration is built.
#[deprecated(note = "use install_crypto_provider for every Rustls entry point")]
pub fn install_server_crypto_provider() -> Result<()> {
    install_crypto_provider()
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

    #[test]
    fn crypto_provider_classifier_rejects_non_ring_provider() {
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

        static NON_RING_RANDOM: NonRingRandom = NonRingRandom;
        let mut non_ring = rustls::crypto::ring::default_provider();
        non_ring.secure_random = &NON_RING_RANDOM;
        let error = require_ring_provider(&non_ring, "the non-ring test provider was selected")
            .expect_err("Finch must reject providers that are not its exact ring provider");
        assert_eq!(
            error.to_string(),
            "Finch requires Rustls's ring crypto provider, but the non-ring test provider was selected",
            "provider conflicts must name the required and conflicting providers"
        );
    }
}
