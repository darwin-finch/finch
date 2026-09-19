//! Declared execution authority for a tool or program.
//!
//! Moved verbatim from the former `src/programs`; `finch-programs` re-exports the same type,
//! so every `finch_programs::ExecutionEffect` path keeps resolving.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Upper bound on what executing a program may affect.
///
/// This is deliberately about observable effects, not implementation language.
/// Pure and read-only programs can run autonomously; mutation and unknown code
/// cross an approval boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEffect {
    Pure,
    VmRead,
    VmWrite,
    WorkspaceRead,
    ExternalRead,
    WorkspaceWrite,
    ExternalWrite,
    Destructive,
    Unclassified,
}

impl ExecutionEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pure => "pure",
            Self::VmRead => "vm_read",
            Self::VmWrite => "vm_write",
            Self::WorkspaceRead => "workspace_read",
            Self::ExternalRead => "external_read",
            Self::WorkspaceWrite => "workspace_write",
            Self::ExternalWrite => "external_write",
            Self::Destructive => "destructive",
            Self::Unclassified => "unclassified",
        }
    }

    pub fn runs_autonomously(self) -> bool {
        matches!(
            self,
            Self::Pure | Self::VmRead | Self::VmWrite | Self::WorkspaceRead | Self::ExternalRead
        )
    }
}

impl std::str::FromStr for ExecutionEffect {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pure" => Ok(Self::Pure),
            "vm_read" => Ok(Self::VmRead),
            "vm_write" => Ok(Self::VmWrite),
            "workspace_read" => Ok(Self::WorkspaceRead),
            "external_read" => Ok(Self::ExternalRead),
            "workspace_write" => Ok(Self::WorkspaceWrite),
            "external_write" => Ok(Self::ExternalWrite),
            "destructive" => Ok(Self::Destructive),
            "unclassified" => Ok(Self::Unclassified),
            other => bail!("unknown execution effect: {other}"),
        }
    }
}
