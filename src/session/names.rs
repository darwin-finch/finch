/// Session name generator — adjective + landscape noun, e.g. "quiet-hill", "silver-lake".
///
/// Distinct word lists from node_name.rs so session names feel different from node names.
/// Landscape nouns give a sense of place; good mnemonic for "where are we talking".
use uuid::Uuid;

const ADJECTIVES: &[&str] = &[
    "quiet", "silver", "golden", "amber", "copper", "misty", "hollow", "bright", "gentle", "wild",
    "steep", "deep", "still", "stone", "ash", "pale", "lone", "slow", "dark", "clear",
];

const NOUNS: &[&str] = &[
    "hill", "lake", "path", "cave", "cliff", "grove", "marsh", "peak", "reef", "vale", "ford",
    "moor", "glen", "cove", "dune", "ridge", "crest", "brook", "field", "shore",
];

/// Namespace UUID for Finch session names (distinct from node namespace).
/// Generated once; baked in so name → UUID is always deterministic.
const SESSION_NS: Uuid = uuid::uuid!("a1b2c3d4-e5f6-7890-abcd-ef1234567890");

/// Generate a random-ish cute session name: "quiet-hill", "silver-lake", etc.
///
/// Uses thread-local randomness via rand.  Not deterministic — call `to_uuid`
/// to get the stable UUID for a known name.
pub fn generate() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Seed from time + process id so two concurrent sessions get different names.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        ^ (std::process::id() as u128);

    let mut h = DefaultHasher::new();
    seed.hash(&mut h);
    let v = h.finish();

    let adj = ADJECTIVES[(v as usize) % ADJECTIVES.len()];
    let noun = NOUNS[((v >> 16) as usize) % NOUNS.len()];
    format!("{adj}-{noun}")
}

/// Derive a stable UUIDv5 from a session name.
///
/// Two callers who agree on the same name will always get the same UUID, regardless
/// of machine or time.  This is the share-able session ID.
pub fn to_uuid(name: &str) -> Uuid {
    Uuid::new_v5(&SESSION_NS, name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_has_hyphen() {
        let name = generate();
        assert!(name.contains('-'), "expected adj-noun, got: {name}");
    }

    #[test]
    fn generate_parts_from_wordlists() {
        let name = generate();
        let parts: Vec<&str> = name.splitn(2, '-').collect();
        assert_eq!(parts.len(), 2);
        assert!(ADJECTIVES.contains(&parts[0]), "unknown adj: {}", parts[0]);
        assert!(NOUNS.contains(&parts[1]), "unknown noun: {}", parts[1]);
    }

    #[test]
    fn to_uuid_is_deterministic() {
        let a = to_uuid("quiet-hill");
        let b = to_uuid("quiet-hill");
        assert_eq!(a, b);
    }

    #[test]
    fn different_names_yield_different_uuids() {
        let a = to_uuid("quiet-hill");
        let b = to_uuid("silver-lake");
        assert_ne!(a, b);
    }

    #[test]
    fn uuid_is_version_5() {
        let id = to_uuid("golden-path");
        assert_eq!(id.get_version_num(), 5);
    }
}
