//! Exact Cap'n Proto codec for portable typed-runtime checkpoints.
//!
//! This module deliberately does not use an opaque serde/JSON payload. Every
//! reachable checkpoint value has a closed schema arm, and decoding fails on
//! unknown discriminants, invalid scalar values, duplicate keyed entries, and
//! host-width integer overflow.

use crate::ipc::schema::finch_ipc_capnp as wire;
use crate::vm::diagnostic::{
    DiagnosticPhase, Severity, SourceLanguage, SourceOrigin, SourceSpan, VmDiagnostic,
};
use crate::vm::effects::{
    CapabilityKind, CapabilityRequirement, EffectSet, FileSelector, FileSelectorTemplate,
    FileSelectorTemplatePart, McpSelectorTemplate, NetworkSelectorTemplate,
    ProcessSelectorTemplate, ProgramSelectorTemplate, ResourceRoot, ResourceSelector,
};
use crate::vm::interpreter::{
    HostSideEffect, UiOperation, UiProgress, VmContinuation, VmFrame, VmSideEffect,
};
use crate::vm::ir::{BasicBlock, Function, Instruction, LocatedInstruction, Module};
use crate::vm::runtime::{
    EffectJournalEntry, EffectJournalState, ProducerFiberRecord, ProducerFiberState,
    TypedRuntimeCheckpoint,
};
use crate::vm::signature::{ControlEffect, StackRow, StackSignature, SuspensionSignature};
use crate::vm::types::{TaskKind, Type, TypedValue};
use crate::vm::verifier::{VerifiedFunction, VerifiedModule};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};

const MAX_NESTING: usize = 128;

fn text(value: capnp::text::Reader<'_>) -> Result<String> {
    Ok(value.to_str()?.to_owned())
}

fn usize_from_wire(value: u64, field: &str) -> Result<usize> {
    usize::try_from(value).with_context(|| format!("{field} does not fit this host's usize"))
}

fn check_depth(depth: usize) -> Result<()> {
    if depth > MAX_NESTING {
        bail!("typed checkpoint exceeds the maximum nesting depth");
    }
    Ok(())
}

fn encode_text_list(mut builder: capnp::text_list::Builder<'_>, values: &[String]) {
    for (index, value) in values.iter().enumerate() {
        builder.set(index as u32, value);
    }
}

fn decode_text_list(reader: capnp::text_list::Reader<'_>) -> Result<Vec<String>> {
    reader.iter().map(|value| text(value?)).collect()
}

fn encode_resource_root(mut builder: wire::resource_root::Builder<'_>, root: &ResourceRoot) {
    match root {
        ResourceRoot::Workspace => builder.set_kind(wire::ResourceRootKind::Workspace),
        ResourceRoot::Project => builder.set_kind(wire::ResourceRootKind::Project),
        ResourceRoot::TaskOutput => builder.set_kind(wire::ResourceRootKind::TaskOutput),
        ResourceRoot::HostMachine => builder.set_kind(wire::ResourceRootKind::HostMachine),
        ResourceRoot::Named(name) => {
            builder.set_kind(wire::ResourceRootKind::Named);
            builder.set_name(name);
        }
    }
}

fn decode_resource_root(reader: wire::resource_root::Reader<'_>) -> Result<ResourceRoot> {
    Ok(match reader.get_kind()? {
        wire::ResourceRootKind::Workspace => ResourceRoot::Workspace,
        wire::ResourceRootKind::Project => ResourceRoot::Project,
        wire::ResourceRootKind::TaskOutput => ResourceRoot::TaskOutput,
        wire::ResourceRootKind::HostMachine => ResourceRoot::HostMachine,
        wire::ResourceRootKind::Named => ResourceRoot::Named(text(reader.get_name()?)?),
    })
}

fn encode_file_selector(mut builder: wire::file_selector::Builder<'_>, value: &FileSelector) {
    encode_resource_root(builder.reborrow().init_root(), &value.root);
    builder.set_pattern(&value.pattern);
}

fn decode_file_selector(reader: wire::file_selector::Reader<'_>) -> Result<FileSelector> {
    Ok(FileSelector {
        root: decode_resource_root(reader.get_root()?)?,
        pattern: text(reader.get_pattern()?)?,
    })
}

fn encode_file_template(
    mut builder: wire::file_selector_template::Builder<'_>,
    value: &FileSelectorTemplate,
) {
    encode_resource_root(builder.reborrow().init_root(), &value.root);
    let mut parts = builder.reborrow().init_parts(value.parts.len() as u32);
    for (index, part) in value.parts.iter().enumerate() {
        let mut encoded = parts.reborrow().get(index as u32);
        match part {
            FileSelectorTemplatePart::Literal { relative } => encoded.set_literal(relative),
            FileSelectorTemplatePart::Argument { index, bound } => {
                let mut argument = encoded.reborrow().init_argument();
                argument.set_index(*index as u64);
                encode_file_selector(argument.reborrow().init_bound(), bound);
            }
        }
    }
    encode_file_selector(builder.reborrow().init_upper_bound(), &value.upper_bound);
}

fn decode_file_template(
    reader: wire::file_selector_template::Reader<'_>,
) -> Result<FileSelectorTemplate> {
    let mut parts = Vec::new();
    for part in reader.get_parts()?.iter() {
        use wire::file_selector_template_part::Which;
        parts.push(match part.which()? {
            Which::Literal(value) => FileSelectorTemplatePart::Literal {
                relative: text(value?)?,
            },
            Which::Argument(value) => {
                let value = value?;
                FileSelectorTemplatePart::Argument {
                    index: usize_from_wire(value.get_index(), "file template argument index")?,
                    bound: decode_file_selector(value.get_bound()?)?,
                }
            }
        });
    }
    Ok(FileSelectorTemplate {
        root: decode_resource_root(reader.get_root()?)?,
        parts,
        upper_bound: decode_file_selector(reader.get_upper_bound()?)?,
    })
}

fn capability_to_wire(value: &CapabilityKind) -> wire::CapabilityKind {
    match value {
        CapabilityKind::VmRead => wire::CapabilityKind::VmRead,
        CapabilityKind::VmWrite => wire::CapabilityKind::VmWrite,
        CapabilityKind::FileRead => wire::CapabilityKind::FileRead,
        CapabilityKind::FileWrite => wire::CapabilityKind::FileWrite,
        CapabilityKind::NetworkConnect => wire::CapabilityKind::NetworkConnect,
        CapabilityKind::AutomationInspect => wire::CapabilityKind::AutomationInspect,
        CapabilityKind::AutomationWrite => wire::CapabilityKind::AutomationWrite,
        CapabilityKind::AgentSpawn => wire::CapabilityKind::AgentSpawn,
        CapabilityKind::AgentAwait => wire::CapabilityKind::AgentAwait,
        CapabilityKind::AgentPoll => wire::CapabilityKind::AgentPoll,
        CapabilityKind::AgentCancel => wire::CapabilityKind::AgentCancel,
        CapabilityKind::ProcessRun => wire::CapabilityKind::ProcessRun,
        CapabilityKind::SessionEmit => wire::CapabilityKind::SessionEmit,
        CapabilityKind::MemoryRead => wire::CapabilityKind::MemoryRead,
        CapabilityKind::MemoryWrite => wire::CapabilityKind::MemoryWrite,
        CapabilityKind::MemoryConsolidate => wire::CapabilityKind::MemoryConsolidate,
        CapabilityKind::ScheduleCreate => wire::CapabilityKind::ScheduleCreate,
        CapabilityKind::ScheduleRead => wire::CapabilityKind::ScheduleRead,
        CapabilityKind::ScheduleManage => wire::CapabilityKind::ScheduleManage,
        CapabilityKind::ProgramInvoke => wire::CapabilityKind::ProgramInvoke,
        CapabilityKind::McpCall => wire::CapabilityKind::McpCall,
        CapabilityKind::UnsafeMemory => wire::CapabilityKind::UnsafeMemory,
    }
}

fn capability_from_wire(value: wire::CapabilityKind) -> CapabilityKind {
    match value {
        wire::CapabilityKind::VmRead => CapabilityKind::VmRead,
        wire::CapabilityKind::VmWrite => CapabilityKind::VmWrite,
        wire::CapabilityKind::FileRead => CapabilityKind::FileRead,
        wire::CapabilityKind::FileWrite => CapabilityKind::FileWrite,
        wire::CapabilityKind::NetworkConnect => CapabilityKind::NetworkConnect,
        wire::CapabilityKind::AutomationInspect => CapabilityKind::AutomationInspect,
        wire::CapabilityKind::AutomationWrite => CapabilityKind::AutomationWrite,
        wire::CapabilityKind::AgentSpawn => CapabilityKind::AgentSpawn,
        wire::CapabilityKind::AgentAwait => CapabilityKind::AgentAwait,
        wire::CapabilityKind::AgentPoll => CapabilityKind::AgentPoll,
        wire::CapabilityKind::AgentCancel => CapabilityKind::AgentCancel,
        wire::CapabilityKind::ProcessRun => CapabilityKind::ProcessRun,
        wire::CapabilityKind::SessionEmit => CapabilityKind::SessionEmit,
        wire::CapabilityKind::MemoryRead => CapabilityKind::MemoryRead,
        wire::CapabilityKind::MemoryWrite => CapabilityKind::MemoryWrite,
        wire::CapabilityKind::MemoryConsolidate => CapabilityKind::MemoryConsolidate,
        wire::CapabilityKind::ScheduleCreate => CapabilityKind::ScheduleCreate,
        wire::CapabilityKind::ScheduleRead => CapabilityKind::ScheduleRead,
        wire::CapabilityKind::ScheduleManage => CapabilityKind::ScheduleManage,
        wire::CapabilityKind::ProgramInvoke => CapabilityKind::ProgramInvoke,
        wire::CapabilityKind::McpCall => CapabilityKind::McpCall,
        wire::CapabilityKind::UnsafeMemory => CapabilityKind::UnsafeMemory,
    }
}

fn encode_selector(mut builder: wire::resource_selector::Builder<'_>, value: &ResourceSelector) {
    match value {
        ResourceSelector::None => builder.set_none(()),
        ResourceSelector::File { selector } => {
            encode_file_selector(builder.reborrow().init_file(), selector)
        }
        ResourceSelector::FileTemplate { template } => {
            encode_file_template(builder.reborrow().init_file_template(), template)
        }
        ResourceSelector::NetworkTemplate { template } => {
            let mut encoded = builder.reborrow().init_network_template();
            encoded.set_host_argument(template.host_argument as u64);
            encoded.set_port_argument(template.port_argument as u64);
            encode_text_list(
                encoded
                    .reborrow()
                    .init_allowed_hosts(template.allowed_hosts.len() as u32),
                &template.allowed_hosts,
            );
            let mut ports = encoded
                .reborrow()
                .init_allowed_ports(template.allowed_ports.len() as u32);
            for (index, port) in template.allowed_ports.iter().enumerate() {
                ports.set(index as u32, *port);
            }
        }
        ResourceSelector::Network { host, ports } => {
            let mut encoded = builder.reborrow().init_network();
            encoded.set_host(host);
            let mut values = encoded.reborrow().init_ports(ports.len() as u32);
            for (index, port) in ports.iter().enumerate() {
                values.set(index as u32, *port);
            }
        }
        ResourceSelector::Automation { application } => {
            let mut encoded = builder.reborrow().init_automation();
            encoded.set_has_application(application.is_some());
            if let Some(application) = application {
                encoded.set_application(application);
            }
        }
        ResourceSelector::Agent {
            providers,
            max_depth,
            max_children,
        } => {
            let mut encoded = builder.reborrow().init_agent();
            encode_text_list(
                encoded.reborrow().init_providers(providers.len() as u32),
                providers,
            );
            encoded.set_max_depth(*max_depth);
            encoded.set_max_children(*max_children);
        }
        ResourceSelector::Process { executables } => encode_text_list(
            builder.reborrow().init_process(executables.len() as u32),
            executables,
        ),
        ResourceSelector::ProcessTemplate { template } => {
            let mut encoded = builder.reborrow().init_process_template();
            encoded.set_executable_argument(template.executable_argument as u64);
            encode_text_list(
                encoded
                    .reborrow()
                    .init_allowed_executables(template.allowed_executables.len() as u32),
                &template.allowed_executables,
            );
        }
        ResourceSelector::Program { languages } => encode_text_list(
            builder.reborrow().init_program(languages.len() as u32),
            languages,
        ),
        ResourceSelector::ProgramTemplate { template } => {
            let mut encoded = builder.reborrow().init_program_template();
            encoded.set_language_argument(template.language_argument as u64);
            encode_text_list(
                encoded
                    .reborrow()
                    .init_allowed_languages(template.allowed_languages.len() as u32),
                &template.allowed_languages,
            );
        }
        ResourceSelector::Mcp { server, tool } => {
            let mut encoded = builder.reborrow().init_mcp();
            encoded.set_server(server);
            encoded.set_tool(tool);
        }
        ResourceSelector::McpTemplate { template } => {
            let mut encoded = builder.reborrow().init_mcp_template();
            encoded.set_server_argument(template.server_argument as u64);
            encoded.set_tool_argument(template.tool_argument as u64);
            encode_text_list(
                encoded
                    .reborrow()
                    .init_allowed_servers(template.allowed_servers.len() as u32),
                &template.allowed_servers,
            );
            encode_text_list(
                encoded
                    .reborrow()
                    .init_allowed_tools(template.allowed_tools.len() as u32),
                &template.allowed_tools,
            );
        }
        ResourceSelector::Memory { tree, path } => {
            let mut encoded = builder.reborrow().init_memory();
            encoded.set_tree(tree);
            encoded.set_path(path);
        }
        ResourceSelector::Schedule { policy } => {
            let mut encoded = builder.reborrow().init_schedule();
            encoded.set_has_policy(policy.is_some());
            if let Some(policy) = policy {
                encoded.set_policy(policy);
            }
        }
    }
}

