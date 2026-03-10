/// Peer registry — machines check in, you query who's alive.
///
/// Any finch daemon can act as a registry.  Machines POST to
/// `/v1/registry/join` on startup and periodically to stay alive.
/// Entries expire after 90 seconds of silence.
///
/// The registry is intentionally simple:
///   - No persistence (machines re-register on restart)
///   - No authentication (run behind a firewall or VPN)
///   - No leader election (point all machines at one registry daemon)
///
/// At 100,000 machines you'd shard this.  Right now one daemon handles it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// How long a machine can be silent before we consider it dead.
const EXPIRY: Duration = Duration::from_secs(90);

/// Default debt threshold: 30 seconds of compute consumed without contributing.
pub const DEFAULT_DEBT_THRESHOLD_MS: i64 = 30_000;

/// What a machine tells the registry about itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerEntry {
    /// Reachable address for `/v1/forth/eval` — host:port or http://host:port.
    pub addr: String,
    /// Human-readable label (e.g. "build-box", "gpu-3").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Arbitrary tags (e.g. ["gpu", "us-west", "16gb"]).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Load estimate 0.0–1.0 (0 = idle, 1 = saturated).  Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<f32>,
    /// Region string (e.g. "us-west-2", "eu-central").  Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Number of logical CPU cores reported by the OS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    /// Total RAM in megabytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_mb: Option<u64>,
    /// Benchmark score: milliseconds to do 10M additions.
    /// Lower is faster.  None = not measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bench_ms: Option<u64>,
}

/// Compute ledger entry for one machine — tracks work done vs. work consumed.
///
/// Unit: milliseconds of wall-clock execution time.
/// Positive balance means the cluster owes this machine.
/// Negative balance means this machine owes the cluster.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LedgerEntry {
    /// Milliseconds of compute this machine has performed for others.
    pub credits_ms: u64,
    /// Milliseconds of compute this machine has consumed from others.
    pub debits_ms: u64,
}

impl LedgerEntry {
    /// Net balance: positive = owed to this machine, negative = this machine owes.
    pub fn balance_ms(&self) -> i64 {
        self.credits_ms as i64 - self.debits_ms as i64
    }
}

/// What the registry stores internally per entry.
struct LiveEntry {
    entry:     PeerEntry,
    last_seen: Instant,
    ledger:    LedgerEntry,
}

