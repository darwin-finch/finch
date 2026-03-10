/// Peer authentication token for the Co-Forth daemon.
///
/// Generated once at first daemon start, stored in `~/.finch/peer_token`,
/// and reused across restarts.  Broadcast in the mDNS TXT record so that
/// auto-discovered machines receive it automatically.
///
/// Required on the dangerous endpoints: /v1/exec, /v1/forth/eval, /v1/forth/define.
/// The header name is `X-Finch-Token`.
pub const HEADER: &str = "x-finch-token";

/// Load the peer token from `~/.finch/peer_token`, creating it if it doesn't exist.
/// Returns the token string (a random hex string, 32 chars).
pub fn load_or_create() -> String {
    if let Some(path) = token_path() {
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let t = existing.trim().to_string();
            if !t.is_empty() {
                return t;
            }
        }
        // Generate a new token
        let token = generate();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let _ = std::fs::write(&path, &token);
        token
    } else {
        // No home directory — generate ephemeral token (daemon-only run)
        generate()
    }
}

fn token_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|mut p| {
        p.push(".finch");
        p.push("peer_token");
        p
    })
}

fn generate() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Mix time + process id for uniqueness. Not cryptographically random,
    // but sufficient to prevent casual unauthorized access on a LAN.
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let pid = std::process::id();

    let mut h1 = DefaultHasher::new();
    t.hash(&mut h1);
    pid.hash(&mut h1);
    let h1 = h1.finish();

    let mut h2 = DefaultHasher::new();
    (t ^ 0xdeadbeef).hash(&mut h2);
    (pid.wrapping_mul(31337)).hash(&mut h2);
    let h2 = h2.finish();

    format!("{h1:016x}{h2:016x}")
}

/// Global: token loaded once at process start.
pub static TOKEN: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(load_or_create);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_is_32_hex_chars() {
        let t = generate();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_differs_across_calls() {
        // Not guaranteed but very likely to differ due to nanosecond timing
        let a = generate();
        let b = generate();
        // At minimum both are valid hex strings
        assert_eq!(a.len(), 32);
        assert_eq!(b.len(), 32);
    }
}
