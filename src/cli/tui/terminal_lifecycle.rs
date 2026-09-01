use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const INACTIVE: u8 = 0;
const ACTIVATING: u8 = 1;
const ACTIVE: u8 = 2;
const CLEANING: u8 = 3;
const APPLICATION_TIMEOUT: Duration = Duration::from_millis(100);
const ACTOR_QUEUE_BOUND: usize = 4;
const OUTPUT_CHUNK_BOUND: usize = 4096;

pub(crate) type ProtocolOperation = fn() -> io::Result<()>;

static PHASE: AtomicU8 = AtomicU8::new(INACTIVE);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static CLEANUP_OWNER: AtomicU64 = AtomicU64::new(0);
static NEXT_CLEANUP_OWNER: AtomicU64 = AtomicU64::new(0);
static OUTPUT_GATE_OWNER: AtomicU64 = AtomicU64::new(0);
static NEXT_OUTPUT_GATE_OWNER: AtomicU64 = AtomicU64::new(0);
static ACTOR: OnceLock<SyncSender<ActorCommand>> = OnceLock::new();
static SUPERVISED_OUTPUT_GATE_PAUSE: AtomicBool = AtomicBool::new(false);
static SUPERVISED_OUTPUT_GATE_PAUSED: AtomicBool = AtomicBool::new(false);
static SUPERVISED_ACTOR_PAUSE: AtomicBool = AtomicBool::new(false);
static SUPERVISED_ACTOR_PAUSED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static SUPERVISED_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

std::thread_local! {
    static HELD_OUTPUT_GATE_OWNER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static HELD_CLEANUP_OWNER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

struct OutputGate {
    owner: u64,
}

impl Drop for OutputGate {
    fn drop(&mut self) {
        OUTPUT_GATE_OWNER
            .compare_exchange(self.owner, 0, Ordering::AcqRel, Ordering::Acquire)
            .ok();
        HELD_OUTPUT_GATE_OWNER.with(|held| {
            if held.get() == self.owner {
                held.set(0);
            }
        });
    }
}

fn acquire_output_gate_until(deadline: Instant) -> io::Result<OutputGate> {
    if HELD_OUTPUT_GATE_OWNER.with(|held| held.get() != 0) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "portable terminal output gate is not reentrant",
        ));
    }
    let owner = NEXT_OUTPUT_GATE_OWNER
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1)
        .max(1);
    loop {
        if OUTPUT_GATE_OWNER
            .compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            HELD_OUTPUT_GATE_OWNER.with(|held| held.set(owner));
            return Ok(OutputGate { owner });
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "portable terminal writer did not quiesce before its deadline",
            ));
        }
        std::thread::yield_now();
    }
}

