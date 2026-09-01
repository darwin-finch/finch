use std::io;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::time::{Duration, Instant};

const INACTIVE: u8 = 0;
const ACTIVATING: u8 = 1;
const ACTIVE: u8 = 2;
const CLEANING: u8 = 3;

static PHASE: AtomicU8 = AtomicU8::new(INACTIVE);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static CLEANUP_OWNER: AtomicU64 = AtomicU64::new(0);
static NEXT_CLEANUP_OWNER: AtomicU64 = AtomicU64::new(0);

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

    #[test]
    fn test_portable_terminal_lease_is_exclusive_and_reusable() {
        let first = ExclusiveTerminalLease::activate(|| Ok(())).unwrap();
        assert!(ExclusiveTerminalLease::activate(|| Ok(())).is_err());
        assert!(first
            .cleanup(|| Err(io::Error::other("injected cleanup failure")))
            .is_err());
        assert!(ExclusiveTerminalLease::activate(|| Ok(())).is_err());
        first.cleanup(|| Ok(())).unwrap();
        let second = ExclusiveTerminalLease::activate(|| Ok(())).unwrap();
        second.cleanup(|| Ok(())).unwrap();
    }
}
