// Node Identity — persistent UUID for each finch instance.
//
// Every finch node gets a stable UUID written to ~/.finch/node_id on first
// run. This identity is used for:
//   - mDNS/network advertisement
//   - Work attribution in distributed mode
//   - Future: points and reputation on the worker network

use anyhow::{Context, Result};
use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const NODE_SIGNING_KEY_FILE: &str = "node-signing.key";

/// Persistent cryptographic identity for authenticating Finch transports.
/// This is intentionally separate from the human-readable/deterministic node
/// UUID: names and UUIDs locate a node, while this key proves possession.
#[derive(Clone)]
pub struct NodeSigningIdentity {
    signing_key: SigningKey,
}

impl NodeSigningIdentity {
    pub fn load_or_create(state_directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_directory)
            .with_context(|| format!("create {}", state_directory.display()))?;
        let lock_path = state_directory.join("node-signing.lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        lock.lock_exclusive()
            .with_context(|| format!("lock {}", lock_path.display()))?;
        let path = state_directory.join(NODE_SIGNING_KEY_FILE);
        let result = match std::fs::read(&path) {
            Ok(bytes) => {
                let secret: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
                    anyhow::anyhow!(
                        "node signing key {} has {} bytes, expected 32",
                        path.display(),
                        bytes.len()
                    )
                })?;
                protect_private_file(&path)?;
                Ok(Self::from_secret(secret))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut secret = [0_u8; 32];
                rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut secret);
                write_private_key(&path, &secret)?;
                Ok(Self::from_secret(secret))
            }
            Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
        };
        FileExt::unlock(&lock).with_context(|| format!("unlock {}", lock_path.display()))?;
        result
    }

    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret),
        }
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing_key.sign(message).to_bytes()
    }

    pub(crate) fn private_key_pkcs8_der(&self) -> Result<Vec<u8>> {
        Ok(self
            .signing_key
            .to_pkcs8_der()
            .context("encode node signing key as PKCS#8")?
            .as_bytes()
            .to_vec())
    }

    pub fn verify(public_key: [u8; 32], message: &[u8], signature: [u8; 64]) -> Result<()> {
        let key = VerifyingKey::from_bytes(&public_key).context("invalid node public key")?;
        let signature = ed25519_dalek::Signature::from_bytes(&signature);
        key.verify(message, &signature)
            .context("node signature does not match")
    }
}

fn write_private_key(path: &Path, secret: &[u8; 32]) -> Result<()> {
    let parent = path.parent().context("node signing key has no parent")?;
    let temporary = parent.join(format!(".node-signing.{}.tmp", Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("create {}", temporary.display()))?;
    std::io::Write::write_all(&mut file, secret)
        .with_context(|| format!("write {}", temporary.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("commit {}", path.display()))?;
    protect_private_file(path)
}

fn protect_private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("protect {}", path.display()))?;
    }
    Ok(())
}

/// A finch node's stable identity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeIdentity {
    /// Stable UUID — never changes after first run
    pub id: Uuid,
    /// Human-readable name (defaults to hostname, user-configurable)
    pub name: String,
    /// Finch version this node is running
    pub version: String,
}

impl NodeIdentity {
    /// Load existing identity or create one on first run.
    /// Persists to `~/.finch/node_id`.
    pub fn load_or_create() -> Result<Self> {
        let path = Self::path()?;

        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read node identity from {}", path.display()))?;
            let id: Self =
                serde_json::from_str(&raw).with_context(|| "Failed to parse node identity JSON")?;
            return Ok(id);
        }

