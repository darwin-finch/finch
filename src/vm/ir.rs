use super::diagnostic::SourceOrigin;
use super::effects::CapabilityRequirement;
use super::signature::StackSignature;
use super::types::{Type, TypedValue};
use super::interpreter::UiOperation;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type BlockId = u32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Module {
    pub version: u32,
    pub name: String,
    pub entry: String,
    pub functions: BTreeMap<String, Function>,
}

impl Module {
    pub fn single(function: Function) -> Self {
        let name = function.name.clone();
        Self {
            version: super::VM_TYPE_SYSTEM_VERSION,
            name: name.clone(),
            entry: name.clone(),
            functions: BTreeMap::from([(name, function)]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Function {
    pub name: String,
    /// Source-level documentation is immutable metadata on a typed function,
    /// never an executable string literal or a capability-bearing value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    pub signature: StackSignature,
    pub locals: Vec<Type>,
    pub captures: Vec<Type>,
    pub entry: BlockId,
    pub blocks: BTreeMap<BlockId, BasicBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BasicBlock {
    pub id: BlockId,
    pub instructions: Vec<LocatedInstruction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocatedInstruction {
    pub instruction: Instruction,
    pub origin: SourceOrigin,
}

impl LocatedInstruction {
    pub fn generated(instruction: Instruction, word: impl Into<String>) -> Self {
        Self {
            instruction,
            origin: SourceOrigin::generated(word),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Instruction {
    Constant {
        value: TypedValue,
    },
    MakeList {
        element_type: Type,
        count: u32,
    },
    /// Pop `count` key/value pairs and construct an immutable typed map.
    /// This keeps collection construction in the shared IR instead of making
    /// one frontend call a host helper or synthesize an untyped JSON object.
    MakeMap {
        key_type: Type,
        value_type: Type,
        count: u32,
    },
    /// Pop one value for every named field and construct an immutable typed
    /// product. Field names and their types are part of the IR contract so a
    /// frontend never has to encode a record as an untyped JSON object.
    MakeRecord {
        fields: Vec<(String, Type)>,
    },
    /// Project a statically named record field. The field-name string remains
    /// on the stack for normal concatenative calling convention, though the
    /// frontend has already proven it matches `field`. The optional result
    /// makes a missing field explicit at the language boundary even though
    /// verified source normally cannot request an absent field.
    RecordGet {
        field: String,
        value_type: Type,
    },
    /// Immutably replace one statically named field and leave a new record
    /// value. The field-name string stays on the stack for ordinary
    /// concatenative calling convention, though the frontend has already
    /// proven it matches `field`. This is not mutation of a shared object.
    RecordSet {
        field: String,
        value_type: Type,
        record_type: Vec<(String, Type)>,
    },
    Dup,
    Drop,
    Swap,
    LocalGet {
        index: u32,
    },
    LocalSet {
        index: u32,
    },
    CaptureGet {
        index: u32,
    },
    MakeClosure {
        function: String,
        capture_count: u32,
        signature: StackSignature,
    },
    Call {
        function: String,
    },
    CallClosure {
        signature: StackSignature,
    },
    CapabilityRequest {
        requirement: CapabilityRequirement,
        input: Vec<Type>,
        output: Vec<Type>,
    },
    /// Allocate a host-owned opaque output handle.  This is an awaited host
    /// call because only the presentation host may mint the handle.
    OutputOpen,
    /// Publish a portable mutation of an explicit output handle.  Unlike
    /// `say`, this never relies on a process-global active WorkUnit.
    UiEffect {
        operation: UiOperation,
        input: Vec<Type>,
        output: Vec<Type>,
    },
    /// Cooperatively return the implicit VM continuation to the event-loop
    /// trampoline. This is not a user-visible first-class continuation.
    Yield,
    /// Spawn a pure, zero-argument closure on the bounded CPU-fiber pool.
    /// The closure is popped and the runtime resumes this continuation with a
    /// typed task handle; it never shares the parent stack or frame.
    DeferCpu,
    /// Poll a local CPU-fiber task without blocking. It lowers to a runtime
    /// scheduler boundary and resumes with `option<T>`.
    PollCpuFiber,
    /// Suspend until a local CPU-fiber task has a terminal result. The parent
    /// continuation is persisted; no UI/event-loop thread blocks.
    JoinCpuFiber,
    /// Request cooperative cancellation of a local CPU-fiber task. This
    /// consumes the handle and resumes with unit; cancellation is observed at
    /// the worker's next VM boundary rather than forcefully killing a thread.
    CancelCpuFiber,
    /// Continue with the `ok` payload of a `result<T, E>`, or immediately
    /// return `err(E)` from the current typed function.  This is the shared
    /// lowering for Lisp `try` and Co-Forth `?`: it is a statically verified
    /// control edge, not a mutable error slot or a catchable exception.
    PropagateResult {
        /// The successful payload returned by the enclosing function's
        /// `result<R, E>` contract. It can differ from this operation's `T`.
        return_ok_type: Type,
        error_type: Type,
    },
    Jump {
        target: BlockId,
    },
    Branch {
        then_block: BlockId,
        else_block: BlockId,
    },
    Return,
    Trap {
        code: String,
    },
}

impl Instruction {
    pub fn is_terminator(&self) -> bool {
        matches!(
            self,
            Self::Jump { .. } | Self::Branch { .. } | Self::Return | Self::Trap { .. }
        )
    }
}