/// The registry — a shared, in-memory map of addr → live entry.
#[derive(Clone)]
pub struct Registry {
    entries: Arc<RwLock<HashMap<String, LiveEntry>>>,
    /// Debt threshold in ms.  When a machine's balance drops below -threshold,
    /// it is flagged and warned on the next eval response.
    pub debt_threshold_ms: i64,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            debt_threshold_ms: DEFAULT_DEBT_THRESHOLD_MS,
        }
    }

    pub fn with_debt_threshold(mut self, threshold_ms: i64) -> Self {
        self.debt_threshold_ms = threshold_ms;
        self
    }

    /// Register or refresh a peer.  Returns the canonical addr (unchanged).
    pub async fn join(&self, entry: PeerEntry) -> String {
        let addr = entry.addr.clone();
        let mut map = self.entries.write().await;
        // Preserve existing ledger on re-join.
        let ledger = map.get(&addr).map(|e| e.ledger.clone()).unwrap_or_default();
        map.insert(addr.clone(), LiveEntry { entry, last_seen: Instant::now(), ledger });
        addr
    }

    /// Record compute performed BY `addr` for someone else (credit).
    pub async fn credit(&self, addr: &str, compute_ms: u64) {
        let mut map = self.entries.write().await;
        if let Some(e) = map.get_mut(addr) {
            e.ledger.credits_ms = e.ledger.credits_ms.saturating_add(compute_ms);
        }
    }

    /// Record compute consumed FROM `addr` by the local machine (debit).
    /// Returns the new balance and whether the debt threshold was just crossed.
    pub async fn debit(&self, addr: &str, compute_ms: u64) -> (i64, bool) {
        let mut map = self.entries.write().await;
        if let Some(e) = map.get_mut(addr) {
            let before = e.ledger.balance_ms();
            e.ledger.debits_ms = e.ledger.debits_ms.saturating_add(compute_ms);
            let after = e.ledger.balance_ms();
            let crossed = before > -self.debt_threshold_ms && after <= -self.debt_threshold_ms;
            (after, crossed)
        } else {
            (0, false)
        }
    }

    /// Check whether `addr` is currently over the debt threshold.
    pub async fn is_in_debt(&self, addr: &str) -> bool {
        let map = self.entries.read().await;
        map.get(addr)
            .map(|e| e.ledger.balance_ms() <= -self.debt_threshold_ms)
            .unwrap_or(false)
    }

    /// Return the ledger for a peer, or None if not registered.
    pub async fn ledger(&self, addr: &str) -> Option<LedgerEntry> {
        self.entries.read().await.get(addr).map(|e| e.ledger.clone())
    }

    /// Clear the ledger for `addr` — called when a settlement is accepted.
    pub async fn settle(&self, addr: &str) {
        let mut map = self.entries.write().await;
        if let Some(e) = map.get_mut(addr) {
            e.ledger = LedgerEntry::default();
        }
    }

    /// Return ledger entries for all live peers.
    pub async fn all_ledgers(&self) -> Vec<(String, LedgerEntry)> {
        let map = self.entries.read().await;
        let now = Instant::now();
        map.iter()
            .filter(|(_, e)| now.duration_since(e.last_seen) < EXPIRY)
            .map(|(addr, e)| (addr.clone(), e.ledger.clone()))
            .collect()
    }

    /// Remove a peer immediately.
    pub async fn leave(&self, addr: &str) {
        self.entries.write().await.remove(addr);
    }

    /// Refresh the last-seen timestamp for an existing peer.
    /// No-op if the peer isn't registered (it should call join instead).
    pub async fn heartbeat(&self, addr: &str) {
        let mut map = self.entries.write().await;
        if let Some(e) = map.get_mut(addr) {
            e.last_seen = Instant::now();
        }
    }

    /// List live peers, optionally filtered by tag and/or region.
    pub async fn peers(&self, tag: Option<&str>, region: Option<&str>) -> Vec<PeerEntry> {
        let map = self.entries.read().await;
        let now = Instant::now();
        map.values()
            .filter(|e| now.duration_since(e.last_seen) < EXPIRY)
            .filter(|e| tag.map_or(true, |t| e.entry.tags.iter().any(|x| x == t)))
            .filter(|e| region.map_or(true, |r| e.entry.region.as_deref() == Some(r)))
            .map(|e| e.entry.clone())
            .collect()
    }

    /// Drop entries that haven't been seen recently.  Call periodically.
    pub async fn expire(&self) {
        let now = Instant::now();
        self.entries.write().await.retain(|_, e| {
            now.duration_since(e.last_seen) < EXPIRY
        });
    }

    /// How many live peers are registered right now.
    pub async fn count(&self) -> usize {
        let map = self.entries.read().await;
        let now = Instant::now();
        map.values()
            .filter(|e| now.duration_since(e.last_seen) < EXPIRY)
            .count()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(addr: &str, tags: &[&str]) -> PeerEntry {
        PeerEntry {
            addr:      addr.to_string(),
            label:     None,
            tags:      tags.iter().map(|s| s.to_string()).collect(),
            load:      None,
            region:    None,
            cpu_cores: None,
            ram_mb:    None,
            bench_ms:  None,
        }
    }

    #[tokio::test]
    async fn test_join_and_list() {
        let r = Registry::new();
        r.join(peer("a:1234", &["gpu"])).await;
        r.join(peer("b:1234", &["cpu"])).await;
        let all = r.peers(None, None).await;
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_filter_by_tag() {
        let r = Registry::new();
        r.join(peer("a:1234", &["gpu"])).await;
        r.join(peer("b:1234", &["cpu"])).await;
        let gpu = r.peers(Some("gpu"), None).await;
        assert_eq!(gpu.len(), 1);
        assert_eq!(gpu[0].addr, "a:1234");
    }

    #[tokio::test]
    async fn test_leave_removes_peer() {
        let r = Registry::new();
        r.join(peer("a:1234", &[])).await;
        assert_eq!(r.count().await, 1);
        r.leave("a:1234").await;
        assert_eq!(r.count().await, 0);
    }

    #[tokio::test]
    async fn test_rejoin_refreshes_entry() {
        let r = Registry::new();
        r.join(peer("a:1234", &["old"])).await;
        r.join(peer("a:1234", &["new"])).await; // re-join with new tags
        let all = r.peers(None, None).await;
        assert_eq!(all.len(), 1);
        assert!(all[0].tags.contains(&"new".to_string()));
    }

    #[tokio::test]
    async fn test_expiry() {
        // Manually insert an expired entry by poking last_seen in the past.
        let r = Registry::new();
        {
            let mut map = r.entries.write().await;
            map.insert("dead:1234".to_string(), super::LiveEntry {
                entry:     peer("dead:1234", &[]),
                last_seen: Instant::now() - EXPIRY - Duration::from_secs(1),
                ledger:    LedgerEntry::default(),
            });
        }
        assert_eq!(r.count().await, 0, "expired entry should not count");
        r.expire().await;
        assert_eq!(r.peers(None, None).await.len(), 0);
    }

    // ── Hardware spec fields ───────────────────────────────────────────────

    fn peer_with_hw(addr: &str, cpu: u32, ram: u64, bench: u64) -> PeerEntry {
        PeerEntry {
            addr:      addr.to_string(),
            label:     None,
            tags:      vec![],
            load:      None,
            region:    None,
            cpu_cores: Some(cpu),
            ram_mb:    Some(ram),
            bench_ms:  Some(bench),
        }
    }

    #[tokio::test]
    async fn test_peer_entry_hw_fields_round_trip() {
        // Hardware fields survive a join → peers() round trip.
        let r = Registry::new();
        r.join(peer_with_hw("a:1234", 8, 16_384, 42)).await;
        let all = r.peers(None, None).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].cpu_cores, Some(8));
        assert_eq!(all[0].ram_mb,    Some(16_384));
        assert_eq!(all[0].bench_ms,  Some(42));
    }

    #[tokio::test]
    async fn test_peer_entry_hw_fields_optional() {
        // A peer without hardware fields is still valid.
        let r = Registry::new();
        r.join(peer("a:1234", &[])).await;
        let all = r.peers(None, None).await;
        assert_eq!(all[0].cpu_cores, None);
        assert_eq!(all[0].ram_mb,    None);
        assert_eq!(all[0].bench_ms,  None);
    }

    #[tokio::test]
    async fn test_rejoin_preserves_hw_fields() {
        // Re-joining with updated hw fields replaces them.
        let r = Registry::new();
        r.join(peer_with_hw("a:1234", 4, 8_192, 100)).await;
        r.join(peer_with_hw("a:1234", 8, 16_384, 50)).await;
        let all = r.peers(None, None).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].cpu_cores, Some(8));
        assert_eq!(all[0].bench_ms,  Some(50));
    }

    #[tokio::test]
    async fn test_peer_entry_hw_serde_round_trip() {
        // Fields survive JSON serialisation and deserialisation.
        let p = peer_with_hw("x:9999", 4, 4_096, 77);
        let json = serde_json::to_string(&p).unwrap();
        let back: PeerEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cpu_cores, Some(4));
        assert_eq!(back.ram_mb,    Some(4_096));
        assert_eq!(back.bench_ms,  Some(77));
    }

    #[tokio::test]
    async fn test_peer_entry_hw_serde_optional_skipped() {
        // None fields are skipped in JSON (skip_serializing_if).
        let p = peer("a:1234", &[]);
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("cpu_cores"));
        assert!(!json.contains("ram_mb"));
        assert!(!json.contains("bench_ms"));
    }
}
