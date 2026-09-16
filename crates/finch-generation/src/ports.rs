//! Injected clocks, cancellation, scheduling, progress, loading, cache,
//! hardware discovery, and telemetry.

use crate::readiness::LoadPhase;
use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Monotonic clock used for elapsed-time metadata. Tests inject a frozen clock.
pub trait MonotonicClock: Send + Sync {
    /// Monotonic timestamp as milliseconds.
    fn now_ms(&self) -> u64;
}

/// Production clock based on [`Instant`].
#[derive(Debug, Clone)]
pub struct SystemMonotonicClock {
    origin: Instant,
}

impl Default for SystemMonotonicClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl MonotonicClock for SystemMonotonicClock {
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }
}

/// Deterministic clock tests can advance.
#[derive(Debug, Default)]
pub struct FrozenMonotonicClock {
    now_ms: AtomicU64,
}

impl FrozenMonotonicClock {
    /// Construct at `now_ms`.
    pub fn new(now_ms: u64) -> Self {
        Self {
            now_ms: AtomicU64::new(now_ms),
        }
    }

    /// Advance the clock by `delta_ms`.
    pub fn advance(&self, delta_ms: u64) {
        self.now_ms.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

impl MonotonicClock for FrozenMonotonicClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

/// Sleeper used for timeouts. Tests inject instant or controllable sleepers.
#[async_trait]
pub trait Sleeper: Send + Sync {
    /// Sleep for `duration`. Instant implementations return immediately.
    async fn sleep(&self, duration: Duration);
}

/// Tokio sleeper.
pub struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Sleeper that never waits. Suitable only for tests that do not race timeout
/// against a live backend.
pub struct InstantSleeper;

#[async_trait]
impl Sleeper for InstantSleeper {
    async fn sleep(&self, _duration: Duration) {}
}

/// Sleeper that parks until [`ControllableSleeper::release`].
#[derive(Default)]
pub struct ControllableSleeper {
    notify: Notify,
}

impl ControllableSleeper {
    /// Construct a parked sleeper.
    pub fn new() -> Self {
        Self {
            notify: Notify::new(),
        }
    }

    /// Unblock one waiter.
    pub fn release(&self) {
        self.notify.notify_one();
    }
}

#[async_trait]
impl Sleeper for ControllableSleeper {
    async fn sleep(&self, _duration: Duration) {
        self.notify.notified().await;
    }
}

/// Secret-free progress sink for model loading.
pub trait ProgressSink: Send + Sync {
    /// Report a load phase and elapsed milliseconds.
    fn report(&self, phase: LoadPhase, elapsed_ms: u64);
}

/// Tracing progress sink.
pub struct TracingProgress;

impl ProgressSink for TracingProgress {
    fn report(&self, phase: LoadPhase, elapsed_ms: u64) {
        tracing::debug!(?phase, elapsed_ms, "generation load progress");
    }
}

/// Hardware snapshot. Not a conformance claim.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HardwareSnapshot {
    /// Accelerator class (`"cpu"`, `"metal"`, `"cuda"`, `"unknown"`).
    pub accelerator: String,
    /// Device memory when known.
    pub memory_bytes: Option<u64>,
}

/// Hardware discovery port.
pub trait HardwareDiscovery: Send + Sync {
    /// Return a secret-free snapshot.
    fn snapshot(&self) -> HardwareSnapshot;
}

/// Unknown-hardware default.
pub struct UnknownHardware;

impl HardwareDiscovery for UnknownHardware {
    fn snapshot(&self) -> HardwareSnapshot {
        HardwareSnapshot {
            accelerator: "unknown".into(),
            memory_bytes: None,
        }
    }
}

/// Model-load phase port. Presence of a loader is not conformance.
pub trait ModelLoader: Send + Sync {
    /// Current load phase.
    fn phase(&self) -> LoadPhase;
}

/// Loader that reports ready without claiming a real model is loaded.
pub struct ReadyLoader;

impl ModelLoader for ReadyLoader {
    fn phase(&self) -> LoadPhase {
        LoadPhase::Ready
    }
}

/// Artifact cache port.
pub trait ArtifactCache: Send + Sync {
    /// Whether `key` is present. Keys must be secret-free.
    fn has(&self, key: &str) -> bool;
}

/// Empty cache.
pub struct EmptyCache;

impl ArtifactCache for EmptyCache {
    fn has(&self, _key: &str) -> bool {
        false
    }
}

/// Secret-free telemetry.
pub trait GenerationTelemetry: Send + Sync {
    /// Emit a named event with secret-free fields.
    fn event(&self, name: &str, fields: &[(&str, &str)]);
}

/// Tracing telemetry.
pub struct TracingTelemetry;

impl GenerationTelemetry for TracingTelemetry {
    fn event(&self, name: &str, fields: &[(&str, &str)]) {
        tracing::debug!(event = name, ?fields, "generation telemetry");
    }
}

/// Blocking work scheduler. Tests inject a same-thread runner.
pub trait BlockingScheduler: Send + Sync {
    /// Run `work` off the generation event path when the host provides a pool.
    fn spawn_blocking(&self, work: Box<dyn FnOnce() + Send>);
}

/// Scheduler that runs work immediately. Deterministic in tests.
pub struct InlineScheduler;

impl BlockingScheduler for InlineScheduler {
    fn spawn_blocking(&self, work: Box<dyn FnOnce() + Send>) {
        work();
    }
}

/// Injected environmental ports for generation.
#[derive(Clone)]
pub struct GenerationPorts {
    /// Monotonic clock.
    pub clock: Arc<dyn MonotonicClock>,
    /// Timeout sleeper.
    pub sleeper: Arc<dyn Sleeper>,
    /// Load progress.
    pub progress: Arc<dyn ProgressSink>,
    /// Model loader.
    pub loader: Arc<dyn ModelLoader>,
    /// Artifact cache.
    pub cache: Arc<dyn ArtifactCache>,
    /// Hardware discovery.
    pub hardware: Arc<dyn HardwareDiscovery>,
    /// Telemetry.
    pub telemetry: Arc<dyn GenerationTelemetry>,
    /// Blocking scheduler.
    pub scheduler: Arc<dyn BlockingScheduler>,
}

impl GenerationPorts {
    /// Production ports: wall monotonic clock, tokio sleep, tracing sinks.
    pub fn production() -> Self {
        Self {
            clock: Arc::new(SystemMonotonicClock::default()),
            sleeper: Arc::new(TokioSleeper),
            progress: Arc::new(TracingProgress),
            loader: Arc::new(ReadyLoader),
            cache: Arc::new(EmptyCache),
            hardware: Arc::new(UnknownHardware),
            telemetry: Arc::new(TracingTelemetry),
            scheduler: Arc::new(InlineScheduler),
        }
    }

