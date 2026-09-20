//! Performing a host effect under authority.
//!
//! Everything a typed program's effect touches on the way out of the VM: the capability handler
//! that dispatches one, the validation a host binding must pass, opening a resource beneath a
//! bound root, and spawning a process. It is half of what `ProgramRuntime` used to be, and it is
//! the half that talks to the operating system.
//!
//! The runtime service itself — submitting, resuming and cancelling executions — stays in `super`.

use super::*;

pub(super) struct TypedHostHandler {
    automation: Arc<AutomationBroker>,
    resource_roots: Arc<RwLock<ResourceRootState>>,
    output: String,
    output_chunks: Vec<String>,
    side_effects: Vec<crate::vm::HostSideEffect>,
    scheduler: Option<agent_vm::AgentVmBinding>,
    memory: Option<Arc<finch_memory::MemorySystem>>,
    mcp_client: Option<Arc<dyn RuntimeMcpClient>>,
    artifact_proposal_host: Option<Arc<dyn ArtifactProposalHost>>,
    mcp_output_schemas: BTreeMap<String, serde_json::Value>,
    vocabulary: String,
    network: Arc<Mutex<HashMap<String, NetworkSocket>>>,
    output_handles: Arc<Mutex<HashMap<String, OutputHandleRecord>>>,
    streams: Arc<Mutex<HashMap<String, HostStream>>>,
    pub(super) execution_id: uuid::Uuid,
    pub(super) resource_generation: u64,
    authorization: HostAuthorizationAudit,
    network_grants: EffectSet,
    typed_effect_sink: Option<TypedEffectSink>,
    deferred_host_effects: DeferredHostEffects,
    effect_audit: Option<crate::runtime::effect_audit::RunnerEffectAuditControl>,
    authorization_attempt: Option<HostAuthorizationAttempt>,
}

pub(super) struct HostAuthorizationAttempt {
    request: CapabilityRequest,
    decision: AuthorizationDecision,
    use_started: bool,
}

pub(super) struct HostAuthorizationAudit {
    pub(super) ledger: Arc<Mutex<CapabilityLedger>>,
    pub(super) policy: Arc<RwLock<CapabilityPolicy>>,
    pub(super) use_gate: Arc<RwLock<()>>,
    pub(super) sink: Option<ProgramRuntimeAuthoritySink>,
    pub(super) context: AuthorizationContext,
    pub(super) reason: String,
    pub(super) program_hash: String,
    pub(super) agent_ancestry: Vec<uuid::Uuid>,
}

impl TypedHostHandler {
    pub(super) fn new(
        automation: Arc<AutomationBroker>,
        resource_roots: Arc<RwLock<ResourceRootState>>,
        scheduler: Option<agent_vm::AgentVmBinding>,
        memory: Option<Arc<finch_memory::MemorySystem>>,
        mcp_client: Option<Arc<dyn RuntimeMcpClient>>,
        artifact_proposal_host: Option<Arc<dyn ArtifactProposalHost>>,
        mcp_output_schemas: BTreeMap<String, serde_json::Value>,
        vocabulary: String,
        network: Arc<Mutex<HashMap<String, NetworkSocket>>>,
        output_handles: Arc<Mutex<HashMap<String, OutputHandleRecord>>>,
        streams: Arc<Mutex<HashMap<String, HostStream>>>,
        execution_id: uuid::Uuid,
        resource_generation: u64,
        authorization: HostAuthorizationAudit,
        network_grants: EffectSet,
        typed_effect_sink: Option<TypedEffectSink>,
        deferred_host_effects: DeferredHostEffects,
        effect_audit: Option<crate::runtime::effect_audit::RunnerEffectAuditControl>,
    ) -> Self {
        Self {
            automation,
            resource_roots,
            output: String::new(),
            output_chunks: Vec::new(),
            side_effects: Vec::new(),
            scheduler,
            memory,
            mcp_client,
            artifact_proposal_host,
            mcp_output_schemas,
            vocabulary,
            network,
            output_handles,
            streams,
            execution_id,
            resource_generation,
            authorization,
            network_grants,
            typed_effect_sink,
            deferred_host_effects,
            effect_audit,
            authorization_attempt: None,
        }
    }

    fn mark_host_use(&mut self) {
        // "Use" is the first point at which the host has either acquired the
        // selected object for observation or is about to issue a potentially
        // mutating external call. Validation, lookup, and opening a read-only
        // object are attempts until they succeed; a write/connect/send/spawn
        // becomes a use immediately before the first syscall that may cause
        // an external effect. Failures before this point roll back the exact
        // authorization fact (and once consumption); failures after it retain
        // the audit because an effect may already have occurred.
        if let Some(attempt) = &mut self.authorization_attempt {
            attempt.use_started = true;
        }
    }

    /// Advance one host-owned stream. The stream's opaque kind/ID is checked
    /// here rather than inferred from a source string, and each backing cursor
    /// remains owned by the ProgramRun that opened it.
    fn stream_next(
        &mut self,
        arguments: &[TypedValue],
        origin: &crate::vm::SourceOrigin,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let [TypedValue::Stream {
            id,
            element_type,
            kind,
            generation,
        }] = arguments
        else {
            return Err(host_binding_error(
                origin,
                "stream-next requires one stream<T>",
            ));
        };
        match kind.as_str() {
            "csv-records" if *element_type == Type::list(Type::String) => {
                let mut streams = self
                    .streams
                    .lock()
                    .map_err(|_| host_binding_error(origin, "stream registry lock poisoned"))?;
                let stream = streams
                    .get_mut(id)
                    .ok_or_else(|| host_binding_error(origin, "CSV stream is unknown or closed"))?;
                if stream.owner != self.execution_id || stream.generation != *generation {
                    return Err(host_binding_error(
                        origin,
                        "CSV stream does not belong to this ProgramRun",
                    ));
                }
                let HostStreamBackend::CsvRecords(reader) = &mut stream.backend else {
                    return Err(host_binding_error(
                        origin,
                        "CSV stream backend is malformed",
                    ));
                };
                if let Some(attempt) = &mut self.authorization_attempt {
                    attempt.use_started = true;
                }
                let record = read_bounded_csv_record(reader)
                    .map_err(|message| host_binding_error(origin, message))?;
                Ok(vec![TypedValue::Option {
                    inner_type: Type::list(Type::String),
                    value: record.map(|fields| {
                        Box::new(TypedValue::List {
                            element_type: Type::String,
                            values: fields.into_iter().map(TypedValue::String).collect(),
                        })
                    }),
                }])
            }
            "file-lines" if *element_type == Type::String => {
                let mut streams = self
                    .streams
                    .lock()
                    .map_err(|_| host_binding_error(origin, "stream registry lock poisoned"))?;
                let stream = streams.get_mut(id).ok_or_else(|| {
                    host_binding_error(origin, "file line stream is unknown or closed")
                })?;
                if stream.owner != self.execution_id || stream.generation != *generation {
                    return Err(host_binding_error(
                        origin,
                        "file line stream does not belong to this ProgramRun",
                    ));
                }
                let HostStreamBackend::FileLines(reader) = &mut stream.backend else {
                    return Err(host_binding_error(
                        origin,
                        "file line stream backend is malformed",
                    ));
                };
                if let Some(attempt) = &mut self.authorization_attempt {
                    attempt.use_started = true;
                }
                let line = read_bounded_utf8_line(reader)
                    .map_err(|message| host_binding_error(origin, message))?;
                Ok(vec![TypedValue::Option {
                    inner_type: Type::String,
                    value: line.map(|line| Box::new(TypedValue::String(line))),
                }])
            }
            "workbook-rows" if *element_type == Type::list(Type::String) => {
                let mut streams = self
                    .streams
                    .lock()
                    .map_err(|_| host_binding_error(origin, "stream registry lock poisoned"))?;
                let stream = streams.get_mut(id).ok_or_else(|| {
                    host_binding_error(origin, "workbook stream is unknown or closed")
                })?;
                if stream.owner != self.execution_id || stream.generation != *generation {
                    return Err(host_binding_error(
                        origin,
                        "workbook stream does not belong to this ProgramRun",
                    ));
                }
                let HostStreamBackend::WorkbookRows(rows) = &mut stream.backend else {
                    return Err(host_binding_error(
                        origin,
                        "workbook stream backend is malformed",
                    ));
                };
                if let Some(attempt) = &mut self.authorization_attempt {
                    attempt.use_started = true;
                }
                Ok(vec![TypedValue::Option {
                    inner_type: Type::list(Type::String),
                    value: rows.next().map(|row| {
                        Box::new(TypedValue::List {
                            element_type: Type::String,
                            values: row.into_iter().map(TypedValue::String).collect(),
                        })
                    }),
                }])
            }
            _ => Err(host_binding_error(
                origin,
                "stream-next received an unknown or malformed stream",
            )),
        }
    }

    /// Explicit stream cancellation/release. Closing twice fails rather than
    /// silently creating a new cursor or masking an ownership violation.
    fn stream_close(
        &mut self,
        arguments: &[TypedValue],
        origin: &crate::vm::SourceOrigin,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let [TypedValue::Stream {
            id,
            element_type,
            kind,
            generation,
        }] = arguments
        else {
            return Err(host_binding_error(
                origin,
                "stream-close requires one stream<T>",
            ));
        };
        let expected_backend = match kind.as_str() {
            "csv-records" if *element_type == Type::list(Type::String) => "csv",
            "file-lines" if *element_type == Type::String => "lines",
            "workbook-rows" if *element_type == Type::list(Type::String) => "workbook",
            _ => {
                return Err(host_binding_error(
                    origin,
                    "stream-close received an unknown or malformed stream",
                ))
            }
        };
        let mut streams = self
            .streams
            .lock()
            .map_err(|_| host_binding_error(origin, "stream registry lock poisoned"))?;
        let stream = streams
            .get(id)
            .ok_or_else(|| host_binding_error(origin, "stream is unknown or closed"))?;
        if stream.owner != self.execution_id || stream.generation != *generation {
            return Err(host_binding_error(
                origin,
                "stream does not belong to this ProgramRun",
            ));
        }
        let backend_matches = matches!(
            (&stream.backend, expected_backend),
            (HostStreamBackend::CsvRecords(_), "csv")
                | (HostStreamBackend::FileLines(_), "lines")
                | (HostStreamBackend::WorkbookRows(_), "workbook")
        );
        if !backend_matches {
            return Err(host_binding_error(origin, "stream backend is malformed"));
        }
        if let Some(attempt) = &mut self.authorization_attempt {
            attempt.use_started = true;
        }
        streams.remove(id);
        Ok(vec![TypedValue::Unit])
    }
}

pub(super) fn typed_agent_task_spec(
    value: &TypedValue,
    origin: &SourceOrigin,
) -> std::result::Result<agents::AgentTaskSpec, VmDiagnostic> {
    let TypedValue::Record(fields) = value else {
        return Err(host_binding_error(
            origin,
            "agent-spawn-with requires an agent task specification record",
        ));
    };
    if value.value_type() != agent_task_spec_type() {
        return Err(host_binding_error(
            origin,
            "agent task specification has the wrong fields or field types",
        ));
    }
    let field = |name: &str| {
        fields
            .iter()
            .find_map(|(field, value)| (field == name).then_some(value))
            .ok_or_else(|| {
                host_binding_error(
                    origin,
                    format!("agent task specification is missing '{name}'"),
                )
            })
    };
    let string = |name: &str| match field(name)? {
        TypedValue::String(value) => Ok(value.clone()),
        _ => Err(host_binding_error(
            origin,
            format!("agent task field '{name}' must be a string"),
        )),
    };
    let integer = |name: &str| match field(name)? {
        TypedValue::Int(value) => Ok(*value),
        _ => Err(host_binding_error(
            origin,
            format!("agent task field '{name}' must be an integer"),
        )),
    };
    let role = match string("role")?.as_str() {
        "general" => agents::AgentRole::General,
        "explore" => agents::AgentRole::Explore,
        "research" => agents::AgentRole::Research,
        "code" => agents::AgentRole::Code,
        role => {
            return Err(host_binding_error(
                origin,
                format!("unknown agent role '{role}'"),
            ))
        }
    };
    let optional = |value: String| (!value.trim().is_empty()).then_some(value);
    let max_turns = usize::try_from(integer("max-turns")?)
        .map_err(|_| host_binding_error(origin, "agent max-turns must be non-negative"))?;
    let timeout_ms = u64::try_from(integer("timeout-ms")?)
        .map_err(|_| host_binding_error(origin, "agent timeout-ms must be non-negative"))?;
    let max_output_bytes = usize::try_from(integer("max-output-bytes")?)
        .map_err(|_| host_binding_error(origin, "agent max-output-bytes must be non-negative"))?;
    let context = match field("context-refs")? {
        TypedValue::List { values, .. } => values
            .iter()
            .map(|value| {
                let TypedValue::Record(fields) = value else {
                    return Err(host_binding_error(
                        origin,
                        "agent context reference must be a record",
                    ));
                };
                let string_field = |name: &str| {
                    fields
                        .iter()
                        .find_map(|(field, value)| (field == name).then_some(value))
                        .and_then(|value| match value {
                            TypedValue::String(value) => Some(value.clone()),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            host_binding_error(
                                origin,
                                format!("agent context reference '{name}' must be a string"),
                            )
                        })
                };
                Ok(agents::AgentContextReference {
                    kind: string_field("kind")?,
                    id: string_field("id")?,
                    sha256: string_field("sha256")?,
                })
            })
            .collect::<std::result::Result<Vec<_>, VmDiagnostic>>()?,
        _ => {
            return Err(host_binding_error(
                origin,
                "agent task field 'context-refs' must be a list of context-reference records",
            ))
        }
    };
    let capability_grant_ids = match field("capabilities")? {
        TypedValue::List { values, .. } => values
            .iter()
            .map(|value| {
                let TypedValue::Resource {
                    kind,
                    handle,
                    generation,
                } = value
                else {
                    return Err(host_binding_error(
                        origin,
                        "agent capability selection requires capability-grant resources",
                    ));
                };
                if kind != "capability-grant" || *generation != 0 {
                    return Err(host_binding_error(
                        origin,
                        "agent capability selection contains an invalid grant resource",
                    ));
                }
                uuid::Uuid::parse_str(handle).map_err(|_| {
                    host_binding_error(origin, "capability-grant resource has an invalid handle")
                })
            })
            .collect::<std::result::Result<Vec<_>, VmDiagnostic>>()?,
        _ => {
            return Err(host_binding_error(
                origin,
                "agent task field 'capabilities' must be a list of capability-grant resources",
            ))
        }
    };
    Ok(agents::AgentTaskSpec {
        task: string("task")?,
        role,
        background: optional(string("background")?),
        provider: optional(string("provider")?),
        model: optional(string("model")?),
        context,
        capability_grant_ids: Some(capability_grant_ids),
        budget: agents::AgentBudget {
            max_turns,
            timeout_ms,
            max_output_bytes,
        },
    })
}