        // First run — generate a new identity
        let identity = Self::generate()?;
        identity.save()?;
        tracing::info!(node_id = %identity.id, "Generated new node identity");
        Ok(identity)
    }

    fn generate() -> Result<Self> {
        // Use the stable cute name (e.g. "tiny-bird") for new nodes.
        // Existing nodes keep their persisted hostname-based name.
        let name = crate::node_name::NAME.clone();

        // UUID v5: deterministic from a finch-specific namespace + hostname.
        // Same machine always gets the same UUID, even across reinstalls.
        let id = Self::device_uuid(&name);
        Ok(Self {
            id,
            name,
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    /// Generate a deterministic UUID v5 for a device fingerprint string.
    /// Uses a fixed finch namespace so the ID is stable and globally unique.
    pub fn device_uuid(fingerprint: &str) -> Uuid {
        // Fixed namespace UUID for finch (generated once, never changes)
        const FINCH_NAMESPACE: Uuid = Uuid::from_bytes([
            0x6b, 0xa7, 0xb8, 0x14, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
            0x30, 0xc8,
        ]);
        Uuid::new_v5(&FINCH_NAMESPACE, fingerprint.as_bytes())
    }

    fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create ~/.finch directory")?;
        }
        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize node identity")?;
        std::fs::write(&path, json)
            .with_context(|| format!("Failed to write node identity to {}", path.display()))?;
        Ok(())
    }

    fn path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        Ok(home.join(".finch").join("node_id"))
    }

    /// Short display prefix (first 8 chars of UUID)
    pub fn short_id(&self) -> String {
        self.id.to_string()[..8].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_identity_roundtrip() {
        let original = NodeIdentity {
            id: Uuid::new_v4(),
            name: "test-node".to_string(),
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(original.id, parsed.id);
        assert_eq!(original.name, parsed.name);
    }

    #[test]
    fn test_short_id() {
        let id = NodeIdentity {
            id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            name: "test".to_string(),
            version: "0.1.0".to_string(),
        };
        assert_eq!(id.short_id(), "550e8400");
        assert_eq!(id.short_id().len(), 8);
    }

    #[test]
    fn test_device_uuid_is_deterministic() {
        // Same fingerprint must always produce the same UUID
        let u1 = NodeIdentity::device_uuid("my-laptop");
        let u2 = NodeIdentity::device_uuid("my-laptop");
        assert_eq!(u1, u2);
    }

    #[test]
    fn test_device_uuid_differs_for_different_fingerprints() {
        let u1 = NodeIdentity::device_uuid("machine-a");
        let u2 = NodeIdentity::device_uuid("machine-b");
        assert_ne!(u1, u2);
    }

    #[test]
    fn test_device_uuid_is_v5() {
        let u = NodeIdentity::device_uuid("any-host");
        // UUID v5 has version nibble = 5
        assert_eq!(u.get_version_num(), 5);
    }

    #[test]
    fn test_short_id_is_always_8_chars() {
        let id = NodeIdentity {
            id: NodeIdentity::device_uuid("some-host"),
            name: "n".to_string(),
            version: "0.1.0".to_string(),
        };
        assert_eq!(id.short_id().len(), 8);
    }

    #[test]
    fn test_identity_version_preserved_in_roundtrip() {
        let original = NodeIdentity {
            id: Uuid::new_v4(),
            name: "x".to_string(),
            version: "1.2.3".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, "1.2.3");
    }

    #[test]
    fn test_identity_name_preserved_in_roundtrip() {
        let original = NodeIdentity {
            id: Uuid::new_v4(),
            name: "my-dev-machine".to_string(),
            version: "0.5.0".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "my-dev-machine");
    }

    #[test]
    fn signing_identity_survives_restart_and_rejects_other_keys() {
        let temp = TempDir::new().unwrap();
        let first = NodeSigningIdentity::load_or_create(temp.path()).unwrap();
        let public_key = first.public_key_bytes();
        let signature = first.sign(b"brain transport transcript");
        NodeSigningIdentity::verify(public_key, b"brain transport transcript", signature).unwrap();

        let restarted = NodeSigningIdentity::load_or_create(temp.path()).unwrap();
        assert_eq!(restarted.public_key_bytes(), public_key);
        assert_eq!(restarted.sign(b"brain transport transcript"), signature);

        let other = NodeSigningIdentity::from_secret([9; 32]);
        assert!(NodeSigningIdentity::verify(
            other.public_key_bytes(),
            b"brain transport transcript",
            signature,
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn signing_identity_private_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        NodeSigningIdentity::load_or_create(temp.path()).unwrap();
        let mode = std::fs::metadata(temp.path().join(NODE_SIGNING_KEY_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn concurrent_first_start_converges_on_one_signing_identity() {
        let temp = TempDir::new().unwrap();
        let path = std::sync::Arc::new(temp.path().to_path_buf());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(9));
        let workers = (0..8)
            .map(|_| {
                let path = std::sync::Arc::clone(&path);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    NodeSigningIdentity::load_or_create(&path)
                        .unwrap()
                        .public_key_bytes()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let keys = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(keys.len(), 1);
    }
}
