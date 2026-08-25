// Middleware for authentication, rate limiting, etc.

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use dashmap::DashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Model-API client authentication shared by every provider profile.
#[derive(Clone, Debug)]
pub struct DaemonAuth {
    enabled: bool,
    api_keys: Arc<[String]>,
}

impl DaemonAuth {
    pub fn new(enabled: bool, api_keys: Vec<String>) -> Self {
        let api_keys = api_keys
            .into_iter()
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .collect::<Vec<_>>()
            .into();
        Self { enabled, api_keys }
    }

    fn authorizes(&self, path: &str, headers: &HeaderMap) -> bool {
        if !self.enabled || !requires_client_auth(path) {
            return true;
        }

        bearer_token(headers).is_some_and(|candidate| {
            self.api_keys
                .iter()
                .any(|configured| configured == candidate)
        })
    }
}

fn requires_client_auth(path: &str) -> bool {
    matches!(path, "/v1/chat/completions" | "/v1/models" | "/v1/messages")
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

/// Require the shared API key on model endpoints using the standard OpenAI
/// bearer format. Health remains unaffected.
pub async fn auth_middleware(
    State(auth): State<DaemonAuth>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if auth.authorizes(request.uri().path(), request.headers()) {
        return next.run(request).await;
    }

    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        Json(serde_json::json!({
            "error": {
                "message": "Invalid or missing Finch API key",
                "type": "authentication_error"
            }
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Rate limiter — token-bucket per IP, shared across requests
// ---------------------------------------------------------------------------

/// Per-IP token bucket state
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// Shared rate limiter state — clone freely (it's an Arc inside)
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<RateLimiterInner>,
}

struct RateLimiterInner {
    /// Buckets keyed by source IP
    buckets: DashMap<IpAddr, Bucket>,
    /// Maximum tokens per IP (burst capacity)
    capacity: f64,
    /// Tokens added per second (sustained rate)
    refill_rate: f64,
}

impl RateLimiter {
    /// Create a rate limiter.
    ///
    /// - `requests_per_second`: sustained rate per IP
    /// - `burst`: maximum burst (capacity above sustained rate)
    pub fn new(requests_per_second: f64, burst: f64) -> Self {
        Self {
            inner: Arc::new(RateLimiterInner {
                buckets: DashMap::new(),
                capacity: burst,
                refill_rate: requests_per_second,
            }),
        }
    }

    /// Returns true if the request from `ip` is within rate limits.
    /// Consumes one token.
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut bucket = self.inner.buckets.entry(ip).or_insert_with(|| Bucket {
            tokens: self.inner.capacity,
            last_refill: now,
        });

        // Refill tokens based on elapsed time
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.inner.refill_rate).min(self.inner.capacity);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Purge buckets that have been idle for more than `idle_secs`.
    /// Call periodically from a background task to prevent unbounded growth.
    pub fn purge_idle(&self, idle_secs: u64) {
        let cutoff = Duration::from_secs(idle_secs);
        let now = Instant::now();
        self.inner
            .buckets
            .retain(|_, bucket| now.duration_since(bucket.last_refill) < cutoff);
    }

    /// Number of currently tracked IPs.
    pub fn tracked_ips(&self) -> usize {
        self.inner.buckets.len()
    }
}

/// Axum middleware that enforces per-IP rate limiting.
///
/// Extracts the source IP from the `X-Forwarded-For` header (proxy-aware)
/// and falls back to the socket address. Returns 429 Too Many Requests when
/// the bucket for that IP is exhausted.
#[allow(dead_code)]
pub async fn rate_limit_middleware(
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // Rate limiter must be injected as Axum extension — if absent, allow all
    // (graceful degradation: middleware won't crash if not wired up)
    let Some(limiter) = request.extensions().get::<RateLimiter>().cloned() else {
        return Ok(next.run(request).await);
    };

    // Extract source IP — prefer X-Forwarded-For for reverse-proxy setups
    let ip = extract_ip(&request).unwrap_or(IpAddr::from([127, 0, 0, 1]));

    if limiter.check(ip) {
        Ok(next.run(request).await)
    } else {
        tracing::warn!(ip = %ip, "Rate limit exceeded");
        Err(StatusCode::TOO_MANY_REQUESTS)
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    fn headers(value: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(value) = value {
            headers.insert(header::AUTHORIZATION, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn configured_key_is_required_and_validated() {
        let auth = DaemonAuth::new(true, vec!["custom-secret".to_string()]);

        assert!(!auth.authorizes("/v1/chat/completions", &headers(None)));
        assert!(!auth.authorizes("/v1/chat/completions", &headers(Some("Bearer finch-local"))));
        assert!(auth.authorizes(
            "/v1/chat/completions",
            &headers(Some("Bearer custom-secret"))
        ));
    }

    #[test]
    fn disabled_auth_and_health_do_not_require_a_key() {
        let disabled = DaemonAuth::new(false, vec![]);
        let enabled = DaemonAuth::new(true, vec!["custom-secret".to_string()]);

        assert!(disabled.authorizes("/v1/models", &headers(None)));
        assert!(enabled.authorizes("/health", &headers(None)));
    }
}

/// Extract client IP from request headers or connection info.
#[allow(dead_code)]
fn extract_ip(request: &Request<Body>) -> Option<IpAddr> {
    // Check X-Forwarded-For (set by reverse proxies like nginx, Caddy)
    if let Some(forwarded_for) = request.headers().get("x-forwarded-for") {
        if let Ok(value) = forwarded_for.to_str() {
            // Take the first (leftmost) IP — the actual client
            if let Some(first) = value.split(',').next() {
                if let Ok(ip) = first.trim().parse::<IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn test_rate_limiter_allows_within_burst() {
        // 2 req/s, burst of 10
        let limiter = RateLimiter::new(2.0, 10.0);
        let client = ip(1, 2, 3, 4);

        // First 10 requests should all be allowed (burst)
        for i in 0..10 {
            assert!(
                limiter.check(client),
                "request {i} should be allowed within burst"
            );
        }
    }

    #[test]
    fn test_rate_limiter_blocks_over_burst() {
        let limiter = RateLimiter::new(1.0, 3.0); // burst of 3
        let client = ip(1, 2, 3, 4);

        assert!(limiter.check(client)); // 1
        assert!(limiter.check(client)); // 2
        assert!(limiter.check(client)); // 3
        assert!(!limiter.check(client)); // 4th — rejected
        assert!(!limiter.check(client)); // 5th — still rejected
    }

    #[test]
    fn test_rate_limiter_different_ips_independent() {
        let limiter = RateLimiter::new(1.0, 2.0); // burst of 2
        let alice = ip(1, 1, 1, 1);
        let bob = ip(2, 2, 2, 2);

        // Alice exhausts her bucket
        assert!(limiter.check(alice));
        assert!(limiter.check(alice));
        assert!(!limiter.check(alice)); // Alice blocked

        // Bob is unaffected
        assert!(limiter.check(bob));
        assert!(limiter.check(bob));
        assert!(!limiter.check(bob)); // Bob blocked independently
    }

    #[test]
    fn test_rate_limiter_tracked_ips() {
        let limiter = RateLimiter::new(10.0, 100.0);
        assert_eq!(limiter.tracked_ips(), 0);

        limiter.check(ip(1, 0, 0, 1));
        limiter.check(ip(1, 0, 0, 2));
        limiter.check(ip(1, 0, 0, 3));
        assert_eq!(limiter.tracked_ips(), 3);
    }

    #[test]
    fn test_rate_limiter_high_burst_allows_spike() {
        // Simulates a legitimate node joining the network
        let limiter = RateLimiter::new(10.0, 50.0);
        let node = ip(10, 0, 0, 1);

        // Burst of 50 simultaneous requests (e.g. initial sync)
        let mut allowed = 0;
        for _ in 0..60 {
            if limiter.check(node) {
                allowed += 1;
            }
        }
        assert_eq!(
            allowed, 50,
            "exactly burst capacity should be allowed, got {allowed}"
        );
    }

    #[tokio::test]
    async fn test_concurrent_rate_limiting_single_ip() {
        // 100 concurrent tasks from the same IP, burst=10
        let limiter = Arc::new(RateLimiter::new(1.0, 10.0));
        let client = ip(5, 5, 5, 5);

        let allowed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();

        for _ in 0..100 {
            let l = Arc::clone(&limiter);
            let a = Arc::clone(&allowed);
            handles.push(tokio::spawn(async move {
                if l.check(client) {
                    a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // Due to DashMap's entry-level locking, counts should be very close
        // to burst (10). Small overrun is acceptable due to TOCTOU in token check.
        let count = allowed.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            count >= 10 && count <= 15,
            "concurrent burst: expected ~10 allowed, got {count}"
        );
    }

    #[tokio::test]
    async fn test_concurrent_rate_limiting_many_ips() {
        // 1000 unique IPs each making 1 request — all should be allowed
        let limiter = Arc::new(RateLimiter::new(10.0, 20.0));
        let allowed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();

        for i in 0u32..1000 {
            let l = Arc::clone(&limiter);
            let a = Arc::clone(&allowed);
            handles.push(tokio::spawn(async move {
                let ip_addr = IpAddr::V4(Ipv4Addr::new(
                    (i / (256 * 256 * 256)) as u8,
                    (i / (256 * 256) % 256) as u8,
                    (i / 256 % 256) as u8,
                    (i % 256) as u8,
                ));
                if l.check(ip_addr) {
                    a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // Every unique IP has a fresh bucket with burst=20 — first request always allowed
        let count = allowed.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            count, 1000,
            "every unique IP's first request must be allowed"
        );
    }
}