pub(super) fn agent_task_status_name(status: agents::AgentTaskStatus) -> &'static str {
    match status {
        agents::AgentTaskStatus::Queued => "queued",
        agents::AgentTaskStatus::Running => "running",
        agents::AgentTaskStatus::Completed => "completed",
        agents::AgentTaskStatus::Failed => "failed",
        agents::AgentTaskStatus::Cancelled => "cancelled",
    }
}

pub(super) fn agent_role_name(role: agents::AgentRole) -> &'static str {
    match role {
        agents::AgentRole::General => "general",
        agents::AgentRole::Explore => "explore",
        agents::AgentRole::Research => "research",
        agents::AgentRole::Code => "code",
    }
}

pub(super) fn typed_agent_task_result(
    result: agents::AgentTaskResult,
    origin: &SourceOrigin,
) -> std::result::Result<TypedValue, VmDiagnostic> {
    let turns = i64::try_from(result.turns)
        .map_err(|_| host_binding_error(origin, "agent turn count exceeds VM integer range"))?;
    let elapsed_ms = i64::try_from(result.elapsed_ms)
        .map_err(|_| host_binding_error(origin, "agent elapsed time exceeds VM integer range"))?;
    let depth = i64::try_from(result.identity.depth)
        .map_err(|_| host_binding_error(origin, "agent depth exceeds VM integer range"))?;
    let value = TypedValue::Record(vec![
        (
            "task-id".into(),
            TypedValue::String(result.identity.task_id.to_string()),
        ),
        (
            "agent-id".into(),
            TypedValue::String(result.identity.agent_id.to_string()),
        ),
        (
            "status".into(),
            TypedValue::String(agent_task_status_name(result.status).into()),
        ),
        (
            "final-message".into(),
            TypedValue::String(result.final_message),
        ),
        (
            "diagnostics".into(),
            TypedValue::List {
                element_type: Type::String,
                values: result
                    .diagnostics
                    .into_iter()
                    .map(TypedValue::String)
                    .collect(),
            },
        ),
        ("turns".into(), TypedValue::Int(turns)),
        ("elapsed-ms".into(), TypedValue::Int(elapsed_ms)),
        (
            "provider-model".into(),
            TypedValue::String(result.identity.provider_model),
        ),
        (
            "starting-context-hash".into(),
            TypedValue::String(result.identity.starting_context_hash),
        ),
        ("depth".into(), TypedValue::Int(depth)),
    ]);
    debug_assert_eq!(value.value_type(), agent_task_result_type());
    Ok(value)
}

/// Project a hydration status onto the record `mem-index-status` returns.
///
/// The counts are options because `Failed` carries none: hydration ended
/// without a trustworthy total, so there is no number to report. Substituting
/// zero would be reporting a measurement that was never taken, which is wrong
/// on its own terms whether or not a reader is misled by it. (`state` and
/// `complete` would still say `failed` and `false` alongside it, so the record
/// as a whole would not claim the index is fine -- the objection is to the
/// fabricated number, not to a contradiction.)
///
/// `complete` is true only for `Ready`. `Degraded` is deliberately not
/// complete even though it will not load more on its own: what loaded is
/// coherent, but it is not everything, so a program asking "did I see the
/// whole index" must get no. (`reload_tree_from_db` can still upgrade it to
/// `Ready` via `clear_failure`; "will never load more" would be too strong.)
///
/// **Call it before the recall it qualifies, not after.** This takes one
/// sample, where `mem-recall` brackets its query with
/// `memory_status::observed(before, after)` and keeps the worse of two. A
/// status read *after* a recall fails open, which is the dangerous direction:
/// hydration can finish between the two instructions, so a recall that saw 100
/// of 2048 nodes is followed by `ready, complete, 2048 of 2048`, and the
/// program concludes a partial answer was total. Sampling first is
/// conservative instead. Not because hydration only ever improves -- it does
/// not, `Loading` can reach `Degraded` or `Failed`, and this file's own
/// fixture drives exactly that -- but because `complete` is true only for
/// `Ready`, and nothing leaves `Ready` for a worse state in-process. So a
/// status taken before the recall can understate completeness but never
/// overstate it.
pub(super) fn typed_memory_index_status(
    status: finch_memory::HydrationStatus,
    origin: &SourceOrigin,
) -> std::result::Result<TypedValue, VmDiagnostic> {
    use finch_memory::HydrationStatus;

    let count = |value: usize| -> std::result::Result<TypedValue, VmDiagnostic> {
        let value = i64::try_from(value).map_err(|_| {
            host_binding_error(origin, "memory node count exceeds the VM integer range")
        })?;
        Ok(TypedValue::Option {
            inner_type: Type::Int,
            value: Some(Box::new(TypedValue::Int(value))),
        })
    };
    let no_count = TypedValue::Option {
        inner_type: Type::Int,
        value: None,
    };
    let reason_of = |reason: Option<String>| TypedValue::Option {
        inner_type: Type::String,
        value: reason.map(|reason| Box::new(TypedValue::String(reason))),
    };

    let (state, complete, loaded, total, reason) = match status {
        HydrationStatus::Ready { nodes } => {
            ("ready", true, count(nodes)?, count(nodes)?, reason_of(None))
        }
        HydrationStatus::Loading { loaded, total } => (
            "loading",
            false,
            count(loaded)?,
            count(total)?,
            reason_of(None),
        ),
        HydrationStatus::Degraded {
            loaded,
            total,
            reason,
        } => (
            "degraded",
            false,
            count(loaded)?,
            count(total)?,
            reason_of(Some(reason)),
        ),
        HydrationStatus::Failed { reason } => (
            "failed",
            false,
            no_count.clone(),
            no_count,
            reason_of(Some(reason)),
        ),
    };

    Ok(TypedValue::Record(vec![
        ("state".into(), TypedValue::String(state.into())),
        ("complete".into(), TypedValue::Bool(complete)),
        ("loaded".into(), loaded),
        ("total".into(), total),
        ("reason".into(), reason),
    ]))
}

pub(super) fn typed_agent_task_snapshot(
    snapshot: agents::AgentTaskSnapshot,
    origin: &SourceOrigin,
) -> std::result::Result<TypedValue, VmDiagnostic> {
    let depth = i64::try_from(snapshot.identity.depth)
        .map_err(|_| host_binding_error(origin, "agent depth exceeds VM integer range"))?;
    let complete = matches!(
        snapshot.status,
        agents::AgentTaskStatus::Completed
            | agents::AgentTaskStatus::Failed
            | agents::AgentTaskStatus::Cancelled
    );
    let value = TypedValue::Record(vec![
        (
            "task-id".into(),
            TypedValue::String(snapshot.identity.task_id.to_string()),
        ),
        (
            "agent-id".into(),
            TypedValue::String(snapshot.identity.agent_id.to_string()),
        ),
        (
            "status".into(),
            TypedValue::String(agent_task_status_name(snapshot.status).into()),
        ),
        ("task".into(), TypedValue::String(snapshot.task)),
        (
            "role".into(),
            TypedValue::String(agent_role_name(snapshot.role).into()),
        ),
        (
            "provider-model".into(),
            TypedValue::String(snapshot.identity.provider_model),
        ),
        (
            "starting-context-hash".into(),
            TypedValue::String(snapshot.identity.starting_context_hash),
        ),
        ("depth".into(), TypedValue::Int(depth)),
        ("complete".into(), TypedValue::Bool(complete)),
    ]);
    debug_assert_eq!(value.value_type(), agent_task_snapshot_type());
    Ok(value)
}

