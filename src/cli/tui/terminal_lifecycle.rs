use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::time::{Duration, Instant};

const INACTIVE: u8 = 0;
const ACTIVATING: u8 = 1;
const ACTIVE: u8 = 2;
const CLEANING: u8 = 3;

static PHASE: AtomicU8 = AtomicU8::new(INACTIVE);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static CLEANUP_OWNER: AtomicU64 = AtomicU64::new(0);
static NEXT_CLEANUP_OWNER: AtomicU64 = AtomicU64::new(0);
static OUTPUT_GATE: AtomicBool = AtomicBool::new(false);
static SUPERVISED_OUTPUT_GATE_PAUSE: AtomicBool = AtomicBool::new(false);
static SUPERVISED_OUTPUT_GATE_PAUSED: AtomicBool = AtomicBool::new(false);

struct OutputGate;

impl Drop for OutputGate {
    fn drop(&mut self) {
        OUTPUT_GATE.store(false, Ordering::Release);
    }
}

fn acquire_output_gate() -> io::Result<OutputGate> {
    let deadline = Instant::now() + Duration::from_millis(100);
    loop {
        if OUTPUT_GATE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Ok(OutputGate);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "portable terminal writer did not quiesce within 100ms",
            ));
        }
        std::thread::yield_now();
    }
}

pub(crate) fn active_generation() -> u64 {
    if PHASE.load(Ordering::Acquire) != ACTIVE {
        return 0;
    }
    GENERATION.load(Ordering::Acquire)
}

fn validate_writer_generation(generation: u64) -> io::Result<()> {
    if generation == 0
        || PHASE.load(Ordering::Acquire) != ACTIVE
        || GENERATION.load(Ordering::Acquire) != generation
    {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "portable terminal writer admission was revoked",
        ));
    }
    Ok(())
}

pub(crate) fn write_generation(
    generation: u64,
    writer: &mut impl Write,
    bytes: &[u8],
) -> io::Result<usize> {
    let _gate = acquire_output_gate()?;
    if SUPERVISED_OUTPUT_GATE_PAUSE.load(Ordering::Acquire) {
        SUPERVISED_OUTPUT_GATE_PAUSED.store(true, Ordering::Release);
        while SUPERVISED_OUTPUT_GATE_PAUSE.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        SUPERVISED_OUTPUT_GATE_PAUSED.store(false, Ordering::Release);
    }
    validate_writer_generation(generation)?;
    writer.write(&bytes[..bytes.len().min(4096)])
}

pub(crate) fn flush_generation(generation: u64, writer: &mut impl Write) -> io::Result<()> {
    let _gate = acquire_output_gate()?;
    validate_writer_generation(generation)?;
    writer.flush()
}

pub(crate) fn supervised_set_output_gate_pause(paused: bool) {
    if !paused {
        SUPERVISED_OUTPUT_GATE_PAUSED.store(false, Ordering::Release);
    }
    SUPERVISED_OUTPUT_GATE_PAUSE.store(paused, Ordering::Release);
}

pub(crate) fn supervised_output_gate_is_paused() -> bool {
    SUPERVISED_OUTPUT_GATE_PAUSED.load(Ordering::Acquire)
}

/// Exclusive process-wide lease used by the actual non-Unix `TuiRenderer`.
///
/// The protocol implementation is injected so this exact ownership source can
/// be compiled and exercised on Windows without copying Finch's lifecycle.
pub(crate) struct ExclusiveTerminalLease {
    generation: u64,
}