/// Revoke only the gate held by the current panic-hook thread. A stale guard
/// releases with CAS, so it cannot clear cleanup's later ownership.
pub(crate) fn revoke_current_thread_output_gate() -> bool {
    let owner = HELD_OUTPUT_GATE_OWNER.with(|held| held.replace(0));
    let gate_revoked = owner != 0
        && OUTPUT_GATE_OWNER
            .compare_exchange(owner, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
    let cleanup_owner = HELD_CLEANUP_OWNER.with(|held| held.replace(0));
    let cleanup_revoked = cleanup_owner != 0
        && CLEANUP_OWNER
            .compare_exchange(cleanup_owner, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
    gate_revoked || cleanup_revoked
}

enum ActorReply {
    Unit(io::Result<()>),
    Written(io::Result<usize>),
}

enum ActorCommand {
    Activate {
        generation: u64,
        activate: ProtocolOperation,
        cleanup: ProtocolOperation,
        reply: SyncSender<ActorReply>,
    },
    Cleanup {
        generation: u64,
        reply: SyncSender<ActorReply>,
    },
    Write {
        generation: u64,
        bytes: Vec<u8>,
        reply: SyncSender<ActorReply>,
    },
    Flush {
        generation: u64,
        reply: SyncSender<ActorReply>,
    },
}

struct ActorSession {
    generation: u64,
    cleanup: ProtocolOperation,
}

fn actor_sender() -> io::Result<&'static SyncSender<ActorCommand>> {
    if let Some(sender) = ACTOR.get() {
        return Ok(sender);
    }
    let (sender, receiver) = mpsc::sync_channel(ACTOR_QUEUE_BOUND);
    std::thread::Builder::new()
        .name("finch-portable-terminal".into())
        .spawn(move || terminal_actor(receiver))?;
    // Activation is process-exclusive, but tolerate a startup race without
    // publishing two command channels: the losing actor observes disconnect.
    let _ = ACTOR.set(sender);
    ACTOR
        .get()
        .ok_or_else(|| io::Error::other("portable terminal actor sender was not published"))
}

fn terminal_actor(receiver: Receiver<ActorCommand>) {
    let mut session: Option<ActorSession> = None;
    while let Ok(command) = receiver.recv() {
        if SUPERVISED_ACTOR_PAUSE.load(Ordering::Acquire) {
            SUPERVISED_ACTOR_PAUSED.store(true, Ordering::Release);
            while SUPERVISED_ACTOR_PAUSE.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            SUPERVISED_ACTOR_PAUSED.store(false, Ordering::Release);
        }
        match command {
            ActorCommand::Activate {
                generation,
                activate,
                cleanup,
                reply,
            } => {
                let activation = activate();
                if activation.is_ok()
                    && PHASE.load(Ordering::Acquire) == ACTIVATING
                    && GENERATION.load(Ordering::Acquire) == generation
                {
                    session = Some(ActorSession {
                        generation,
                        cleanup,
                    });
                    let _ = reply.send(ActorReply::Unit(Ok(())));
                    continue;
                }

                // Activation failed or its caller revoked the attempt while a
                // supported console call was in progress. Roll back on this
                // same actor so no queued output can overtake reset.
                let rollback = cleanup();
                let rollback_failed = rollback.is_err();
                let result = aggregate_activation_rollback(activation, rollback);
                if rollback_failed {
                    session = Some(ActorSession {
                        generation,
                        cleanup,
                    });
                    PHASE.store(CLEANING, Ordering::Release);
                } else {
                    session = None;
                    CLEANUP_OWNER.store(0, Ordering::Release);
                    PHASE.store(INACTIVE, Ordering::Release);
                }
                let _ = reply.send(ActorReply::Unit(result));
            }
            ActorCommand::Cleanup { generation, reply } => {
                let result = match session.as_ref() {
                    Some(active) if active.generation == generation => (active.cleanup)(),
                    Some(_) => Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "portable terminal actor owns another generation",
                    )),
                    None => Ok(()),
                };
                if result.is_ok() {
                    session = None;
                }
                let _ = reply.send(ActorReply::Unit(result));
            }
            ActorCommand::Write {
                generation,
                bytes,
                reply,
            } => {
                let result = match session.as_ref() {
                    Some(active)
                        if active.generation == generation
                            && PHASE.load(Ordering::Acquire) == ACTIVE =>
                    {
                        io::stdout().write(&bytes)
                    }
                    _ => Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "portable terminal actor rejected a stale writer",
                    )),
                };
                let _ = reply.send(ActorReply::Written(result));
            }
            ActorCommand::Flush { generation, reply } => {
                let result = match session.as_ref() {
                    Some(active)
                        if active.generation == generation
                            && PHASE.load(Ordering::Acquire) == ACTIVE =>
                    {
                        io::stdout().flush()
                    }
                    _ => Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "portable terminal actor rejected a stale flush",
                    )),
                };
                let _ = reply.send(ActorReply::Unit(result));
            }
        }
    }
}

