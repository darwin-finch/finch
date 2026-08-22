use super::effects::{CapabilityKind, CapabilityRequirement, EffectSet, ResourceSelector};
use super::signature::{ControlEffect, StackRow, StackSignature};
use super::types::Type;
use super::verifier::Vocabulary;
use std::collections::BTreeMap;

fn pure(input: Vec<Type>, output: Vec<Type>) -> StackSignature {
    StackSignature::pure(
        StackRow::polymorphic("S", input),
        StackRow::polymorphic("S", output),
    )
}

fn capability(
    input: Vec<Type>,
    output: Vec<Type>,
    requirement: CapabilityRequirement,
) -> StackSignature {
    StackSignature {
        type_parameters: Vec::new(),
        input: StackRow::polymorphic("S", input),
        output: StackRow::polymorphic("S", output),
        effects: EffectSet::from_requirement(requirement),
        control: ControlEffect::MaySuspend,
    }
}

fn unscoped(kind: CapabilityKind) -> CapabilityRequirement {
    CapabilityRequirement {
        capability: kind,
        selector: ResourceSelector::None,
    }
}

/// Canonical signatures for the first verified core. Runtime implementations,
/// provider documentation, and tests consume this registry rather than keeping
/// independent handwritten signature tables.
pub fn core_vocabulary() -> Vocabulary {
    let a = Type::Variable("A".into());
    let int_binary = || pure(vec![Type::Int, Type::Int], vec![Type::Int]);
    let comparison = || pure(vec![Type::Int, Type::Int], vec![Type::Bool]);
    BTreeMap::from([
        ("+".into(), int_binary()),
        ("-".into(), int_binary()),
        ("*".into(), int_binary()),
        ("/".into(), int_binary()),
        ("mod".into(), int_binary()),
        ("=".into(), comparison()),
        ("<".into(), comparison()),
        (">".into(), comparison()),
        ("<=".into(), comparison()),
        (">=".into(), comparison()),
        ("negate".into(), pure(vec![Type::Int], vec![Type::Int])),
        ("abs".into(), pure(vec![Type::Int], vec![Type::Int])),
        ("not".into(), pure(vec![Type::Bool], vec![Type::Bool])),
        (
            "dup".into(),
            pure(vec![a.clone()], vec![a.clone(), a.clone()]),
        ),
        ("drop".into(), pure(vec![a.clone()], Vec::new())),
        (
            "swap".into(),
            pure(
                vec![Type::Variable("A".into()), Type::Variable("B".into())],
                vec![Type::Variable("B".into()), Type::Variable("A".into())],
            ),
        ),
        (
            "str-cat".into(),
            pure(vec![Type::String, Type::String], vec![Type::String]),
        ),
        (
            "list-length".into(),
            pure(vec![Type::list(a.clone())], vec![Type::Int]),
        ),
        (
            "list-get".into(),
            pure(vec![Type::list(a.clone()), Type::Int], vec![a.clone()]),
        ),
        (
            "say".into(),
            capability(
                vec![Type::String],
                vec![Type::Unit],
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "emit".into(),
            capability(
                vec![Type::String],
                vec![Type::Unit],
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "mem-recall".into(),
            capability(
                vec![Type::String],
                vec![Type::list(Type::String)],
                CapabilityRequirement {
                    capability: CapabilityKind::MemoryRead,
                    selector: ResourceSelector::Memory {
                        tree: "session".into(),
                        path: "**".into(),
                    },
                },
            ),
        ),
        (
            "mem-store".into(),
            capability(
                vec![Type::String],
                vec![Type::Resource("memory-node".into())],
                CapabilityRequirement {
                    capability: CapabilityKind::MemoryWrite,
                    selector: ResourceSelector::Memory {
                        tree: "session".into(),
                        path: "**".into(),
                    },
                },
            ),
        ),
        (
            "schedule-create".into(),
            capability(
                vec![Type::String, Type::Resource("program-ref".into())],
                vec![Type::Resource("schedule".into())],
                CapabilityRequirement {
                    capability: CapabilityKind::ScheduleCreate,
                    selector: ResourceSelector::Schedule { policy: None },
                },
            ),
        ),
        (
            "agent-spawn".into(),
            capability(
                vec![Type::String],
                vec![Type::Task(Box::new(Type::String))],
                unscoped(CapabilityKind::AgentSpawn),
            ),
        ),
        (
            "agent-await".into(),
            capability(
                vec![Type::Task(Box::new(Type::String))],
                vec![Type::result(
                    Type::String,
                    Type::Resource("agent-error".into()),
                )],
                unscoped(CapabilityKind::AgentAwait),
            ),
        ),
        (
            "automation-availability".into(),
            capability(
                Vec::new(),
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::AutomationInspect,
                    selector: ResourceSelector::Automation { application: None },
                },
            ),
        ),
        (
            "automation-displays".into(),
            capability(
                Vec::new(),
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::AutomationInspect,
                    selector: ResourceSelector::Automation { application: None },
                },
            ),
        ),
        (
            "automation-windows".into(),
            capability(
                Vec::new(),
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::AutomationInspect,
                    selector: ResourceSelector::Automation { application: None },
                },
            ),
        ),
        (
            "automation-click".into(),
            capability(
                vec![Type::Float, Type::Float, Type::String, Type::Int],
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::AutomationWrite,
                    selector: ResourceSelector::Automation { application: None },
                },
            ),
        ),
        (
            "automation-type".into(),
            capability(
                vec![Type::String, Type::Int],
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::AutomationWrite,
                    selector: ResourceSelector::Automation { application: None },
                },
            ),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_words_are_generated_from_one_registry() {
        let vocabulary = core_vocabulary();
        assert_eq!(vocabulary["dup"].to_string(), "( S A -- S A A ! {} )");
        assert!(!vocabulary["agent-spawn"].effects.is_pure());
    }
}