impl ExclusiveTerminalLease {
    pub(crate) fn activate(
        activate_protocols: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Self> {
        if PHASE
            .compare_exchange(INACTIVE, ACTIVATING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "another portable terminal session is active or cleaning up",
            ));
        }
        let generation = GENERATION
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            .max(1);
        GENERATION.store(generation, Ordering::Release);
        CLEANUP_OWNER.store(0, Ordering::Release);
        let _gate = match acquire_output_gate() {
            Ok(gate) => gate,
            Err(error) => {
                PHASE
                    .compare_exchange(ACTIVATING, INACTIVE, Ordering::AcqRel, Ordering::Acquire)
                    .ok();
                return Err(error);
            }
        };
        if let Err(error) = activate_protocols() {
            PHASE.store(INACTIVE, Ordering::Release);
            return Err(error);
        }
        if PHASE
            .compare_exchange(ACTIVATING, ACTIVE, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "portable terminal activation was revoked",
            ));
        }
        Ok(Self { generation })
    }

    pub(crate) fn cleanup(
        &self,
        cleanup_protocols: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        cleanup_generation(self.generation, cleanup_protocols)
    }
}

/// The exact non-Unix lifecycle/output actor owned by `TuiRenderer`.
///
/// Keeping the generation with the actor prevents an output object retained
/// from an older renderer from publishing into a replacement session.
pub(crate) struct PortableRendererSession {
    lease: ExclusiveTerminalLease,
    cleanup_protocols: fn() -> io::Result<()>,
}

impl PortableRendererSession {
    pub(crate) fn activate(
        activate_protocols: fn() -> io::Result<()>,
        cleanup_protocols: fn() -> io::Result<()>,
    ) -> io::Result<Self> {
        let lease = ExclusiveTerminalLease::activate(activate_protocols)?;
        Ok(Self {
            lease,
            cleanup_protocols,
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.lease.generation
    }

    pub(crate) fn write(&self, writer: &mut impl Write, bytes: &[u8]) -> io::Result<usize> {
        write_generation(self.generation(), writer, bytes)
    }

    pub(crate) fn cleanup(&self) -> io::Result<()> {
        self.lease.cleanup(self.cleanup_protocols)
    }
}

impl Drop for PortableRendererSession {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

pub(crate) fn cleanup_active(cleanup_protocols: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    cleanup_generation(GENERATION.load(Ordering::Acquire), cleanup_protocols)
}

fn cleanup_generation(
    generation: u64,
    cleanup_protocols: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if generation == 0 || GENERATION.load(Ordering::Acquire) != generation {
        return Ok(());
    }
    match PHASE.compare_exchange(ACTIVE, CLEANING, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            let owner = claim_cleanup_owner();
            return finish_cleanup(generation, owner, cleanup_protocols);
        }
        Err(INACTIVE) => return Ok(()),
        Err(CLEANING) => {
            if CLEANUP_OWNER.load(Ordering::Acquire) != 0 {
                let deadline = Instant::now() + Duration::from_millis(100);
                while CLEANUP_OWNER.load(Ordering::Acquire) != 0
                    && PHASE.load(Ordering::Acquire) == CLEANING
                    && Instant::now() < deadline
                {
                    std::thread::yield_now();
                }
                if PHASE.load(Ordering::Acquire) == INACTIVE {
                    return Ok(());
                }
                if CLEANUP_OWNER.load(Ordering::Acquire) != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "portable terminal cleanup did not quiesce within 100ms",
                    ));
                }
            }
            let owner = claim_cleanup_owner();
            return finish_cleanup(generation, owner, cleanup_protocols);
        }
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "portable terminal lifecycle is transitioning",
            ));
        }
    }
}

fn claim_cleanup_owner() -> u64 {
    let owner = NEXT_CLEANUP_OWNER
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1)
        .max(1);
    CLEANUP_OWNER.store(owner, Ordering::Release);
    owner
}