fn aggregate_activation_rollback(
    activation: io::Result<()>,
    rollback: io::Result<()>,
) -> io::Result<()> {
    match (activation, rollback) {
        (Ok(()), Ok(())) => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "portable terminal activation was revoked and rolled back",
        )),
        (Err(activation), Ok(())) => Err(activation),
        (Ok(()), Err(rollback)) => Err(io::Error::other(format!(
            "portable terminal activation was revoked and rollback failed: {rollback}"
        ))),
        (Err(activation), Err(rollback)) => Err(io::Error::other(format!(
            "portable terminal activation failed: {activation}; rollback failed: {rollback}"
        ))),
    }
}

fn send_command_until(command: ActorCommand, deadline: Instant) -> io::Result<()> {
    let sender = actor_sender()?;
    let mut command = command;
    loop {
        match sender.try_send(command) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                command = returned;
                std::thread::yield_now();
            }
            Err(TrySendError::Full(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "portable terminal actor queue remained full",
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "portable terminal actor stopped",
                ));
            }
        }
    }
}

fn wait_reply_until(receiver: &Receiver<ActorReply>, deadline: Instant) -> io::Result<ActorReply> {
    loop {
        match receiver.try_recv() {
            Ok(reply) => return Ok(reply),
            Err(TryRecvError::Empty) if Instant::now() < deadline => std::thread::yield_now(),
            Err(TryRecvError::Empty) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "portable terminal actor operation exceeded its deadline",
                ));
            }
            Err(TryRecvError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "portable terminal actor response channel closed",
                ));
            }
        }
    }
}

