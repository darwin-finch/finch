use super::effects::{
    CapabilityKind, CapabilityRequirement, EffectSet, FileSelector, FileSelectorTemplate,
    FileSelectorTemplatePart, NetworkSelectorTemplate, ProcessSelectorTemplate,
    ProgramSelectorTemplate, ResourceRoot, ResourceSelector,
};
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

fn path_template() -> FileSelectorTemplate {
    let upper_bound = FileSelector::parse("./**").expect("valid workspace root");
    FileSelectorTemplate {
        root: ResourceRoot::Workspace,
        parts: vec![FileSelectorTemplatePart::Argument {
            index: 0,
            bound: upper_bound.clone(),
        }],
        upper_bound,
    }
}

fn host_path_selector() -> FileSelector {
    FileSelector::parse("${host-machine}/**").expect("valid host-machine root")
}

fn host_path_template() -> FileSelectorTemplate {
    let upper_bound = host_path_selector();
    FileSelectorTemplate {
        root: ResourceRoot::HostMachine,
        parts: vec![FileSelectorTemplatePart::Argument {
            index: 0,
            bound: upper_bound.clone(),
        }],
        upper_bound,
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
            "path".into(),
            pure(
                vec![Type::String],
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
            ),
        ),
        // `host-path` is intentionally a different refined type from
        // workspace `path`. It only identifies a child of the host-installed
        // root; it neither installs that root nor grants filesystem access.
        (
            "host-path".into(),
            pure(vec![Type::String], vec![Type::Path(host_path_selector())]),
        ),
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
        ("bytes".into(), pure(vec![Type::String], vec![Type::Bytes])),
        ("space".into(), pure(Vec::new(), vec![Type::String])),
        (
            "json-parse".into(),
            pure(
                vec![Type::String],
                vec![Type::result(Type::Json, Type::String)],
            ),
        ),
        (
            "json-stringify".into(),
            pure(vec![Type::Json], vec![Type::String]),
        ),
        (
            "json-get".into(),
            pure(
                vec![Type::Json, Type::String],
                vec![Type::Option(Box::new(Type::Json))],
            ),
        ),
        (
            "json-as-string".into(),
            pure(
                vec![Type::Json],
                vec![Type::Option(Box::new(Type::String))],
            ),
        ),
        (
            "json-as-int".into(),
            pure(
                vec![Type::Json],
                vec![Type::Option(Box::new(Type::Int))],
            ),
        ),
        (
            "json-as-bool".into(),
            pure(
                vec![Type::Json],
                vec![Type::Option(Box::new(Type::Bool))],
            ),
        ),
        // A control-only cooperative scheduling point. The source frontends
        // lower this word to `Instruction::Yield`, not a normal core call.
        ("yield".into(), pure(Vec::new(), Vec::new())),
        (
            "int-to-string".into(),
            pure(vec![Type::Int], vec![Type::String]),
        ),
        ("atoi".into(), pure(vec![Type::String], vec![Type::Int])),
        (
            "some".into(),
            pure(vec![a.clone()], vec![Type::Option(Box::new(a.clone()))]),
        ),
        (
            "none".into(),
            pure(Vec::new(), vec![Type::Option(Box::new(a.clone()))]),
        ),
        (
            "is-some".into(),
            pure(vec![Type::Option(Box::new(a.clone()))], vec![Type::Bool]),
        ),
        (
            "unwrap".into(),
            pure(vec![Type::Option(Box::new(a.clone()))], vec![a.clone()]),
        ),
        (
            "ok".into(),
            pure(
                vec![a.clone()],
                vec![Type::Result(Box::new(a.clone()), Box::new(Type::Dynamic))],
            ),
        ),
        (
            "err".into(),
            pure(
                vec![a.clone()],
                vec![Type::Result(Box::new(Type::Dynamic), Box::new(a.clone()))],
            ),
        ),
        (
            "is-ok".into(),
            pure(
                vec![Type::Result(
                    Box::new(a.clone()),
                    Box::new(Type::Variable("E".into())),
                )],
                vec![Type::Bool],
            ),
        ),
        (
            "result-unwrap".into(),
            pure(
                vec![Type::Result(
                    Box::new(a.clone()),
                    Box::new(Type::Variable("E".into())),
                )],
                vec![a.clone()],
            ),
        ),
        (
            "result-error".into(),
            pure(
                vec![Type::Result(
                    Box::new(Type::Variable("A".into())),
                    Box::new(Type::Variable("E".into())),
                )],
                vec![Type::Variable("E".into())],
            ),
        ),
        (
            "file-read".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::Bytes],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        // Bounded range reads keep large CSV/text processing out of the
        // model context and avoid materializing an entire file in the VM.
        // The path still determines the concrete `file.read` selector.
        (
            "file-size".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::Int],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "file-slice".into(),
            capability(
                vec![
                    Type::Path(FileSelector::parse("./**").expect("valid workspace root")),
                    Type::Int,
                    Type::Int,
                ],
                vec![Type::Bytes],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        // Line cursors are host-issued resources. Only `file-lines-open`
        // receives a path selector; `next`/`close` can operate under the
        // already-authorized opaque resource and cannot fabricate a path.
        (
            "file-lines-open".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::Resource("file-line-cursor".into())],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "file-lines-next".into(),
            capability(
                vec![Type::Resource("file-line-cursor".into())],
                vec![Type::Option(Box::new(Type::String))],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        (
            "file-lines-close".into(),
            capability(
                vec![Type::Resource("file-line-cursor".into())],
                vec![Type::Unit],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        // CSV records need their own cursor: quoted fields may legally span
        // physical lines, so a line cursor cannot safely model a CSV row.
        (
            "csv-open".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::Resource("csv-record-cursor".into())],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "csv-next".into(),
            capability(
                vec![Type::Resource("csv-record-cursor".into())],
                vec![Type::Option(Box::new(Type::list(Type::String)))],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        (
            "csv-close".into(),
            capability(
                vec![Type::Resource("csv-record-cursor".into())],
                vec![Type::Unit],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        (
            "file-write".into(),
            capability(
                vec![
                    Type::Path(FileSelector::parse("./**").expect("valid workspace root")),
                    Type::Bytes,
                ],
                vec![Type::Unit],
                CapabilityRequirement {
                    capability: CapabilityKind::FileWrite,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        // Whole-machine operations remain structurally distinct from their
        // workspace counterparts. A host adapter must explicitly install the
        // host root, and grants still have to cover the concrete selector.
        (
            "host-file-read".into(),
            capability(
                vec![Type::Path(host_path_selector())],
                vec![Type::Bytes],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: host_path_template(),
                    },
                },
            ),
        ),
        (
            "host-file-write".into(),
            capability(
                vec![Type::Path(host_path_selector()), Type::Bytes],
                vec![Type::Unit],
                CapabilityRequirement {
                    capability: CapabilityKind::FileWrite,
                    selector: ResourceSelector::FileTemplate {
                        template: host_path_template(),
                    },
                },
            ),
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
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "emit".into(),
            capability(
                vec![Type::String],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-open".into(),
            capability(
                vec![Type::String],
                vec![Type::Resource("output-handle".into())],
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-append".into(),
            capability(
                vec![Type::Resource("output-handle".into()), Type::String],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-replace".into(),
            capability(
                vec![Type::Resource("output-handle".into()), Type::String],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-status".into(),
            capability(
                vec![Type::Resource("output-handle".into()), Type::String],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-progress".into(),
            capability(
                vec![Type::Resource("output-handle".into()), Type::Int, Type::Int],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-complete".into(),
            capability(
                vec![Type::Resource("output-handle".into())],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-fail".into(),
            capability(
                vec![Type::Resource("output-handle".into()), Type::String],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "vm-vocabulary".into(),
            capability(
                Vec::new(),
                vec![Type::String],
                unscoped(CapabilityKind::VmRead),
            ),
        ),
        (
            "process-run".into(),
            capability(
                vec![Type::String, Type::list(Type::String)],
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::ProcessRun,
                    selector: ResourceSelector::ProcessTemplate {
                        template: ProcessSelectorTemplate {
                            executable_argument: 0,
                            allowed_executables: Vec::new(),
                        },
                    },
                },
            ),
        ),
        // A proposal is an explicit, user-editable artifact boundary.  The
        // source itself is just data here: accepting it never executes a
        // shell, Python, or Finch program implicitly.  The host returns
        // `none` for cancel, `some(ok source)` for execute, and
        // `some(err context)` for a request to continue the conversation.
        (
            "proposal-open".into(),
            capability(
                vec![Type::String, Type::String, Type::String],
                vec![Type::Option(Box::new(Type::Result(
                    Box::new(Type::String),
                    Box::new(Type::String),
                )))],
                CapabilityRequirement {
                    capability: CapabilityKind::ProgramInvoke,
                    selector: ResourceSelector::ProgramTemplate {
                        template: ProgramSelectorTemplate {
                            language_argument: 0,
                            allowed_languages: vec![
                                "bash".into(),
                                "sh".into(),
                                "shell".into(),
                                "python".into(),
                                "py".into(),
                                "lisp".into(),
                                "finch".into(),
                                "forth".into(),
                                "coforth".into(),
                                "co-forth".into(),
                                "text".into(),
                            ],
                        },
                    },
                },
            ),
        ),
        (
            "network-connect".into(),
            capability(
                vec![Type::String, Type::Int],
                vec![Type::Resource("network-socket".into())],
                CapabilityRequirement {
                    capability: CapabilityKind::NetworkConnect,
                    selector: ResourceSelector::NetworkTemplate {
                        template: NetworkSelectorTemplate {
                            host_argument: 0,
                            port_argument: 1,
                            allowed_hosts: Vec::new(),
                            allowed_ports: Vec::new(),
                        },
                    },
                },
            ),
        ),
        (
            "network-send".into(),
            capability(
                vec![Type::Resource("network-socket".into()), Type::Bytes],
                vec![Type::Bytes],
                unscoped(CapabilityKind::NetworkConnect),
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
                vec![Type::String, Type::Int],
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
                vec![Type::String],
                unscoped(CapabilityKind::AgentAwait),
            ),
        ),
        (
            "agent-poll".into(),
            capability(
                vec![Type::Task(Box::new(Type::String))],
                vec![Type::String],
                unscoped(CapabilityKind::AgentPoll),
            ),
        ),
        (
            "agent-cancel".into(),
            capability(
                vec![Type::Task(Box::new(Type::String))],
                vec![Type::Unit],
                unscoped(CapabilityKind::AgentCancel),
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

    #[test]
    fn language_schema_advertises_every_core_capability_kind() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../vocabulary/language/schema.json")).unwrap();
        let advertised = schema["$defs"]["capability"]["properties"]["capability"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        for capability in [
            CapabilityKind::VmRead,
            CapabilityKind::VmWrite,
            CapabilityKind::FileRead,
            CapabilityKind::FileWrite,
            CapabilityKind::NetworkConnect,
            CapabilityKind::AutomationInspect,
            CapabilityKind::AutomationWrite,
            CapabilityKind::AgentSpawn,
            CapabilityKind::AgentAwait,
            CapabilityKind::AgentPoll,
            CapabilityKind::AgentCancel,
            CapabilityKind::ProcessRun,
            CapabilityKind::SessionEmit,
            CapabilityKind::MemoryRead,
            CapabilityKind::MemoryWrite,
            CapabilityKind::MemoryConsolidate,
            CapabilityKind::ScheduleCreate,
            CapabilityKind::ScheduleRead,
            CapabilityKind::ScheduleManage,
            CapabilityKind::ProgramInvoke,
            CapabilityKind::UnsafeMemory,
        ] {
            let serialized = serde_json::to_value(capability).unwrap();
            assert!(advertised.contains(serialized.as_str().unwrap()));
        }
    }
}
