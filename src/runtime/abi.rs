//! Frozen portable Runtime/Application ABI types.
//!
//! These records are embedder-neutral. Brain supplies identity through the
//! ports below; this module never names `crate::brain`. Live attached-console
//! streaming is issue #57 and is not implemented here.

use crate::runtime::{VmEffectEnvelope, VmEffectHandle, VmResume};
use crate::vm::{HostSideEffect, TypedValue, VmDiagnostic, VmSideEffect};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn abi_version() -> u32 {
    crate::vm::RUNTIME_APPLICATION_ABI_VERSION
}

/// Frozen identity of one verified ProgramRun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProgramRun {
    /// Runtime/Application ABI version carried with this run identity.
    #[serde(default = "abi_version")]
    pub abi_version: u32,
    /// VM execution identity; together with an effect sequence it is the
    /// idempotency key for `VmSideEffect` / `VmResume`.
    pub execution_id: Uuid,
    /// Optional embedder Brain identity. Runtime does not interpret it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brain_id: Option<Uuid>,
    /// Optional embedder client/attachment identity. Runtime does not
    /// interpret it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<Uuid>,
}

impl ProgramRun {
    /// Identify a ProgramRun from its VM execution id.
    pub fn new(execution_id: Uuid) -> Self {
        Self {
            abi_version: abi_version(),
            execution_id,
            brain_id: None,
            client_id: None,
        }
    }

    /// Bind embedder-neutral Brain/client ports onto this run identity.
    pub fn with_identity(mut self, identity: DeliveryConsumerIdentity) -> Self {
        self.brain_id = Some(identity.brain_id);
        self.client_id = Some(identity.client_id);
        self
    }

    /// Named handle for one journaled effect on this run.
    pub fn effect_handle(self, sequence: u64) -> VmEffectHandle {
        VmEffectHandle {
            execution_id: self.execution_id,
            sequence,
        }
    }
}

/// Embedder-neutral delivery consumer. Brain implements this with its Brain
/// id and a client/attachment id; other hosts mint their own UUIDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeliveryConsumerIdentity {
    pub brain_id: Uuid,
    pub client_id: Uuid,
}

impl DeliveryConsumerIdentity {
    /// Construct a Brain/client delivery identity.
    pub fn new(brain_id: Uuid, client_id: Uuid) -> Self {
        Self {
            brain_id,
            client_id,
        }
    }

    /// Canonical durable-log key for this identity.
    pub fn wire_key(self) -> String {
        format!("{}/{}", self.brain_id, self.client_id)
    }

    /// Parse a canonical Brain/client wire key.
    pub fn parse_wire_key(key: &str) -> Option<Self> {
        let (brain, client) = key.split_once('/')?;
        Some(Self {
            brain_id: brain.parse().ok()?,
            client_id: client.parse().ok()?,
        })
    }
}

/// Per-consumer delivery cursor over one ProgramRun's effect sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeliveryCursor {
    pub execution_id: Uuid,
    pub through_sequence: u64,
}

impl DeliveryCursor {
    /// Cursor that has acknowledged `through_sequence` inclusive.
    pub fn through(execution_id: Uuid, through_sequence: u64) -> Self {
        Self {
            execution_id,
            through_sequence,
        }
    }
}

/// Host-issued concurrent output handle bound to one ProgramRun.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutputHandleRef {
    pub execution_id: Uuid,
    pub handle: String,
    pub generation: u64,
}

impl OutputHandleRef {
    /// Construct a ProgramRun-owned output handle reference.
    pub fn new(execution_id: Uuid, handle: impl Into<String>, generation: u64) -> Self {
        Self {
            execution_id,
            handle: handle.into(),
            generation,
        }
    }
}

/// One versioned Runtime/Application ABI record. Cap'n Proto packed frames
/// carry this same closed set for local IPC and later WebSocket reuse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum RuntimeApplicationMessage {
    ProgramRun {
        run: ProgramRun,
    },
    Diagnostic {
        diagnostic: VmDiagnostic,
    },
    Envelope {
        envelope: VmEffectEnvelope,
    },
    Resume {
        resume: VmResume,
    },
    EffectHandle {
        handle: VmEffectHandle,
    },
    OutputHandle {
        handle: OutputHandleRef,
    },
    CursorAck {
        consumer: DeliveryConsumerIdentity,
        cursor: DeliveryCursor,
    },
}

impl RuntimeApplicationMessage {
    /// ABI version carried by this record family.
    pub fn abi_version(&self) -> u32 {
        match self {
            Self::ProgramRun { run } => run.abi_version,
            _ => abi_version(),
        }
    }
}

pub(crate) fn output_handle_ref(
    execution_id: Uuid,
    effect: &VmSideEffect,
) -> Option<OutputHandleRef> {
    match &effect.event {
        HostSideEffect::Ui {
            target:
                Some(TypedValue::Resource {
                    kind,
                    handle,
                    generation,
                }),
            ..
        } if kind == "output-handle" && !handle.is_empty() => Some(OutputHandleRef {
            execution_id,
            handle: handle.clone(),
            generation: *generation,
        }),
        _ => None,
    }
}