fn decode_selector(reader: wire::resource_selector::Reader<'_>) -> Result<ResourceSelector> {
    use wire::resource_selector::Which;
    Ok(match reader.which()? {
        Which::None(()) => ResourceSelector::None,
        Which::File(value) => ResourceSelector::File {
            selector: decode_file_selector(value?)?,
        },
        Which::FileTemplate(value) => ResourceSelector::FileTemplate {
            template: decode_file_template(value?)?,
        },
        Which::NetworkTemplate(value) => {
            let value = value?;
            ResourceSelector::NetworkTemplate {
                template: NetworkSelectorTemplate {
                    host_argument: usize_from_wire(
                        value.get_host_argument(),
                        "network template host argument",
                    )?,
                    port_argument: usize_from_wire(
                        value.get_port_argument(),
                        "network template port argument",
                    )?,
                    allowed_hosts: decode_text_list(value.get_allowed_hosts()?)?,
                    allowed_ports: value.get_allowed_ports()?.iter().collect(),
                },
            }
        }
        Which::Network(value) => {
            let value = value?;
            ResourceSelector::Network {
                host: text(value.get_host()?)?,
                ports: value.get_ports()?.iter().collect(),
            }
        }
        Which::Automation(value) => {
            let value = value?;
            ResourceSelector::Automation {
                application: value
                    .get_has_application()
                    .then(|| text(value.get_application()?))
                    .transpose()?,
            }
        }
        Which::Agent(value) => {
            let value = value?;
            ResourceSelector::Agent {
                providers: decode_text_list(value.get_providers()?)?,
                max_depth: value.get_max_depth(),
                max_children: value.get_max_children(),
            }
        }
        Which::Process(value) => ResourceSelector::Process {
            executables: decode_text_list(value?)?,
        },
        Which::ProcessTemplate(value) => {
            let value = value?;
            ResourceSelector::ProcessTemplate {
                template: ProcessSelectorTemplate {
                    executable_argument: usize_from_wire(
                        value.get_executable_argument(),
                        "process template executable argument",
                    )?,
                    allowed_executables: decode_text_list(value.get_allowed_executables()?)?,
                },
            }
        }
        Which::Program(value) => ResourceSelector::Program {
            languages: decode_text_list(value?)?,
        },
        Which::ProgramTemplate(value) => {
            let value = value?;
            ResourceSelector::ProgramTemplate {
                template: ProgramSelectorTemplate {
                    language_argument: usize_from_wire(
                        value.get_language_argument(),
                        "program template language argument",
                    )?,
                    allowed_languages: decode_text_list(value.get_allowed_languages()?)?,
                },
            }
        }
        Which::Mcp(value) => {
            let value = value?;
            ResourceSelector::Mcp {
                server: text(value.get_server()?)?,
                tool: text(value.get_tool()?)?,
            }
        }
        Which::McpTemplate(value) => {
            let value = value?;
            ResourceSelector::McpTemplate {
                template: McpSelectorTemplate {
                    server_argument: usize_from_wire(
                        value.get_server_argument(),
                        "MCP template server argument",
                    )?,
                    tool_argument: usize_from_wire(
                        value.get_tool_argument(),
                        "MCP template tool argument",
                    )?,
                    allowed_servers: decode_text_list(value.get_allowed_servers()?)?,
                    allowed_tools: decode_text_list(value.get_allowed_tools()?)?,
                },
            }
        }
        Which::Memory(value) => {
            let value = value?;
            ResourceSelector::Memory {
                tree: text(value.get_tree()?)?,
                path: text(value.get_path()?)?,
            }
        }
        Which::Schedule(value) => {
            let value = value?;
            ResourceSelector::Schedule {
                policy: value
                    .get_has_policy()
                    .then(|| text(value.get_policy()?))
                    .transpose()?,
            }
        }
    })
}

fn encode_requirement(
    mut builder: wire::capability_requirement::Builder<'_>,
    value: &CapabilityRequirement,
) {
    builder.set_capability(capability_to_wire(&value.capability));
    encode_selector(builder.reborrow().init_selector(), &value.selector);
}

fn decode_requirement(
    reader: wire::capability_requirement::Reader<'_>,
) -> Result<CapabilityRequirement> {
    Ok(CapabilityRequirement {
        capability: capability_from_wire(reader.get_capability()?),
        selector: decode_selector(reader.get_selector()?)?,
    })
}

pub(crate) fn encode_effects(
    mut builder: capnp::struct_list::Builder<'_, wire::capability_requirement::Owned>,
    value: &EffectSet,
) {
    for (index, requirement) in value.0.iter().enumerate() {
        encode_requirement(builder.reborrow().get(index as u32), requirement);
    }
}

pub(crate) fn decode_effects(
    reader: capnp::struct_list::Reader<'_, wire::capability_requirement::Owned>,
) -> Result<EffectSet> {
    let mut values = BTreeSet::new();
    for requirement in reader.iter() {
        let requirement = decode_requirement(requirement)?;
        if !values.insert(requirement) {
            bail!("typed checkpoint contains a duplicate capability requirement");
        }
    }
    Ok(EffectSet(values))
}

fn encode_suspension(
    mut builder: wire::suspension_signature::Builder<'_>,
    value: &SuspensionSignature,
    depth: usize,
) -> Result<()> {
    encode_type(
        builder.reborrow().init_yield_type(),
        &value.yield_type,
        depth + 1,
    )?;
    encode_type(
        builder.reborrow().init_resume_type(),
        &value.resume_type,
        depth + 1,
    )
}

fn decode_suspension(
    reader: wire::suspension_signature::Reader<'_>,
    depth: usize,
) -> Result<SuspensionSignature> {
    Ok(SuspensionSignature {
        yield_type: Box::new(decode_type(reader.get_yield_type()?, depth + 1)?),
        resume_type: Box::new(decode_type(reader.get_resume_type()?, depth + 1)?),
    })
}

fn encode_type_list(
    mut builder: capnp::struct_list::Builder<'_, wire::typed_type::Owned>,
    values: &[Type],
    depth: usize,
) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        encode_type(builder.reborrow().get(index as u32), value, depth + 1)?;
    }
    Ok(())
}

fn decode_type_list(
    reader: capnp::struct_list::Reader<'_, wire::typed_type::Owned>,
    depth: usize,
) -> Result<Vec<Type>> {
    reader
        .iter()
        .map(|value| decode_type(value, depth + 1))
        .collect()
}

fn encode_fields(
    mut builder: capnp::struct_list::Builder<'_, wire::typed_field::Owned>,
    fields: &[(String, Type)],
    depth: usize,
) -> Result<()> {
    for (index, (name, value_type)) in fields.iter().enumerate() {
        let mut field = builder.reborrow().get(index as u32);
        field.set_name(name);
        encode_type(field.reborrow().init_type(), value_type, depth + 1)?;
    }
    Ok(())
}

fn decode_fields(
    reader: capnp::struct_list::Reader<'_, wire::typed_field::Owned>,
    depth: usize,
) -> Result<Vec<(String, Type)>> {
    let mut names = BTreeSet::new();
    let mut fields = Vec::with_capacity(reader.len() as usize);
    for field in reader.iter() {
        let name = text(field.get_name()?)?;
        if !names.insert(name.clone()) {
            bail!("typed checkpoint contains duplicate field {name:?}");
        }
        fields.push((name, decode_type(field.get_type()?, depth + 1)?));
    }
    Ok(fields)
}

fn encode_variants(
    mut builder: capnp::struct_list::Builder<'_, wire::typed_variant::Owned>,
    variants: &[(String, Option<Type>)],
    depth: usize,
) -> Result<()> {
    for (index, (name, payload)) in variants.iter().enumerate() {
        let mut variant = builder.reborrow().get(index as u32);
        variant.set_name(name);
        variant.set_has_payload(payload.is_some());
        if let Some(payload) = payload {
            encode_type(variant.reborrow().init_payload(), payload, depth + 1)?;
        }
    }
    Ok(())
}

fn decode_variants(
    reader: capnp::struct_list::Reader<'_, wire::typed_variant::Owned>,
    depth: usize,
) -> Result<Vec<(String, Option<Type>)>> {
    let mut names = BTreeSet::new();
    let mut variants = Vec::with_capacity(reader.len() as usize);
    for variant in reader.iter() {
        let name = text(variant.get_name()?)?;
        if !names.insert(name.clone()) {
            bail!("typed checkpoint contains duplicate variant {name:?}");
        }
        let payload = variant
            .get_has_payload()
            .then(|| decode_type(variant.get_payload()?, depth + 1))
            .transpose()?;
        variants.push((name, payload));
    }
    Ok(variants)
}

fn encode_type(
    mut builder: wire::typed_type::Builder<'_>,
    value: &Type,
    depth: usize,
) -> Result<()> {
    check_depth(depth)?;
    match value {
        Type::Unit => builder.set_unit(()),
        Type::Bool => builder.set_bool_type(()),
        Type::Int => builder.set_int_type(()),
        Type::UInt => builder.set_uint_type(()),
        Type::Float => builder.set_float_type(()),
        Type::Char => builder.set_char_type(()),
        Type::Symbol => builder.set_symbol_type(()),
        Type::String => builder.set_string_type(()),
        Type::Bytes => builder.set_bytes_type(()),
        Type::Json => builder.set_json_type(()),
        Type::Path(selector) => encode_file_selector(builder.reborrow().init_path(), selector),
        Type::List(element) => encode_type(builder.reborrow().init_list(), element, depth + 1)?,
        Type::Map(key, value) => {
            let mut encoded = builder.reborrow().init_map();
            encode_type(encoded.reborrow().init_key(), key, depth + 1)?;
            encode_type(encoded.reborrow().init_value(), value, depth + 1)?;
        }
        Type::Option(inner) => encode_type(builder.reborrow().init_option(), inner, depth + 1)?,
        Type::Result(ok, error) => {
            let mut encoded = builder.reborrow().init_result();
            encode_type(encoded.reborrow().init_ok(), ok, depth + 1)?;
            encode_type(encoded.reborrow().init_error(), error, depth + 1)?;
        }
        Type::Record(fields) => encode_fields(
            builder.reborrow().init_record(fields.len() as u32),
            fields,
            depth + 1,
        )?,
        Type::Variant(variants) => encode_variants(
            builder.reborrow().init_variant(variants.len() as u32),
            variants,
            depth + 1,
        )?,
        Type::Function {
            arguments,
            result,
            effects,
            suspension,
        } => {
            let mut encoded = builder.reborrow().init_function();
            encode_type_list(
                encoded.reborrow().init_arguments(arguments.len() as u32),
                arguments,
                depth + 1,
            )?;
            encode_type(encoded.reborrow().init_result(), result, depth + 1)?;
            encode_effects(
                encoded.reborrow().init_effects(effects.0.len() as u32),
                effects,
            );
            encoded.set_has_suspension(suspension.is_some());
            if let Some(suspension) = suspension {
                encode_suspension(encoded.reborrow().init_suspension(), suspension, depth + 1)?;
            }
        }
        Type::Task(result) => encode_type(builder.reborrow().init_task(), result, depth + 1)?,
        Type::Fiber(yield_type, result_type) => {
            let mut encoded = builder.reborrow().init_fiber();
            encode_type(encoded.reborrow().init_yield_type(), yield_type, depth + 1)?;
            encode_type(
                encoded.reborrow().init_result_type(),
                result_type,
                depth + 1,
            )?;
        }
        Type::Stream(element) => encode_type(builder.reborrow().init_stream(), element, depth + 1)?,
        Type::Resource(kind) => builder.set_resource(kind),
        Type::Capability(kind) => builder.set_capability(kind),
        Type::Variable(name) => builder.set_variable(name),
        Type::Dynamic => builder.set_dynamic_type(()),
    }
    Ok(())
}

