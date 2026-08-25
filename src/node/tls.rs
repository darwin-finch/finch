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
}