impl crate::vm::CapabilityHandler for TypedHostHandler {
    fn prepare_awaited_effect(
        &mut self,
        effect: &mut VmSideEffect,
    ) -> std::result::Result<(), VmDiagnostic> {
        let binding = registered_host_binding(&effect.requirement, &effect.origin)?;
        let crate::vm::HostSideEffect::Request { arguments } = &effect.event else {
            return Err(host_binding_error(
                &effect.origin,
                "typed host operation requires a typed host request",
            ));
        };
        if effect.requirement.capability == CapabilityKind::NetworkConnect
            && binding == Some(CoreHostBinding::NetworkSend)
        {
            let Some(TypedValue::Resource {
                kind,
                handle,
                generation,
            }) = arguments.first()
            else {
                return Err(host_binding_error(
                    &effect.origin,
                    "network-send requires a socket resource",
                ));
            };
            if kind != "network-socket" {
                return Err(host_binding_error(
                    &effect.origin,
                    "network-send resource is not a network socket",
                ));
            }
            let sockets = self
                .network
                .lock()
                .map_err(|_| host_binding_error(&effect.origin, "network lock poisoned"))?;
            let socket = sockets
                .get(handle)
                .ok_or_else(|| host_binding_error(&effect.origin, "unknown network socket"))?;
            if socket.owner != self.execution_id || socket.generation != *generation {
                return Err(host_binding_error(
                    &effect.origin,
                    "network socket is stale or belongs to another ProgramRun",
                ));
            }
            effect.requirement.selector = ResourceSelector::Network {
                host: socket.host.clone(),
                ports: vec![socket.port],
            };
        } else if effect.requirement.capability == CapabilityKind::FileRead
            && matches!(
                binding,
                Some(
                    CoreHostBinding::FileLinesNext
                        | CoreHostBinding::FileLinesClose
                        | CoreHostBinding::CsvNext
                        | CoreHostBinding::CsvClose
                        | CoreHostBinding::StreamNext
                        | CoreHostBinding::StreamClose
                )
            )
        {
            let Some(TypedValue::Stream { id, generation, .. }) = arguments.first() else {
                return Err(host_binding_error(
                    &effect.origin,
                    "stream operation requires a stream resource",
                ));
            };
            let streams = self
                .streams
                .lock()
                .map_err(|_| host_binding_error(&effect.origin, "stream registry lock poisoned"))?;
            let stream = streams
                .get(id)
                .ok_or_else(|| host_binding_error(&effect.origin, "stream is unknown or closed"))?;
            if stream.owner != self.execution_id || stream.generation != *generation {
                return Err(host_binding_error(
                    &effect.origin,
                    "stream does not belong to this ProgramRun",
                ));
            }
            effect.requirement = stream.requirement.clone();
        } else if effect.requirement.capability == CapabilityKind::ProcessRun {
            if binding != Some(CoreHostBinding::ProcessRun) {
                return Err(host_binding_error(
                    &effect.origin,
                    "process execution requires the registered process-run host binding",
                ));
            }
            let [TypedValue::String(command), TypedValue::List { values, .. }] =
                arguments.as_slice()
            else {
                return Err(host_binding_error(
                    &effect.origin,
                    "process-run requires an executable and string arguments",
                ));
            };
            let process_arguments = values
                .iter()
                .map(|value| match value {
                    TypedValue::String(value) => Ok(value.clone()),
                    _ => Err(host_binding_error(
                        &effect.origin,
                        "process-run arguments must be strings",
                    )),
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let identity = open_process_executable(command, &process_arguments)
                .map(|opened| opened.identity)
                .map_err(|message| host_binding_error(&effect.origin, message))?;
            effect.requirement.selector = ResourceSelector::Process {
                executables: vec![identity.encode()],
            };
        }
        validate_core_host_request(binding, &effect.requirement, arguments, &effect.origin)
    }

    fn authorize_awaited_effect(
        &mut self,
        effect: &VmSideEffect,
    ) -> std::result::Result<(), VmDiagnostic> {
        if effect.requirement.capability == CapabilityKind::ProcessRun {
            validate_process_effect(effect)?;
        }
        let requested = EffectSet::from_requirement(effect.requirement.clone());
        if TypedRuntime::intrinsic_grants().grants(&requested) {
            return Ok(());
        }
        let arguments = match &effect.event {
            crate::vm::HostSideEffect::Request { arguments } => arguments.clone(),
            _ => {
                return Err(VmDiagnostic::error(
                    "E-HOST-002",
                    crate::vm::DiagnosticPhase::HostCall,
                    "VM await boundary did not carry a host request",
                    Some(effect.origin.clone()),
                ));
            }
        };
        let request_key = format!("effect:{}", effect.sequence);
        let request = CapabilityRequest {
            id: uuid::Uuid::new_v5(&self.execution_id, request_key.as_bytes()),
            execution_id: self.execution_id,
            effect_sequence: Some(effect.sequence),
            requirement: effect.requirement.clone(),
            arguments,
            reason: self.authorization.reason.clone(),
            origin: effect.origin.clone(),
            agent_ancestry: self.authorization.agent_ancestry.clone(),
            program_hash: self.authorization.program_hash.clone(),
        };
        let policy = self
            .authorization
            .policy
            .read()
            .map_err(|_| host_binding_error(&effect.origin, "capability policy lock poisoned"))?
            .clone();
        if !policy.permits(&effect.requirement) {
            return Err(VmDiagnostic::error(
                "E-CAP-006",
                crate::vm::DiagnosticPhase::HostCall,
                format!(
                    "capability {:?} is denied by policy {}",
                    effect.requirement.capability, policy.policy_hash
                ),
                Some(effect.origin.clone()),
            ));
        }
        let mut context = self.authorization.context.clone();
        context.now_unix_ms = unix_time_ms();
        context.policy_hash = policy.policy_hash.clone();
        let mut ledger =
            self.authorization.ledger.lock().map_err(|_| {
                host_binding_error(&effect.origin, "capability ledger lock poisoned")
            })?;
        let recorded = ledger.recorded_authorization(&request, &context);
        let previous = recorded.is_none().then(|| ledger.clone());
        let decision =
            recorded.unwrap_or_else(|| ledger.authorize(&request, &context, "typed-host-boundary"));
        if let (Some(previous), Some(sink)) = (previous.as_ref(), &self.authorization.sink) {
            let Some(project_id) = context.project_id.clone() else {
                *ledger = previous.clone();
                return Err(host_binding_error(
                    &effect.origin,
                    "host authorization has no project identity",
                ));
            };
            let roots = self.resource_roots.read().map_err(|_| {
                host_binding_error(&effect.origin, "resource-root binding lock poisoned")
            })?;
            let state = authority_state_from_parts(
                context.session_id,
                project_id,
                policy,
                ledger.clone(),
                &roots,
            );
            if let Err(error) = sink(state) {
                *ledger = previous.clone();
                return Err(host_binding_error(
                    &effect.origin,
                    format!("persist host authorization audit: {error:#}"),
                ));
            }
        }
        match decision {
            AuthorizationDecision::Allowed { .. } => {
                self.authorization_attempt = Some(HostAuthorizationAttempt {
                    request,
                    decision,
                    use_started: false,
                });
                Ok(())
            }
            AuthorizationDecision::ApprovalRequired => Err(VmDiagnostic::error(
                "E-CAP-006",
                crate::vm::DiagnosticPhase::HostCall,
                "capability was revoked, expired, or outside its approved scope at the host boundary",
                Some(effect.origin.clone()),
            )),
            AuthorizationDecision::Denied { reason } => Err(VmDiagnostic::error(
                "E-CAP-006",
                crate::vm::DiagnosticPhase::HostCall,
                format!("capability is denied at the host boundary: {reason}"),
                Some(effect.origin.clone()),
            )),
        }
    }

    fn observe_awaited_effect(
        &mut self,
        effect: &VmSideEffect,
    ) -> std::result::Result<(), VmDiagnostic> {
        // `output-open` is unusual among awaited host effects in the
        // synchronous Finch adapter.  The adapter immediately issues a
        // program-owned handle and projects a `Ui::Create` event with this
        // effect's sequence below in `request_effect`.  Forwarding the
        // preceding request as well would give one client projection two
        // different events at the same `(execution_id, sequence)`: it would
        // advance its cursor for the no-op request and then discard Create as
        // a duplicate.  A portable deferred host, in contrast, owns handle
        // issuance and must receive the original request.
        let synchronous_output_open = effect.origin.word.as_deref() == Some("output-open")
            && !self.deferred_host_effects.defers(effect);
        if synchronous_output_open {
            return Ok(());
        }
        if self.deferred_host_effects.defers(effect) && self.authorization_attempt.is_some() {
            let Some((request, decision)) = self
                .authorization_attempt
                .as_ref()
                .map(|attempt| (attempt.request.clone(), attempt.decision.clone()))
            else {
                unreachable!("checked above")
            };
            let authority_use_gate = Arc::clone(&self.authorization.use_gate);
            let _authority_use = authority_use_gate
                .read()
                .map_err(|_| host_binding_error(&effect.origin, "authority-use gate poisoned"))?;
            let policy = self
                .authorization
                .policy
                .read()
                .map_err(|_| host_binding_error(&effect.origin, "capability policy lock poisoned"))?
                .clone();
            let mut context = self.authorization.context.clone();
            context.now_unix_ms = unix_time_ms();
            context.policy_hash = policy.policy_hash.clone();
            let ledger_handle = Arc::clone(&self.authorization.ledger);
            let mut ledger = ledger_handle.lock().map_err(|_| {
                host_binding_error(&effect.origin, "capability ledger lock poisoned")
            })?;
            if ledger.recorded_authorization(&request, &context) != Some(decision) {
                let authorized = ledger.clone();
                ledger.rollback_authorization(&request);
                if let Some(sink) = &self.authorization.sink {
                    let Some(project_id) = context.project_id.clone() else {
                        *ledger = authorized;
                        self.authorization_attempt = None;
                        return Err(host_binding_error(
                            &effect.origin,
                            "deferred host authorization has no project identity during invalidation",
                        ));
                    };
                    let roots = self.resource_roots.read().map_err(|_| {
                        host_binding_error(&effect.origin, "resource-root binding lock poisoned")
                    })?;
                    if let Err(error) = sink(authority_state_from_parts(
                        context.session_id,
                        project_id,
                        policy,
                        ledger.clone(),
                        &roots,
                    )) {
                        *ledger = authorized;
                        self.authorization_attempt = None;
                        return Err(host_binding_error(
                            &effect.origin,
                            format!("persist invalidated deferred host authorization: {error:#}"),
                        ));
                    }
                }
                self.authorization_attempt = None;
                return Err(host_binding_error(
                    &effect.origin,
                    "authorization was revoked, expired, or replaced before deferred dispatch",
                ));
            }
            drop(ledger);
            self.mark_host_use();
            if let Some(sink) = &self.typed_effect_sink {
                sink(VmEffectEnvelope {
                    execution_id: self.execution_id,
                    effect: effect.clone(),
                });
            }
            self.authorization_attempt = None;
            return Ok(());
        }
        if let Some(sink) = &self.typed_effect_sink {
            sink(VmEffectEnvelope {
                execution_id: self.execution_id,
                effect: effect.clone(),
            });
        }
        Ok(())
    }

    fn defer_awaited_effect(&self, effect: &VmSideEffect) -> bool {
        // An event-loop/IDE binding can own proposal editing without blocking
        // the runner in `$EDITOR`. The portable effect already carries its
        // exact output row and sequence; the host resumes it later with the
        // accepted/chat/cancel value. Plain `submit` retains the existing
        // synchronous compatibility adapter while that UI is migrated.
        self.deferred_host_effects.defers(effect)
    }

    fn request_effect(
        &mut self,
        effect: &VmSideEffect,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let crate::vm::HostSideEffect::Request { arguments } = &effect.event else {
            return Err(VmDiagnostic::error(
                "E-HOST-002",
                crate::vm::DiagnosticPhase::HostCall,
                "VM await boundary did not carry a host request",
                Some(effect.origin.clone()),
            ));
        };

        // Named-Brain execution receives a daemon-owned audit proxy. Reserve
        // the immutable intent, then fsync AwaitingHostResult immediately
        // before the first host binding can perform a physical operation.
        let permit = if let Some(control) = self.effect_audit.clone() {
            let runtime = tokio::runtime::Handle::current();
            let reservation = runtime
                .block_on(control.reserve(self.execution_id, effect.clone()))
                .map_err(|error| {
                    host_binding_error(
                        &effect.origin,
                        format!("durable effect audit reservation failed: {error}"),
                    )
                })?;
            Some(runtime.block_on(reservation.begin()).map_err(|error| {
                host_binding_error(
                    &effect.origin,
                    format!("durable effect audit begin failed: {error}"),
                )
            })?)
        } else {
            None
        };

        let values = self.request_with_authority_lease(
            &effect.requirement,
            arguments.clone(),
            &effect.origin,
        );

        if let Some(permit) = permit {
            let outcome = match &values {
                Ok(values) => crate::runtime::effect_audit::RunnerHostEffectOutcome::Acknowledged {
                    values: values.clone(),
                },
                Err(_) => crate::runtime::effect_audit::RunnerHostEffectOutcome::FailedPartial {
                    detail: "host binding failed after physical dispatch was authorized"
                        .to_string(),
                },
            };
            tokio::runtime::Handle::current()
                .block_on(permit.finish(outcome))
                .map_err(|error| {
                    host_binding_error(
                        &effect.origin,
                        format!("durable effect audit finish failed: {error}"),
                    )
                })?;
        }

        let values = values?;

        // `output-open` awaits a host-issued opaque handle. Project its
        // corresponding Create event immediately, but retain the original
        // request in the durable VM journal for audit/resume semantics.
        if effect.origin.word.as_deref() == Some("output-open") {
            let (Some(TypedValue::String(title)), Some(target)) = (
                match &effect.event {
                    crate::vm::HostSideEffect::Request { arguments } => arguments.first(),
                    _ => None,
                },
                values.first(),
            ) else {
                return Err(host_binding_error(
                    &effect.origin,
                    "output-open host response is invalid",
                ));
            };
            if let Some(sink) = &self.typed_effect_sink {
                let mut create = effect.clone();
                create.event = crate::vm::HostSideEffect::Ui {
                    operation: crate::vm::UiOperation::Create,
                    target: Some(target.clone()),
                    text: Some(title.clone()),
                    progress: None,
                };
                sink(VmEffectEnvelope {
                    execution_id: self.execution_id,
                    effect: create,
                });
            }
        }
        Ok(values)
    }

    fn request_with_authority_lease(
        &mut self,
        requirement: &CapabilityRequirement,
        arguments: Vec<TypedValue>,
        origin: &SourceOrigin,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let Some((request, decision)) = self
            .authorization_attempt
            .as_ref()
            .map(|attempt| (attempt.request.clone(), attempt.decision.clone()))
        else {
            return self.request(requirement, arguments, origin);
        };
        #[cfg(test)]
        run_authorization_before_lease_hook(self.execution_id, requirement.capability.clone());
        let authority_use_gate = Arc::clone(&self.authorization.use_gate);
        let _authority_use = authority_use_gate
            .read()
            .map_err(|_| host_binding_error(origin, "authority-use gate poisoned"))?;
        let policy = self
            .authorization
            .policy
            .read()
            .map_err(|_| host_binding_error(origin, "capability policy lock poisoned"))?
            .clone();
        let mut context = self.authorization.context.clone();
        context.now_unix_ms = unix_time_ms();
        context.policy_hash = policy.policy_hash.clone();
        #[cfg(test)]
        run_authorization_before_use_hook(self.execution_id, requirement.capability.clone());
        let ledger_handle = Arc::clone(&self.authorization.ledger);
        let mut ledger = ledger_handle
            .lock()
            .map_err(|_| host_binding_error(origin, "capability ledger lock poisoned"))?;
        if ledger.recorded_authorization(&request, &context) != Some(decision) {
            let authorized = ledger.clone();
            ledger.rollback_authorization(&request);
            if let Some(sink) = &self.authorization.sink {
                let Some(project_id) = context.project_id.clone() else {
                    *ledger = authorized;
                    self.authorization_attempt = None;
                    return Err(host_binding_error(
                        origin,
                        "host authorization has no project identity during invalidation",
                    ));
                };
                let roots = self.resource_roots.read().map_err(|_| {
                    host_binding_error(origin, "resource-root binding lock poisoned")
                })?;
                if let Err(error) = sink(authority_state_from_parts(
                    context.session_id,
                    project_id,
                    policy.clone(),
                    ledger.clone(),
                    &roots,
                )) {
                    *ledger = authorized;
                    self.authorization_attempt = None;
                    return Err(host_binding_error(
                        origin,
                        format!("persist invalidated host authorization: {error:#}"),
                    ));
                }
            }
            self.authorization_attempt = None;
            return Err(host_binding_error(
                origin,
                "authorization was revoked, expired, or replaced before host use",
            ));
        }
        drop(ledger);

        // The shared authority-use gate makes host dispatch and revocation or
        // policy/root mutation linearizable without retaining the non-reentrant
        // ledger mutex across host code. Agent spawning intentionally re-enters
        // the ledger to snapshot its grant ceiling.
        let mut result = self.request(requirement, arguments, origin);
        let attempt = self
            .authorization_attempt
            .take()
            .expect("authorization attempt exists while lease is held");
        if !attempt.use_started {
            if result.is_ok() {
                result = Err(host_binding_error(
                    origin,
                    "host binding completed without finalizing its authority use",
                ));
            }
            let mut ledger = ledger_handle
                .lock()
                .map_err(|_| host_binding_error(origin, "capability ledger lock poisoned"))?;
            let authorized = ledger.clone();
            ledger.rollback_authorization(&attempt.request);
            if let Some(sink) = &self.authorization.sink {
                let Some(project_id) = context.project_id else {
                    *ledger = authorized;
                    return Err(host_binding_error(
                        origin,
                        "host authorization has no project identity during rollback",
                    ));
                };
                let roots = self.resource_roots.read().map_err(|_| {
                    host_binding_error(origin, "resource-root binding lock poisoned")
                })?;
                if let Err(error) = sink(authority_state_from_parts(
                    context.session_id,
                    project_id,
                    policy,
                    ledger.clone(),
                    &roots,
                )) {
                    *ledger = authorized;
                    return Err(host_binding_error(
                        origin,
                        format!("persist unused host authorization rollback: {error:#}"),
                    ));
                }
            }
        }
        result
    }

    fn request(
        &mut self,
        requirement: &CapabilityRequirement,
        arguments: Vec<TypedValue>,
        origin: &crate::vm::SourceOrigin,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let binding = registered_host_binding(requirement, origin)?;
        // `prepare_awaited_effect` rejects forged portable continuations, but
        // embedders may call this trait method directly. Repeat the complete
        // binding check at the final operation boundary so no direct adapter
        // can substitute runtime arguments after authority was derived.
        validate_core_host_request(binding, requirement, &arguments, origin)?;
        let request = match requirement.capability {
            crate::vm::CapabilityKind::SessionEmit => {
                // `output-open` uses the same session-emission authority as
                // ordinary visible output, but its awaited host request
                // returns an opaque handle rather than emitting its title as
                // a response chunk. Recognize it before validating the
                // ordinary one-string `say` ABI.
                if origin.word.as_deref() == Some("output-open") {
                    let [TypedValue::String(_title)] = arguments.as_slice() else {
                        return Err(VmDiagnostic::error(
                            "E-HOST-001",
                            crate::vm::DiagnosticPhase::HostCall,
                            "output-open requires one title string",
                            Some(origin.clone()),
                        ));
                    };
                    let handle = uuid::Uuid::new_v4().to_string();
                    self.output_handles
                        .lock()
                        .map_err(|_| {
                            host_binding_error(origin, "output handle registry lock poisoned")
                        })?
                        .insert(
                            handle.clone(),
                            OutputHandleRecord {
                                owner: self.execution_id,
                                generation: 0,
                            },
                        );
                    return Ok(vec![TypedValue::Resource {
                        kind: "output-handle".into(),
                        handle,
                        generation: 0,
                    }]);
                }
                let [TypedValue::String(text)] = arguments.as_slice() else {
                    return Err(VmDiagnostic::error(
                        "E-HOST-001",
                        crate::vm::DiagnosticPhase::HostCall,
                        "session.emit requires one string",
                        Some(origin.clone()),
                    ));
                };
                self.output.push_str(text);
                self.output_chunks.push(text.clone());
                self.emit(text);
                return Ok(vec![TypedValue::Unit]);
            }
            crate::vm::CapabilityKind::VmRead => {
                if origin.word.as_deref() == Some("vm-vocabulary") {
                    return Ok(vec![TypedValue::String(self.vocabulary.clone())]);
                }
                if origin.word.as_deref() == Some("capability-list") {
                    let ledger = self.authorization.ledger.lock().map_err(|_| {
                        host_binding_error(origin, "capability ledger lock poisoned")
                    })?;
                    let values = ledger
                        .grants
                        .active_grants_for(&self.authorization.context)
                        .filter(|grant| {
                            self.network_grants
                                .grants(&EffectSet::from_requirement(grant.requirement.clone()))
                        })
                        .map(|grant| {
                            let requirement = serde_json::to_value(&grant.requirement)
                                .map_err(|error| host_binding_error(origin, error.to_string()))?;
                            Ok(TypedValue::Record(vec![
                                (
                                    "grant".into(),
                                    TypedValue::Resource {
                                        kind: "capability-grant".into(),
                                        handle: grant.id.to_string(),
                                        generation: 0,
                                    },
                                ),
                                ("requirement".into(), TypedValue::Json(requirement)),
                            ]))
                        })
                        .collect::<std::result::Result<Vec<_>, VmDiagnostic>>()?;
                    return Ok(vec![TypedValue::List {
                        element_type: capability_grant_entry_type(),
                        values,
                    }]);
                }
                return Err(host_binding_error(
                    origin,
                    "unknown VM inspection operation",
                ));
            }
            crate::vm::CapabilityKind::AutomationInspect => match binding {
                Some(CoreHostBinding::AutomationDisplays) => AutomationRequest::Displays,
                Some(CoreHostBinding::AutomationWindows) => AutomationRequest::Windows,
                Some(CoreHostBinding::AutomationAvailability) => AutomationRequest::Availability,
                _ => {
                    return Err(host_binding_error(
                        origin,
                        "automation inspection requires its exact registered host binding",
                    ))
                }
            },
            crate::vm::CapabilityKind::AutomationWrite => match binding {
                Some(CoreHostBinding::AutomationClick) => {
                    let [TypedValue::Float(x), TypedValue::Float(y), TypedValue::String(button), TypedValue::Int(count)] =
                        arguments.as_slice()
                    else {
                        return Err(host_binding_error(
                            origin,
                            "automation-click argument types are invalid",
                        ));
                    };
                    AutomationRequest::Click {
                        x: *x,
                        y: *y,
                        button: button.clone(),
                        count: u8::try_from(*count).map_err(|_| {
                            host_binding_error(origin, "click count is out of range")
                        })?,
                    }
                }
                Some(CoreHostBinding::AutomationType) => {
                    let [TypedValue::String(text), TypedValue::Int(delay_ms)] =
                        arguments.as_slice()
                    else {
                        return Err(host_binding_error(
                            origin,
                            "automation-type argument types are invalid",
                        ));
                    };
                    AutomationRequest::Type {
                        text: text.clone(),
                        delay_ms: u64::try_from(*delay_ms).map_err(|_| {
                            host_binding_error(origin, "delay must be non-negative")
                        })?,
                    }
                }
                _ => {
                    return Err(host_binding_error(
                        origin,
                        "automation mutation requires its exact registered host binding",
                    ))
                }
            },
            crate::vm::CapabilityKind::FileRead => {
                match origin.word.as_deref() {
                    Some("csv-next") | Some("file-lines-next") | Some("stream-next") => {
                        return self.stream_next(&arguments, origin);
                    }
                    Some("csv-close") | Some("file-lines-close") | Some("stream-close") => {
                        return self.stream_close(&arguments, origin);
                    }
                    _ => {}
                }
                let (relative, selector) = match arguments.first() {
                    Some(TypedValue::Path { relative, selector }) => (relative, selector),
                    _ => {
                        return Err(host_binding_error(
                            origin,
                            "file read operations require a refined path as their first argument",
                        ));
                    }
                };
                let mode = if matches!(
                    binding,
                    Some(CoreHostBinding::TreeList | CoreHostBinding::TreeMerkle)
                ) {
                    SecureOpenMode::ReadDirectory
                } else {
                    SecureOpenMode::ReadFile
                };
                let mut file = self
                    .open_secure_resource(selector, relative, mode)
                    .map_err(|message| host_binding_error(origin, message))?;
                self.mark_host_use();
                match origin.word.as_deref() {
                    Some("workbook-open") | Some("workbook-sheet-open") => {
                        let sheet = match arguments.as_slice() {
                            [_] if origin.word.as_deref() == Some("workbook-open") => None,
                            [_, TypedValue::String(sheet)]
                                if origin.word.as_deref() == Some("workbook-sheet-open") =>
                            {
                                Some(sheet.as_str())
                            }
                            _ => {
                                return Err(host_binding_error(
                                    origin,
                                    "workbook-open requires a path; workbook-sheet-open requires a path and sheet name",
                                ))
                            }
                        };
                        let rows = read_workbook_rows(&file, relative, sheet)
                            .map_err(|message| host_binding_error(origin, message))?;
                        let handle = uuid::Uuid::new_v4().to_string();
                        self.streams
                            .lock()
                            .map_err(|_| {
                                host_binding_error(origin, "stream registry lock poisoned")
                            })?
                            .insert(
                                handle.clone(),
                                HostStream {
                                    owner: self.execution_id,
                                    generation: 0,
                                    requirement: requirement.clone(),
                                    backend: HostStreamBackend::WorkbookRows(rows.into_iter()),
                                },
                            );
                        return Ok(vec![TypedValue::Stream {
                            id: handle,
                            element_type: Type::list(Type::String),
                            kind: "workbook-rows".into(),
                            generation: 0,
                        }]);
                    }
                    Some("workbook-sheets") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(
                                origin,
                                "workbook-sheets requires one path",
                            ));
                        }
                        let sheets = read_workbook_sheet_names(&file, relative)
                            .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::List {
                            element_type: Type::String,
                            values: sheets.into_iter().map(TypedValue::String).collect(),
                        }]);
                    }
                    Some("workbook-range") => {
                        let [_, TypedValue::String(sheet), TypedValue::Int(start_row), TypedValue::Int(start_column), TypedValue::Int(row_count), TypedValue::Int(column_count)] =
                            arguments.as_slice()
                        else {
                            return Err(host_binding_error(
                                origin,
                                "workbook-range requires path, sheet, start row, start column, row count, and column count",
                            ));
                        };
                        let values = read_workbook_range(
                            &file,
                            relative,
                            sheet,
                            *start_row,
                            *start_column,
                            *row_count,
                            *column_count,
                        )
                        .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::List {
                            element_type: Type::list(Type::String),
                            values: values
                                .into_iter()
                                .map(|row| TypedValue::List {
                                    element_type: Type::String,
                                    values: row.into_iter().map(TypedValue::String).collect(),
                                })
                                .collect(),
                        }]);
                    }
                    Some("workbook-summary") => {
                        let [_, TypedValue::String(sheet), TypedValue::Int(max_rows)] =
                            arguments.as_slice()
                        else {
                            return Err(host_binding_error(
                                origin,
                                "workbook-summary requires a path, sheet name, and maximum data-row count",
                            ));
                        };
                        let max_rows = usize::try_from(*max_rows).map_err(|_| {
                            host_binding_error(
                                origin,
                                "workbook-summary maximum rows must be between 1 and 100000",
                            )
                        })?;
                        if !(1..=100_000).contains(&max_rows) {
                            return Err(host_binding_error(
                                origin,
                                "workbook-summary maximum rows must be between 1 and 100000",
                            ));
                        }
                        let summary = summarize_workbook(&file, relative, sheet, max_rows)
                            .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::Json(summary)]);
                    }
                    Some("csv-open") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(origin, "csv-open requires one path"));
                        }
                        let handle = uuid::Uuid::new_v4().to_string();
                        self.streams
                            .lock()
                            .map_err(|_| {
                                host_binding_error(origin, "stream registry lock poisoned")
                            })?
                            .insert(
                                handle.clone(),
                                HostStream {
                                    owner: self.execution_id,
                                    generation: 0,
                                    requirement: requirement.clone(),
                                    backend: HostStreamBackend::CsvRecords(BufReader::new(file)),
                                },
                            );
                        return Ok(vec![TypedValue::Stream {
                            id: handle,
                            element_type: Type::list(Type::String),
                            kind: "csv-records".into(),
                            generation: 0,
                        }]);
                    }
                    Some("csv-summary") => {
                        let [_, TypedValue::Int(max_rows)] = arguments.as_slice() else {
                            return Err(host_binding_error(
                                origin,
                                "csv-summary requires a path and maximum data-row count",
                            ));
                        };
                        let max_rows = usize::try_from(*max_rows).map_err(|_| {
                            host_binding_error(
                                origin,
                                "csv-summary maximum rows must be between 1 and 100000",
                            )
                        })?;
                        if !(1..=100_000).contains(&max_rows) {
                            return Err(host_binding_error(
                                origin,
                                "csv-summary maximum rows must be between 1 and 100000",
                            ));
                        }
                        let summary = summarize_csv(BufReader::new(file), max_rows)
                            .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::Json(summary)]);
                    }
                    Some("file-lines-open") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(
                                origin,
                                "file-lines-open requires one path",
                            ));
                        }
                        let handle = uuid::Uuid::new_v4().to_string();
                        self.streams
                            .lock()
                            .map_err(|_| {
                                host_binding_error(origin, "stream registry lock poisoned")
                            })?
                            .insert(
                                handle.clone(),
                                HostStream {
                                    owner: self.execution_id,
                                    generation: 0,
                                    requirement: requirement.clone(),
                                    backend: HostStreamBackend::FileLines(BufReader::new(file)),
                                },
                            );
                        return Ok(vec![TypedValue::Stream {
                            id: handle,
                            element_type: Type::String,
                            kind: "file-lines".into(),
                            generation: 0,
                        }]);
                    }
                    Some("file-size") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(origin, "file-size requires one path"));
                        }
                        let size = file
                            .metadata()
                            .map_err(|error| host_binding_error(origin, error.to_string()))?
                            .len();
                        let size = i64::try_from(size).map_err(|_| {
                            host_binding_error(origin, "file is too large to represent")
                        })?;
                        return Ok(vec![TypedValue::Int(size)]);
                    }
                    Some("file-hash") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(origin, "file-hash requires one path"));
                        }
                        let digest = sha256_file_handle(&file)
                            .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::String(hex_digest(&digest))]);
                    }
                    Some("tree-list") => {
                        let [_, TypedValue::Int(max_entries)] = arguments.as_slice() else {
                            return Err(host_binding_error(
                                origin,
                                "tree-list requires a directory path and maximum entry count",
                            ));
                        };
                        let max_entries = usize::try_from(*max_entries).map_err(|_| {
                            host_binding_error(
                                origin,
                                "tree-list maximum entries must be between 1 and 100000",
                            )
                        })?;
                        if !(1..=100_000).contains(&max_entries) {
                            return Err(host_binding_error(
                                origin,
                                "tree-list maximum entries must be between 1 and 100000",
                            ));
                        }
                        let (entries, truncated) = list_directory_tree(file, max_entries)
                            .map_err(|message| host_binding_error(origin, message))?;
                        let value = TypedValue::Record(vec![
                            (
                                "entries".into(),
                                TypedValue::List {
                                    element_type: tree_entry_type(),
                                    values: entries,
                                },
                            ),
                            ("truncated".into(), TypedValue::Bool(truncated)),
                        ]);
                        debug_assert_eq!(value.value_type(), tree_listing_type());
                        return Ok(vec![value]);
                    }
                    Some("tree-merkle") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(
                                origin,
                                "tree-merkle requires one path",
                            ));
                        }
                        let digest = merkle_directory(file)
                            .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::String(digest)]);
                    }
                    Some("file-slice") => {
                        let [_, TypedValue::Int(offset), TypedValue::Int(length)] =
                            arguments.as_slice()
                        else {
                            return Err(host_binding_error(
                                origin,
                                "file-slice requires a path, non-negative byte offset, and length",
                            ));
                        };
                        let offset = u64::try_from(*offset).map_err(|_| {
                            host_binding_error(origin, "file-slice offset must be non-negative")
                        })?;
                        let length = usize::try_from(*length).map_err(|_| {
                            host_binding_error(origin, "file-slice length must be non-negative")
                        })?;
                        const MAX_FILE_SLICE_BYTES: usize = 8 * 1024 * 1024;
                        if length > MAX_FILE_SLICE_BYTES {
                            return Err(host_binding_error(
                                origin,
                                format!(
                                    "file-slice length exceeds the {MAX_FILE_SLICE_BYTES}-byte per-call limit"
                                ),
                            ));
                        }
                        file.seek(SeekFrom::Start(offset))
                            .map_err(|error| host_binding_error(origin, error.to_string()))?;
                        let mut bytes = vec![0; length];
                        let read = file
                            .read(&mut bytes)
                            .map_err(|error| host_binding_error(origin, error.to_string()))?;
                        bytes.truncate(read);
                        return Ok(vec![TypedValue::Bytes(bytes)]);
                    }
                    _ => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(origin, "file-read requires one path"));
                        }
                        let mut bytes = Vec::new();
                        file.read_to_end(&mut bytes)
                            .map_err(|error| host_binding_error(origin, error.to_string()))?;
                        return Ok(vec![TypedValue::Bytes(bytes)]);
                    }
                }
            }
            crate::vm::CapabilityKind::FileWrite => {
                let [TypedValue::Path { relative, selector }, TypedValue::Bytes(bytes)] =
                    arguments.as_slice()
                else {
                    return Err(host_binding_error(
                        origin,
                        "file-write requires a path and bytes",
                    ));
                };
                self.mark_host_use();
                let mut file = self
                    .open_secure_resource(selector, relative, SecureOpenMode::WriteFile)
                    .map_err(|message| host_binding_error(origin, message))?;
                file.write_all(bytes)
                    .and_then(|_| file.sync_all())
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Unit]);
            }
            crate::vm::CapabilityKind::AgentSpawn => {
                let [argument] = arguments.as_slice() else {
                    return Err(host_binding_error(
                        origin,
                        "agent spawn requires exactly one task or task specification",
                    ));
                };
                let spec = if origin.word.as_deref() == Some("agent-spawn-with") {
                    typed_agent_task_spec(argument, origin)?
                } else {
                    let TypedValue::String(task) = argument else {
                        return Err(host_binding_error(origin, "agent-spawn requires one task"));
                    };
                    agents::AgentTaskSpec {
                        task: task.clone(),
                        role: Default::default(),
                        background: None,
                        provider: None,
                        model: None,
                        context: Vec::new(),
                        capability_grant_ids: None,
                        budget: Default::default(),
                    }
                };
                let Some(binding) = self.scheduler.clone() else {
                    return Err(host_binding_error(origin, "agent scheduler is unavailable"));
                };
                let spawn_binding = binding.clone();
                self.mark_host_use();
                let identity = binding
                    .block_on(async move { spawn_binding.spawn_spec(spec).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Task {
                    id: identity.task_id.to_string(),
                    result_type: agent_task_result_type(),
                    kind: crate::vm::TaskKind::Agent,
                }]);
            }
            crate::vm::CapabilityKind::AgentAwait => {
                let [TypedValue::Task { id: task_id, .. }] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "agent-await requires one task"));
                };
                let Some(binding) = self.scheduler.clone() else {
                    return Err(host_binding_error(origin, "agent scheduler is unavailable"));
                };
                let task_id = agent_vm::parse_task_id(task_id)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let wait_binding = binding.clone();
                self.mark_host_use();
                let result = binding
                    .block_on(async move { wait_binding.wait(task_id).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![typed_agent_task_result(result, origin)?]);
            }
            crate::vm::CapabilityKind::AgentPoll => {
                let [TypedValue::Task { id: task_id, .. }] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "agent-poll requires one task"));
                };
                let Some(binding) = self.scheduler.clone() else {
                    return Err(host_binding_error(origin, "agent scheduler is unavailable"));
                };
                let task_id = agent_vm::parse_task_id(task_id)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let poll_binding = binding.clone();
                self.mark_host_use();
                let snapshot = binding
                    .block_on(async move { poll_binding.poll(task_id).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![typed_agent_task_snapshot(snapshot, origin)?]);
            }
            crate::vm::CapabilityKind::AgentCancel => {
                let [TypedValue::Task { id: task_id, .. }] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "agent-cancel requires one task"));
                };
                let Some(binding) = self.scheduler.clone() else {
                    return Err(host_binding_error(origin, "agent scheduler is unavailable"));
                };
                let task_id = agent_vm::parse_task_id(task_id)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let cancel_binding = binding.clone();
                self.mark_host_use();
                binding
                    .block_on(async move { cancel_binding.cancel(task_id).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Unit]);
            }
            crate::vm::CapabilityKind::MemoryRead => {
                // `mem-index-status` shares `mem-recall`'s authority: both read
                // the session index, and neither should be reachable without
                // memory access. So this arm is entered by capability and split
                // by registered host binding. Branching on the binding rather
                // than `origin.word` keeps a renamed source word from selecting
                // a different host effect.
                if binding == Some(CoreHostBinding::MemoryIndexStatus) {
                    let Some(memory) = self.memory.clone() else {
                        return Err(host_binding_error(origin, "memory service is unavailable"));
                    };
                    self.mark_host_use();
                    return Ok(vec![typed_memory_index_status(
                        memory.hydration_status(),
                        origin,
                    )?]);
                }
                let [TypedValue::String(query)] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "mem-recall requires one query"));
                };
                let Some(memory) = self.memory.clone() else {
                    return Err(host_binding_error(origin, "memory service is unavailable"));
                };
                let query = query.clone();
                self.mark_host_use();
                // A typed list has nowhere to put a caveat, so this surface
                // cannot narrate a partial index the way the tools and the
                // status strip do -- and an empty `List<String>` reads to the
                // calling program as "no such memory" (#275).
                //
                // So the unusable case fails instead of lying: with a `Failed`
                // index the answer carries no information either way, and an
                // error is something a program can branch on. `Loading` and
                // `Degraded` still return what has loaded, because refusing
                // during ordinary startup hydration would break working
                // programs to report a condition that resolves itself; that
                // residual -- a typed program cannot tell a partial empty
                // result from a true one -- needs a status word to fix, not a
                // refusal here.
                let before = memory.hydration_status();
                let for_status = memory.clone();
                let values = block_on_host(async move { memory.query(&query, None).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let observed =
                    finch_memory::memory_status::observed(before, for_status.hydration_status());
                if let finch_memory::HydrationStatus::Failed { reason } = &observed {
                    return Err(host_binding_error(
                        origin,
                        format!(
                            "memory index is unavailable, so mem-recall cannot answer: {reason}"
                        ),
                    ));
                }
                if let Some(caveat) = finch_memory::memory_status::caveat(&observed) {
                    tracing::warn!(%caveat, "mem-recall answered from a partial memory index");
                }
                return Ok(vec![TypedValue::List {
                    element_type: Type::String,
                    values: values.into_iter().map(TypedValue::String).collect(),
                }]);
            }
            crate::vm::CapabilityKind::MemoryWrite => {
                let [TypedValue::String(content)] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "mem-store requires one string"));
                };
                let Some(memory) = self.memory.clone() else {
                    return Err(host_binding_error(origin, "memory service is unavailable"));
                };
                let content = content.clone();
                self.mark_host_use();
                block_on_host(async move {
                    memory
                        .insert_conversation("assistant", &content, None, None)
                        .await
                })
                .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Resource {
                    kind: "memory-node".into(),
                    handle: uuid::Uuid::new_v4().to_string(),
                    generation: 0,
                }]);
            }
            crate::vm::CapabilityKind::McpCall => {
                let ResourceSelector::Mcp {
                    server: authorized_server,
                    tool: authorized_tool,
                } = &requirement.selector
                else {
                    return Err(host_binding_error(
                        origin,
                        "mcp-call reached the host without a concrete server/tool selector",
                    ));
                };
                let parameters = match arguments.as_slice() {
                    [TypedValue::String(server), TypedValue::String(tool), TypedValue::Json(parameters)] =>
                    {
                        if server != authorized_server || tool != authorized_tool {
                            return Err(host_binding_error(
                                origin,
                                "mcp-call arguments do not match the authorized server/tool selector",
                            ));
                        }
                        parameters.clone()
                    }
                    [parameters]
                        if origin
                            .word
                            .as_deref()
                            .is_some_and(|word| word.starts_with("mcp.")) =>
                    {
                        typed_mcp_arguments(parameters).map_err(|error| {
                            host_binding_error(origin, format!("encode MCP arguments: {error}"))
                        })?
                    }
                    _ => {
                        return Err(host_binding_error(
                            origin,
                            "MCP calls require either server/tool/JSON or one namespaced binding argument",
                        ));
                    }
                };
                let Some(client) = self.mcp_client.clone() else {
                    return Err(host_binding_error(origin, "MCP client is unavailable"));
                };
                let wire_name = format!("mcp_{authorized_server}_{authorized_tool}");
                self.mark_host_use();
                let response = block_on_host(async move {
                    client.execute_tool_value(&wire_name, parameters).await
                })
                .map_err(|error| host_binding_error(origin, error.to_string()))?;
                if let Some(schema) = origin
                    .word
                    .as_deref()
                    .and_then(|word| self.mcp_output_schemas.get(word))
                {
                    let structured = response.get("structuredContent").ok_or_else(|| {
                        host_binding_error(
                            origin,
                            "MCP result omitted structuredContent required by its output schema",
                        )
                    })?;
                    mcp::validate_output(schema, structured).map_err(|error| {
                        host_binding_error(origin, format!("validate MCP result: {error:#}"))
                    })?;
                }
                return Ok(vec![TypedValue::Json(response)]);
            }
            crate::vm::CapabilityKind::ProcessRun => {
                let [TypedValue::String(command), TypedValue::List { values, .. }] =
                    arguments.as_slice()
                else {
                    return Err(host_binding_error(
                        origin,
                        "process-run requires a command and string arguments",
                    ));
                };
                let arguments = values
                    .iter()
                    .map(|value| match value {
                        TypedValue::String(value) => Ok(value.clone()),
                        _ => Err(host_binding_error(
                            origin,
                            "process-run arguments must be strings",
                        )),
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let executable =
                    validate_process_request(binding, requirement, command, &arguments, origin)?;
                let child = match spawn_open_process(executable) {
                    Ok(child) => child,
                    Err(error) => {
                        return Err(host_binding_error(origin, error));
                    }
                };
                // `Command::spawn` waits for the child-side exec error pipe.
                // Once it returns a child, the verified descriptor has been
                // executed and the authorization is a real use even if the
                // program later exits unsuccessfully.
                self.mark_host_use();
                let output = child
                    .wait_with_output()
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                if !output.status.success() {
                    return Err(host_binding_error(
                        origin,
                        format!("process exited with status {}", output.status),
                    ));
                }
                return Ok(vec![TypedValue::String(
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                )]);
            }
            crate::vm::CapabilityKind::ProgramInvoke => {
                let [TypedValue::String(language), TypedValue::String(intent), TypedValue::String(source)] =
                    arguments.as_slice()
                else {
                    return Err(host_binding_error(
                        origin,
                        "proposal-open requires language, intent, and source strings",
                    ));
                };
                let language = language.clone();
                let intent = intent.clone();
                let source = source.clone();
                let Some(host) = self.artifact_proposal_host.clone() else {
                    return Err(host_binding_error(
                        origin,
                        "proposal-open has no application proposal host binding",
                    ));
                };
                self.mark_host_use();
                let decision = block_on_host(async move {
                    host.propose_artifact(&language, &intent, &source).await
                })
                .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let value = match decision {
                    ArtifactProposalDecision::Execute { source } => TypedValue::Option {
                        inner_type: Type::Result(Box::new(Type::String), Box::new(Type::String)),
                        value: Some(Box::new(TypedValue::Result {
                            ok_type: Type::String,
                            error_type: Type::String,
                            is_ok: true,
                            value: Box::new(TypedValue::String(source)),
                        })),
                    },
                    ArtifactProposalDecision::Chat { context } => TypedValue::Option {
                        inner_type: Type::Result(Box::new(Type::String), Box::new(Type::String)),
                        value: Some(Box::new(TypedValue::Result {
                            ok_type: Type::String,
                            error_type: Type::String,
                            is_ok: false,
                            value: Box::new(TypedValue::String(context)),
                        })),
                    },
                    ArtifactProposalDecision::Cancel => TypedValue::Option {
                        inner_type: Type::Result(Box::new(Type::String), Box::new(Type::String)),
                        value: None,
                    },
                };
                return Ok(vec![value]);
            }
            crate::vm::CapabilityKind::NetworkConnect => {
                if origin.word.as_deref() == Some("network-connect") {
                    let [TypedValue::String(host), TypedValue::Int(port)] = arguments.as_slice()
                    else {
                        return Err(host_binding_error(
                            origin,
                            "network-connect requires host and port",
                        ));
                    };
                    let port = u16::try_from(*port)
                        .map_err(|_| host_binding_error(origin, "network port is out of range"))?;
                    let address = (host.as_str(), port)
                        .to_socket_addrs()
                        .map_err(|error| host_binding_error(origin, error.to_string()))?
                        .next()
                        .ok_or_else(|| host_binding_error(origin, "host has no addresses"))?;
                    self.mark_host_use();
                    let stream =
                        TcpStream::connect_timeout(&address, std::time::Duration::from_secs(5))
                            .map_err(|error| host_binding_error(origin, error.to_string()))?;
                    let handle = uuid::Uuid::new_v4().to_string();
                    self.network
                        .lock()
                        .map_err(|_| host_binding_error(origin, "network lock poisoned"))?
                        .insert(
                            handle.clone(),
                            NetworkSocket {
                                stream,
                                host: host.clone(),
                                port,
                                owner: self.execution_id,
                                generation: self.resource_generation,
                            },
                        );
                    return Ok(vec![TypedValue::Resource {
                        kind: "network-socket".into(),
                        handle,
                        generation: self.resource_generation,
                    }]);
                }
                let [TypedValue::Resource {
                    kind,
                    handle,
                    generation,
                }, TypedValue::Bytes(payload)] = arguments.as_slice()
                else {
                    return Err(host_binding_error(
                        origin,
                        "network-send requires a socket and bytes",
                    ));
                };
                if kind != "network-socket" {
                    return Err(host_binding_error(
                        origin,
                        "resource is not a network socket",
                    ));
                }
                let network = Arc::clone(&self.network);
                let mut sockets = network
                    .lock()
                    .map_err(|_| host_binding_error(origin, "network lock poisoned"))?;
                let socket = sockets
                    .get_mut(handle)
                    .ok_or_else(|| host_binding_error(origin, "unknown network socket"))?;
                if socket.owner != self.execution_id || socket.generation != *generation {
                    return Err(host_binding_error(
                        origin,
                        "network socket is stale or belongs to another ProgramRun",
                    ));
                }
                let endpoint = CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::NetworkConnect,
                    selector: crate::vm::ResourceSelector::Network {
                        host: socket.host.clone(),
                        ports: vec![socket.port],
                    },
                };
                if !self
                    .network_grants
                    .grants(&EffectSet::from_requirement(endpoint))
                {
                    return Err(host_binding_error(
                        origin,
                        "network socket endpoint is no longer covered by an active grant",
                    ));
                }
                self.mark_host_use();
                socket
                    .stream
                    .write_all(payload)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let mut response = vec![0; 4096];
                let size = socket
                    .stream
                    .read(&mut response)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                response.truncate(size);
                return Ok(vec![TypedValue::Bytes(response)]);
            }
            _ => {
                return Err(host_binding_error(
                    origin,
                    "authorized capability has no typed host binding",
                ));
            }
        };
        self.mark_host_use();
        let value = self
            .automation
            .execute(request)
            .map_err(|error| host_binding_error(origin, error.to_string()))?;
        Ok(vec![TypedValue::String(value.to_string())])
    }

    fn output(&self) -> String {
        self.output.clone()
    }

    fn output_chunks(&self) -> Vec<String> {
        self.output_chunks.clone()
    }

    fn side_effects(&self) -> Vec<crate::vm::HostSideEffect> {
        self.side_effects.clone()
    }

    fn side_effect(
        &mut self,
        effect: &crate::vm::VmSideEffect,
    ) -> std::result::Result<(), VmDiagnostic> {
        match &effect.event {
            crate::vm::HostSideEffect::Emit { text } => {
                self.output.push_str(text);
                self.output_chunks.push(text.clone());
            }
            crate::vm::HostSideEffect::Ui {
                target, operation, ..
            } => {
                let Some(TypedValue::Resource {
                    kind,
                    handle,
                    generation,
                }) = target
                else {
                    return Err(VmDiagnostic::error(
                        "E-OUTPUT-HANDLE-001",
                        crate::vm::DiagnosticPhase::HostCall,
                        "UI updates require an output-handle resource",
                        Some(effect.origin.clone()),
                    ));
                };
                let record = self
                    .output_handles
                    .lock()
                    .map_err(|_| {
                        VmDiagnostic::error(
                            "E-OUTPUT-HANDLE-002",
                            crate::vm::DiagnosticPhase::HostCall,
                            "output handle registry is unavailable",
                            Some(effect.origin.clone()),
                        )
                    })?
                    .get(handle)
                    .copied();
                let valid = kind == "output-handle"
                    && record.is_some_and(|record| {
                        record.owner == self.execution_id && record.generation == *generation
                    });
                if !valid {
                    return Err(VmDiagnostic::error(
                        "E-OUTPUT-HANDLE-003",
                        crate::vm::DiagnosticPhase::HostCall,
                        "output handle is unknown, stale, or belongs to another program run",
                        Some(effect.origin.clone()),
                    ));
                }
                if matches!(
                    operation,
                    crate::vm::UiOperation::Complete | crate::vm::UiOperation::Fail
                ) {
                    self.output_handles
                        .lock()
                        .map_err(|_| {
                            VmDiagnostic::error(
                                "E-OUTPUT-HANDLE-002",
                                crate::vm::DiagnosticPhase::HostCall,
                                "output handle registry is unavailable",
                                Some(effect.origin.clone()),
                            )
                        })?
                        .remove(handle);
                }
            }
            crate::vm::HostSideEffect::Request { .. } => {
                return Err(VmDiagnostic::error(
                    "E-HOST-003",
                    crate::vm::DiagnosticPhase::HostCall,
                    "host requests must be handled at a capability boundary, not as emitted UI events",
                    Some(effect.origin.clone()),
                ));
            }
        }
        if let Some(sink) = &self.typed_effect_sink {
            sink(VmEffectEnvelope {
                execution_id: self.execution_id,
                effect: effect.clone(),
            });
        }
        self.side_effects.push(effect.event.clone());
        Ok(())
    }
}