fn decode_type(reader: wire::typed_type::Reader<'_>, depth: usize) -> Result<Type> {
    check_depth(depth)?;
    use wire::typed_type::Which;
    Ok(match reader.which()? {
        Which::Unit(()) => Type::Unit,
        Which::BoolType(()) => Type::Bool,
        Which::IntType(()) => Type::Int,
        Which::UintType(()) => Type::UInt,
        Which::FloatType(()) => Type::Float,
        Which::CharType(()) => Type::Char,
        Which::SymbolType(()) => Type::Symbol,
        Which::StringType(()) => Type::String,
        Which::BytesType(()) => Type::Bytes,
        Which::JsonType(()) => Type::Json,
        Which::Path(value) => Type::Path(decode_file_selector(value?)?),
        Which::List(value) => Type::List(Box::new(decode_type(value?, depth + 1)?)),
        Which::Map(value) => {
            let value = value?;
            Type::Map(
                Box::new(decode_type(value.get_key()?, depth + 1)?),
                Box::new(decode_type(value.get_value()?, depth + 1)?),
            )
        }
        Which::Option(value) => Type::Option(Box::new(decode_type(value?, depth + 1)?)),
        Which::Result(value) => {
            let value = value?;
            Type::Result(
                Box::new(decode_type(value.get_ok()?, depth + 1)?),
                Box::new(decode_type(value.get_error()?, depth + 1)?),
            )
        }
        Which::Record(value) => Type::Record(decode_fields(value?, depth + 1)?),
        Which::Variant(value) => Type::Variant(decode_variants(value?, depth + 1)?),
        Which::Function(value) => {
            let value = value?;
            Type::Function {
                arguments: decode_type_list(value.get_arguments()?, depth + 1)?,
                result: Box::new(decode_type(value.get_result()?, depth + 1)?),
                effects: decode_effects(value.get_effects()?)?,
                suspension: value
                    .get_has_suspension()
                    .then(|| decode_suspension(value.get_suspension()?, depth + 1))
                    .transpose()?,
            }
        }
        Which::Task(value) => Type::Task(Box::new(decode_type(value?, depth + 1)?)),
        Which::Fiber(value) => {
            let value = value?;
            Type::Fiber(
                Box::new(decode_type(value.get_yield_type()?, depth + 1)?),
                Box::new(decode_type(value.get_result_type()?, depth + 1)?),
            )
        }
        Which::Stream(value) => Type::Stream(Box::new(decode_type(value?, depth + 1)?)),
        Which::Resource(value) => Type::Resource(text(value?)?),
        Which::Capability(value) => Type::Capability(text(value?)?),
        Which::Variable(value) => Type::Variable(text(value?)?),
        Which::DynamicType(()) => Type::Dynamic,
    })
}

fn control_to_wire(value: ControlEffect) -> wire::ControlEffect {
    match value {
        ControlEffect::Returns => wire::ControlEffect::Returns,
        ControlEffect::MayThrow => wire::ControlEffect::MayThrow,
        ControlEffect::MaySuspend => wire::ControlEffect::MaySuspend,
        ControlEffect::NeverReturns => wire::ControlEffect::NeverReturns,
    }
}

fn control_from_wire(value: wire::ControlEffect) -> ControlEffect {
    match value {
        wire::ControlEffect::Returns => ControlEffect::Returns,
        wire::ControlEffect::MayThrow => ControlEffect::MayThrow,
        wire::ControlEffect::MaySuspend => ControlEffect::MaySuspend,
        wire::ControlEffect::NeverReturns => ControlEffect::NeverReturns,
    }
}

fn encode_stack_row(
    mut builder: wire::stack_row::Builder<'_>,
    value: &StackRow,
    depth: usize,
) -> Result<()> {
    builder.set_has_tail(value.tail.is_some());
    if let Some(tail) = &value.tail {
        builder.set_tail(tail);
    }
    encode_type_list(
        builder.reborrow().init_values(value.values.len() as u32),
        &value.values,
        depth + 1,
    )
}

fn decode_stack_row(reader: wire::stack_row::Reader<'_>, depth: usize) -> Result<StackRow> {
    Ok(StackRow {
        tail: reader
            .get_has_tail()
            .then(|| text(reader.get_tail()?))
            .transpose()?,
        values: decode_type_list(reader.get_values()?, depth + 1)?,
    })
}

fn encode_signature(
    mut builder: wire::stack_signature::Builder<'_>,
    value: &StackSignature,
    depth: usize,
) -> Result<()> {
    check_depth(depth)?;
    encode_text_list(
        builder
            .reborrow()
            .init_type_parameters(value.type_parameters.len() as u32),
        &value.type_parameters,
    );
    encode_stack_row(builder.reborrow().init_input(), &value.input, depth + 1)?;
    encode_stack_row(builder.reborrow().init_output(), &value.output, depth + 1)?;
    encode_effects(
        builder
            .reborrow()
            .init_effects(value.effects.0.len() as u32),
        &value.effects,
    );
    builder.set_control(control_to_wire(value.control));
    builder.set_has_suspension(value.suspension.is_some());
    if let Some(suspension) = &value.suspension {
        encode_suspension(builder.reborrow().init_suspension(), suspension, depth + 1)?;
    }
    Ok(())
}

fn decode_signature(
    reader: wire::stack_signature::Reader<'_>,
    depth: usize,
) -> Result<StackSignature> {
    check_depth(depth)?;
    Ok(StackSignature {
        type_parameters: decode_text_list(reader.get_type_parameters()?)?,
        input: decode_stack_row(reader.get_input()?, depth + 1)?,
        output: decode_stack_row(reader.get_output()?, depth + 1)?,
        effects: decode_effects(reader.get_effects()?)?,
        control: control_from_wire(reader.get_control()?),
        suspension: reader
            .get_has_suspension()
            .then(|| decode_suspension(reader.get_suspension()?, depth + 1))
            .transpose()?,
    })
}

fn language_to_wire(value: SourceLanguage) -> wire::SourceLanguage {
    match value {
        SourceLanguage::Forth => wire::SourceLanguage::Forth,
        SourceLanguage::Lisp => wire::SourceLanguage::Lisp,
        SourceLanguage::FinchIr => wire::SourceLanguage::FinchIr,
        SourceLanguage::Native => wire::SourceLanguage::Native,
        SourceLanguage::Provider => wire::SourceLanguage::Provider,
    }
}

fn language_from_wire(value: wire::SourceLanguage) -> SourceLanguage {
    match value {
        wire::SourceLanguage::Forth => SourceLanguage::Forth,
        wire::SourceLanguage::Lisp => SourceLanguage::Lisp,
        wire::SourceLanguage::FinchIr => SourceLanguage::FinchIr,
        wire::SourceLanguage::Native => SourceLanguage::Native,
        wire::SourceLanguage::Provider => SourceLanguage::Provider,
    }
}

fn encode_span(mut builder: wire::source_span::Builder<'_>, value: &SourceSpan) {
    builder.set_source_id(&value.source_id);
    builder.set_start_byte(value.start_byte as u64);
    builder.set_end_byte(value.end_byte as u64);
    builder.set_start_line(value.start_line as u64);
    builder.set_start_column(value.start_column as u64);
    builder.set_end_line(value.end_line as u64);
    builder.set_end_column(value.end_column as u64);
}

fn decode_span(reader: wire::source_span::Reader<'_>) -> Result<SourceSpan> {
    Ok(SourceSpan {
        source_id: text(reader.get_source_id()?)?,
        start_byte: usize_from_wire(reader.get_start_byte(), "source start byte")?,
        end_byte: usize_from_wire(reader.get_end_byte(), "source end byte")?,
        start_line: usize_from_wire(reader.get_start_line(), "source start line")?,
        start_column: usize_from_wire(reader.get_start_column(), "source start column")?,
        end_line: usize_from_wire(reader.get_end_line(), "source end line")?,
        end_column: usize_from_wire(reader.get_end_column(), "source end column")?,
    })
}

fn encode_origin(
    mut builder: wire::source_origin::Builder<'_>,
    value: &SourceOrigin,
    depth: usize,
) -> Result<()> {
    check_depth(depth)?;
    builder.set_language(language_to_wire(value.language));
    builder.set_has_span(value.span.is_some());
    if let Some(span) = &value.span {
        encode_span(builder.reborrow().init_span(), span);
    }
    builder.set_has_word(value.word.is_some());
    if let Some(word) = &value.word {
        builder.set_word(word);
    }
    builder.set_has_expansion(value.expansion.is_some());
    if let Some(expansion) = &value.expansion {
        encode_origin(builder.reborrow().init_expansion(), expansion, depth + 1)?;
    }
    Ok(())
}

fn decode_origin(reader: wire::source_origin::Reader<'_>, depth: usize) -> Result<SourceOrigin> {
    check_depth(depth)?;
    Ok(SourceOrigin {
        language: language_from_wire(reader.get_language()?),
        span: reader
            .get_has_span()
            .then(|| decode_span(reader.get_span()?))
            .transpose()?,
        word: reader
            .get_has_word()
            .then(|| text(reader.get_word()?))
            .transpose()?,
        expansion: reader
            .get_has_expansion()
            .then(|| decode_origin(reader.get_expansion()?, depth + 1).map(Box::new))
            .transpose()?,
    })
}

fn task_kind_to_wire(value: TaskKind) -> wire::TaskKind {
    match value {
        TaskKind::Agent => wire::TaskKind::Agent,
        TaskKind::CpuFiber => wire::TaskKind::CpuFiber,
    }
}

fn task_kind_from_wire(value: wire::TaskKind) -> TaskKind {
    match value {
        wire::TaskKind::Agent => TaskKind::Agent,
        wire::TaskKind::CpuFiber => TaskKind::CpuFiber,
    }
}

pub(super) fn encode_value_list(
    mut builder: capnp::struct_list::Builder<'_, wire::typed_value::Owned>,
    values: &[TypedValue],
    depth: usize,
) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        encode_value(builder.reborrow().get(index as u32), value, depth + 1)?;
    }
    Ok(())
}

pub(super) fn decode_value_list(
    reader: capnp::struct_list::Reader<'_, wire::typed_value::Owned>,
    depth: usize,
) -> Result<Vec<TypedValue>> {
    reader
        .iter()
        .map(|value| decode_value(value, depth + 1))
        .collect()
}

fn encode_value(
    mut builder: wire::typed_value::Builder<'_>,
    value: &TypedValue,
    depth: usize,
) -> Result<()> {
    check_depth(depth)?;
    match value {
        TypedValue::Unit => builder.set_unit(()),
        TypedValue::Bool(value) => builder.set_bool_value(*value),
        TypedValue::Int(value) => builder.set_int_value(*value),
        TypedValue::UInt(value) => builder.set_uint_value(*value),
        TypedValue::Float(value) => builder.set_float_value(*value),
        TypedValue::Char(value) => builder.set_char_value(*value as u32),
        TypedValue::Symbol(value) => builder.set_symbol(value),
        TypedValue::String(value) => builder.set_string(value),
        TypedValue::Bytes(value) => builder.set_bytes(value),
        TypedValue::Json(value) => {
            super::brain_codec::encode_json_value(builder.reborrow().init_json(), value)?
        }
        TypedValue::Path { selector, relative } => {
            let mut encoded = builder.reborrow().init_path();
            encode_file_selector(encoded.reborrow().init_selector(), selector);
            encoded.set_relative(relative);
        }
        TypedValue::List {
            element_type,
            values,
        } => {
            let mut encoded = builder.reborrow().init_list();
            encode_type(
                encoded.reborrow().init_element_type(),
                element_type,
                depth + 1,
            )?;
            encode_value_list(
                encoded.reborrow().init_values(values.len() as u32),
                values,
                depth + 1,
            )?;
        }
        TypedValue::Map {
            key_type,
            value_type,
            entries,
        } => {
            let mut encoded = builder.reborrow().init_map();
            encode_type(encoded.reborrow().init_key_type(), key_type, depth + 1)?;
            encode_type(encoded.reborrow().init_value_type(), value_type, depth + 1)?;
            let mut encoded_entries = encoded.reborrow().init_entries(entries.len() as u32);
            for (index, (key, value)) in entries.iter().enumerate() {
                let mut entry = encoded_entries.reborrow().get(index as u32);
                encode_value(entry.reborrow().init_key(), key, depth + 1)?;
                encode_value(entry.reborrow().init_value(), value, depth + 1)?;
            }
        }
        TypedValue::Option { inner_type, value } => {
            let mut encoded = builder.reborrow().init_option();
            encode_type(encoded.reborrow().init_inner_type(), inner_type, depth + 1)?;
            encoded.set_has_value(value.is_some());
            if let Some(value) = value {
                encode_value(encoded.reborrow().init_value(), value, depth + 1)?;
            }
        }
        TypedValue::Result {
            ok_type,
            error_type,
            is_ok,
            value,
        } => {
            let mut encoded = builder.reborrow().init_result();
            encode_type(encoded.reborrow().init_ok_type(), ok_type, depth + 1)?;
            encode_type(encoded.reborrow().init_error_type(), error_type, depth + 1)?;
            encoded.set_is_ok(*is_ok);
            encode_value(encoded.reborrow().init_value(), value, depth + 1)?;
        }
        TypedValue::Record(fields) => {
            let mut encoded = builder.reborrow().init_record(fields.len() as u32);
            for (index, (name, value)) in fields.iter().enumerate() {
                let mut field = encoded.reborrow().get(index as u32);
                field.set_name(name);
                encode_value(field.reborrow().init_value(), value, depth + 1)?;
            }
        }
        TypedValue::Variant { name, value } => {
            let mut encoded = builder.reborrow().init_variant();
            encoded.set_name(name);
            encoded.set_has_value(value.is_some());
            if let Some(value) = value {
                encode_value(encoded.reborrow().init_value(), value, depth + 1)?;
            }
        }
        TypedValue::Closure {
            function,
            captures,
            signature,
        } => {
            let mut encoded = builder.reborrow().init_closure();
            encoded.set_function(function);
            encode_value_list(
                encoded.reborrow().init_captures(captures.len() as u32),
                captures,
                depth + 1,
            )?;
            encode_signature(encoded.reborrow().init_signature(), signature, depth + 1)?;
        }
        TypedValue::Task {
            id,
            result_type,
            kind,
        } => {
            let mut encoded = builder.reborrow().init_task();
            encoded.set_id(id);
            encode_type(
                encoded.reborrow().init_result_type(),
                result_type,
                depth + 1,
            )?;
            encoded.set_kind(task_kind_to_wire(*kind));
        }
        TypedValue::Fiber {
            id,
            yield_type,
            result_type,
        } => {
            let mut encoded = builder.reborrow().init_fiber();
            encoded.set_id(id);
            encode_type(encoded.reborrow().init_yield_type(), yield_type, depth + 1)?;
            encode_type(
                encoded.reborrow().init_result_type(),
                result_type,
                depth + 1,
            )?;
        }
        TypedValue::Stream {
            id,
            element_type,
            kind,
            generation,
        } => {
            let mut encoded = builder.reborrow().init_stream();
            encoded.set_id(id);
            encode_type(
                encoded.reborrow().init_element_type(),
                element_type,
                depth + 1,
            )?;
            encoded.set_kind(kind);
            encoded.set_generation(*generation);
        }
        TypedValue::Resource {
            kind,
            handle,
            generation,
        } => {
            let mut encoded = builder.reborrow().init_resource();
            encoded.set_kind(kind);
            encoded.set_handle(handle);
            encoded.set_generation(*generation);
        }
        TypedValue::Dynamic {
            runtime_type,
            value,
        } => {
            let mut encoded = builder.reborrow().init_dynamic_value();
            encode_type(
                encoded.reborrow().init_runtime_type(),
                runtime_type,
                depth + 1,
            )?;
            encode_value(encoded.reborrow().init_value(), value, depth + 1)?;
        }
    }
    Ok(())
}