fn finish_cleanup(
    generation: u64,
    owner: u64,
    cleanup_protocols: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let _gate = match acquire_output_gate() {
        Ok(gate) => gate,
        Err(error) => {
            CLEANUP_OWNER
                .compare_exchange(owner, 0, Ordering::AcqRel, Ordering::Acquire)
                .ok();
            return Err(error);
        }
    };
    if GENERATION.load(Ordering::Acquire) != generation
        || PHASE.load(Ordering::Acquire) != CLEANING
        || CLEANUP_OWNER.load(Ordering::Acquire) != owner
    {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "portable terminal cleanup ownership changed",
        ));
    }
    if let Err(error) = cleanup_protocols() {
        // Fail closed: a replacement cannot activate after an incomplete
        // cleanup. Relinquishing only the lifecycle owner lets a later caller
        // retry this exact generation without admitting a replacement.
        CLEANUP_OWNER
            .compare_exchange(owner, 0, Ordering::AcqRel, Ordering::Acquire)
            .ok();
        return Err(error);
    }
    if GENERATION.load(Ordering::Acquire) != generation
        || PHASE.load(Ordering::Acquire) != CLEANING
        || CLEANUP_OWNER.load(Ordering::Acquire) != owner
    {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "portable terminal cleanup ownership changed",
        ));
    }
    CLEANUP_OWNER.store(0, Ordering::Release);
    PHASE.store(INACTIVE, Ordering::Release);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn activate_protocols() -> io::Result<()> {
        Ok(())
    }

    fn cleanup_protocols() -> io::Result<()> {
        Ok(())
    }

    #[test]
    fn test_portable_terminal_lease_is_exclusive_and_reusable() {
        let first = ExclusiveTerminalLease::activate(|| Ok(())).unwrap();
        let mut output = Vec::new();
        assert_eq!(
            write_generation(first.generation, &mut output, b"active").unwrap(),
            6
        );
        assert!(ExclusiveTerminalLease::activate(|| Ok(())).is_err());
        assert!(first
            .cleanup(|| Err(io::Error::other("injected cleanup failure")))
            .is_err());
        assert!(ExclusiveTerminalLease::activate(|| Ok(())).is_err());
        assert!(write_generation(first.generation, &mut output, b"stale").is_err());
        first.cleanup(|| Ok(())).unwrap();
        let second = ExclusiveTerminalLease::activate(|| Ok(())).unwrap();
        assert!(write_generation(first.generation, &mut output, b"old").is_err());
        second.cleanup(|| Ok(())).unwrap();

        // Fail-before: a writer parked after admission left cleanup's attempt
        // owner set forever, so the exact renderer actor could never repair or
        // admit another renderer after the 100 ms timeout.
        let renderer = Arc::new(
            PortableRendererSession::activate(activate_protocols, cleanup_protocols).unwrap(),
        );
        supervised_set_output_gate_pause(true);
        let writer_renderer = Arc::clone(&renderer);
        let writer = std::thread::spawn(move || {
            let mut writer_output = Vec::new();
            let result = writer_renderer.write(&mut writer_output, b"late-frame");
            (result, writer_output)
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !supervised_output_gate_is_paused() {
            assert!(Instant::now() < deadline, "portable writer did not pause");
            std::thread::yield_now();
        }
        let cleanup_started = Instant::now();
        assert!(renderer.cleanup().is_err());
        assert!(cleanup_started.elapsed() < Duration::from_millis(250));
        assert!(PortableRendererSession::activate(activate_protocols, cleanup_protocols).is_err());
        supervised_set_output_gate_pause(false);
        let (writer_result, writer_output) = writer.join().unwrap();
        assert!(writer_result.is_err());
        assert!(writer_output.is_empty());
        renderer.cleanup().unwrap();
        let replacement =
            PortableRendererSession::activate(activate_protocols, cleanup_protocols).unwrap();
        assert!(renderer.write(&mut output, b"old-renderer").is_err());
        replacement.cleanup().unwrap();

        // Fail-before: timing out before protocol activation returned with the
        // global lifecycle stranded in ACTIVATING.
        let _gate = acquire_output_gate().unwrap();
        assert!(ExclusiveTerminalLease::activate(|| Ok(())).is_err());
        assert_eq!(PHASE.load(Ordering::Acquire), INACTIVE);
    }
}