impl TypedHostHandler {
    fn open_secure_resource(
        &self,
        selector: &crate::vm::FileSelector,
        relative: &str,
        mode: SecureOpenMode,
    ) -> std::result::Result<std::fs::File, String> {
        let binding = self
            .resource_roots
            .read()
            .map_err(|_| "resource-root binding lock poisoned".to_string())?
            .bindings
            .get(&selector.root)
            .cloned()
            .ok_or_else(|| format!("{} root is not installed by this host", selector.root))?;
        open_resource_beneath_mode(&binding, selector, relative, mode)
    }
}

/// Resolve a portable host request through the same core-word registry used
/// by the parser, verifier, and provider discovery.  A verified module should
/// already make this true; the host boundary repeats the check so a corrupted
/// cached module or foreign embedder cannot route (for example) a `file-read`
/// request through an unrelated word name with the same coarse capability.
pub(super) fn registered_host_binding(
    requirement: &CapabilityRequirement,
    origin: &crate::vm::SourceOrigin,
) -> std::result::Result<Option<CoreHostBinding>, VmDiagnostic> {
    let Some(name) = origin.word.as_deref() else {
        // Embedders may produce a host request with a generated origin. Its
        // capability/result rows are still checked by the VM resume boundary.
        return Ok(None);
    };
    let Some(spec) = core_word_spec(name) else {
        if let (&CapabilityKind::McpCall, ResourceSelector::Mcp { server, tool }) =
            (&requirement.capability, &requirement.selector)
        {
            if name == format!("mcp.{server}.{tool}") {
                return Ok(Some(CoreHostBinding::McpCall));
            }
            return Err(host_binding_error(
                origin,
                "namespaced MCP word does not match its concrete server/tool selector",
            ));
        }
        // User-defined calls inherit a source origin at a higher level; they
        // must not be mistaken for missing core host bindings.
        return Ok(None);
    };

    let binding = match spec.implementation {
        CoreWordImplementation::HostEffect(binding) => binding,
        // `output-open` is an explicit VM instruction which deliberately
        // awaits a host-issued opaque resource. It is the only instruction
        // class currently reaching this request adapter.
        CoreWordImplementation::VmInstruction if name == "output-open" => return Ok(None),
        implementation => {
            return Err(host_binding_error(
                origin,
                format!(
                    "core word '{name}' is registered as {implementation:?}, not as a host request"
                ),
            ))
        }
    };

    let declares_capability = spec
        .signature
        .effects
        .0
        .iter()
        .any(|declared| declared.capability == requirement.capability);
    if !declares_capability {
        return Err(host_binding_error(
            origin,
            format!(
                "core word '{name}' is registered without capability {:?}",
                requirement.capability
            ),
        ));
    }
    Ok(Some(binding))
}