fn decode_value(reader: wire::typed_value::Reader<'_>, depth: usize) -> Result<TypedValue> {
    check_depth(depth)?;
    use wire::typed_value::Which;
    Ok(match reader.which()? {
        Which::Unit(()) => TypedValue::Unit,
        Which::BoolValue(value) => TypedValue::Bool(value),
        Which::IntValue(value) => TypedValue::Int(value),
        Which::UintValue(value) => TypedValue::UInt(value),
        Which::FloatValue(value) => TypedValue::Float(value),
        Which::CharValue(value) => TypedValue::Char(
            char::from_u32(value).ok_or_else(|| anyhow!("invalid Unicode scalar {value}"))?,
        ),
        Which::Symbol(value) => TypedValue::Symbol(text(value?)?),
        Which::String(value) => TypedValue::String(text(value?)?),
        Which::Bytes(value) => TypedValue::Bytes(value?.to_vec()),
        Which::Json(value) => TypedValue::Json(super::brain_codec::decode_json_value(value?)?),
        Which::Path(value) => {
            let value = value?;
            TypedValue::Path {
                selector: decode_file_selector(value.get_selector()?)?,
                relative: text(value.get_relative()?)?,
            }
        }
        Which::List(value) => {
            let value = value?;
            TypedValue::List {
                element_type: decode_type(value.get_element_type()?, depth + 1)?,
                values: decode_value_list(value.get_values()?, depth + 1)?,
            }
        }
        Which::Map(value) => {
            let value = value?;
            let mut entries = Vec::new();
            for entry in value.get_entries()?.iter() {
                let pair = (
                    decode_value(entry.get_key()?, depth + 1)?,
                    decode_value(entry.get_value()?, depth + 1)?,
                );
                if entries.iter().any(|(key, _)| key == &pair.0) {
                    bail!("typed checkpoint contains a duplicate map key");
                }
                entries.push(pair);
            }
            TypedValue::Map {
                key_type: decode_type(value.get_key_type()?, depth + 1)?,
                value_type: decode_type(value.get_value_type()?, depth + 1)?,
                entries,
            }
        }
        Which::Option(value) => {
            let value = value?;
            TypedValue::Option {
                inner_type: decode_type(value.get_inner_type()?, depth + 1)?,
                value: value
                    .get_has_value()
                    .then(|| decode_value(value.get_value()?, depth + 1).map(Box::new))
                    .transpose()?,
            }
        }
        Which::Result(value) => {
            let value = value?;
            TypedValue::Result {
                ok_type: decode_type(value.get_ok_type()?, depth + 1)?,
                error_type: decode_type(value.get_error_type()?, depth + 1)?,
                is_ok: value.get_is_ok(),
                value: Box::new(decode_value(value.get_value()?, depth + 1)?),
            }
        }
        Which::Record(value) => {
            let mut names = BTreeSet::new();
            let mut fields = Vec::new();
            for field in value?.iter() {
                let name = text(field.get_name()?)?;
                if !names.insert(name.clone()) {
                    bail!("typed checkpoint contains duplicate record field {name:?}");
                }
                fields.push((name, decode_value(field.get_value()?, depth + 1)?));
            }
            TypedValue::Record(fields)
        }
        Which::Variant(value) => {
            let value = value?;
            TypedValue::Variant {
                name: text(value.get_name()?)?,
                value: value
                    .get_has_value()
                    .then(|| decode_value(value.get_value()?, depth + 1).map(Box::new))
                    .transpose()?,
            }
        }
        Which::Closure(value) => {
            let value = value?;
            TypedValue::Closure {
                function: text(value.get_function()?)?,
                captures: decode_value_list(value.get_captures()?, depth + 1)?,
                signature: decode_signature(value.get_signature()?, depth + 1)?,
            }
        }
        Which::Task(value) => {
            let value = value?;
            TypedValue::Task {
                id: text(value.get_id()?)?,
                result_type: decode_type(value.get_result_type()?, depth + 1)?,
                kind: task_kind_from_wire(value.get_kind()?),
            }
        }
        Which::Fiber(value) => {
            let value = value?;
            TypedValue::Fiber {
                id: text(value.get_id()?)?,
                yield_type: decode_type(value.get_yield_type()?, depth + 1)?,
                result_type: decode_type(value.get_result_type()?, depth + 1)?,
            }
        }
        Which::Stream(value) => {
            let value = value?;
            TypedValue::Stream {
                id: text(value.get_id()?)?,
                element_type: decode_type(value.get_element_type()?, depth + 1)?,
                kind: text(value.get_kind()?)?,
                generation: value.get_generation(),
            }
        }
        Which::Resource(value) => {
            let value = value?;
            TypedValue::Resource {
                kind: text(value.get_kind()?)?,
                handle: text(value.get_handle()?)?,
                generation: value.get_generation(),
            }
        }
        Which::DynamicValue(value) => {
            let value = value?;
            TypedValue::Dynamic {
                runtime_type: decode_type(value.get_runtime_type()?, depth + 1)?,
                value: Box::new(decode_value(value.get_value()?, depth + 1)?),
            }
        }
    })
}

fn ui_to_wire(value: UiOperation) -> wire::UiOperation {
    match value {
        UiOperation::Create => wire::UiOperation::Create,
        UiOperation::Append => wire::UiOperation::Append,
        UiOperation::Replace => wire::UiOperation::Replace,
        UiOperation::Status => wire::UiOperation::Status,
        UiOperation::Progress => wire::UiOperation::Progress,
        UiOperation::Complete => wire::UiOperation::Complete,
        UiOperation::Fail => wire::UiOperation::Fail,
    }
}

fn ui_from_wire(value: wire::UiOperation) -> UiOperation {
    match value {
        wire::UiOperation::Create => UiOperation::Create,
        wire::UiOperation::Append => UiOperation::Append,
        wire::UiOperation::Replace => UiOperation::Replace,
        wire::UiOperation::Status => UiOperation::Status,
        wire::UiOperation::Progress => UiOperation::Progress,
        wire::UiOperation::Complete => UiOperation::Complete,
        wire::UiOperation::Fail => UiOperation::Fail,
    }
}

fn encode_instruction(
    mut builder: wire::instruction::Builder<'_>,
    value: &Instruction,
    depth: usize,
) -> Result<()> {
    check_depth(depth)?;
    match value {
        Instruction::Constant { value } => {
            encode_value(builder.reborrow().init_constant(), value, depth + 1)?
        }
        Instruction::MakeList {
            element_type,
            count,
        } => {
            let mut encoded = builder.reborrow().init_make_list();
            encode_type(encoded.reborrow().init_type(), element_type, depth + 1)?;
            encoded.set_count(*count);
        }
        Instruction::MakeMap {
            key_type,
            value_type,
            count,
        } => {
            let mut encoded = builder.reborrow().init_make_map();
            encode_type(encoded.reborrow().init_key_type(), key_type, depth + 1)?;
            encode_type(encoded.reborrow().init_value_type(), value_type, depth + 1)?;
            encoded.set_count(*count);
        }
        Instruction::MakeRecord { fields } => encode_fields(
            builder.reborrow().init_make_record(fields.len() as u32),
            fields,
            depth + 1,
        )?,
        Instruction::MakeVariant {
            variants,
            tag,
            payload_type,
        }
        | Instruction::VariantGet {
            variants,
            tag,
            payload_type,
        } => {
            let mut encoded = match value {
                Instruction::MakeVariant { .. } => builder.reborrow().init_make_variant(),
                _ => builder.reborrow().init_variant_get(),
            };
            encode_variants(
                encoded.reborrow().init_variants(variants.len() as u32),
                variants,
                depth + 1,
            )?;
            encoded.set_tag(tag);
            encoded.set_has_payload_type(payload_type.is_some());
            if let Some(payload_type) = payload_type {
                encode_type(
                    encoded.reborrow().init_payload_type(),
                    payload_type,
                    depth + 1,
                )?;
            }
        }
        Instruction::RecordGet { field, value_type } => {
            let mut encoded = builder.reborrow().init_record_get();
            encoded.set_field(field);
            encode_type(encoded.reborrow().init_value_type(), value_type, depth + 1)?;
        }
        Instruction::RecordSet {
            field,
            value_type,
            record_type,
        } => {
            let mut encoded = builder.reborrow().init_record_set();
            encoded.set_field(field);
            encode_type(encoded.reborrow().init_value_type(), value_type, depth + 1)?;
            encode_fields(
                encoded
                    .reborrow()
                    .init_record_type(record_type.len() as u32),
                record_type,
                depth + 1,
            )?;
        }
        Instruction::Dup => builder.set_dup(()),
        Instruction::Drop => builder.set_drop(()),
        Instruction::Swap => builder.set_swap(()),
        Instruction::LocalGet { index } => builder.set_local_get(*index),
        Instruction::LocalSet { index } => builder.set_local_set(*index),
        Instruction::CaptureGet { index } => builder.set_capture_get(*index),
        Instruction::MakeClosure {
            function,
            capture_count,
            signature,
        } => {
            let mut encoded = builder.reborrow().init_make_closure();
            encoded.set_function(function);
            encoded.set_capture_count(*capture_count);
            encode_signature(encoded.reborrow().init_signature(), signature, depth + 1)?;
        }
        Instruction::Call { function } => builder.set_call(function),
        Instruction::CallClosure { signature } => {
            encode_signature(builder.reborrow().init_call_closure(), signature, depth + 1)?
        }
        Instruction::CapabilityRequest {
            requirement,
            input,
            output,
        } => {
            let mut encoded = builder.reborrow().init_capability_request();
            encode_requirement(encoded.reborrow().init_requirement(), requirement);
            encode_type_list(
                encoded.reborrow().init_input(input.len() as u32),
                input,
                depth + 1,
            )?;
            encode_type_list(
                encoded.reborrow().init_output(output.len() as u32),
                output,
                depth + 1,
            )?;
        }
        Instruction::OutputOpen => builder.set_output_open(()),
        Instruction::UiEffect {
            operation,
            input,
            output,
        } => {
            let mut encoded = builder.reborrow().init_ui_effect();
            encoded.set_operation(ui_to_wire(*operation));
            encode_type_list(
                encoded.reborrow().init_input(input.len() as u32),
                input,
                depth + 1,
            )?;
            encode_type_list(
                encoded.reborrow().init_output(output.len() as u32),
                output,
                depth + 1,
            )?;
        }
        Instruction::Yield { value_type } => {
            encode_type(builder.reborrow().init_yield(), value_type, depth + 1)?
        }
        Instruction::DeferFiber => builder.set_defer_fiber(()),
        Instruction::NextFiber => builder.set_next_fiber(()),
        Instruction::JoinFiber => builder.set_join_fiber(()),
        Instruction::CancelFiber => builder.set_cancel_fiber(()),
        Instruction::DeferCpu => builder.set_defer_cpu(()),
        Instruction::PollCpuFiber => builder.set_poll_cpu_fiber(()),
        Instruction::JoinCpuFiber => builder.set_join_cpu_fiber(()),
        Instruction::CancelCpuFiber => builder.set_cancel_cpu_fiber(()),
        Instruction::PropagateResult {
            return_ok_type,
            error_type,
        } => {
            let mut encoded = builder.reborrow().init_propagate_result();
            encode_type(encoded.reborrow().init_ok(), return_ok_type, depth + 1)?;
            encode_type(encoded.reborrow().init_error(), error_type, depth + 1)?;
        }
        Instruction::Jump { target } => builder.set_jump(*target),
        Instruction::Branch {
            then_block,
            else_block,
        } => {
            let mut encoded = builder.reborrow().init_branch();
            encoded.set_then_block(*then_block);
            encoded.set_else_block(*else_block);
        }
        Instruction::Return => builder.set_return_instruction(()),
        Instruction::Trap { code } => builder.set_trap(code),
    }
    Ok(())
}