    /// Deterministic test ports with a frozen clock and instant sleeper.
    pub fn test() -> Self {
        Self {
            clock: Arc::new(FrozenMonotonicClock::new(0)),
            sleeper: Arc::new(InstantSleeper),
            progress: Arc::new(TracingProgress),
            loader: Arc::new(ReadyLoader),
            cache: Arc::new(EmptyCache),
            hardware: Arc::new(UnknownHardware),
            telemetry: Arc::new(TracingTelemetry),
            scheduler: Arc::new(InlineScheduler),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frozen_clock_advances_without_wall_time() {
        let clock = FrozenMonotonicClock::new(10);
        clock.advance(25);
        assert_eq!(
            clock.now_ms(),
            35,
            "frozen clock must move only by advance(); now={}",
            clock.now_ms()
        );
    }

    #[tokio::test]
    async fn test_controllable_sleeper_parks_until_release() {
        let sleeper = Arc::new(ControllableSleeper::new());
        let flag = Arc::new(AtomicU64::new(0));
        let sleeper_task = Arc::clone(&sleeper);
        let flag_task = Arc::clone(&flag);
        let handle = tokio::spawn(async move {
            sleeper_task.sleep(Duration::from_secs(3600)).await;
            flag_task.store(1, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;
        assert_eq!(
            flag.load(Ordering::SeqCst),
            0,
            "controllable sleeper released before release()"
        );
        sleeper.release();
        handle.await.unwrap();
        assert_eq!(flag.load(Ordering::SeqCst), 1);
    }
}