/// Re-derive the exact authority and ABI of a core host request from the
/// immutable vocabulary at the final host boundary. The VM normally creates
/// this tuple, but cached/foreign continuations are untrusted here: a coarse
/// capability kind, forged origin, or same-shaped argument list must never
/// select a different host operation.
pub(super) fn validate_core_host_request(
    binding: Option<CoreHostBinding>,
    requirement: &CapabilityRequirement,
    arguments: &[TypedValue],
    origin: &SourceOrigin,
) -> std::result::Result<(), VmDiagnostic> {
    let word = origin.word.as_deref();
    if binding.is_none() {
        if word == Some("output-open")
            && matches!(arguments, [TypedValue::String(_)])
            && requirement.capability == CapabilityKind::SessionEmit
            && requirement.selector == ResourceSelector::None
        {
            return Ok(());
        }
        return Err(host_binding_error(
            origin,
            "host request has no exact registered core binding",
        ));
    }
    let binding = binding.expect("checked above");

    if let Some(name) = word {
        if let Some(spec) = core_word_spec(name) {
            let declared = spec
                .signature
                .effects
                .0
                .iter()
                .find(|declared| declared.capability == requirement.capability)
                .ok_or_else(|| {
                    host_binding_error(origin, "core binding does not declare this capability")
                })?;
            let expected = crate::vm::instantiate_requirement(declared, arguments)
                .map_err(|message| host_binding_error(origin, message))?;
            let dynamically_bound = matches!(
                binding,
                CoreHostBinding::NetworkSend
                    | CoreHostBinding::FileLinesNext
                    | CoreHostBinding::FileLinesClose
                    | CoreHostBinding::CsvNext
                    | CoreHostBinding::CsvClose
                    | CoreHostBinding::StreamNext
                    | CoreHostBinding::StreamClose
                    | CoreHostBinding::ProcessRun
            );
            if !dynamically_bound && expected != *requirement {
                return Err(host_binding_error(
                    origin,
                    "runtime arguments do not derive the authorized capability requirement",
                ));
            }
        }
    }

    let string = |value: &TypedValue| matches!(value, TypedValue::String(_));
    let integer = |value: &TypedValue| matches!(value, TypedValue::Int(_));
    let float = |value: &TypedValue| matches!(value, TypedValue::Float(_));
    let path = |value: &TypedValue| matches!(value, TypedValue::Path { .. });
    let bytes = |value: &TypedValue| matches!(value, TypedValue::Bytes(_));
    let stream = |value: &TypedValue| matches!(value, TypedValue::Stream { .. });
    let resource = |value: &TypedValue, kind: &str| matches!(value, TypedValue::Resource { kind: actual, .. } if actual == kind);
    let task = |value: &TypedValue| matches!(value, TypedValue::Task { .. });
    let valid_arguments = match binding {
        CoreHostBinding::SessionEmit => matches!(arguments, [value] if string(value)),
        CoreHostBinding::VmVocabulary
        | CoreHostBinding::CapabilityList
        | CoreHostBinding::AutomationAvailability
        | CoreHostBinding::AutomationDisplays
        | CoreHostBinding::AutomationWindows => arguments.is_empty(),
        CoreHostBinding::FileRead
        | CoreHostBinding::FileHash
        | CoreHostBinding::TreeMerkle
        | CoreHostBinding::FileSize
        | CoreHostBinding::FileLinesOpen
        | CoreHostBinding::CsvOpen
        | CoreHostBinding::WorkbookOpen
        | CoreHostBinding::WorkbookSheets => matches!(arguments, [value] if path(value)),
        CoreHostBinding::TreeList => {
            matches!(arguments, [first, second] if path(first) && integer(second))
        }
        CoreHostBinding::FileSlice => matches!(arguments, [first, second, third]
            if path(first) && integer(second) && integer(third)),
        CoreHostBinding::WorkbookSheetOpen => {
            matches!(arguments, [first, second] if path(first) && string(second))
        }
        CoreHostBinding::WorkbookRange => matches!(arguments,
            [first, sheet, row, column, rows, columns]
                if path(first) && string(sheet) && integer(row) && integer(column)
                    && integer(rows) && integer(columns)),
        CoreHostBinding::WorkbookSummary => matches!(arguments,
            [first, sheet, rows] if path(first) && string(sheet) && integer(rows)),
        CoreHostBinding::CsvSummary => {
            matches!(arguments, [first, rows] if path(first) && integer(rows))
        }
        CoreHostBinding::FileLinesNext
        | CoreHostBinding::FileLinesClose
        | CoreHostBinding::CsvNext
        | CoreHostBinding::CsvClose
        | CoreHostBinding::StreamNext
        | CoreHostBinding::StreamClose => matches!(arguments, [value] if stream(value)),
        CoreHostBinding::FileWrite => {
            matches!(arguments, [first, second] if path(first) && bytes(second))
        }
        CoreHostBinding::ProcessRun => matches!(arguments,
            [command, TypedValue::List { values, .. }]
                if string(command) && values.iter().all(string)),
        CoreHostBinding::McpCall => {
            matches!(arguments,
            [server, tool, TypedValue::Json(_)] if string(server) && string(tool))
                || arguments.len() == 1 && word.is_some_and(|name| name.starts_with("mcp."))
        }
        CoreHostBinding::ProposalOpen => {
            matches!(arguments, [first, second, third]
                if string(first) && string(second) && string(third))
        }
        CoreHostBinding::NetworkConnect => {
            matches!(arguments, [host, port] if string(host) && integer(port))
        }
        CoreHostBinding::NetworkSend => {
            matches!(arguments, [socket, payload]
                if resource(socket, "network-socket") && bytes(payload))
        }
        CoreHostBinding::MemoryRecall | CoreHostBinding::MemoryStore => {
            matches!(arguments, [value] if string(value))
        }
        CoreHostBinding::MemoryIndexStatus => arguments.is_empty(),
        CoreHostBinding::ScheduleCreate => {
            matches!(arguments, [source, interval] if string(source) && integer(interval))
        }
        CoreHostBinding::ScheduleGet | CoreHostBinding::ScheduleCancel => {
            matches!(arguments, [value] if resource(value, "schedule"))
        }
        CoreHostBinding::AgentSpawn => matches!(arguments, [value] if string(value)),
        CoreHostBinding::AgentSpawnWith => matches!(arguments, [TypedValue::Record(_)]),
        CoreHostBinding::AgentAwait | CoreHostBinding::AgentPoll | CoreHostBinding::AgentCancel => {
            matches!(arguments, [value] if task(value))
        }
        CoreHostBinding::AutomationClick => matches!(arguments, [x, y, button, count]
            if float(x) && float(y) && string(button) && integer(count)),
        CoreHostBinding::AutomationType => {
            matches!(arguments, [text, delay] if string(text) && integer(delay))
        }
    };
    if !valid_arguments {
        return Err(host_binding_error(
            origin,
            format!("runtime arguments do not match the {binding:?} host ABI"),
        ));
    }

    let stream_kind_matches = match binding {
        CoreHostBinding::FileLinesNext | CoreHostBinding::FileLinesClose => matches!(
            arguments,
            [TypedValue::Stream {
                kind,
                element_type,
                ..
            }] if kind == "file-lines" && *element_type == Type::String
        ),
        CoreHostBinding::CsvNext | CoreHostBinding::CsvClose => matches!(
            arguments,
            [TypedValue::Stream {
                kind,
                element_type,
                ..
            }] if kind == "csv-records" && *element_type == Type::list(Type::String)
        ),
        _ => true,
    };
    if !stream_kind_matches {
        return Err(host_binding_error(
            origin,
            "stream resource kind does not match the registered host binding",
        ));
    }

    if matches!(
        binding,
        CoreHostBinding::FileRead
            | CoreHostBinding::FileHash
            | CoreHostBinding::TreeMerkle
            | CoreHostBinding::FileSize
            | CoreHostBinding::FileLinesOpen
            | CoreHostBinding::CsvOpen
            | CoreHostBinding::WorkbookOpen
            | CoreHostBinding::WorkbookSheets
            | CoreHostBinding::TreeList
            | CoreHostBinding::FileSlice
            | CoreHostBinding::WorkbookSheetOpen
            | CoreHostBinding::WorkbookRange
            | CoreHostBinding::WorkbookSummary
            | CoreHostBinding::CsvSummary
            | CoreHostBinding::FileWrite
    ) {
        let Some(TypedValue::Path {
            selector: runtime_selector,
            relative,
        }) = arguments.first()
        else {
            unreachable!("file binding ABI was validated above")
        };
        let ResourceSelector::File {
            selector: authorized_selector,
        } = &requirement.selector
        else {
            return Err(host_binding_error(
                origin,
                "file host binding requires a concrete file selector",
            ));
        };
        let exact_selector = crate::vm::FileSelectorTemplate {
            root: runtime_selector.root.clone(),
            parts: vec![crate::vm::FileSelectorTemplatePart::Argument {
                index: 0,
                bound: runtime_selector.clone(),
            }],
            upper_bound: runtime_selector.clone(),
        }
        .instantiate(arguments)
        .map_err(|error| host_binding_error(origin, error.to_string()))?;
        if &exact_selector != authorized_selector || !runtime_selector.matches(relative) {
            return Err(host_binding_error(
                origin,
                "runtime path does not match the authorized file selector",
            ));
        }
    }

    if binding == CoreHostBinding::ProcessRun {
        let TypedValue::String(command) = &arguments[0] else {
            unreachable!("validated above")
        };
        let ResourceSelector::Process { executables } = &requirement.selector else {
            return Err(host_binding_error(
                origin,
                "process selector is not concrete",
            ));
        };
        let [encoded] = executables.as_slice() else {
            return Err(host_binding_error(origin, "process selector is not exact"));
        };
        let identity = ProcessExecutableIdentity::decode(encoded)
            .map_err(|message| host_binding_error(origin, message))?;
        if identity.path != *command {
            return Err(host_binding_error(
                origin,
                "process command does not match the authorized executable identity",
            ));
        }
    }
    Ok(())
}