fn decode_variant_instruction(
    reader: wire::variant_instruction::Reader<'_>,
    depth: usize,
) -> Result<(Vec<(String, Option<Type>)>, String, Option<Type>)> {
    Ok((
        decode_variants(reader.get_variants()?, depth + 1)?,
        text(reader.get_tag()?)?,
        reader
            .get_has_payload_type()
            .then(|| decode_type(reader.get_payload_type()?, depth + 1))
            .transpose()?,
    ))
}

fn decode_instruction(reader: wire::instruction::Reader<'_>, depth: usize) -> Result<Instruction> {
    check_depth(depth)?;
    use wire::instruction::Which;
    Ok(match reader.which()? {
        Which::Constant(value) => Instruction::Constant {
            value: decode_value(value?, depth + 1)?,
        },
        Which::MakeList(value) => {
            let value = value?;
            Instruction::MakeList {
                element_type: decode_type(value.get_type()?, depth + 1)?,
                count: value.get_count(),
            }
        }
        Which::MakeMap(value) => {
            let value = value?;
            Instruction::MakeMap {
                key_type: decode_type(value.get_key_type()?, depth + 1)?,
                value_type: decode_type(value.get_value_type()?, depth + 1)?,
                count: value.get_count(),
            }
        }
        Which::MakeRecord(value) => Instruction::MakeRecord {
            fields: decode_fields(value?, depth + 1)?,
        },
        Which::MakeVariant(value) => {
            let (variants, tag, payload_type) = decode_variant_instruction(value?, depth + 1)?;
            Instruction::MakeVariant {
                variants,
                tag,
                payload_type,
            }
        }
        Which::VariantGet(value) => {
            let (variants, tag, payload_type) = decode_variant_instruction(value?, depth + 1)?;
            Instruction::VariantGet {
                variants,
                tag,
                payload_type,
            }
        }
        Which::RecordGet(value) => {
            let value = value?;
            Instruction::RecordGet {
                field: text(value.get_field()?)?,
                value_type: decode_type(value.get_value_type()?, depth + 1)?,
            }
        }
        Which::RecordSet(value) => {
            let value = value?;
            Instruction::RecordSet {
                field: text(value.get_field()?)?,
                value_type: decode_type(value.get_value_type()?, depth + 1)?,
                record_type: decode_fields(value.get_record_type()?, depth + 1)?,
            }
        }
        Which::Dup(()) => Instruction::Dup,
        Which::Drop(()) => Instruction::Drop,
        Which::Swap(()) => Instruction::Swap,
        Which::LocalGet(index) => Instruction::LocalGet { index },
        Which::LocalSet(index) => Instruction::LocalSet { index },
        Which::CaptureGet(index) => Instruction::CaptureGet { index },
        Which::MakeClosure(value) => {
            let value = value?;
            Instruction::MakeClosure {
                function: text(value.get_function()?)?,
                capture_count: value.get_capture_count(),
                signature: decode_signature(value.get_signature()?, depth + 1)?,
            }
        }
        Which::Call(value) => Instruction::Call {
            function: text(value?)?,
        },
        Which::CallClosure(value) => Instruction::CallClosure {
            signature: decode_signature(value?, depth + 1)?,
        },
        Which::CapabilityRequest(value) => {
            let value = value?;
            Instruction::CapabilityRequest {
                requirement: decode_requirement(value.get_requirement()?)?,
                input: decode_type_list(value.get_input()?, depth + 1)?,
                output: decode_type_list(value.get_output()?, depth + 1)?,
            }
        }
        Which::OutputOpen(()) => Instruction::OutputOpen,
        Which::UiEffect(value) => {
            let value = value?;
            Instruction::UiEffect {
                operation: ui_from_wire(value.get_operation()?),
                input: decode_type_list(value.get_input()?, depth + 1)?,
                output: decode_type_list(value.get_output()?, depth + 1)?,
            }
        }
        Which::Yield(value) => Instruction::Yield {
            value_type: decode_type(value?, depth + 1)?,
        },
        Which::DeferFiber(()) => Instruction::DeferFiber,
        Which::NextFiber(()) => Instruction::NextFiber,
        Which::JoinFiber(()) => Instruction::JoinFiber,
        Which::CancelFiber(()) => Instruction::CancelFiber,
        Which::DeferCpu(()) => Instruction::DeferCpu,
        Which::PollCpuFiber(()) => Instruction::PollCpuFiber,
        Which::JoinCpuFiber(()) => Instruction::JoinCpuFiber,
        Which::CancelCpuFiber(()) => Instruction::CancelCpuFiber,
        Which::PropagateResult(value) => {
            let value = value?;
            Instruction::PropagateResult {
                return_ok_type: decode_type(value.get_ok()?, depth + 1)?,
                error_type: decode_type(value.get_error()?, depth + 1)?,
            }
        }
        Which::Jump(target) => Instruction::Jump { target },
        Which::Branch(value) => {
            let value = value?;
            Instruction::Branch {
                then_block: value.get_then_block(),
                else_block: value.get_else_block(),
            }
        }
        Which::ReturnInstruction(()) => Instruction::Return,
        Which::Trap(value) => Instruction::Trap {
            code: text(value?)?,
        },
    })
}

fn encode_function(
    mut builder: wire::vm_function::Builder<'_>,
    value: &Function,
    depth: usize,
) -> Result<()> {
    check_depth(depth)?;
    builder.set_name(&value.name);
    builder.set_has_documentation(value.documentation.is_some());
    if let Some(documentation) = &value.documentation {
        builder.set_documentation(documentation);
    }
    encode_signature(
        builder.reborrow().init_signature(),
        &value.signature,
        depth + 1,
    )?;
    encode_type_list(
        builder.reborrow().init_locals(value.locals.len() as u32),
        &value.locals,
        depth + 1,
    )?;
    encode_type_list(
        builder
            .reborrow()
            .init_captures(value.captures.len() as u32),
        &value.captures,
        depth + 1,
    )?;
    builder.set_entry(value.entry);
    let mut blocks = builder.reborrow().init_blocks(value.blocks.len() as u32);
    for (index, (key, block)) in value.blocks.iter().enumerate() {
        if key != &block.id {
            bail!(
                "function block map key {key} does not match block id {}",
                block.id
            );
        }
        let mut encoded = blocks.reborrow().get(index as u32);
        encoded.set_id(block.id);
        let mut instructions = encoded
            .reborrow()
            .init_instructions(block.instructions.len() as u32);
        for (instruction_index, located) in block.instructions.iter().enumerate() {
            let mut encoded = instructions.reborrow().get(instruction_index as u32);
            encode_instruction(
                encoded.reborrow().init_instruction(),
                &located.instruction,
                depth + 1,
            )?;
            encode_origin(encoded.reborrow().init_origin(), &located.origin, depth + 1)?;
        }
    }
    Ok(())
}

fn decode_function(reader: wire::vm_function::Reader<'_>, depth: usize) -> Result<Function> {
    check_depth(depth)?;
    let mut blocks = BTreeMap::new();
    for block in reader.get_blocks()?.iter() {
        let id = block.get_id();
        let mut instructions = Vec::new();
        for located in block.get_instructions()?.iter() {
            instructions.push(LocatedInstruction {
                instruction: decode_instruction(located.get_instruction()?, depth + 1)?,
                origin: decode_origin(located.get_origin()?, depth + 1)?,
            });
        }
        if blocks.insert(id, BasicBlock { id, instructions }).is_some() {
            bail!("typed checkpoint contains duplicate basic block id {id}");
        }
    }
    Ok(Function {
        name: text(reader.get_name()?)?,
        documentation: reader
            .get_has_documentation()
            .then(|| text(reader.get_documentation()?))
            .transpose()?,
        signature: decode_signature(reader.get_signature()?, depth + 1)?,
        locals: decode_type_list(reader.get_locals()?, depth + 1)?,
        captures: decode_type_list(reader.get_captures()?, depth + 1)?,
        entry: reader.get_entry(),
        blocks,
    })
}

fn encode_named_functions(
    mut builder: capnp::struct_list::Builder<'_, wire::named_function::Owned>,
    values: &BTreeMap<String, Function>,
    depth: usize,
) -> Result<()> {
    for (index, (name, function)) in values.iter().enumerate() {
        let mut encoded = builder.reborrow().get(index as u32);
        encoded.set_name(name);
        encode_function(encoded.reborrow().init_function(), function, depth + 1)?;
    }
    Ok(())
}

fn decode_named_functions(
    reader: capnp::struct_list::Reader<'_, wire::named_function::Owned>,
    depth: usize,
) -> Result<BTreeMap<String, Function>> {
    let mut functions = BTreeMap::new();
    for encoded in reader.iter() {
        let name = text(encoded.get_name()?)?;
        let function = decode_function(encoded.get_function()?, depth + 1)?;
        if functions.insert(name.clone(), function).is_some() {
            bail!("typed checkpoint contains duplicate function key {name:?}");
        }
    }
    Ok(functions)
}

fn encode_module(
    mut builder: wire::vm_module::Builder<'_>,
    value: &Module,
    depth: usize,
) -> Result<()> {
    builder.set_version(value.version);
    builder.set_name(&value.name);
    builder.set_entry(&value.entry);
    encode_named_functions(
        builder
            .reborrow()
            .init_functions(value.functions.len() as u32),
        &value.functions,
        depth + 1,
    )
}

fn decode_module(reader: wire::vm_module::Reader<'_>, depth: usize) -> Result<Module> {
    Ok(Module {
        version: reader.get_version(),
        name: text(reader.get_name()?)?,
        entry: text(reader.get_entry()?)?,
        functions: decode_named_functions(reader.get_functions()?, depth + 1)?,
    })
}

fn encode_verified_function(
    mut builder: wire::verified_function::Builder<'_>,
    value: &VerifiedFunction,
    depth: usize,
) -> Result<()> {
    builder.set_name(&value.name);
    encode_effects(
        builder
            .reborrow()
            .init_inferred_effects(value.inferred_effects.0.len() as u32),
        &value.inferred_effects,
    );
    builder.set_has_inferred_suspension(value.inferred_suspension.is_some());
    if let Some(suspension) = &value.inferred_suspension {
        encode_suspension(
            builder.reborrow().init_inferred_suspension(),
            suspension,
            depth + 1,
        )?;
    }
    encode_type_list(
        builder
            .reborrow()
            .init_entry_stack(value.entry_stack.len() as u32),
        &value.entry_stack,
        depth + 1,
    )?;
    let mut stacks = builder
        .reborrow()
        .init_block_stacks(value.block_stacks.len() as u32);
    for (index, (block, stack)) in value.block_stacks.iter().enumerate() {
        let mut encoded = stacks.reborrow().get(index as u32);
        encoded.set_block(*block);
        encode_type_list(
            encoded.reborrow().init_stack(stack.len() as u32),
            stack,
            depth + 1,
        )?;
    }
    Ok(())
}

fn decode_verified_function(
    reader: wire::verified_function::Reader<'_>,
    depth: usize,
) -> Result<VerifiedFunction> {
    let mut block_stacks = BTreeMap::new();
    for encoded in reader.get_block_stacks()?.iter() {
        let block = encoded.get_block();
        let stack = decode_type_list(encoded.get_stack()?, depth + 1)?;
        if block_stacks.insert(block, stack).is_some() {
            bail!("typed checkpoint contains duplicate verified block stack {block}");
        }
    }
    Ok(VerifiedFunction {
        name: text(reader.get_name()?)?,
        inferred_effects: decode_effects(reader.get_inferred_effects()?)?,
        inferred_suspension: reader
            .get_has_inferred_suspension()
            .then(|| decode_suspension(reader.get_inferred_suspension()?, depth + 1))
            .transpose()?,
        entry_stack: decode_type_list(reader.get_entry_stack()?, depth + 1)?,
        block_stacks,
    })
}

fn encode_verified_module(
    mut builder: wire::verified_module::Builder<'_>,
    value: &VerifiedModule,
    depth: usize,
) -> Result<()> {
    encode_module(builder.reborrow().init_module(), &value.module, depth + 1)?;
    let mut functions = builder
        .reborrow()
        .init_functions(value.functions.len() as u32);
    for (index, (key, function)) in value.functions.iter().enumerate() {
        if key != &function.name {
            bail!(
                "verified function map key {key:?} does not match function name {:?}",
                function.name
            );
        }
        encode_verified_function(functions.reborrow().get(index as u32), function, depth + 1)?;
    }
    Ok(())
}

