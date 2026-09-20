//! Portable VM effect envelope, moved verbatim from the former `src/runtime/mod.rs`.
//!
//! The envelope rides [`crate::types::LiveOutputSink::vm_effect_envelope`],
//! so the tool API must be able to name it. `runtime` re-exports the same
//! type; the runtime-coupled methods (`program_run`, `output_handle`)
//! remain on `runtime` as an extension trait over this type.

use finch_vm::VmSideEffect;
use serde::{Deserialize, Serialize};

/// A portable VM event attached to its owning ProgramRun. The VM event itself
/// remains embedder-neutral; the envelope provides the other half of its
/// idempotency key to a host/UI callback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmEffectEnvelope {
    pub execution_id: uuid::Uuid,
    pub effect: VmSideEffect,
}

/// Stable identity for one journaled VM effect. It is usable as a proposal
/// handle while a `program.invoke` request awaits an editor/IDE result, and
/// is equally valid for every other portable host effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VmEffectHandle {
    pub execution_id: uuid::Uuid,
    pub sequence: u64,
}

impl VmEffectEnvelope {
    /// Stable `(execution_id, sequence)` handle for this envelope.
    pub fn handle(&self) -> VmEffectHandle {
        VmEffectHandle {
            execution_id: self.execution_id,
            sequence: self.effect.sequence,
        }
    }
}
