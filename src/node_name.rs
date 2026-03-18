/// Per-machine cute name — generated once, stored in `~/.finch/node_name`.
///
/// Format: adjective-animal  e.g. "tiny-bird", "warm-fox", "swift-moth"
/// Stable across restarts; advertised via mDNS so peers see a friendly label.

const ADJECTIVES: &[&str] = &[
    "tiny", "warm", "swift", "still", "pale",
    "deep", "soft", "wild", "bright", "slow",
    "thin", "blue", "grey", "gold", "dark",
    "free", "glad", "kind", "clear", "bold",
];

const ANIMALS: &[&str] = &[
    "bird", "fox", "moth", "hare", "fish",
    "bear", "swan", "hawk", "crab", "lark",
    "crane", "dove", "wolf", "deer", "wren",
    "bee",  "bat",  "cat",  "crow", "fawn",
];

/// Load the node name from `~/.finch/node_name`, creating it if absent.
pub fn load_or_create() -> String {
    if let Some(path) = name_path() {
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let n = existing.trim().to_string();
            if !n.is_empty() {
                return n;
            }
        }
        let name = generate();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let _ = std::fs::write(&path, &name);
        name
    } else {
        generate()
    }
}

fn name_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|mut p| {
        p.push(".finch");
        p.push("node_name");
        p
    })
}

/// Generate a name seeded from the machine hostname for determinism,
/// falling back to a time-seeded random if the hostname is unavailable.
fn generate() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let seed_str = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos().to_string())
                .unwrap_or_else(|_| "finch".to_string())
        });

    let mut h = DefaultHasher::new();
    seed_str.hash(&mut h);
    let v = h.finish();

    let adj   = ADJECTIVES[(v as usize) % ADJECTIVES.len()];
    let anim  = ANIMALS[((v >> 16) as usize) % ANIMALS.len()];
    format!("{adj}-{anim}")
}

/// This machine's name — loaded once at process start.
pub static NAME: std::sync::LazyLock<String> = std::sync::LazyLock::new(load_or_create);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_has_hyphen() {
        let n = generate();
        assert!(n.contains('-'), "name should be adj-animal: {n}");
    }

    #[test]
    fn test_generate_is_deterministic_for_same_seed() {
        // Two calls with the same hostname will produce the same output
        // (hostname is stable within a test run).
        let a = generate();
        let b = generate();
        assert_eq!(a, b);
    }

    #[test]
    fn test_name_parts_from_wordlists() {
        let n = generate();
        let parts: Vec<&str> = n.splitn(2, '-').collect();
        assert_eq!(parts.len(), 2);
        assert!(ADJECTIVES.contains(&parts[0]), "unknown adj: {}", parts[0]);
        assert!(ANIMALS.contains(&parts[1]), "unknown animal: {}", parts[1]);
    }
}