fn decode_verified_module(
    reader: wire::verified_module::Reader<'_>,
    depth: usize,
) -> Result<VerifiedModule> {
    let mut functions = BTreeMap::new();
    for encoded in reader.get_functions()?.iter() {
        let function = decode_verified_function(encoded, depth + 1)?;
        if functions.insert(function.name.clone(), function).is_some() {
            bail!("typed checkpoint contains duplicate verified function name");
        }
    }
    Ok(VerifiedModule {
        module: decode_module(reader.get_module()?, depth + 1)?,
        functions,
    })
}

fn encode_frame(
    mut builder: wire::vm_frame::Builder<'_>,
    value: &VmFrame,
    depth: usize,
) -> Result<()> {
    builder.set_function(&value.function);
    builder.set_block(value.block);
    builder.set_instruction(value.instruction as u64);
    builder.set_stack_base(value.stack_base as u64);
    builder.set_output_arity(value.output_arity as u64);
    encode_type_list(
        builder
            .reborrow()
            .init_output_types(value.output_types.len() as u32),
        &value.output_types,
        depth + 1,
    )?;
    encode_value_list(
        builder.reborrow().init_locals(value.locals.len() as u32),
        &value.locals,
        depth + 1,
    )?;
    encode_value_list(
        builder
            .reborrow()
            .init_captures(value.captures.len() as u32),
        &value.captures,
        depth + 1,
    )
}

fn decode_frame(reader: wire::vm_frame::Reader<'_>, depth: usize) -> Result<VmFrame> {
    Ok(VmFrame {
        function: text(reader.get_function()?)?,
        block: reader.get_block(),
        instruction: usize_from_wire(reader.get_instruction(), "frame instruction")?,
        stack_base: usize_from_wire(reader.get_stack_base(), "frame stack base")?,
        output_arity: usize_from_wire(reader.get_output_arity(), "frame output arity")?,
        output_types: decode_type_list(reader.get_output_types()?, depth + 1)?,
        locals: decode_value_list(reader.get_locals()?, depth + 1)?,
        captures: decode_value_list(reader.get_captures()?, depth + 1)?,
    })
}

fn encode_continuation(
    mut builder: wire::vm_continuation::Builder<'_>,
    value: &VmContinuation,
    depth: usize,
) -> Result<()> {
    encode_value_list(
        builder.reborrow().init_stack(value.stack.len() as u32),
        &value.stack,
        depth + 1,
    )?;
    let mut frames = builder.reborrow().init_frames(value.frames.len() as u32);
    for (index, frame) in value.frames.iter().enumerate() {
        encode_frame(frames.reborrow().get(index as u32), frame, depth + 1)?;
    }
    builder.set_fuel(value.fuel);
    builder.set_next_effect_sequence(value.next_effect_sequence);
    Ok(())
}

fn decode_continuation(
    reader: wire::vm_continuation::Reader<'_>,
    depth: usize,
) -> Result<VmContinuation> {
    Ok(VmContinuation {
        stack: decode_value_list(reader.get_stack()?, depth + 1)?,
        frames: reader
            .get_frames()?
            .iter()
            .map(|frame| decode_frame(frame, depth + 1))
            .collect::<Result<Vec<_>>>()?,
        fuel: reader.get_fuel(),
        next_effect_sequence: reader.get_next_effect_sequence(),
    })
}

fn severity_to_wire(value: Severity) -> wire::Severity {
    match value {
        Severity::Note => wire::Severity::Note,
        Severity::Warning => wire::Severity::Warning,
        Severity::Error => wire::Severity::Error,
    }
}

fn severity_from_wire(value: wire::Severity) -> Severity {
    match value {
        wire::Severity::Note => Severity::Note,
        wire::Severity::Warning => Severity::Warning,
        wire::Severity::Error => Severity::Error,
    }
}

fn phase_to_wire(value: DiagnosticPhase) -> wire::DiagnosticPhase {
    match value {
        DiagnosticPhase::Reader => wire::DiagnosticPhase::Reader,
        DiagnosticPhase::MacroExpansion => wire::DiagnosticPhase::MacroExpansion,
        DiagnosticPhase::NameResolution => wire::DiagnosticPhase::NameResolution,
        DiagnosticPhase::TypeInference => wire::DiagnosticPhase::TypeInference,
        DiagnosticPhase::Verification => wire::DiagnosticPhase::Verification,
        DiagnosticPhase::Linking => wire::DiagnosticPhase::Linking,
        DiagnosticPhase::Authorization => wire::DiagnosticPhase::Authorization,
        DiagnosticPhase::Availability => wire::DiagnosticPhase::Availability,
        DiagnosticPhase::Approval => wire::DiagnosticPhase::Approval,
        DiagnosticPhase::Interpretation => wire::DiagnosticPhase::Interpretation,
        DiagnosticPhase::HostCall => wire::DiagnosticPhase::HostCall,
        DiagnosticPhase::NativeExecution => wire::DiagnosticPhase::NativeExecution,
        DiagnosticPhase::TransactionCommit => wire::DiagnosticPhase::TransactionCommit,
        DiagnosticPhase::ChildExecution => wire::DiagnosticPhase::ChildExecution,
        DiagnosticPhase::Cancellation => wire::DiagnosticPhase::Cancellation,
        DiagnosticPhase::ResourceLimit => wire::DiagnosticPhase::ResourceLimit,
    }
}

fn phase_from_wire(value: wire::DiagnosticPhase) -> DiagnosticPhase {
    match value {
        wire::DiagnosticPhase::Reader => DiagnosticPhase::Reader,
        wire::DiagnosticPhase::MacroExpansion => DiagnosticPhase::MacroExpansion,
        wire::DiagnosticPhase::NameResolution => DiagnosticPhase::NameResolution,
        wire::DiagnosticPhase::TypeInference => DiagnosticPhase::TypeInference,
        wire::DiagnosticPhase::Verification => DiagnosticPhase::Verification,
        wire::DiagnosticPhase::Linking => DiagnosticPhase::Linking,
        wire::DiagnosticPhase::Authorization => DiagnosticPhase::Authorization,
        wire::DiagnosticPhase::Availability => DiagnosticPhase::Availability,
        wire::DiagnosticPhase::Approval => DiagnosticPhase::Approval,
        wire::DiagnosticPhase::Interpretation => DiagnosticPhase::Interpretation,
        wire::DiagnosticPhase::HostCall => DiagnosticPhase::HostCall,
        wire::DiagnosticPhase::NativeExecution => DiagnosticPhase::NativeExecution,
        wire::DiagnosticPhase::TransactionCommit => DiagnosticPhase::TransactionCommit,
        wire::DiagnosticPhase::ChildExecution => DiagnosticPhase::ChildExecution,
        wire::DiagnosticPhase::Cancellation => DiagnosticPhase::Cancellation,
        wire::DiagnosticPhase::ResourceLimit => DiagnosticPhase::ResourceLimit,
    }
}

fn encode_diagnostic(
    mut builder: wire::vm_diagnostic::Builder<'_>,
    value: &VmDiagnostic,
    depth: usize,
) -> Result<()> {
    check_depth(depth)?;
    builder.set_code(&value.code);
    builder.set_severity(severity_to_wire(value.severity));
    builder.set_phase(phase_to_wire(value.phase));
    builder.set_message(&value.message);
    builder.set_has_primary(value.primary.is_some());
    if let Some(primary) = &value.primary {
        encode_origin(builder.reborrow().init_primary(), primary, depth + 1)?;
    }
    let mut related = builder.reborrow().init_related(value.related.len() as u32);
    for (index, origin) in value.related.iter().enumerate() {
        encode_origin(related.reborrow().get(index as u32), origin, depth + 1)?;
    }
    encode_type_list(
        builder
            .reborrow()
            .init_expected_types(value.expected_types.len() as u32),
        &value.expected_types,
        depth + 1,
    )?;
    encode_type_list(
        builder
            .reborrow()
            .init_found_types(value.found_types.len() as u32),
        &value.found_types,
        depth + 1,
    )?;
    encode_effects(
        builder
            .reborrow()
            .init_expected_effects(value.expected_effects.0.len() as u32),
        &value.expected_effects,
    );
    encode_effects(
        builder
            .reborrow()
            .init_found_effects(value.found_effects.0.len() as u32),
        &value.found_effects,
    );
    builder.set_has_capability(value.capability.is_some());
    if let Some(capability) = &value.capability {
        encode_requirement(builder.reborrow().init_capability(), capability);
    }
    encode_text_list(
        builder.reborrow().init_trace(value.trace.len() as u32),
        &value.trace,
    );
    encode_text_list(
        builder.reborrow().init_hints(value.hints.len() as u32),
        &value.hints,
    );
    builder.set_has_cause(value.cause.is_some());
    if let Some(cause) = &value.cause {
        encode_diagnostic(builder.reborrow().init_cause(), cause, depth + 1)?;
    }
    Ok(())
}

fn decode_diagnostic(
    reader: wire::vm_diagnostic::Reader<'_>,
    depth: usize,
) -> Result<VmDiagnostic> {
    check_depth(depth)?;
    Ok(VmDiagnostic {
        code: text(reader.get_code()?)?,
        severity: severity_from_wire(reader.get_severity()?),
        phase: phase_from_wire(reader.get_phase()?),
        message: text(reader.get_message()?)?,
        primary: reader
            .get_has_primary()
            .then(|| decode_origin(reader.get_primary()?, depth + 1))
            .transpose()?,
        related: reader
            .get_related()?
            .iter()
            .map(|origin| decode_origin(origin, depth + 1))
            .collect::<Result<Vec<_>>>()?,
        expected_types: decode_type_list(reader.get_expected_types()?, depth + 1)?,
        found_types: decode_type_list(reader.get_found_types()?, depth + 1)?,
        expected_effects: decode_effects(reader.get_expected_effects()?)?,
        found_effects: decode_effects(reader.get_found_effects()?)?,
        capability: reader
            .get_has_capability()
            .then(|| decode_requirement(reader.get_capability()?))
            .transpose()?,
        trace: decode_text_list(reader.get_trace()?)?,
        hints: decode_text_list(reader.get_hints()?)?,
        cause: reader
            .get_has_cause()
            .then(|| decode_diagnostic(reader.get_cause()?, depth + 1).map(Box::new))
            .transpose()?,
    })
}

fn encode_producer(
    mut builder: wire::producer_fiber_record::Builder<'_>,
    value: &ProducerFiberRecord,
    depth: usize,
) -> Result<()> {
    encode_verified_module(builder.reborrow().init_module(), &value.module, depth + 1)?;
    encode_type(
        builder.reborrow().init_yield_type(),
        &value.yield_type,
        depth + 1,
    )?;
    encode_type(
        builder.reborrow().init_result_type(),
        &value.result_type,
        depth + 1,
    )?;
    let mut state = builder.reborrow().init_state();
    match &value.state {
        ProducerFiberState::Ready { continuation } => {
            encode_continuation(state.reborrow().init_ready(), continuation, depth + 1)?
        }
        ProducerFiberState::Completed { result } => {
            encode_value(state.reborrow().init_completed(), result, depth + 1)?
        }
        ProducerFiberState::Failed { diagnostic } => {
            encode_diagnostic(state.reborrow().init_failed(), diagnostic, depth + 1)?
        }
        ProducerFiberState::Cancelled => state.set_cancelled(()),
    }
    Ok(())
}

fn decode_producer(
    reader: wire::producer_fiber_record::Reader<'_>,
    depth: usize,
) -> Result<ProducerFiberRecord> {
    use wire::producer_fiber_state::Which;
    let state = match reader.get_state()?.which()? {
        Which::Ready(value) => ProducerFiberState::Ready {
            continuation: decode_continuation(value?, depth + 1)?,
        },
        Which::Completed(value) => ProducerFiberState::Completed {
            result: decode_value(value?, depth + 1)?,
        },
        Which::Failed(value) => ProducerFiberState::Failed {
            diagnostic: decode_diagnostic(value?, depth + 1)?,
        },
        Which::Cancelled(()) => ProducerFiberState::Cancelled,
    };
    Ok(ProducerFiberRecord {
        module: decode_verified_module(reader.get_module()?, depth + 1)?,
        yield_type: decode_type(reader.get_yield_type()?, depth + 1)?,
        result_type: decode_type(reader.get_result_type()?, depth + 1)?,
        state,
    })
}

