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

/// Complete contract for a callable word or function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackSignature {
    pub type_parameters: Vec<String>,
    pub input: StackRow,
    pub output: StackRow,
    pub effects: EffectSet,
    pub control: ControlEffect,
}

impl StackSignature {
    pub fn pure(input: StackRow, output: StackRow) -> Self {
        Self {
            type_parameters: Vec::new(),
            input,
            output,
            effects: EffectSet::pure(),
            control: ControlEffect::Returns,
        }
    }
}

impl fmt::Display for StackSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "( {} -- {} ! {} )",
            self.input, self.output, self.effects
        )
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
        assert_eq!(signature.to_string(), "( S int int -- S int ! {} )");
    }
}