pub(super) fn host_binding_error(
    origin: &crate::vm::SourceOrigin,
    message: impl Into<String>,
) -> VmDiagnostic {
    VmDiagnostic::error(
        "E-HOST-002",
        crate::vm::DiagnosticPhase::HostCall,
        message,
        Some(origin.clone()),
    )
}

pub(super) fn validate_process_request(
    binding: Option<CoreHostBinding>,
    requirement: &CapabilityRequirement,
    command: &str,
    arguments: &[String],
    origin: &crate::vm::SourceOrigin,
) -> std::result::Result<OpenedProcessExecutable, VmDiagnostic> {
    if binding != Some(CoreHostBinding::ProcessRun) {
        return Err(host_binding_error(
            origin,
            "process execution requires the registered process-run host binding",
        ));
    }
    let ResourceSelector::Process { executables } = &requirement.selector else {
        return Err(host_binding_error(
            origin,
            "process-run reached the host without a concrete process selector",
        ));
    };
    let [encoded] = executables.as_slice() else {
        return Err(host_binding_error(
            origin,
            "process-run requires exactly one stable executable identity",
        ));
    };
    let approved = ProcessExecutableIdentity::decode(encoded)
        .map_err(|message| host_binding_error(origin, message))?;
    let current = open_process_executable(command, arguments)
        .map_err(|message| host_binding_error(origin, message))?;
    if approved != current.identity {
        return Err(host_binding_error(
            origin,
            "process-run executable identity no longer matches the authorized selector",
        ));
    }
    Ok(current)
}

pub(super) const PROCESS_IDENTITY_PREFIX: &str = "finch-process-v3:";