pub(super) fn encode_vm_side_effect(
    mut builder: wire::vm_side_effect::Builder<'_>,
    value: &VmSideEffect,
) -> Result<()> {
    builder.set_protocol_version(value.protocol_version);
    builder.set_sequence(value.sequence);
    encode_requirement(builder.reborrow().init_requirement(), &value.requirement);
    let mut event = builder.reborrow().init_event();
    match &value.event {
        HostSideEffect::Emit { text } => event.set_emit(text),
        HostSideEffect::Request { arguments } => {
            encode_value_list(event.init_request(arguments.len() as u32), arguments, 0)?;
        }
        HostSideEffect::Ui {
            operation,
            target,
            text,
            progress,
        } => {
            let mut ui = event.init_ui();
            ui.set_operation(ui_to_wire(*operation));
            ui.set_has_target(target.is_some());
            if let Some(target) = target {
                encode_value(ui.reborrow().init_target(), target, 0)?;
            }
            ui.set_has_text(text.is_some());
            if let Some(text) = text {
                ui.set_text(text);
            }
            ui.set_has_progress(progress.is_some());
            if let Some(progress) = progress {
                let mut encoded = ui.reborrow().init_progress();
                encoded.set_completed(progress.completed);
                encoded.set_has_total(progress.total.is_some());
                if let Some(total) = progress.total {
                    encoded.set_total(total);
                }
            }
        }
    }
    encode_type_list(
        builder.reborrow().init_output(value.output.len() as u32),
        &value.output,
        0,
    )?;
    encode_origin(builder.reborrow().init_origin(), &value.origin, 0)
}

pub(super) fn decode_vm_side_effect(
    reader: wire::vm_side_effect::Reader<'_>,
) -> Result<VmSideEffect> {
    use wire::vm_host_side_effect::Which;
    let event = match reader.get_event()?.which()? {
        Which::Emit(text_value) => HostSideEffect::Emit {
            text: text(text_value?)?,
        },
        Which::Request(arguments) => HostSideEffect::Request {
            arguments: decode_value_list(arguments?, 0)?,
        },
        Which::Ui(ui) => {
            let ui = ui?;
            HostSideEffect::Ui {
                operation: ui_from_wire(ui.get_operation()?),
                target: ui
                    .get_has_target()
                    .then(|| decode_value(ui.get_target()?, 0))
                    .transpose()?,
                text: ui
                    .get_has_text()
                    .then(|| text(ui.get_text()?))
                    .transpose()?,
                progress: ui
                    .get_has_progress()
                    .then(|| {
                        let progress = ui.get_progress()?;
                        Ok::<UiProgress, anyhow::Error>(UiProgress {
                            completed: progress.get_completed(),
                            total: progress.get_has_total().then(|| progress.get_total()),
                        })
                    })
                    .transpose()?,
            }
        }
    };
    Ok(VmSideEffect {
        protocol_version: reader.get_protocol_version(),
        sequence: reader.get_sequence(),
        requirement: decode_requirement(reader.get_requirement()?)?,
        event,
        output: decode_type_list(reader.get_output()?, 0)?,
        origin: decode_origin(reader.get_origin()?, 0)?,
    })
}

pub(super) fn encode_effect_journal_state(
    mut builder: wire::vm_effect_journal_state::Builder<'_>,
    value: &EffectJournalState,
) -> Result<()> {
    match value {
        EffectJournalState::Proposed => builder.set_proposed(()),
        EffectJournalState::AwaitingApproval => builder.set_awaiting_approval(()),
        EffectJournalState::AwaitingHostResult => builder.set_awaiting_host_result(()),
        EffectJournalState::Acknowledged { values } => {
            encode_value_list(builder.init_acknowledged(values.len() as u32), values, 0)?;
        }
        EffectJournalState::Denied => builder.set_denied(()),
        EffectJournalState::Cancelled => builder.set_cancelled(()),
        EffectJournalState::Failed { diagnostic } => {
            encode_diagnostic(builder.init_failed(), diagnostic, 0)?;
        }
    }
    Ok(())
}

pub(super) fn decode_effect_journal_state(
    reader: wire::vm_effect_journal_state::Reader<'_>,
) -> Result<EffectJournalState> {
    use wire::vm_effect_journal_state::Which;
    Ok(match reader.which()? {
        Which::Proposed(()) => EffectJournalState::Proposed,
        Which::AwaitingApproval(()) => EffectJournalState::AwaitingApproval,
        Which::AwaitingHostResult(()) => EffectJournalState::AwaitingHostResult,
        Which::Acknowledged(values) => EffectJournalState::Acknowledged {
            values: decode_value_list(values?, 0)?,
        },
        Which::Denied(()) => EffectJournalState::Denied,
        Which::Cancelled(()) => EffectJournalState::Cancelled,
        Which::Failed(diagnostic) => EffectJournalState::Failed {
            diagnostic: decode_diagnostic(diagnostic?, 0)?,
        },
    })
}

pub(super) fn encode_effect_record(
    mut builder: wire::brain_effect_record::Builder<'_>,
    execution_id: uuid::Uuid,
    entry: &EffectJournalEntry,
) -> Result<()> {
    builder.set_execution_id(&execution_id.to_string());
    encode_vm_side_effect(builder.reborrow().init_effect(), &entry.effect)?;
    encode_effect_journal_state(builder.reborrow().init_state(), &entry.state)
}

pub(super) fn decode_effect_record(
    reader: wire::brain_effect_record::Reader<'_>,
) -> Result<(uuid::Uuid, EffectJournalEntry)> {
    Ok((
        text(reader.get_execution_id()?)?.parse()?,
        EffectJournalEntry {
            effect: decode_vm_side_effect(reader.get_effect()?)?,
            state: decode_effect_journal_state(reader.get_state()?)?,
        },
    ))
}

pub(super) fn encode_checkpoint(
    mut builder: wire::typed_runtime_checkpoint::Builder<'_>,
    value: &TypedRuntimeCheckpoint,
) -> Result<()> {
    builder.set_version(value.version);
    encode_value_list(
        builder.reborrow().init_stack(value.stack.len() as u32),
        &value.stack,
        0,
    )?;
    encode_named_functions(
        builder
            .reborrow()
            .init_functions(value.functions.len() as u32),
        &value.functions,
        0,
    )?;
    let mut fibers = builder
        .reborrow()
        .init_producer_fibers(value.producer_fibers.len() as u32);
    for (index, (id, record)) in value.producer_fibers.iter().enumerate() {
        let mut encoded = fibers.reborrow().get(index as u32);
        encoded.set_id(id);
        encode_producer(encoded.reborrow().init_record(), record, 0)?;
    }
    Ok(())
}

pub(super) fn decode_checkpoint(
    reader: wire::typed_runtime_checkpoint::Reader<'_>,
) -> Result<TypedRuntimeCheckpoint> {
    let mut producer_fibers = BTreeMap::new();
    for encoded in reader.get_producer_fibers()?.iter() {
        let id = text(encoded.get_id()?)?;
        let record = decode_producer(encoded.get_record()?, 0)?;
        if producer_fibers.insert(id.clone(), record).is_some() {
            bail!("typed checkpoint contains duplicate producer fiber id {id:?}");
        }
    }
    Ok(TypedRuntimeCheckpoint {
        version: reader.get_version(),
        stack: decode_value_list(reader.get_stack()?, 0)?,
        functions: decode_named_functions(reader.get_functions()?, 0)?,
        producer_fibers,
    })
}

/// Encode one durable typed-runtime checkpoint using the same closed native
/// schema used by runner registration and result transport.
pub(crate) fn encode_checkpoint_bytes(value: &TypedRuntimeCheckpoint) -> Result<Vec<u8>> {
    let mut message = capnp::message::Builder::new_default();
    encode_checkpoint(
        message.init_root::<wire::typed_runtime_checkpoint::Builder<'_>>(),
        value,
    )?;
    let mut encoded = Vec::new();
    capnp::serialize::write_message(&mut encoded, &message)?;
    Ok(encoded)
}