fn unit_command_until(
    deadline: Instant,
    build: impl FnOnce(SyncSender<ActorReply>) -> ActorCommand,
) -> io::Result<()> {
    let (reply, response) = mpsc::sync_channel(1);
    send_command_until(build(reply), deadline)?;
    match wait_reply_until(&response, deadline)? {
        ActorReply::Unit(result) => result,
        ActorReply::Written(_) => Err(io::Error::other(
            "portable terminal actor returned the wrong response",
        )),
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

pub(crate) fn write_generation(generation: u64, bytes: &[u8]) -> io::Result<usize> {
    let deadline = Instant::now() + APPLICATION_TIMEOUT;
    let _gate = acquire_output_gate_until(deadline)?;
    if SUPERVISED_OUTPUT_GATE_PAUSE.load(Ordering::Acquire) {
        SUPERVISED_OUTPUT_GATE_PAUSED.store(true, Ordering::Release);
        while SUPERVISED_OUTPUT_GATE_PAUSE.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        SUPERVISED_OUTPUT_GATE_PAUSED.store(false, Ordering::Release);
    }
    validate_writer_generation(generation)?;
    let (reply, response) = mpsc::sync_channel(1);
    send_command_until(
        ActorCommand::Write {
            generation,
            bytes: bytes[..bytes.len().min(OUTPUT_CHUNK_BOUND)].to_vec(),
            reply,
        },
        deadline,
    )?;
    match wait_reply_until(&response, deadline)? {
        ActorReply::Written(result) => result,
        ActorReply::Unit(_) => Err(io::Error::other(
            "portable terminal actor returned the wrong write response",
        )),
    }
}

pub(crate) fn flush_generation(generation: u64) -> io::Result<()> {
    let deadline = Instant::now() + APPLICATION_TIMEOUT;
    let _gate = acquire_output_gate_until(deadline)?;
    validate_writer_generation(generation)?;
    unit_command_until(deadline, |reply| ActorCommand::Flush { generation, reply })
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

pub(crate) fn supervised_set_actor_pause(paused: bool) {
    if !paused {
        SUPERVISED_ACTOR_PAUSED.store(false, Ordering::Release);
    }
    SUPERVISED_ACTOR_PAUSE.store(paused, Ordering::Release);
}

pub(crate) fn supervised_actor_is_paused() -> bool {
    SUPERVISED_ACTOR_PAUSED.load(Ordering::Acquire)
}

#[cfg(test)]
pub(crate) fn supervised_test_lock() -> std::sync::MutexGuard<'static, ()> {
    SUPERVISED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Exclusive process-wide lease used by the actual non-Unix `TuiRenderer`.
pub(crate) struct ExclusiveTerminalLease {
    generation: u64,
}

impl ExclusiveTerminalLease {
    pub(crate) fn activate(
        activate_protocols: ProtocolOperation,
        cleanup_protocols: ProtocolOperation,
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
        let deadline = Instant::now() + APPLICATION_TIMEOUT;
        let _gate = match acquire_output_gate_until(deadline) {
            Ok(gate) => gate,
            Err(error) => {
                PHASE
                    .compare_exchange(ACTIVATING, INACTIVE, Ordering::AcqRel, Ordering::Acquire)
                    .ok();
                return Err(error);
            }
        };
        let result = unit_command_until(deadline, |reply| ActorCommand::Activate {
            generation,
            activate: activate_protocols,
            cleanup: cleanup_protocols,
            reply,
        });
        if let Err(error) = result {
            // The actor may still be inside a supported console call. Revoke
            // publication now; it will perform rollback before any later
            // queued operation. Never publish INACTIVE from an unknown result.
            PHASE
                .compare_exchange(ACTIVATING, CLEANING, Ordering::AcqRel, Ordering::Acquire)
                .ok();
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

    pub(crate) fn cleanup(&self) -> io::Result<()> {
        cleanup_generation_until(self.generation, Instant::now() + APPLICATION_TIMEOUT)
    }
}

/// Exact non-Unix lifecycle/output actor owned by `TuiRenderer`.
pub(crate) struct PortableRendererSession {
    lease: ExclusiveTerminalLease,
}

impl PortableRendererSession {
    pub(crate) fn activate(
        activate_protocols: ProtocolOperation,
        cleanup_protocols: ProtocolOperation,
    ) -> io::Result<Self> {
        Ok(Self {
            lease: ExclusiveTerminalLease::activate(activate_protocols, cleanup_protocols)?,
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.lease.generation
    }

    pub(crate) fn write(&self, bytes: &[u8]) -> io::Result<usize> {
        write_generation(self.generation(), bytes)
    }

    pub(crate) fn cleanup(&self) -> io::Result<()> {
        self.lease.cleanup()
    }
}

impl Drop for PortableRendererSession {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

pub(crate) fn cleanup_active(cleanup_protocols: ProtocolOperation) -> io::Result<()> {
    cleanup_active_until(Instant::now() + APPLICATION_TIMEOUT, cleanup_protocols)
}

pub(crate) fn cleanup_active_until(
    deadline: Instant,
    _cleanup_protocols: ProtocolOperation,
) -> io::Result<()> {
    cleanup_generation_until(GENERATION.load(Ordering::Acquire), deadline)
}

fn cleanup_generation_until(generation: u64, deadline: Instant) -> io::Result<()> {
    if generation == 0 || GENERATION.load(Ordering::Acquire) != generation {
        return Ok(());
    }
    match PHASE.compare_exchange(ACTIVE, CLEANING, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {}
        Err(INACTIVE) => return Ok(()),
        Err(CLEANING) => {
            let observed_owner = CLEANUP_OWNER.load(Ordering::Acquire);
            while observed_owner != 0
                && CLEANUP_OWNER.load(Ordering::Acquire) == observed_owner
                && PHASE.load(Ordering::Acquire) == CLEANING
                && Instant::now() < deadline
            {
                std::thread::yield_now();
            }
            if PHASE.load(Ordering::Acquire) == INACTIVE {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "portable terminal cleanup owner exceeded its deadline",
                ));
            }
        }
        Err(ACTIVATING) => {
            PHASE
                .compare_exchange(ACTIVATING, CLEANING, Ordering::AcqRel, Ordering::Acquire)
                .ok();
        }
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "portable terminal lifecycle is transitioning",
            ));
        }
    }
    let owner = NEXT_CLEANUP_OWNER
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1)
        .max(1);
    CLEANUP_OWNER.store(owner, Ordering::Release);
    HELD_CLEANUP_OWNER.with(|held| held.set(owner));
    let result = finish_cleanup_until(generation, owner, deadline);
    HELD_CLEANUP_OWNER.with(|held| {
        if held.get() == owner {
            held.set(0);
        }
    });
    result
}

fn finish_cleanup_until(generation: u64, owner: u64, deadline: Instant) -> io::Result<()> {
    let _gate = match acquire_output_gate_until(deadline) {
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
    let result = unit_command_until(deadline, |reply| ActorCommand::Cleanup {
        generation,
        reply,
    });
    if let Err(error) = result {
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
            "portable terminal cleanup ownership changed after reset",
        ));
    }
    CLEANUP_OWNER.store(0, Ordering::Release);
    PHASE.store(INACTIVE, Ordering::Release);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activate_protocols() -> io::Result<()> {
        Ok(())
    }

    fn cleanup_protocols() -> io::Result<()> {
        Ok(())
    }

    #[test]
    fn test_portable_terminal_actor_is_exclusive_bounded_and_reusable() {
        let _serial = supervised_test_lock();
        let stale = acquire_output_gate_until(Instant::now() + APPLICATION_TIMEOUT).unwrap();
        assert!(revoke_current_thread_output_gate());
        let replacement_gate =
            acquire_output_gate_until(Instant::now() + APPLICATION_TIMEOUT).unwrap();
        drop(stale);
        assert_eq!(
            OUTPUT_GATE_OWNER.load(Ordering::Acquire),
            replacement_gate.owner,
            "stale portable gate guard cleared its replacement owner"
        );
        drop(replacement_gate);
        CLEANUP_OWNER.store(41, Ordering::Release);
        HELD_CLEANUP_OWNER.with(|held| held.set(41));
        assert!(revoke_current_thread_output_gate());
        assert_eq!(CLEANUP_OWNER.load(Ordering::Acquire), 0);

        let first =
            PortableRendererSession::activate(activate_protocols, cleanup_protocols).unwrap();
        assert!(PortableRendererSession::activate(activate_protocols, cleanup_protocols).is_err());
        assert_eq!(first.write(b"active").unwrap(), 6);

        supervised_set_output_gate_pause(true);
        let generation = first.generation();
        let writer = std::thread::spawn(move || write_generation(generation, b"late-frame"));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !supervised_output_gate_is_paused() {
            assert!(Instant::now() < deadline, "portable writer did not pause");
            std::thread::yield_now();
        }
        let cleanup_started = Instant::now();
        assert!(first.cleanup().is_err());
        assert!(cleanup_started.elapsed() < Duration::from_millis(250));
        assert!(PortableRendererSession::activate(activate_protocols, cleanup_protocols).is_err());
        supervised_set_output_gate_pause(false);
        assert!(writer.join().unwrap().is_err());
        first.cleanup().unwrap();

        let replacement =
            PortableRendererSession::activate(activate_protocols, cleanup_protocols).unwrap();
        assert!(first.write(b"old-generation").is_err());
        replacement.cleanup().unwrap();

        // Fail-before: caller-thread Windows writes could block forever. The
        // production actor bounds the caller and revokes a staged frame before
        // its delayed execution; cleanup stays fail-closed until actor progress.
        let renderer =
            PortableRendererSession::activate(activate_protocols, cleanup_protocols).unwrap();
        supervised_set_actor_pause(true);
        let generation = renderer.generation();
        let writer = std::thread::spawn(move || write_generation(generation, b"staged-frame"));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !supervised_actor_is_paused() {
            assert!(Instant::now() < deadline, "portable actor did not pause");
            std::thread::yield_now();
        }
        assert!(writer.join().unwrap().is_err());
        assert!(renderer.cleanup().is_err());
        assert!(PortableRendererSession::activate(activate_protocols, cleanup_protocols).is_err());
        supervised_set_actor_pause(false);
        renderer.cleanup().unwrap();
        PortableRendererSession::activate(activate_protocols, cleanup_protocols)
            .unwrap()
            .cleanup()
            .unwrap();
    }
}
