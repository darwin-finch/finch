use super::effects::EffectSet;
use super::types::Type;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A typed stack row. `tail` names the polymorphic stack below `values`.
/// Values are ordered from the bottom of the visible row to its top.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackRow {
    pub tail: Option<String>,
    pub values: Vec<Type>,
}

impl StackRow {
    pub fn closed(values: Vec<Type>) -> Self {
        Self { tail: None, values }
    }

    pub fn polymorphic(tail: impl Into<String>, values: Vec<Type>) -> Self {
        Self {
            tail: Some(tail.into()),
            values,
        }
    }
}

impl fmt::Display for StackRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(tail) = &self.tail {
            f.write_str(tail)?;
            if !self.values.is_empty() {
                f.write_str(" ")?;
            }
        }
        for (index, value) in self.values.iter().enumerate() {
            if index != 0 {
                f.write_str(" ")?;
            }
            write!(f, "{value}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlEffect {
    Returns,
    MayThrow,
    MaySuspend,
    NeverReturns,
}

/// Typed contract for a callable that may cooperatively suspend.
///
/// `yield_type` is published to the fiber's consumer. `resume_type` is the
/// value supplied when execution continues. The first protocol version is
/// one-way, so source frontends infer `unit` for `resume_type`, but retaining
/// it in the contract avoids a later incompatible function-type change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SuspensionSignature {
    pub yield_type: Box<Type>,
    pub resume_type: Box<Type>,
}

impl SuspensionSignature {
    pub fn one_way(yield_type: Type) -> Self {
        Self {
            yield_type: Box::new(yield_type),
            resume_type: Box::new(Type::Unit),
        }
    }
}

/// Complete contract for a callable word or function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackSignature {
    pub type_parameters: Vec<String>,
    pub input: StackRow,
    pub output: StackRow,
    pub effects: EffectSet,
    pub control: ControlEffect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspension: Option<SuspensionSignature>,
}

impl StackSignature {
    pub fn pure(input: StackRow, output: StackRow) -> Self {
        Self {
            type_parameters: Vec::new(),
            input,
            output,
            effects: EffectSet::pure(),
            control: ControlEffect::Returns,
            suspension: None,
        }
    }
}

impl fmt::Display for StackSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "( {} -- {} ! {} )",
            self.input, self.output, self.effects
        )?;
        if let Some(suspension) = &self.suspension {
            write!(
                f,
                " yields<{},{}>",
                suspension.yield_type, suspension.resume_type
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_polymorphic_stack_direction_unambiguously() {
        let signature = StackSignature::pure(
            StackRow::polymorphic("S", vec![Type::Int, Type::Int]),
            StackRow::polymorphic("S", vec![Type::Int]),
        );
        assert_eq!(signature.to_string(), "( S int int -- S int ! pure )");
    }
}