pub(super) fn validate_restored_process_authority(ledger: &CapabilityLedger) -> Result<()> {
    for grant in &ledger.grants.grants {
        if grant.requirement.capability != CapabilityKind::ProcessRun {
            continue;
        }
        let ResourceSelector::Process { executables } = &grant.requirement.selector else {
            bail!("stored process capability grant has an invalid selector");
        };
        for executable in executables {
            let approved = ProcessExecutableIdentity::decode(executable).map_err(|_| {
                anyhow::anyhow!(
                    "legacy process capability grant cannot be restored; reapprove the executable to bind a stable v3 invocation identity"
                )
            })?;
            let current = open_process_executable(&approved.path, &approved.arguments)
                .map_err(anyhow::Error::msg)?
                .identity;
            if current != approved {
                bail!("stored process capability identity changed since approval");
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ProcessExecutableIdentity {
    pub(super) path: String,
    sha256: String,
    device: u64,
    inode: u64,
    pub(super) arguments: Vec<String>,
    pub(super) environment_sha256: String,
    pub(super) cwd_path: String,
    cwd_device: u64,
    cwd_inode: u64,
}

pub(super) struct OpenedProcessExecutable {
    pub(super) identity: ProcessExecutableIdentity,
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    file: std::fs::File,
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    cwd: std::fs::File,
}

impl ProcessExecutableIdentity {
    pub(super) fn encode(&self) -> String {
        format!(
            "{PROCESS_IDENTITY_PREFIX}{}",
            serde_json::to_string(self).expect("process identity is serializable")
        )
    }

    pub(super) fn decode(encoded: &str) -> std::result::Result<Self, String> {
        let json = encoded
            .strip_prefix(PROCESS_IDENTITY_PREFIX)
            .ok_or_else(|| "process selector is not a stable executable identity".to_string())?;
        serde_json::from_str(json).map_err(|_| "process selector identity is malformed".into())
    }
}

/// Open an executable to the authority identity persisted in grants and
/// audit. Device/inode and content are derived from the same open descriptor
/// later passed to `fexecve`, so pathname replacement cannot change the
/// object selected for execution.
pub(super) fn resolve_process_executable(
    command: &str,
) -> std::result::Result<ProcessExecutableIdentity, String> {
    Ok(open_process_executable(command, &[])?.identity)
}

pub(super) fn open_process_executable(
    command: &str,
    arguments: &[String],
) -> std::result::Result<OpenedProcessExecutable, String> {
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )))]
    {
        let _ = (command, arguments);
        return Err(
            "process-run is unsupported on this platform because stable opened-object execution is unavailable"
                .into(),
        );
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    {
        use std::os::fd::FromRawFd;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

        let supplied = Path::new(command);
        if !supplied.is_absolute() {
            return Err("process-run executable must be an absolute canonical path; PATH and relative lookup are forbidden".into());
        }
        let canonical = std::fs::canonicalize(supplied).map_err(|error| {
            format!(
                "resolve process executable '{}': {error}",
                supplied.display()
            )
        })?;
        if canonical != supplied {
            return Err(
                "process-run executable must already be canonical and must not be a symlink".into(),
            );
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&canonical)
            .map_err(|error| {
                format!("open process executable '{}': {error}", canonical.display())
            })?;
        let metadata = file.metadata().map_err(|error| {
            format!(
                "inspect process executable '{}': {error}",
                canonical.display()
            )
        })?;
        if !metadata.is_file() {
            return Err("process-run executable must be a regular file".into());
        }
        let path_metadata = std::fs::metadata(&canonical).map_err(|error| {
            format!(
                "recheck process executable '{}': {error}",
                canonical.display()
            )
        })?;
        if metadata.dev() != path_metadata.dev() || metadata.ino() != path_metadata.ino() {
            return Err("process-run executable changed while it was being opened".into());
        }
        ensure_effective_execute_permission(&canonical, &metadata)?;
        let mut source = file.try_clone().map_err(|error| error.to_string())?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        let snapshot_directory = tempfile::tempdir()
            .map_err(|error| format!("create private executable snapshot directory: {error}"))?;
        let snapshot_path = snapshot_directory.path().join("executable");
        let mut snapshot_writer = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&snapshot_path)
            .map_err(|error| format!("create private executable snapshot: {error}"))?;
        #[cfg(test)]
        run_snapshot_write_hook(&canonical, &snapshot_path);
        std::io::copy(&mut source, &mut snapshot_writer)
            .map_err(|error| format!("snapshot process executable: {error}"))?;
        // Deliberately not `sync_all`. The snapshot is reopened by path just
        // below, but read-after-write through the page cache is coherent
        // without a sync and `File` is unbuffered in userspace, so both the
        // hash and the exec see every byte written here. Nothing reads this
        // file after the process exits -- it is unlinked before the exec --
        // so durability bought nothing.
        //
        // It did cost. The fsync sat inside the window during which a
        // concurrent `fork()` can inherit this writer and block the exec with
        // `ETXTBSY` (#287), so removing it shortens that window. By how much
        // is unmeasured; the copy above is inside the window too.
        snapshot_writer
            .set_permissions(std::fs::Permissions::from_mode(0o500))
            .map_err(|error| format!("seal process executable snapshot mode: {error}"))?;
        drop(snapshot_writer);
        let snapshot = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&snapshot_path)
            .map_err(|error| format!("reopen sealed executable snapshot: {error}"))?;
        std::fs::remove_file(&snapshot_path)
            .map_err(|error| format!("unlink sealed executable snapshot: {error}"))?;
        drop(snapshot_directory);
        let digest = sha256_open_file(&snapshot)?;

        let cwd_path = std::env::current_dir()
            .map_err(|error| format!("resolve process working directory: {error}"))?;
        let cwd_fd = nix::fcntl::open(
            &cwd_path,
            nix::fcntl::OFlag::O_RDONLY
                | nix::fcntl::OFlag::O_DIRECTORY
                | nix::fcntl::OFlag::O_NOFOLLOW
                | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .map_err(|error| format!("open process working directory: {error}"))?;
        let cwd = unsafe { std::fs::File::from_raw_fd(cwd_fd) };
        let cwd_metadata = cwd
            .metadata()
            .map_err(|error| format!("inspect process working directory: {error}"))?;
        let environment_sha256 = format!("{:x}", Sha256::digest(b""));
        Ok(OpenedProcessExecutable {
            identity: ProcessExecutableIdentity {
                path: canonical.to_string_lossy().into_owned(),
                sha256: hex_digest(&digest),
                device: metadata.dev(),
                inode: metadata.ino(),
                arguments: arguments.to_vec(),
                environment_sha256,
                cwd_path: cwd_path.to_string_lossy().into_owned(),
                cwd_device: cwd_metadata.dev(),
                cwd_inode: cwd_metadata.ino(),
            },
            file: snapshot,
            cwd,
        })
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
pub(super) fn ensure_effective_execute_permission(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> std::result::Result<(), String> {
    use nix::unistd::{access, getegid, geteuid, getgroups, AccessFlags};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mode = metadata.permissions().mode();
    let euid = geteuid().as_raw();
    let egid = getegid().as_raw();
    let executable = if euid == 0 {
        mode & 0o111 != 0
    } else if euid == metadata.uid() {
        mode & 0o100 != 0
    } else {
        let in_group = egid == metadata.gid()
            || getgroups()
                .map_err(|error| format!("read effective process groups: {error}"))?
                .iter()
                .any(|group| group.as_raw() == metadata.gid());
        if in_group {
            mode & 0o010 != 0
        } else {
            mode & 0o001 != 0
        }
    };
    if !executable {
        return Err("process-run executable is not executable by the effective user".into());
    }
    access(path, AccessFlags::X_OK).map_err(|error| {
        format!(
            "process-run executable is not executable by the effective user: launch access check failed: {error}"
        )
    })
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
pub(super) fn sha256_open_file(file: &std::fs::File) -> std::result::Result<[u8; 32], String> {
    let mut file = file.try_clone().map_err(|error| error.to_string())?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    Ok(output)
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
pub(super) type ProcessBeforeExecHook = (String, Box<dyn FnOnce() + Send>);

#[cfg(test)]
pub(super) type AuthorizationBeforeUseHook = (uuid::Uuid, CapabilityKind, Box<dyn FnOnce() + Send>);

#[cfg(test)]
pub(super) static AUTHORIZATION_BEFORE_USE_HOOK: std::sync::OnceLock<
    Mutex<Vec<AuthorizationBeforeUseHook>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(super) static AUTHORIZATION_BEFORE_LEASE_HOOK: std::sync::OnceLock<
    Mutex<Vec<AuthorizationBeforeUseHook>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(super) fn run_authorization_hook(
    hooks: &std::sync::OnceLock<Mutex<Vec<AuthorizationBeforeUseHook>>>,
    execution_id: uuid::Uuid,
    capability: CapabilityKind,
) {
    let mut hook = hooks
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(index) = hook
        .iter()
        .position(|(expected_execution, expected_capability, _)| {
            *expected_execution == execution_id && *expected_capability == capability
        })
    {
        let (_, _, callback) = hook.remove(index);
        callback();
    }
}

#[cfg(test)]
pub(super) fn run_authorization_before_use_hook(
    execution_id: uuid::Uuid,
    capability: CapabilityKind,
) {
    run_authorization_hook(&AUTHORIZATION_BEFORE_USE_HOOK, execution_id, capability);
}

#[cfg(test)]
pub(super) fn run_authorization_before_lease_hook(
    execution_id: uuid::Uuid,
    capability: CapabilityKind,
) {
    run_authorization_hook(&AUTHORIZATION_BEFORE_LEASE_HOOK, execution_id, capability);
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
pub(super) static PROCESS_BEFORE_EXEC_HOOK: std::sync::OnceLock<
    Mutex<Option<ProcessBeforeExecHook>>,
> = std::sync::OnceLock::new();

/// Called while the private executable snapshot is still open for writing,
/// with the executable's canonical path and the snapshot's path.
///
/// That window is invisible from outside `open_process_executable`: the
/// snapshot is unlinked before it is executed, so no test can otherwise hold a
/// descriptor to it. #287 is precisely a failure inside that window -- a
/// descriptor still open for writing when the exec runs -- so a test needs to
/// be able to open one deterministically instead of waiting for CI to
/// interleave two `process-run` calls by chance.
///
/// Keyed on the executable path, like the before-exec hook, so a concurrent
/// unrelated `process-run` does not trip another test's callback.
#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
pub(super) type SnapshotWriteHooks = HashMap<String, Box<dyn FnMut(&Path) + Send>>;

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
pub(super) static SNAPSHOT_WRITE_HOOK: std::sync::OnceLock<Mutex<SnapshotWriteHooks>> =
    std::sync::OnceLock::new();

/// Counts execs the kernel refused with `ETXTBSY`, per executable.
///
/// Without this, a test that means to exercise the retry can pass for the
/// wrong reason: if the blocking descriptor happens to be released before the
/// first exec -- any pause before the spawn is enough -- the exec succeeds
/// on attempt one, the assertions all hold, and the retry is never run. The
/// test asserts this count moved, so "the retry works" cannot be concluded
/// from a run where nothing was retried.
///
/// Keyed by executable rather than global, because the tests here run in
/// parallel and one deliberately provokes refusals forever. A single counter
/// would let its refusals satisfy the other test's guard, which would make the
/// guard against vacuity itself vacuous.
#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
pub(super) static TEXT_FILE_BUSY_RETRIES: std::sync::OnceLock<Mutex<HashMap<String, usize>>> =
    std::sync::OnceLock::new();

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
pub(super) fn record_text_file_busy(executable: &str) {
    *TEXT_FILE_BUSY_RETRIES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(executable.to_string())
        .or_insert(0) += 1;
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
pub(super) fn text_file_busy_refusals(executable: &str) -> usize {
    TEXT_FILE_BUSY_RETRIES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(executable)
        .copied()
        .unwrap_or(0)
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
pub(super) fn run_snapshot_write_hook(executable: &Path, snapshot: &Path) {
    // Poison-tolerant on purpose. The callback runs under this lock, so a
    // panic inside one test's hook would otherwise poison the map for every
    // later `open_process_executable` in the binary, and the guard's own drop
    // would panic during unwind. One root cause, many misattributed failures.
    let mut hooks = SNAPSHOT_WRITE_HOOK
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(callback) = hooks.get_mut(executable.to_string_lossy().as_ref()) {
        callback(snapshot);
    }
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
pub(super) fn run_process_before_exec_hook(path: &str) {
    let mut hook = PROCESS_BEFORE_EXEC_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("process before-exec test hook lock");
    if hook.as_ref().is_some_and(|(expected, _)| expected == path) {
        let (_, callback) = hook.take().expect("matching process hook");
        callback();
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
pub(super) fn spawn_open_process(
    executable: OpenedProcessExecutable,
) -> std::result::Result<std::process::Child, String> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    let argv = std::iter::once(executable.identity.path.as_str())
        .chain(executable.identity.arguments.iter().map(String::as_str))
        .map(|value| CString::new(value).map_err(|_| "process argument contains NUL".to_string()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let environment = Vec::<CString>::new();
    let identity_path = executable.identity.path.clone();
    let fd = executable.file.as_raw_fd();
    let cwd_fd = executable.cwd.as_raw_fd();
    #[cfg(test)]
    run_process_before_exec_hook(&executable.identity.path);
    let mut command = std::process::Command::new("/bin/false");
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            nix::unistd::fchdir(cwd_fd)
                .map_err(|error| std::io::Error::from_raw_os_error(error as i32))?;
            match nix::unistd::fexecve(fd, &argv, &environment) {
                Ok(never) => match never {},
                Err(error) => Err(std::io::Error::from_raw_os_error(error as i32)),
            }
        });
    }
    spawn_retrying_text_file_busy(&mut command, &identity_path)
}

/// How many times to re-attempt an exec that returns `ETXTBSY`, and how long
/// to wait between attempts. The loop breaks before sleeping on its final
/// attempt, so eight attempts means seven waits: `5ms * (1 + 2 + ... + 7)`.
///
/// That is 140ms of sleep as a *floor*, not a ceiling -- `thread::sleep`
/// guarantees at least its duration. Nothing bounds a fully refused exec from
/// above in production; only the test does.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
pub(super) const TEXT_FILE_BUSY_ATTEMPTS: u32 = 8;

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
pub(super) const TEXT_FILE_BUSY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(5);

/// Spawn, re-attempting while the kernel reports `ETXTBSY`.
///
/// Every `process-run` writes a private snapshot of the authorized executable
/// and then execs it. The kernel refuses to exec a file that any process holds
/// open for writing, and `fork()` copies the whole descriptor table -- so a
/// second `process-run` forking anywhere inside the first one's write window
/// gives its child an inherited write descriptor, and the first one's exec
/// fails with "Text file busy" (#287). The descriptor closes when that child
/// execs, which makes the condition transient and self-clearing.
///
/// Retrying cannot widen the authorization. The object is already pinned by
/// the time this runs: the snapshot is sealed `0o500`, unlinked, and
/// identity-checked, and every attempt execs the same descriptor, so a retry
/// has no path to a different file than the one approved.
///
/// A genuinely permanent `ETXTBSY` still fails, with the kernel's own message
/// rather than a hang.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
pub(super) fn spawn_retrying_text_file_busy(
    command: &mut std::process::Command,
    executable: &str,
) -> std::result::Result<std::process::Child, String> {
    for attempt in 1..=TEXT_FILE_BUSY_ATTEMPTS {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.raw_os_error() == Some(nix::libc::ETXTBSY) => {
                #[cfg(test)]
                record_text_file_busy(executable);
                tracing::debug!(
                    executable,
                    attempt,
                    "exec refused with ETXTBSY; a descriptor still holds the \
                     snapshot open for writing, retrying"
                );
                if attempt == TEXT_FILE_BUSY_ATTEMPTS {
                    break;
                }
                // The caller already blocks on `wait_with_output`, so sleeping
                // here does not introduce blocking the call path did not have.
                std::thread::sleep(TEXT_FILE_BUSY_BACKOFF * attempt);
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    tracing::warn!(
        executable,
        attempts = TEXT_FILE_BUSY_ATTEMPTS,
        "exec refused with ETXTBSY on every attempt; giving up"
    );
    Err(format!(
        "execute process snapshot: Text file busy (os error {}) after {} attempts -- \
         a descriptor has held the executable open for writing throughout",
        nix::libc::ETXTBSY,
        TEXT_FILE_BUSY_ATTEMPTS
    ))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
)))]
pub(super) fn spawn_open_process(
    _executable: OpenedProcessExecutable,
) -> std::result::Result<std::process::Child, String> {
    Err("process-run is unsupported on this platform".into())
}

pub(super) fn normalize_process_grant(
    requirement: &mut CapabilityRequirement,
) -> std::result::Result<(), String> {
    if requirement.capability != CapabilityKind::ProcessRun {
        return Ok(());
    }
    let ResourceSelector::Process { executables } = &mut requirement.selector else {
        return Err("process-run grants require a typed process selector".into());
    };
    for executable in executables.iter_mut() {
        if executable.starts_with(PROCESS_IDENTITY_PREFIX) {
            let approved = ProcessExecutableIdentity::decode(executable)?;
            let current = open_process_executable(&approved.path, &approved.arguments)?.identity;
            if approved != current {
                return Err("process grant identity does not match the current executable".into());
            }
        } else {
            *executable = resolve_process_executable(executable)?.encode();
        }
    }
    Ok(())
}

pub(super) fn validate_process_effect(
    effect: &VmSideEffect,
) -> std::result::Result<(), VmDiagnostic> {
    let binding = registered_host_binding(&effect.requirement, &effect.origin)?;
    let crate::vm::HostSideEffect::Request { arguments } = &effect.event else {
        return Err(host_binding_error(
            &effect.origin,
            "process-run requires a typed host request",
        ));
    };
    let [TypedValue::String(command), TypedValue::List { values, .. }] = arguments.as_slice()
    else {
        return Err(host_binding_error(
            &effect.origin,
            "process-run requires an executable and string arguments",
        ));
    };
    let process_arguments = values
        .iter()
        .map(|value| match value {
            TypedValue::String(value) => Ok(value.clone()),
            _ => Err(host_binding_error(
                &effect.origin,
                "process-run arguments must be strings",
            )),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    validate_process_request(
        binding,
        &effect.requirement,
        command,
        &process_arguments,
        &effect.origin,
    )
    .map(|_| ())
}

/// Convert the statically admitted MCP input subset into JSON. Optional
/// record fields represented by `none` are omitted rather than serialized as
/// null; explicit JSON callers retain full control through generic mcp-call.
/// Run an async host effect to completion from synchronous typed-program code.
///
/// **Callers must not be on a runtime worker thread.** The `join` below blocks
/// the calling thread; if that thread is a worker and the future needs the
/// runtime to make progress — `mem-store` waits for the MemTree loader — the
/// worker it needs is the one blocking, and on a runtime with a single worker
/// that is a deadlock.
///
/// Every reachable in-tree caller satisfies this incidentally, through the
/// `tokio::task::spawn_blocking` hop that wraps both `TypedHostHandler` drive
/// sites. That hop is load-bearing, not incidental convenience, and is marked
/// as such at both sites. (The Co-Forth interpreter also called in through
/// `AgentVmBinding` with no hop, reachable only when a binding was attached by
/// a function that had no callers; #294 removed that subtree, so the two drive
/// sites are now the whole set.)
///
/// A `tokio::task::block_in_place` here would release the worker and make the
/// requirement unnecessary — but it panics inside a `LocalSet`, and
/// `Handle::runtime_flavor()` reports `MultiThread` there, so the panic cannot
/// be guarded against. `src/main.rs` runs the whole interactive REPL inside
/// `local.run_until(...)`, so that trade would swap a deadlock no in-tree
/// caller can reach for a panic one could. (The Co-Forth interpreter recorded
/// the same constraint independently; #294 removed it, so this is the only
/// place the reasoning is written down now.)
///
/// The residual hazard is in-tree, not out of it: a future third drive site
/// that constructs a `TypedHostHandler` without the hop. Two tests fail if that
/// happens -- `typed_mem_store_completes_on_a_single_worker_runtime` in this
/// module, and `typed_agent_await_completes_on_a_single_worker_runtime` in
/// `runtime::scheduler`, which covers the `agent-await` consumer. Both submit
/// non-suspending programs, so both exercise `execute_typed_program`'s hop;
/// neither reaches the one in `resume_typed_program`, which is uncovered.
///
/// An out-of-tree caller cannot reach this. `block_on_host` and
/// `TypedHostHandler` are private, and the public submit API performs the hop
/// itself — a caller on a worker is the case the test exercises and passes.
/// `AgentVmBinding::block_on` delegates here rather than repeating the shape,
/// so `agent-await` inherits both the requirement and this explanation. Both it
/// and `AgentVmBinding::new` are `pub(crate)`: narrowing only the method would
/// have left a composed path open, since a binding an external crate can build
/// and attach to a `Forth` reaches this from a worker with no hop (#289).
pub(super) fn block_on_host<F, T>(future: F) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| anyhow::anyhow!("typed host requires a Tokio runtime"))?;
    std::thread::scope(|scope| {
        scope
            .spawn(move || handle.block_on(future))
            .join()
            .map_err(|_| anyhow::anyhow!("typed host worker panicked"))?
    })
}

pub(super) fn authority_state_from_parts(
    session_id: uuid::Uuid,
    project_id: String,
    policy: CapabilityPolicy,
    ledger: CapabilityLedger,
    roots: &ResourceRootState,
) -> ProgramRuntimeAuthorityState {
    ProgramRuntimeAuthorityState {
        format_version: PROGRAM_RUNTIME_AUTHORITY_STATE_VERSION,
        session_id,
        project_id,
        policy,
        ledger,
        resource_roots: roots
            .bindings
            .values()
            .map(|binding| binding.as_ref().clone())
            .collect(),
        resource_root_audit: roots.audit.clone(),
    }
}

pub(super) fn resource_root_binding_record(
    root: crate::vm::ResourceRoot,
    supplied: &Path,
    generation: u64,
    whole_machine: bool,
    bound_at_unix_ms: u64,
) -> Result<ResourceRootBindingRecord> {
    let canonical = supplied
        .canonicalize()
        .with_context(|| format!("resolve resource root '{}'", supplied.display()))?;
    let metadata = std::fs::symlink_metadata(&canonical)
        .with_context(|| format!("inspect resource root '{}'", canonical.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "resource root is not a stable directory: {}",
            canonical.display()
        );
    }
    #[cfg(unix)]
    let (device, inode) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let (device, inode) = (0, 0);
    Ok(ResourceRootBindingRecord {
        root,
        path: canonical,
        device,
        inode,
        generation,
        whole_machine,
        bound_at_unix_ms,
    })
}

pub(super) fn validate_resource_root_authority(
    bindings: &[ResourceRootBindingRecord],
    audit: &[ResourceRootAuditEntry],
) -> Result<ResourceRootState> {
    let mut replayed = BTreeMap::<crate::vm::ResourceRoot, (u64, PathBuf, bool, u64)>::new();
    let mut next_generation = 1_u64;
    let mut previous_time = 0_u64;
    for (index, entry) in audit.iter().enumerate() {
        if entry.sequence != index as u64 + 1
            || entry.generation == 0
            || entry.actor.trim().is_empty()
            || entry.at_unix_ms < previous_time
        {
            bail!("resource-root audit sequence is malformed");
        }
        previous_time = entry.at_unix_ms;
        if entry.whole_machine
            && (entry.root != crate::vm::ResourceRoot::HostMachine || entry.path != Path::new("/"))
        {
            bail!("resource-root audit contains an invalid whole-machine binding");
        }
        match entry.action {
            ResourceRootAuditAction::Bound => {
                if entry.generation != next_generation || replayed.contains_key(&entry.root) {
                    bail!("resource-root audit contains an invalid bind transition");
                }
                replayed.insert(
                    entry.root.clone(),
                    (
                        entry.generation,
                        entry.path.clone(),
                        entry.whole_machine,
                        entry.at_unix_ms,
                    ),
                );
                next_generation = next_generation
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("resource-root generation overflow"))?;
            }
            ResourceRootAuditAction::Revoked => {
                let Some((generation, path, whole_machine, _)) = replayed.remove(&entry.root)
                else {
                    bail!("resource-root audit revokes an inactive root");
                };
                if generation != entry.generation
                    || path != entry.path
                    || whole_machine != entry.whole_machine
                {
                    bail!("resource-root audit revoke does not match the active binding");
                }
            }
        }
    }
    let mut active = BTreeMap::new();
    for binding in bindings {
        if binding.generation == 0 {
            bail!("resource-root generation must be non-zero");
        }
        if binding.whole_machine
            && (binding.root != crate::vm::ResourceRoot::HostMachine
                || binding.path != Path::new("/"))
        {
            bail!("resource-root authority contains an invalid whole-machine binding");
        }
        let current = resource_root_binding_record(
            binding.root.clone(),
            &binding.path,
            binding.generation,
            binding.whole_machine,
            binding.bound_at_unix_ms,
        )?;
        if &current != binding {
            bail!(
                "resource-root identity changed since approval: {}",
                binding.path.display()
            );
        }
        let Some((generation, path, whole_machine, bound_at)) = replayed.get(&binding.root) else {
            bail!("active resource root has no replayed audit authority");
        };
        if *generation != binding.generation
            || *path != binding.path
            || *whole_machine != binding.whole_machine
            || *bound_at != binding.bound_at_unix_ms
        {
            bail!("active resource root does not match its latest audit event");
        }
        if active
            .insert(binding.root.clone(), Arc::new(binding.clone()))
            .is_some()
        {
            bail!("authority state contains duplicate active resource roots");
        }
    }
    if active.len() != replayed.len() {
        bail!("resource-root active bindings do not match replayed audit state");
    }
    Ok(ResourceRootState {
        bindings: active,
        audit: audit.to_vec(),
    })
}

#[derive(Debug, Clone, Copy)]
pub(super) enum SecureOpenMode {
    ReadFile,
    WriteFile,
    ReadDirectory,
}

#[cfg(unix)]
pub(super) fn open_resource_beneath_mode(
    binding: &ResourceRootBindingRecord,
    selector: &crate::vm::FileSelector,
    relative: &str,
    mode: SecureOpenMode,
) -> std::result::Result<std::fs::File, String> {
    use nix::fcntl::{open, openat, OFlag};
    use nix::sys::stat::{fstat, Mode};
    use std::os::fd::{AsRawFd, FromRawFd};

    if !selector.matches(relative) || relative.contains(['*', '?']) {
        return Err("path is outside its declared selector".to_string());
    }
    let components = Path::new(relative)
        .components()
        .map(|component| match component {
            std::path::Component::Normal(name) => Ok(name.to_owned()),
            _ => Err("resource path must contain only normal relative components".to_string()),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if components.is_empty() {
        return Err("resource path is empty".into());
    }
    let root_fd = open(
        &binding.path,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| format!("open resource root '{}': {error}", binding.path.display()))?;
    let mut directory = unsafe { std::fs::File::from_raw_fd(root_fd) };
    let root_stat = fstat(directory.as_raw_fd()).map_err(|error| error.to_string())?;
    if root_stat.st_dev as u64 != binding.device || root_stat.st_ino as u64 != binding.inode {
        return Err("resource-root identity changed since it was bound".into());
    }
    for component in &components[..components.len() - 1] {
        let fd = openat(
            Some(directory.as_raw_fd()),
            component.as_os_str(),
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| format!("open resource path component: {error}"))?;
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    let final_component = components
        .last()
        .expect("non-empty components checked above")
        .as_os_str();
    #[cfg(test)]
    run_resource_before_final_open_hook(relative);
    let (flags, create_mode) = match mode {
        SecureOpenMode::ReadFile => (
            OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        ),
        SecureOpenMode::ReadDirectory => (
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        ),
        SecureOpenMode::WriteFile => (
            OFlag::O_WRONLY
                | OFlag::O_CREAT
                | OFlag::O_NONBLOCK
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        ),
    };
    let fd = openat(
        Some(directory.as_raw_fd()),
        final_component,
        flags,
        create_mode,
    )
    .map_err(|error| format!("open resource object: {error}"))?;
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let opened = fstat(file.as_raw_fd()).map_err(|error| error.to_string())?;
    let opened_kind = nix::sys::stat::SFlag::from_bits_truncate(opened.st_mode);
    match mode {
        SecureOpenMode::ReadFile | SecureOpenMode::WriteFile
            if !opened_kind.contains(nix::sys::stat::SFlag::S_IFREG) =>
        {
            return Err("resource object is not a regular file".into());
        }
        SecureOpenMode::ReadDirectory if !opened_kind.contains(nix::sys::stat::SFlag::S_IFDIR) => {
            return Err("resource object is not a directory".into());
        }
        _ => {}
    }
    if matches!(mode, SecureOpenMode::WriteFile) {
        nix::unistd::ftruncate(&file, 0)
            .map_err(|error| format!("truncate resource object: {error}"))?;
    }
    Ok(file)
}

#[cfg(all(test, unix))]
pub(super) type ResourceBeforeFinalOpenHook = (String, Box<dyn FnOnce() + Send>);

#[cfg(all(test, unix))]
pub(super) static RESOURCE_BEFORE_FINAL_OPEN_HOOK: std::sync::OnceLock<
    Mutex<Vec<ResourceBeforeFinalOpenHook>>,
> = std::sync::OnceLock::new();

#[cfg(all(test, unix))]
pub(super) fn run_resource_before_final_open_hook(relative: &str) {
    let mut hook = RESOURCE_BEFORE_FINAL_OPEN_HOOK
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(index) = hook.iter().position(|(expected, _)| expected == relative) {
        let (_, callback) = hook.remove(index);
        callback();
    }
}

#[cfg(not(unix))]
pub(super) fn open_resource_beneath_mode(
    _binding: &ResourceRootBindingRecord,
    _selector: &crate::vm::FileSelector,
    _relative: &str,
    _mode: SecureOpenMode,
) -> std::result::Result<std::fs::File, String> {
    Err("descriptor-relative resource access is unsupported on this platform".into())
}