/// Decode one durable typed-runtime checkpoint. Trailing bytes are rejected so
/// a content-addressed checkpoint has exactly one unambiguous representation.
pub(crate) fn decode_checkpoint_bytes(encoded: &[u8]) -> Result<TypedRuntimeCheckpoint> {
    let mut cursor = std::io::Cursor::new(encoded);
    let message =
        capnp::serialize::read_message(&mut cursor, capnp::message::ReaderOptions::new())?;
    if cursor.position() != encoded.len() as u64 {
        bail!("typed checkpoint contains trailing bytes");
    }
    decode_checkpoint(message.get_root::<wire::typed_runtime_checkpoint::Reader<'_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::programs::ProgramLanguage;
    use crate::vm::{TypedExecutionStatus, TypedRuntime};

    fn round_trip_value(value: &TypedValue) -> Result<TypedValue> {
        let mut message = capnp::message::Builder::new_default();
        encode_value(
            message.init_root::<wire::typed_value::Builder<'_>>(),
            value,
            0,
        )?;
        let words = capnp::serialize::write_message_to_words(&message);
        let reader = capnp::serialize::read_message_from_flat_slice(
            &mut words.as_slice(),
            capnp::message::ReaderOptions::new(),
        )?;
        decode_value(reader.get_root::<wire::typed_value::Reader<'_>>()?, 0)
    }

    fn round_trip_checkpoint(value: &TypedRuntimeCheckpoint) -> Result<TypedRuntimeCheckpoint> {
        decode_checkpoint_bytes(&encode_checkpoint_bytes(value)?)
    }

    fn round_trip_effect_record(
        execution_id: uuid::Uuid,
        entry: &EffectJournalEntry,
    ) -> Result<(uuid::Uuid, EffectJournalEntry)> {
        let mut message = capnp::message::Builder::new_default();
        encode_effect_record(
            message.init_root::<wire::brain_effect_record::Builder<'_>>(),
            execution_id,
            entry,
        )?;
        let words = capnp::serialize::write_message_to_words(&message);
        let reader = capnp::serialize::read_message_from_flat_slice(
            &mut words.as_slice(),
            capnp::message::ReaderOptions::new(),
        )?;
        decode_effect_record(reader.get_root::<wire::brain_effect_record::Reader<'_>>()?)
    }

    fn round_trip_type(value: &Type) -> Result<Type> {
        let mut message = capnp::message::Builder::new_default();
        encode_type(
            message.init_root::<wire::typed_type::Builder<'_>>(),
            value,
            0,
        )?;
        let words = capnp::serialize::write_message_to_words(&message);
        let reader = capnp::serialize::read_message_from_flat_slice(
            &mut words.as_slice(),
            capnp::message::ReaderOptions::new(),
        )?;
        decode_type(reader.get_root::<wire::typed_type::Reader<'_>>()?, 0)
    }

    fn round_trip_instruction(value: &Instruction) -> Result<Instruction> {
        let mut message = capnp::message::Builder::new_default();
        encode_instruction(
            message.init_root::<wire::instruction::Builder<'_>>(),
            value,
            0,
        )?;
        let words = capnp::serialize::write_message_to_words(&message);
        let reader = capnp::serialize::read_message_from_flat_slice(
            &mut words.as_slice(),
            capnp::message::ReaderOptions::new(),
        )?;
        decode_instruction(reader.get_root::<wire::instruction::Reader<'_>>()?, 0)
    }

    fn sample_requirement() -> CapabilityRequirement {
        CapabilityRequirement {
            capability: CapabilityKind::FileRead,
            selector: ResourceSelector::File {
                selector: FileSelector {
                    root: ResourceRoot::Workspace,
                    pattern: "src/**".into(),
                },
            },
        }
    }

    fn sample_signature() -> StackSignature {
        StackSignature {
            type_parameters: vec!["T".into()],
            input: StackRow::polymorphic("S", vec![Type::Int]),
            output: StackRow::polymorphic("S", vec![Type::String]),
            effects: EffectSet::from_requirement(sample_requirement()),
            control: ControlEffect::MaySuspend,
            suspension: Some(SuspensionSignature::one_way(Type::String)),
        }
    }

    #[test]
    fn test_failed_effect_record_preserves_vm_diagnostic_found_value_origin() -> Result<()> {
        let primary = SourceOrigin {
            language: SourceLanguage::Forth,
            span: Some(SourceSpan::bytes("input.forth", 6, 9)),
            word: Some("say".into()),
            expansion: None,
        };
        let producer = SourceOrigin {
            language: SourceLanguage::Forth,
            span: Some(SourceSpan::bytes("input.forth", 4, 5)),
            word: Some("+".into()),
            expansion: None,
        };
        let mut diagnostic = VmDiagnostic::type_mismatch(Type::String, Type::Int, Some(primary));
        diagnostic.set_found_value_origin(producer);
        let execution_id = uuid::Uuid::new_v4();
        let effect = EffectJournalEntry {
            effect: VmSideEffect {
                protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
                sequence: 1,
                requirement: CapabilityRequirement {
                    capability: CapabilityKind::SessionEmit,
                    selector: ResourceSelector::None,
                },
                event: HostSideEffect::Emit {
                    text: "unpublished".into(),
                },
                output: Vec::new(),
                origin: SourceOrigin::generated("say"),
            },
            state: EffectJournalState::Failed {
                diagnostic: diagnostic.clone(),
            },
        };

        let decoded = round_trip_effect_record(execution_id, &effect)?;
        assert_eq!(
            decoded,
            (execution_id, effect.clone()),
            "Cap'n Proto containing effect-record codec lost structured found-value provenance; \
             decoded={decoded:#?}; original={effect:#?}"
        );
        let EffectJournalState::Failed { diagnostic } = decoded.1.state else {
            panic!("decoded containing effect record changed its failure state: {decoded:#?}")
        };
        assert_eq!(
            diagnostic.found_value_origin().and_then(|origin| origin.word.as_deref()),
            Some("+"),
            "containing effect-record projection dropped the machine-readable producer origin; diagnostic={diagnostic:#?}"
        );
        Ok(())
    }

    #[test]
    fn execute_once_effect_records_round_trip_without_json() -> Result<()> {
        let execution_id = uuid::Uuid::new_v4();
        let effects = vec![
            EffectJournalEntry {
                effect: VmSideEffect {
                    protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
                    sequence: 1,
                    requirement: CapabilityRequirement {
                        capability: CapabilityKind::SessionEmit,
                        selector: ResourceSelector::None,
                    },
                    event: HostSideEffect::Emit {
                        text: "hello".into(),
                    },
                    output: Vec::new(),
                    origin: SourceOrigin::generated("say"),
                },
                state: EffectJournalState::Acknowledged { values: Vec::new() },
            },
            EffectJournalEntry {
                effect: VmSideEffect {
                    protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
                    sequence: 2,
                    requirement: sample_requirement(),
                    event: HostSideEffect::Request {
                        arguments: vec![TypedValue::String("src/lib.rs".into())],
                    },
                    output: vec![Type::Bytes],
                    origin: SourceOrigin::generated("file-read"),
                },
                state: EffectJournalState::AwaitingHostResult,
            },
            EffectJournalEntry {
                effect: VmSideEffect {
                    protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
                    sequence: 3,
                    requirement: CapabilityRequirement {
                        capability: CapabilityKind::SessionEmit,
                        selector: ResourceSelector::None,
                    },
                    event: HostSideEffect::Ui {
                        operation: UiOperation::Progress,
                        target: Some(TypedValue::Resource {
                            kind: "output-handle".into(),
                            handle: "h1".into(),
                            generation: 4,
                        }),
                        text: Some("working".into()),
                        progress: Some(UiProgress {
                            completed: 2,
                            total: Some(5),
                        }),
                    },
                    output: Vec::new(),
                    origin: SourceOrigin::generated("output-progress"),
                },
                state: EffectJournalState::Cancelled,
            },
        ];

        for effect in effects {
            assert_eq!(
                round_trip_effect_record(execution_id, &effect)?,
                (execution_id, effect)
            );
        }
        Ok(())
    }

    #[test]
    fn typed_values_round_trip_without_json_envelopes() -> Result<()> {
        let selector = FileSelector {
            root: ResourceRoot::Workspace,
            pattern: "src/**".into(),
        };
        let signature = sample_signature();
        let values = vec![
            TypedValue::Unit,
            TypedValue::Bool(true),
            TypedValue::Int(-4),
            TypedValue::UInt(u64::MAX),
            TypedValue::Float(3.5),
            TypedValue::Char('🪶'),
            TypedValue::Symbol("symbol".into()),
            TypedValue::String("text".into()),
            TypedValue::Bytes(vec![0, 1, 255]),
            TypedValue::Json(serde_json::json!({"a": [1, true, null]})),
            TypedValue::Path {
                selector: selector.clone(),
                relative: "src/lib.rs".into(),
            },
            TypedValue::List {
                element_type: Type::Int,
                values: vec![TypedValue::Int(1), TypedValue::Int(2)],
            },
            TypedValue::Map {
                key_type: Type::String,
                value_type: Type::Int,
                entries: vec![(TypedValue::String("a".into()), TypedValue::Int(1))],
            },
            TypedValue::Option {
                inner_type: Type::Int,
                value: Some(Box::new(TypedValue::Int(7))),
            },
            TypedValue::Result {
                ok_type: Type::Int,
                error_type: Type::String,
                is_ok: false,
                value: Box::new(TypedValue::String("no".into())),
            },
            TypedValue::Record(vec![("age".into(), TypedValue::Int(38))]),
            TypedValue::Variant {
                name: "some".into(),
                value: Some(Box::new(TypedValue::Int(9))),
            },
            TypedValue::Closure {
                function: "lambda$1".into(),
                captures: vec![TypedValue::Int(2)],
                signature,
            },
            TypedValue::Task {
                id: "task".into(),
                result_type: Type::String,
                kind: TaskKind::CpuFiber,
            },
            TypedValue::Fiber {
                id: "fiber".into(),
                yield_type: Type::Int,
                result_type: Type::String,
            },
            TypedValue::Stream {
                id: "stream".into(),
                element_type: Type::Bytes,
                kind: "file-lines".into(),
                generation: 8,
            },
            TypedValue::Resource {
                kind: "output".into(),
                handle: "opaque".into(),
                generation: 3,
            },
            TypedValue::Dynamic {
                runtime_type: Type::Int,
                value: Box::new(TypedValue::Int(12)),
            },
        ];
        for value in values {
            assert_eq!(round_trip_value(&value)?, value);
        }
        Ok(())
    }

    #[test]
    fn every_type_arm_round_trips() -> Result<()> {
        let selectors = vec![
            ResourceSelector::None,
            ResourceSelector::File {
                selector: FileSelector {
                    root: ResourceRoot::Named("root".into()),
                    pattern: "**".into(),
                },
            },
            ResourceSelector::FileTemplate {
                template: FileSelectorTemplate {
                    root: ResourceRoot::Project,
                    parts: vec![
                        FileSelectorTemplatePart::Literal {
                            relative: "src".into(),
                        },
                        FileSelectorTemplatePart::Argument {
                            index: 2,
                            bound: FileSelector {
                                root: ResourceRoot::Project,
                                pattern: "src/**".into(),
                            },
                        },
                    ],
                    upper_bound: FileSelector {
                        root: ResourceRoot::Project,
                        pattern: "src/**".into(),
                    },
                },
            },
            ResourceSelector::NetworkTemplate {
                template: NetworkSelectorTemplate {
                    host_argument: 0,
                    port_argument: 1,
                    allowed_hosts: vec!["localhost".into()],
                    allowed_ports: vec![443],
                },
            },
            ResourceSelector::Network {
                host: "localhost".into(),
                ports: vec![80, 443],
            },
            ResourceSelector::Automation {
                application: Some("Terminal".into()),
            },
            ResourceSelector::Agent {
                providers: vec!["grok".into()],
                max_depth: 2,
                max_children: 3,
            },
            ResourceSelector::Process {
                executables: vec!["git".into()],
            },
            ResourceSelector::ProcessTemplate {
                template: ProcessSelectorTemplate {
                    executable_argument: 4,
                    allowed_executables: vec!["cargo".into()],
                },
            },
            ResourceSelector::Program {
                languages: vec!["lisp".into()],
            },
            ResourceSelector::ProgramTemplate {
                template: ProgramSelectorTemplate {
                    language_argument: 5,
                    allowed_languages: vec!["forth".into()],
                },
            },
            ResourceSelector::Mcp {
                server: "fs".into(),
                tool: "read".into(),
            },
            ResourceSelector::McpTemplate {
                template: McpSelectorTemplate {
                    server_argument: 6,
                    tool_argument: 7,
                    allowed_servers: vec!["fs".into()],
                    allowed_tools: vec!["read".into()],
                },
            },
            ResourceSelector::Memory {
                tree: "user".into(),
                path: "preferences".into(),
            },
            ResourceSelector::Schedule {
                policy: Some("daily".into()),
            },
        ];
        let effects = EffectSet(
            selectors
                .into_iter()
                .enumerate()
                .map(|(index, selector)| CapabilityRequirement {
                    capability: if index % 2 == 0 {
                        CapabilityKind::VmRead
                    } else {
                        CapabilityKind::VmWrite
                    },
                    selector,
                })
                .collect(),
        );
        let types = vec![
            Type::Unit,
            Type::Bool,
            Type::Int,
            Type::UInt,
            Type::Float,
            Type::Char,
            Type::Symbol,
            Type::String,
            Type::Bytes,
            Type::Json,
            Type::Path(FileSelector {
                root: ResourceRoot::TaskOutput,
                pattern: "result".into(),
            }),
            Type::List(Box::new(Type::Int)),
            Type::Map(Box::new(Type::String), Box::new(Type::Int)),
            Type::Option(Box::new(Type::String)),
            Type::Result(Box::new(Type::Int), Box::new(Type::String)),
            Type::Record(vec![("field".into(), Type::Int)]),
            Type::Variant(vec![
                ("none".into(), None),
                ("some".into(), Some(Type::Int)),
            ]),
            Type::Function {
                arguments: vec![Type::Int],
                result: Box::new(Type::String),
                effects,
                suspension: Some(SuspensionSignature::one_way(Type::Int)),
            },
            Type::Task(Box::new(Type::Int)),
            Type::Fiber(Box::new(Type::Int), Box::new(Type::String)),
            Type::Stream(Box::new(Type::Bytes)),
            Type::Resource("socket".into()),
            Type::Capability("grant".into()),
            Type::Variable("T".into()),
            Type::Dynamic,
        ];
        for value in types {
            assert_eq!(round_trip_type(&value)?, value);
        }
        Ok(())
    }

    #[test]
    fn every_instruction_arm_round_trips() -> Result<()> {
        let variants = vec![("none".into(), None), ("some".into(), Some(Type::Int))];
        let fields = vec![("field".into(), Type::Int)];
        let signature = sample_signature();
        let instructions = vec![
            Instruction::Constant {
                value: TypedValue::Int(1),
            },
            Instruction::MakeList {
                element_type: Type::Int,
                count: 2,
            },
            Instruction::MakeMap {
                key_type: Type::String,
                value_type: Type::Int,
                count: 1,
            },
            Instruction::MakeRecord {
                fields: fields.clone(),
            },
            Instruction::MakeVariant {
                variants: variants.clone(),
                tag: "some".into(),
                payload_type: Some(Type::Int),
            },
            Instruction::VariantGet {
                variants: variants.clone(),
                tag: "none".into(),
                payload_type: None,
            },
            Instruction::RecordGet {
                field: "field".into(),
                value_type: Type::Int,
            },
            Instruction::RecordSet {
                field: "field".into(),
                value_type: Type::Int,
                record_type: fields,
            },
            Instruction::Dup,
            Instruction::Drop,
            Instruction::Swap,
            Instruction::LocalGet { index: 1 },
            Instruction::LocalSet { index: 2 },
            Instruction::CaptureGet { index: 3 },
            Instruction::MakeClosure {
                function: "lambda$1".into(),
                capture_count: 1,
                signature: signature.clone(),
            },
            Instruction::Call {
                function: "foo".into(),
            },
            Instruction::CallClosure {
                signature: signature.clone(),
            },
            Instruction::CapabilityRequest {
                requirement: sample_requirement(),
                input: vec![Type::Path(FileSelector {
                    root: ResourceRoot::Workspace,
                    pattern: "src/**".into(),
                })],
                output: vec![Type::Bytes],
            },
            Instruction::OutputOpen,
            Instruction::UiEffect {
                operation: UiOperation::Progress,
                input: vec![Type::Int],
                output: vec![Type::Unit],
            },
            Instruction::Yield {
                value_type: Type::Int,
            },
            Instruction::DeferFiber,
            Instruction::NextFiber,
            Instruction::JoinFiber,
            Instruction::CancelFiber,
            Instruction::DeferCpu,
            Instruction::PollCpuFiber,
            Instruction::JoinCpuFiber,
            Instruction::CancelCpuFiber,
            Instruction::PropagateResult {
                return_ok_type: Type::Int,
                error_type: Type::String,
            },
            Instruction::Jump { target: 4 },
            Instruction::Branch {
                then_block: 5,
                else_block: 6,
            },
            Instruction::Return,
            Instruction::Trap {
                code: "E-TEST".into(),
            },
        ];
        for instruction in instructions {
            assert_eq!(round_trip_instruction(&instruction)?, instruction);
        }
        Ok(())
    }

    #[test]
    fn real_closure_and_suspended_fiber_checkpoint_round_trip() -> Result<()> {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Lisp,
            "checkpoint.lisp",
            "(begin \
               (define (make-adder (n : int)) (lambda ((x : int)) (+ n x))) \
               (make-adder 7) \
               (defer (lambda () (begin (yield 11) 13))))",
            20_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);
        let checkpoint = runtime
            .checkpoint()
            .map_err(|error| anyhow!(error.to_string()))?;
        assert!(!checkpoint.functions.is_empty());
        assert_eq!(checkpoint.producer_fibers.len(), 1);

        let mut ambiguous = encode_checkpoint_bytes(&checkpoint)?;
        ambiguous.extend_from_slice(&[0; 8]);
        assert!(decode_checkpoint_bytes(&ambiguous)
            .unwrap_err()
            .to_string()
            .contains("trailing bytes"));

        let decoded = round_trip_checkpoint(&checkpoint)?;
        assert_eq!(decoded, checkpoint);

        let mut restored = TypedRuntime::from_checkpoint(decoded)
            .map_err(|errors| anyhow!("checkpoint failed verification: {errors:?}"))?;
        let advanced = restored.execute(
            ProgramLanguage::Forth,
            "advance.forth",
            "fiber-next",
            20_000,
        );
        assert_eq!(advanced.status, TypedExecutionStatus::Completed);
        assert!(matches!(
            advanced.values.last(),
            Some(TypedValue::Result { is_ok: true, value, .. }) if **value == TypedValue::Int(11)
        ));
        Ok(())
    }
}
