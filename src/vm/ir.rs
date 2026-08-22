use super::diagnostic::SourceOrigin;
use super::effects::CapabilityRequirement;
use super::signature::StackSignature;
use super::types::{Type, TypedValue};
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
