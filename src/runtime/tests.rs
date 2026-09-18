use super::VmEffectEnvelopeRuntimeMethods;
use super::*;

fn production_host_handler(runtime: &ProgramRuntime) -> TypedHostHandler {
    let execution_id = uuid::Uuid::new_v4();
    TypedHostHandler::new(
        Arc::clone(&runtime.automation),
        Arc::clone(&runtime.resource_roots),
        None,
        None,
        None,
        BTreeMap::new(),
        "[]".into(),
        Arc::clone(&runtime.network),
        Arc::clone(&runtime.output_handles),
        Arc::clone(&runtime.streams),
        execution_id,
        runtime.manifest_generation(),
        HostAuthorizationAudit {
            ledger: Arc::clone(&runtime.capability_ledger),
            policy: Arc::clone(&runtime.capability_policy),
            use_gate: Arc::clone(&runtime.authority_use_gate),
            sink: None,
            context: runtime.authorization_context_for(None).unwrap(),
            reason: "host-boundary regression".into(),
            program_hash: "test-program".into(),
            agent_ancestry: Vec::new(),
        },
        TypedRuntime::intrinsic_grants(),
        None,
        DeferredHostEffects::None,
        None,
    )
}

#[test]
fn host_requests_are_routed_through_registered_core_bindings() {
    let origin = SourceOrigin::generated("file-read");
    let requirement = core_word_spec("file-read")
        .unwrap()
        .signature
        .effects
        .0
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(
        registered_host_binding(&requirement, &origin).unwrap(),
        Some(CoreHostBinding::FileRead)
    );

    let wrong_requirement = CapabilityRequirement {
        capability: crate::vm::CapabilityKind::SessionEmit,
        selector: crate::vm::ResourceSelector::None,
    };
    assert!(registered_host_binding(&wrong_requirement, &origin).is_err());
    assert!(registered_host_binding(&requirement, &SourceOrigin::generated("+"),).is_err());

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    {
        let process_requirement = CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProcessRun,
            selector: crate::vm::ResourceSelector::Process {
                executables: vec![resolve_process_executable("/usr/bin/true")
                    .unwrap()
                    .encode()],
            },
        };
        assert_eq!(
            registered_host_binding(
                &process_requirement,
                &SourceOrigin::generated("process-run"),
            )
            .unwrap(),
            Some(CoreHostBinding::ProcessRun)
        );
        assert_eq!(
            registered_host_binding(
                &process_requirement,
                &SourceOrigin::generated("legacy-process-run"),
            )
            .unwrap(),
            None
        );
        let process_origin = SourceOrigin::generated("process-run");
        assert!(validate_process_request(
            Some(CoreHostBinding::ProcessRun),
            &process_requirement,
            "/usr/bin/true",
            &[],
            &process_origin,
        )
        .is_ok());
        assert!(validate_process_request(
            None,
            &process_requirement,
            "/usr/bin/true",
            &[],
            &SourceOrigin::generated("legacy-process-run"),
        )
        .is_err());
        assert!(validate_process_request(
            Some(CoreHostBinding::ProcessRun),
            &process_requirement,
            "/usr/bin/false",
            &[],
            &process_origin,
        )
        .is_err());
        let empty = CapabilityRequirement {
            capability: CapabilityKind::ProcessRun,
            selector: ResourceSelector::Process {
                executables: Vec::new(),
            },
        };
        assert!(validate_process_request(
            Some(CoreHostBinding::ProcessRun),
            &empty,
            "/usr/bin/true",
            &[],
            &process_origin,
        )
        .is_err());
        let multiple = CapabilityRequirement {
            capability: CapabilityKind::ProcessRun,
            selector: ResourceSelector::Process {
                executables: vec![
                    resolve_process_executable("/usr/bin/true")
                        .unwrap()
                        .encode(),
                    resolve_process_executable("/usr/bin/false")
                        .unwrap()
                        .encode(),
                ],
            },
        };
        assert!(validate_process_request(
            Some(CoreHostBinding::ProcessRun),
            &multiple,
            "/usr/bin/true",
            &[],
            &process_origin,
        )
        .is_err());
    }
}

#[test]
fn every_host_dispatch_rejects_a_same_capability_wrong_binding_or_abi() {
    let requirement = CapabilityRequirement {
        capability: CapabilityKind::AutomationWrite,
        selector: ResourceSelector::Automation { application: None },
    };
    let click = vec![
        TypedValue::Float(1.0),
        TypedValue::Float(2.0),
        TypedValue::String("left".into()),
        TypedValue::Int(1),
    ];
    let origin = SourceOrigin::generated("automation-click");
    assert!(validate_core_host_request(
        Some(CoreHostBinding::AutomationClick),
        &requirement,
        &click,
        &origin,
    )
    .is_ok());
    assert!(validate_core_host_request(
        Some(CoreHostBinding::AutomationType),
        &requirement,
        &click,
        &origin,
    )
    .is_err());
    assert!(validate_core_host_request(
        Some(CoreHostBinding::AutomationClick),
        &requirement,
        &[TypedValue::String("text".into()), TypedValue::Int(0)],
        &origin,
    )
    .is_err());

    let file_requirement = CapabilityRequirement::file(
        crate::vm::FileOperation::Read,
        crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
    );
    let broad = crate::vm::FileSelector::parse("./**").unwrap();
    assert!(validate_core_host_request(
        Some(CoreHostBinding::FileRead),
        &file_requirement,
        &[TypedValue::Path {
            selector: broad,
            relative: "README.md".into(),
        }],
        &SourceOrigin::generated("file-read"),
    )
    .is_err());

    let mut checked = 0_usize;
    for (name, spec) in crate::vm::core_word_registry() {
        let CoreWordImplementation::HostEffect(binding) = spec.implementation else {
            continue;
        };
        let Some(declared) = spec.signature.effects.0.first() else {
            panic!("host binding '{name}' has no declared authority");
        };
        let hostile = vec![TypedValue::Unit; 9];
        assert!(
            validate_core_host_request(
                Some(binding),
                declared,
                &hostile,
                &SourceOrigin::generated(name.clone()),
            )
            .is_err(),
            "host binding '{name}' accepted a hostile ABI",
        );
        checked += 1;
    }
    assert!(checked > 30, "expected the complete core host registry");

    let runtime = ProgramRuntime::new();
    let mut host = production_host_handler(&runtime);
    let error = crate::vm::CapabilityHandler::request(
        &mut host,
        &requirement,
        vec![TypedValue::String("hostile".into()), TypedValue::Int(0)],
        &origin,
    )
    .expect_err("the production host boundary must reject automation ABI substitution");
    assert_eq!(error.code, "E-HOST-002");

    let error = crate::vm::CapabilityHandler::request(
        &mut host,
        &file_requirement,
        vec![TypedValue::Path {
            selector: crate::vm::FileSelector::parse("./**").unwrap(),
            relative: "README.md".into(),
        }],
        &SourceOrigin::generated("file-read"),
    )
    .expect_err("the production host boundary must reject selector/argument substitution");
    assert_eq!(error.code, "E-HOST-002");
}

#[test]
fn network_send_rejects_a_stale_program_run_generation() {
    let listener = match std::net::TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(error) => panic!("failed to bind test listener: {error}"),
    };
    let port = listener.local_addr().unwrap().port();
    let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let (_server, _) = listener.accept().unwrap();
    let runtime = ProgramRuntime::new();
    let mut host = production_host_handler(&runtime);
    let handle = uuid::Uuid::new_v4().to_string();
    runtime.network.lock().unwrap().insert(
        handle.clone(),
        NetworkSocket {
            stream: client,
            host: "127.0.0.1".into(),
            port,
            owner: host.execution_id,
            generation: host.resource_generation,
        },
    );
    let requirement = CapabilityRequirement {
        capability: CapabilityKind::NetworkConnect,
        selector: ResourceSelector::Network {
            host: "127.0.0.1".into(),
            ports: vec![port],
        },
    };
    let stale_generation = host.resource_generation + 1;
    let error = crate::vm::CapabilityHandler::request(
        &mut host,
        &requirement,
        vec![
            TypedValue::Resource {
                kind: "network-socket".into(),
                handle,
                generation: stale_generation,
            },
            TypedValue::Bytes(b"hostile".to_vec()),
        ],
        &SourceOrigin::generated("network-send"),
    )
    .unwrap_err();
    assert_eq!(error.code, "E-HOST-002");
    assert!(error.message.contains("stale"));
}

#[test]
fn bounded_line_reader_preserves_cursor_position_and_normalizes_newlines() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"first\r\nsecond\nlast").unwrap();
    let mut reader = BufReader::new(file.reopen().unwrap());
    assert_eq!(
        read_bounded_utf8_line(&mut reader).unwrap(),
        Some("first".into())
    );
    assert_eq!(
        read_bounded_utf8_line(&mut reader).unwrap(),
        Some("second".into())
    );
    assert_eq!(
        read_bounded_utf8_line(&mut reader).unwrap(),
        Some("last".into())
    );
    assert_eq!(read_bounded_utf8_line(&mut reader).unwrap(), None);
}

#[test]
fn bounded_line_reader_accepts_a_limit_sized_crlf_line() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut writer = file.reopen().unwrap();
    writer.write_all(&vec![b'x'; 1024 * 1024]).unwrap();
    writer.write_all(b"\r\n").unwrap();
    let mut reader = BufReader::new(file.reopen().unwrap());

    assert_eq!(
        read_bounded_utf8_line(&mut reader).unwrap(),
        Some("x".repeat(1024 * 1024))
    );
    assert_eq!(read_bounded_utf8_line(&mut reader).unwrap(), None);
}

#[test]
fn bounded_csv_reader_keeps_quoted_multiline_records_intact() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(
        b"name,note\r\nAda,\"first line\nsecond, with comma\"\r\n\"Grace \"\"Amazing\"\"\",done\n",
    )
    .unwrap();
    let mut reader = BufReader::new(file.reopen().unwrap());

    assert_eq!(
        read_bounded_csv_record(&mut reader).unwrap(),
        Some(vec!["name".into(), "note".into()])
    );
    assert_eq!(
        read_bounded_csv_record(&mut reader).unwrap(),
        Some(vec!["Ada".into(), "first line\nsecond, with comma".into()])
    );
    assert_eq!(
        read_bounded_csv_record(&mut reader).unwrap(),
        Some(vec!["Grace \"Amazing\"".into(), "done".into()])
    );
    assert_eq!(read_bounded_csv_record(&mut reader).unwrap(), None);
}

#[test]
fn bounded_csv_reader_rejects_malformed_quote_boundaries() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"one,\"two\"oops\n").unwrap();
    let mut reader = BufReader::new(file.reopen().unwrap());

    assert!(read_bounded_csv_record(&mut reader)
        .unwrap_err()
        .contains("after closing quote"));
}

fn submission(
    language: ProgramLanguage,
    source: &str,
    effect: ExecutionEffect,
) -> ProgramSubmission {
    ProgramSubmission {
        language,
        source_id: None,
        source: source.to_string(),
        intent: "test".to_string(),
        effect,
        declared_capabilities: Vec::new(),
        manifest_generation: 1,
        expected_revision: None,
        budget: None,
    }
}

#[tokio::test]
async fn typed_only_submission_never_uses_the_legacy_forth_interpreter() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            // The legacy interpreter accepts this classic Forth
            // definition, while the typed frontend correctly requires an
            // explicit stack signature.
            ": legacy-double 2 * ;",
            ExecutionEffect::VmWrite,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Failed);
    assert!(outcome.vm_diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E-FORTH-SIG-001"
            && diagnostic
                .primary
                .as_ref()
                .and_then(|origin| origin.span.as_ref())
                .is_some()
    }));

    let state = runtime.inspect().await.unwrap();
    assert!(state
        .vocabulary
        .iter()
        .all(|word| word.name != "legacy-double"));
}

#[tokio::test]
async fn typed_boundary_preserves_symbols_and_results() {
    let runtime = ProgramRuntime::new();
    let symbol = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "'bash",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(symbol.values, vec![ProgramValue::Symbol("bash".into())]);

    let result = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(ok 7)",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(
        result.values,
        vec![
            ProgramValue::Symbol("bash".into()),
            ProgramValue::Result {
                ok: true,
                value: Box::new(ProgramValue::Int(7)),
            },
        ]
    );
}

#[tokio::test]
async fn rejects_stale_manifest_generation() {
    let runtime = ProgramRuntime::new();
    let mut request = submission(ProgramLanguage::Forth, "1", ExecutionEffect::Pure);
    request.manifest_generation = 0;
    let error = runtime.submit(request).await.unwrap_err();
    assert!(error.to_string().contains("stale VM manifest"));
}

#[tokio::test]
async fn source_cannot_hide_external_effect_behind_pure_declaration() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "s\" path\" path s\" data\" bytes file-write",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
    assert!(outcome
        .required_capabilities
        .iter()
        .any(|requirement| requirement.capability == crate::vm::CapabilityKind::FileWrite));
    assert!(outcome
        .inferred_capabilities
        .iter()
        .any(|requirement| requirement.capability == crate::vm::CapabilityKind::FileWrite));
    assert!(matches!(
        outcome.inferred_capabilities[0].selector,
        crate::vm::ResourceSelector::FileTemplate { .. }
    ));
    assert!(matches!(
        outcome.required_capabilities[0].selector,
        crate::vm::ResourceSelector::File { .. }
    ));
}

#[tokio::test]
async fn completed_outcome_retains_effects_from_an_untaken_branch() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "false if s\" missing.txt\" path file-read else s\" local\" bytes then",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert!(outcome.required_capabilities.is_empty());
    assert!(outcome
        .inferred_capabilities
        .iter()
        .any(|requirement| { requirement.capability == crate::vm::CapabilityKind::FileRead }));
}

#[tokio::test]
async fn portable_lisp_uses_the_typed_vm_without_forth_text() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(+ 3 (* 4 2))",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.backend, ExecutionBackend::TypedVm);
    assert_eq!(outcome.values, vec![ProgramValue::Int(11)]);
}

#[tokio::test]
async fn managed_json_fields_cross_the_public_runtime_boundary() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(json-get (result-unwrap (json-parse \"{\\\"nested\\\":{\\\"answer\\\":42}}\")) \"nested\")",
                ExecutionEffect::Pure,
            ))
            .await
            .expect("managed JSON field lookup succeeds");
    assert_eq!(
        outcome.values,
        vec![ProgramValue::Option(Some(Box::new(ProgramValue::Json(
            serde_json::json!({"answer": 42}),
        ))))]
    );
}

#[tokio::test]
async fn typed_dictionary_is_shared_between_forth_and_lisp_submissions() {
    let runtime = ProgramRuntime::new();
    let definition = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            ": square ( S int -- S int ! pure ) dup * ;",
            ExecutionEffect::VmWrite,
        ))
        .await
        .unwrap();
    assert_eq!(definition.backend, ExecutionBackend::TypedVm);
    let call = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(square 12)",
            ExecutionEffect::VmRead,
        ))
        .await
        .unwrap();
    assert_eq!(call.backend, ExecutionBackend::TypedVm);
    assert_eq!(call.values, vec![ProgramValue::Int(144)]);
}

#[tokio::test]
async fn say_is_a_typed_lisp_response_program() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(say \"hello from Lisp\")",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert_eq!(outcome.backend, ExecutionBackend::TypedVm);
    assert_eq!(outcome.output, "hello from Lisp");
}

#[tokio::test]
async fn typed_maps_cross_the_public_program_runtime_boundary_structurally() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(map \"answer\" 42 \"other\" 7)",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();

    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert_eq!(
        outcome.values,
        vec![ProgramValue::Map(vec![
            (ProgramValue::String("answer".into()), ProgramValue::Int(42)),
            (ProgramValue::String("other".into()), ProgramValue::Int(7)),
        ])]
    );
}

#[tokio::test]
async fn enabled_automation_still_requires_an_explicit_typed_grant() {
    let runtime = ProgramRuntime::with_automation(true);
    let denied = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(automation-availability)",
            ExecutionEffect::ExternalRead,
        ))
        .await
        .unwrap();
    assert_eq!(denied.status, ExecutionStatus::AuthorizationRequired);

    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::AutomationInspect,
            selector: crate::vm::ResourceSelector::Automation { application: None },
        })
        .unwrap();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(automation-availability)",
            ExecutionEffect::ExternalRead,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.backend, ExecutionBackend::TypedVm);
    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert!(matches!(
        outcome.values.first(),
        Some(ProgramValue::String(_))
    ));
}

#[tokio::test]
async fn approved_typed_file_read_resumes_with_a_refined_path() {
    let runtime = ProgramRuntime::new();
    let request = submission(
        ProgramLanguage::Lisp,
        "(begin (say \"checking\") (file-read (path \"Cargo.toml\")))",
        ExecutionEffect::WorkspaceRead,
    );
    let pending = runtime.submit(request).await.unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    assert_eq!(pending.required_capabilities.len(), 1);
    assert_eq!(pending.output, "checking");
    assert!(matches!(
        pending.effect_journal.as_slice(),
        [
            crate::vm::EffectJournalEntry {
                state: crate::vm::EffectJournalState::Acknowledged { values },
                ..
            },
            crate::vm::EffectJournalEntry {
                state: crate::vm::EffectJournalState::AwaitingApproval,
                ..
            },
        ] if values.is_empty()
    ));
    assert_eq!(
        pending.approval_prompts[0].request.origin.word.as_deref(),
        Some("file-read")
    );
    let effect_sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("concrete approval requests carry their VM effect sequence");
    assert!(matches!(
        pending.approval_prompts[0].request.arguments.as_slice(),
        [TypedValue::Path { relative, .. }] if relative == "Cargo.toml"
    ));
    let pending_info = runtime
        .pending_typed_execution(pending.execution_id)
        .unwrap()
        .expect("authorization should retain a daemon continuation");
    assert_eq!(pending_info.resume_effect_sequence, Some(effect_sequence));
    assert!(matches!(
        pending_info.reason,
        PendingTypedReason::AuthorizationRequired { .. }
    ));
    assert!(runtime
        .resume_typed_execution_for_effect(pending.execution_id, effect_sequence + 1)
        .await
        .is_err());
    assert!(runtime
        .pending_typed_execution(pending.execution_id)
        .unwrap()
        .is_some());
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
    let approved = runtime
        .resume_typed_execution_for_effect(pending.execution_id, effect_sequence)
        .await
        .unwrap();
    assert_eq!(approved.status, ExecutionStatus::Completed);
    assert_eq!(approved.output, "checking");
    assert!(matches!(
        approved.values.first(),
        Some(ProgramValue::Bytes(_))
    ));
    assert!(matches!(
        approved.effect_journal.as_slice(),
        [
            crate::vm::EffectJournalEntry {
                state: crate::vm::EffectJournalState::Acknowledged { .. },
                ..
            },
            crate::vm::EffectJournalEntry {
                state: crate::vm::EffectJournalState::Acknowledged { values },
                ..
            },
        ] if matches!(values.as_slice(), [TypedValue::Bytes(_)])
    ));
}

#[tokio::test]
async fn approval_requests_are_stable_and_preserve_agent_ancestry() {
    let runtime = ProgramRuntime::new();
    let root_agent_id = uuid::Uuid::new_v4();
    let parent_agent_id = uuid::Uuid::new_v4();
    let caller = agents::AgentIdentity {
        agent_id: uuid::Uuid::new_v4(),
        task_id: uuid::Uuid::new_v4(),
        parent_agent_id: Some(parent_agent_id),
        root_agent_id,
        depth: 2,
        provider_model: "test-provider".into(),
        vm_revision: 0,
        manifest_generation: runtime.manifest_generation(),
        starting_context_hash: "test-context".into(),
        grant_ceiling: EffectSet::pure(),
        brain_run_id: None,
    };
    let request = submission(
        ProgramLanguage::Lisp,
        "(file-read (path \"Cargo.toml\"))",
        ExecutionEffect::WorkspaceRead,
    );
    let pending = runtime
        .submit_as(request, Some(caller.clone()))
        .await
        .unwrap();
    let prompt = &pending.approval_prompts[0];
    assert_eq!(
        prompt.request.agent_ancestry,
        vec![root_agent_id, parent_agent_id, caller.agent_id]
    );

    let stored = runtime
        .pending_typed
        .lock()
        .unwrap()
        .get(&pending.execution_id)
        .cloned()
        .unwrap();
    let rendered_again = approval_prompts(
        pending.execution_id,
        &pending.required_capabilities,
        &stored.source,
        &stored.intent,
        Some(&stored.suspension),
        Some(&caller),
    );
    assert_eq!(rendered_again[0].request.id, prompt.request.id);
    assert_eq!(
        rendered_again[0].request.effect_sequence,
        prompt.request.effect_sequence
    );
}

#[tokio::test]
async fn task_scoped_grants_apply_only_to_the_matching_program_run() {
    let runtime = ProgramRuntime::new();
    let allowed_task = uuid::Uuid::new_v4();
    let requirement = crate::vm::CapabilityRequirement::file(
        crate::vm::FileOperation::Read,
        crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
    );
    runtime
        .issue_typed_capability(
            requirement,
            GrantScope::Task {
                task_id: allowed_task,
            },
            "test-user",
            None,
        )
        .unwrap();
    let identity = |task_id| agents::AgentIdentity {
        agent_id: uuid::Uuid::new_v4(),
        task_id,
        parent_agent_id: None,
        root_agent_id: uuid::Uuid::new_v4(),
        depth: 0,
        provider_model: "test-provider".into(),
        vm_revision: runtime.revision(),
        manifest_generation: runtime.manifest_generation(),
        starting_context_hash: "test-context".into(),
        grant_ceiling: EffectSet::pure(),
        brain_run_id: None,
    };
    let source = || {
        submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        )
    };

    let allowed = runtime
        .submit_as(source(), Some(identity(allowed_task)))
        .await
        .unwrap();
    assert_eq!(allowed.status, ExecutionStatus::Completed);

    let unrelated = runtime
        .submit_as(source(), Some(identity(uuid::Uuid::new_v4())))
        .await
        .unwrap();
    assert_eq!(unrelated.status, ExecutionStatus::AuthorizationRequired);
}

#[tokio::test]
async fn child_grant_ceiling_blocks_later_ambient_expansion_but_allows_task_approval() {
    let runtime = ProgramRuntime::new();
    let task_id = uuid::Uuid::new_v4();
    let child = agents::AgentIdentity {
        agent_id: uuid::Uuid::new_v4(),
        task_id,
        parent_agent_id: None,
        root_agent_id: uuid::Uuid::new_v4(),
        depth: 0,
        provider_model: "test-provider".into(),
        vm_revision: runtime.revision(),
        manifest_generation: runtime.manifest_generation(),
        starting_context_hash: "test-context".into(),
        grant_ceiling: runtime.effective_grants_for(None).unwrap(),
        brain_run_id: None,
    };
    let requirement = crate::vm::CapabilityRequirement::file(
        crate::vm::FileOperation::Read,
        crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
    );
    runtime
        .issue_typed_capability(
            requirement.clone(),
            GrantScope::Session {
                session_id: runtime.capability_session_id(),
            },
            "test-user",
            None,
        )
        .unwrap();
    let source = || {
        submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        )
    };

    let ambient = runtime
        .submit_as(source(), Some(child.clone()))
        .await
        .unwrap();
    assert_eq!(ambient.status, ExecutionStatus::AuthorizationRequired);
    runtime
        .cancel_typed_execution(ambient.execution_id)
        .unwrap();

    runtime
        .issue_typed_capability(requirement, GrantScope::Task { task_id }, "test-user", None)
        .unwrap();
    let explicitly_approved = runtime.submit_as(source(), Some(child)).await.unwrap();
    assert_eq!(explicitly_approved.status, ExecutionStatus::Completed);
}

#[tokio::test]
async fn exact_once_grants_never_enter_ambient_program_run_authority() {
    let runtime = ProgramRuntime::new();
    runtime
        .issue_typed_capability(
            crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
            ),
            GrantScope::Once {
                request_id: uuid::Uuid::new_v4(),
            },
            "test-user",
            None,
        )
        .unwrap();

    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
}

#[tokio::test]
async fn allow_once_resumes_exactly_one_runtime_effect() {
    let runtime = ProgramRuntime::new();
    let first = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(begin (file-read (path \"Cargo.toml\")) (file-read (path \"Cargo.lock\")))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    let first_prompt = first.approval_prompts[0].clone();
    let second = runtime
        .resolve_typed_approval(&first_prompt, ApprovalChoice::AllowOnce, "test-user")
        .await
        .unwrap();
    assert_eq!(second.status, ExecutionStatus::AuthorizationRequired);
    assert_ne!(
        second.approval_prompts[0].request.id,
        first_prompt.request.id
    );

    let ledger = runtime.capability_ledger().unwrap();
    let once = ledger
        .grants
        .grants
        .iter()
        .find(|grant| {
            matches!(
                grant.scope,
                GrantScope::Once { request_id }
                    if request_id == first_prompt.request.id
            )
        })
        .expect("allow-once records an exact grant");
    assert!(once.consumed_at_unix_ms.is_some());
    assert!(matches!(
        ledger
            .authorization_audit
            .last()
            .map(|entry| &entry.decision),
        Some(AuthorizationDecision::Allowed { .. })
    ));
}

#[tokio::test]
async fn session_approval_is_reused_only_after_exact_prompt_validation() {
    let runtime = ProgramRuntime::new();
    let source = || {
        submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        )
    };
    let pending = runtime.submit(source()).await.unwrap();
    let mut forged = pending.approval_prompts[0].clone();
    forged.request.id = uuid::Uuid::new_v4();
    assert!(runtime
        .resolve_typed_approval(&forged, ApprovalChoice::AllowSession, "test-user")
        .await
        .is_err());
    assert!(runtime
        .pending_typed_execution(pending.execution_id)
        .unwrap()
        .is_some());
    assert!(runtime
        .capability_ledger()
        .unwrap()
        .grants
        .grants
        .is_empty());

    let approved = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowSession,
            "test-user",
        )
        .await
        .unwrap();
    assert_eq!(approved.status, ExecutionStatus::Completed);
    let reused = runtime.submit(source()).await.unwrap();
    assert_eq!(reused.status, ExecutionStatus::Completed);
    assert!(reused.approval_prompts.is_empty());
    let ledger = runtime.capability_ledger().unwrap();
    let grant_id = ledger.grants.grants[0].id;
    assert_eq!(ledger.authorization_audit.len(), 2);
    assert!(ledger.authorization_audit.iter().all(|entry| matches!(
        entry.decision,
        AuthorizationDecision::Allowed { grant_id: used } if used == grant_id
    )));
    assert_ne!(
        ledger.authorization_audit[0].execution_id, ledger.authorization_audit[1].execution_id,
        "each actual host boundary keeps its owning ProgramRun identity"
    );
}

#[tokio::test]
async fn denied_approval_is_audited_and_discards_the_continuation() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    let denied = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::Deny,
            "test-user",
        )
        .await
        .unwrap();
    assert_eq!(denied.status, ExecutionStatus::Failed);
    assert!(matches!(
        denied.effect_journal.last().map(|entry| &entry.state),
        Some(crate::vm::EffectJournalState::Denied)
    ));
    assert!(runtime
        .pending_typed_execution(pending.execution_id)
        .unwrap()
        .is_none());
    assert!(matches!(
        runtime
            .capability_ledger()
            .unwrap()
            .authorization_audit
            .last()
            .map(|entry| &entry.decision),
        Some(AuthorizationDecision::Denied { .. })
    ));
}

#[tokio::test]
async fn typed_file_slice_reads_a_bounded_range_without_loading_the_file() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            "s\"Cargo.toml\" path 0 9 file-slice",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("file-slice must create a portable host request");
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
    let completed = runtime
        .resume_typed_execution_for_effect(pending.execution_id, sequence)
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(
        completed.values,
        vec![ProgramValue::Bytes(b"[package]".to_vec())]
    );
}

#[tokio::test]
async fn typed_file_hash_returns_sha256_without_materializing_file_bytes() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            "s\"Cargo.toml\" path file-hash",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("file-hash must create a portable host request");
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
    let outcome = runtime
        .resume_typed_execution_for_effect(pending.execution_id, sequence)
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert!(matches!(
        outcome.values.as_slice(),
        [ProgramValue::String(digest)]
            if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    ));
}

#[test]
fn tree_merkle_is_stable_and_changes_with_tree_contents() {
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("tree");
    std::fs::create_dir_all(tree.join("nested")).unwrap();
    std::fs::write(tree.join("alpha.txt"), "alpha").unwrap();
    std::fs::write(tree.join("nested").join("beta.txt"), "beta").unwrap();

    let first = merkle_directory(std::fs::File::open(&tree).unwrap()).unwrap();
    assert_eq!(first.len(), 64);
    assert_eq!(
        first,
        merkle_directory(std::fs::File::open(&tree).unwrap()).unwrap()
    );

    std::fs::write(tree.join("nested").join("beta.txt"), "changed").unwrap();
    assert_ne!(
        first,
        merkle_directory(std::fs::File::open(&tree).unwrap()).unwrap()
    );
}

#[test]
fn tree_list_is_sorted_bounded_and_structural() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("a-dir")).unwrap();
    std::fs::write(root.path().join("z.txt"), "z").unwrap();
    std::fs::write(root.path().join("a-dir").join("b.txt"), "beta").unwrap();
    std::fs::write(root.path().join("a.txt"), "alpha").unwrap();

    let (entries, truncated) =
        list_directory_tree(std::fs::File::open(root.path()).unwrap(), 3).unwrap();
    assert!(truncated);
    assert_eq!(entries.len(), 3);
    let paths = entries
        .iter()
        .map(|entry| match entry {
            TypedValue::Record(fields) => fields
                .iter()
                .find_map(|(name, value)| {
                    (name == "path")
                        .then_some(value)
                        .and_then(|value| match value {
                            TypedValue::String(path) => Some(path.as_str()),
                            _ => None,
                        })
                })
                .expect("tree entry path"),
            _ => panic!("tree-list must return records"),
        })
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["a-dir", "a-dir/b.txt", "a.txt"]);
    assert!(entries
        .iter()
        .all(|entry| entry.value_type() == tree_entry_type()));
}

#[tokio::test]
async fn typed_tree_list_has_identical_lisp_and_forth_results() {
    let mut results = Vec::new();
    for (language, source) in [
        (
            ProgramLanguage::Lisp,
            "(tree-list (path \"crates/finch-vm/src\") 5)",
        ),
        (
            ProgramLanguage::Forth,
            "s\"crates/finch-vm/src\" path 5 tree-list",
        ),
    ] {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let outcome = runtime
            .submit_typed_only(submission(language, source, ExecutionEffect::WorkspaceRead))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert!(matches!(
            outcome.values.as_slice(),
            [ProgramValue::Record(fields)]
                if fields.iter().any(|(name, value)| {
                    name == "truncated" && value == &ProgramValue::Bool(true)
                })
        ));
        results.push(outcome.values);
    }
    assert_eq!(results[0], results[1]);
}

#[tokio::test]
async fn typed_file_line_cursor_reads_one_bounded_line_at_a_time() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            "(let ((stream (file-lines-open (path \"Cargo.toml\")))) (stream-next stream))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("file-lines-open must create a portable host request");
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
    let completed = runtime
        .resume_typed_execution_for_effect(pending.execution_id, sequence)
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(
        completed.values,
        vec![ProgramValue::Option(Some(Box::new(ProgramValue::String(
            "[package]".into()
        ))))]
    );
}

#[tokio::test]
async fn file_stream_follow_up_rechecks_the_minting_selector_after_revocation() {
    let runtime = ProgramRuntime::new();
    let original_grant = runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
    let suspended = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            "s\"Cargo.toml\" path file-lines-open unit yield stream-next",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(suspended.status, ExecutionStatus::Suspended);

    assert!(runtime.revoke_typed_capability(original_grant).unwrap());
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./README.md").unwrap(),
        ))
        .unwrap();
    let resumed = runtime
        .resume_typed_execution(suspended.execution_id)
        .await
        .unwrap();
    assert_eq!(resumed.status, ExecutionStatus::AuthorizationRequired);
    assert_eq!(
        resumed.required_capabilities,
        vec![crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
        )]
    );
    let ledger = runtime.capability_ledger().unwrap();
    assert_eq!(ledger.authorization_audit.len(), 1);
    assert!(matches!(
        ledger.authorization_audit[0].decision,
        AuthorizationDecision::Allowed { .. }
    ));
}

#[tokio::test]
async fn typed_csv_cursor_reads_one_record_and_releases_its_handle() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            "s\"Cargo.toml\" path csv-open dup stream-next swap stream-close drop",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("csv-open must create a portable host request");
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
    let completed = runtime
        .resume_typed_execution_for_effect(pending.execution_id, sequence)
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(
        completed.values,
        vec![ProgramValue::Option(Some(Box::new(ProgramValue::List(
            vec![ProgramValue::String("[package]".into()),]
        ))))]
    );
}

#[tokio::test]
async fn typed_workbook_cursor_and_sheet_listing_match_across_frontends() {
    let directory = tempfile::tempdir_in(".").unwrap();
    let workbook_path = directory.path().join("typed-workbook.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    {
        let sheet = workbook.add_worksheet();
        sheet.set_name("First").unwrap();
        sheet.write_string(0, 0, "first").unwrap();
    }
    {
        let sheet = workbook.add_worksheet();
        sheet.set_name("Data").unwrap();
        sheet.write_string(0, 0, "answer").unwrap();
        sheet.write_number(0, 1, 42).unwrap();
    }
    {
        let sheet = workbook.add_worksheet();
        sheet.set_name("Stats").unwrap();
        sheet.write_string(0, 0, "name").unwrap();
        sheet.write_string(0, 1, "score").unwrap();
        sheet.write_string(0, 2, "note").unwrap();
        sheet.write_string(1, 0, "Ada").unwrap();
        sheet.write_number(1, 1, 10).unwrap();
        sheet.write_string(1, 2, "ok").unwrap();
        sheet.write_string(2, 0, "Bob").unwrap();
        sheet.write_number(2, 1, 20).unwrap();
        sheet.write_string(3, 0, "Cy").unwrap();
        sheet.write_string(3, 1, "not-a-number").unwrap();
        sheet.write_string(3, 2, "ok").unwrap();
    }
    workbook.save(&workbook_path).unwrap();
    let canonical_workbook = workbook_path.canonicalize().unwrap();
    let canonical_workspace = std::env::current_dir().unwrap().canonicalize().unwrap();
    let relative = canonical_workbook
        .strip_prefix(canonical_workspace)
        .unwrap()
        .to_string_lossy()
        .into_owned();

    for (language, source) in [
        (
            ProgramLanguage::Lisp,
            format!(
                "(let ((rows (workbook-sheet-open (path \"{relative}\") \"Data\"))) \
                       (let ((row (unwrap (stream-next rows)))) \
                         (begin (stream-close rows) (list-get row 1))))"
            ),
        ),
        (
            ProgramLanguage::Forth,
            format!(
                "\"{relative}\" path \"Data\" workbook-sheet-open \
                     dup stream-next unwrap 1 list-get swap stream-close drop"
            ),
        ),
    ] {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let outcome = runtime
            .submit_typed_only(submission(
                language,
                &source,
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            ExecutionStatus::Completed,
            "{language:?} workbook execution failed: {outcome:#?}"
        );
        assert_eq!(
            outcome.values,
            vec![ProgramValue::String("42".into())],
            "{language:?} did not read the named workbook row"
        );
    }

    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
    let outcome = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            &format!("(workbook-sheets (path \"{relative}\"))"),
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert_eq!(
        outcome.values,
        vec![ProgramValue::List(vec![
            ProgramValue::String("First".into()),
            ProgramValue::String("Data".into()),
            ProgramValue::String("Stats".into()),
        ])]
    );

    let expected_range = ProgramValue::List(vec![
        ProgramValue::List(vec![
            ProgramValue::String("Ada".into()),
            ProgramValue::String("10".into()),
        ]),
        ProgramValue::List(vec![
            ProgramValue::String("Bob".into()),
            ProgramValue::String("20".into()),
        ]),
    ]);
    let expected_summary = serde_json::json!({
        "sheet": "Stats",
        "headers": ["name", "score", "note"],
        "sampled_rows": 2,
        "truncated": true,
        "columns": [
            {"index": 0, "name": "name", "empty": 0, "non_empty": 2, "numeric": 0, "min": null, "max": null, "mean": null},
            {"index": 1, "name": "score", "empty": 0, "non_empty": 2, "numeric": 2, "min": 10.0, "max": 20.0, "mean": 15.0},
            {"index": 2, "name": "note", "empty": 1, "non_empty": 1, "numeric": 0, "min": null, "max": null, "mean": null}
        ]
    });
    for (language, range_source, summary_source) in [
        (
            ProgramLanguage::Lisp,
            format!("(workbook-range (path \"{relative}\") \"Stats\" 1 0 2 2)"),
            format!("(workbook-summary (path \"{relative}\") \"Stats\" 2)"),
        ),
        (
            ProgramLanguage::Forth,
            format!("\"{relative}\" path \"Stats\" 1 0 2 2 workbook-range"),
            format!("\"{relative}\" path \"Stats\" 2 workbook-summary"),
        ),
    ] {
        for (source, expected) in [
            (range_source, expected_range.clone()),
            (summary_source, ProgramValue::Json(expected_summary.clone())),
        ] {
            let runtime = ProgramRuntime::new();
            runtime
                .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                    crate::vm::FileOperation::Read,
                    crate::vm::FileSelector::parse("./**").unwrap(),
                ))
                .unwrap();
            let outcome = runtime
                .submit_typed_only(submission(
                    language,
                    &source,
                    ExecutionEffect::WorkspaceRead,
                ))
                .await
                .unwrap();
            assert_eq!(
                outcome.status,
                ExecutionStatus::Completed,
                "{language:?} workbook aggregate failed: {outcome:#?}"
            );
            assert_eq!(outcome.values, vec![expected]);
        }
    }
}

#[tokio::test]
async fn csv_summary_is_bounded_and_identical_across_frontends() {
    let mut file = tempfile::Builder::new()
        .prefix("finch-csv-summary-")
        .suffix(".csv")
        .tempfile_in(".")
        .unwrap();
    file.write_all(b"name,score,note\nAda,10,ok\nBob,20,\nCy,not-a-number,ok\n")
        .unwrap();
    let relative = file
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let expected = serde_json::json!({
        "headers": ["name", "score", "note"],
        "sampled_rows": 2,
        "truncated": true,
        "columns": [
            {"index": 0, "name": "name", "empty": 0, "non_empty": 2, "numeric": 0, "min": null, "max": null, "mean": null},
            {"index": 1, "name": "score", "empty": 0, "non_empty": 2, "numeric": 2, "min": 10.0, "max": 20.0, "mean": 15.0},
            {"index": 2, "name": "note", "empty": 1, "non_empty": 1, "numeric": 0, "min": null, "max": null, "mean": null}
        ]
    });

    for (language, source) in [
        (
            ProgramLanguage::Lisp,
            format!("(csv-summary (path \"{relative}\") 2)"),
        ),
        (
            ProgramLanguage::Forth,
            format!("\"{relative}\" path 2 csv-summary"),
        ),
    ] {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let outcome = runtime
            .submit_typed_only(submission(
                language,
                &source,
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert_eq!(outcome.values, vec![ProgramValue::Json(expected.clone())]);
    }
}

#[test]
fn csv_summary_rejects_rows_wider_than_the_header() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"one,two\n1,2,3\n").unwrap();
    let error = summarize_csv(BufReader::new(file.reopen().unwrap()), 10).unwrap_err();
    assert!(error.contains("has 3 fields but the header declares 2"));
}

#[tokio::test]
async fn typed_csv_cursor_branches_on_a_record_without_unwrapping() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\"Cargo.toml\" path csv-open dup csv-next if-some 0 list-get say else s\"No records.\" say then csv-close",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
    let sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("csv-open must create a portable host request");
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
    let completed = runtime
        .resume_typed_execution_for_effect(pending.execution_id, sequence)
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(completed.values, vec![ProgramValue::Nil]);
    assert_eq!(completed.output, "[package]");
}

#[tokio::test]
async fn typed_file_line_cursor_streams_a_text_file_through_a_verified_loop() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            "s\"Cargo.toml\" path file-lines-open \
                 begin: lines true while \
                   dup stream-next if-some \
                     say \
                   else \
                     break lines \
                   then \
                 repeat \
                 stream-close",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("file-lines-open must create a portable host request");
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();

    let completed = runtime
        .resume_typed_execution_for_effect(pending.execution_id, sequence)
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(completed.values, vec![ProgramValue::Nil]);
    assert!(completed.output.starts_with("[package]"));
}

#[tokio::test]
async fn typed_lisp_file_line_cursor_streams_a_text_file_through_a_verified_loop() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            "(let ((cursor (file-lines-open (path \"Cargo.toml\")))) \
                   (begin \
                     (while :label lines true \
                       (match-option (stream-next cursor) \
                         (some line (say line)) \
                         (none (break lines)))) \
                     (stream-close cursor)))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("file-lines-open must create a portable host request");
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();

    let completed = runtime
        .resume_typed_execution_for_effect(pending.execution_id, sequence)
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert!(completed.values.is_empty());
    assert!(completed.output.starts_with("[package]"));
}

#[tokio::test]
async fn typed_runtime_accepts_a_portable_external_effect_result() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("awaited host effect must have a stable sequence");
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();

    let completed = runtime
        .resume_vm_effect(VmResume {
            execution_id: pending.execution_id,
            sequence,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Bytes(b"from external event loop".to_vec())],
            },
        })
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(
        completed.values,
        vec![ProgramValue::Bytes(b"from external event loop".to_vec())]
    );
    assert!(matches!(
        completed.effect_journal.last(),
        Some(crate::vm::EffectJournalEntry {
            state: crate::vm::EffectJournalState::Acknowledged { values },
            ..
        }) if values == &vec![TypedValue::Bytes(b"from external event loop".to_vec())]
    ));
}

#[tokio::test]
async fn named_brain_schedule_submission_defers_only_the_schedule_host_result() {
    let runtime = ProgramRuntime::new();
    let requirement = crate::vm::CapabilityRequirement {
        capability: crate::vm::CapabilityKind::ScheduleCreate,
        selector: crate::vm::ResourceSelector::Schedule { policy: None },
    };
    runtime.grant_typed_capability(requirement.clone()).unwrap();
    let (sink, receiver) = typed_effect_channel();
    let pending = runtime
        .submit_typed_only_with_deferred_schedule_effects(
            submission(
                ProgramLanguage::Lisp,
                "(schedule-create \"(say \\\"later\\\")\" 1770000000)",
                ExecutionEffect::Unclassified,
            ),
            sink,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::Suspended);
    let envelope = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    assert_eq!(envelope.execution_id, pending.execution_id);
    assert_eq!(envelope.effect.requirement, requirement);
    assert!(matches!(
        envelope.effect.event,
        crate::vm::HostSideEffect::Request { ref arguments }
            if matches!(arguments.as_slice(),
                [TypedValue::String(_), TypedValue::Int(1770000000)])
    ));

    let completed = runtime
        .resume_vm_effect(VmResume {
            execution_id: envelope.execution_id,
            sequence: envelope.effect.sequence,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Resource {
                    kind: "schedule".into(),
                    handle: uuid::Uuid::new_v4().to_string(),
                    generation: 0,
                }],
            },
        })
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert!(matches!(
        completed.values.as_slice(),
        [ProgramValue::Resource { kind, .. }] if kind == "schedule"
    ));
}

#[tokio::test]
async fn cancellation_marks_a_pending_capability_request_in_the_effect_journal() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("pending request has a portable sequence");

    let cancelled = runtime
        .resume_vm_effect(VmResume {
            execution_id: pending.execution_id,
            sequence,
            response: VmResumeResponse::Cancelled {
                reason: Some("host shut down".into()),
            },
        })
        .await
        .expect("pending request should produce a cancelled outcome");
    assert_eq!(cancelled.status, ExecutionStatus::Cancelled);
    assert!(cancelled.diagnostics[0].contains("host shut down"));
    assert!(matches!(
        cancelled.effect_journal.as_slice(),
        [crate::vm::EffectJournalEntry {
            state: crate::vm::EffectJournalState::Cancelled,
            ..
        }]
    ));
}

#[tokio::test]
async fn stale_portable_cancellation_keeps_the_current_continuation() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    let sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("pending request has a portable sequence");

    assert!(runtime
        .resume_vm_effect(VmResume {
            execution_id: pending.execution_id,
            sequence: sequence + 1,
            response: VmResumeResponse::Cancelled { reason: None },
        })
        .await
        .is_err());
    assert!(runtime
        .pending_typed_execution(pending.execution_id)
        .unwrap()
        .is_some());

    let cancelled = runtime
        .resume_vm_effect(VmResume {
            execution_id: pending.execution_id,
            sequence,
            response: VmResumeResponse::Cancelled { reason: None },
        })
        .await
        .unwrap();
    assert_eq!(cancelled.status, ExecutionStatus::Cancelled);
}

#[tokio::test]
async fn portable_denial_records_the_exact_effect_without_resuming_it() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    let sequence = pending.approval_prompts[0]
        .request
        .effect_sequence
        .expect("pending request has a portable sequence");

    let denied = runtime
        .resume_vm_effect(VmResume {
            execution_id: pending.execution_id,
            sequence,
            response: VmResumeResponse::Denied {
                reason: "user declined workspace access".into(),
            },
        })
        .await
        .unwrap();
    assert_eq!(denied.status, ExecutionStatus::Failed);
    assert!(denied.diagnostics[0].contains("user declined workspace access"));
    assert!(matches!(
        denied.effect_journal.last(),
        Some(crate::vm::EffectJournalEntry {
            state: crate::vm::EffectJournalState::Denied,
            ..
        })
    ));
    assert!(runtime
        .pending_typed_execution(pending.execution_id)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn typed_yield_keeps_one_execution_id_and_accumulates_streamed_output() {
    let runtime = ProgramRuntime::new();
    let yielded = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(begin (say \"before\") (yield) (say \"after\"))",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(yielded.status, ExecutionStatus::Suspended);
    assert_eq!(yielded.output, "before");

    let completed = runtime
        .resume_typed_execution(yielded.execution_id)
        .await
        .unwrap();
    assert_eq!(completed.execution_id, yielded.execution_id);
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(completed.output, "beforeafter");
    assert!(runtime
        .pending_typed_execution(yielded.execution_id)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn typed_effect_sink_is_per_run_and_survives_yield() {
    let runtime = ProgramRuntime::new();
    let first_events = Arc::new(Mutex::new(Vec::new()));
    let first_sink: TypedEffectSink = {
        let first_events = Arc::clone(&first_events);
        Arc::new(move |effect| {
            if let crate::vm::HostSideEffect::Emit { text } = effect.effect.event {
                first_events.lock().unwrap().push(text);
            }
        })
    };
    let second_events = Arc::new(Mutex::new(Vec::new()));
    let second_sink: TypedEffectSink = {
        let second_events = Arc::clone(&second_events);
        Arc::new(move |effect| {
            if let crate::vm::HostSideEffect::Emit { text } = effect.effect.event {
                second_events.lock().unwrap().push(text);
            }
        })
    };

    let yielded = runtime
        .submit_with_typed_effect_sink(
            submission(
                ProgramLanguage::Lisp,
                "(begin (say \"first-before\") (yield) (say \"first-after\"))",
                ExecutionEffect::Pure,
            ),
            first_sink,
        )
        .await
        .unwrap();
    runtime
        .resume_typed_execution(yielded.execution_id)
        .await
        .unwrap();
    runtime
        .submit_with_typed_effect_sink(
            submission(
                ProgramLanguage::Lisp,
                "(say \"second\")",
                ExecutionEffect::Pure,
            ),
            second_sink,
        )
        .await
        .unwrap();

    assert_eq!(
        &*first_events.lock().unwrap(),
        &vec!["first-before".to_string(), "first-after".to_string()]
    );
    assert_eq!(&*second_events.lock().unwrap(), &vec!["second".to_string()]);
}

#[tokio::test]
async fn typed_effect_channel_preserves_one_run_event_order() {
    let runtime = ProgramRuntime::new();
    let (sink, receiver) = typed_effect_channel();
    let outcome = runtime
        .submit_with_typed_effect_sink(
            submission(
                ProgramLanguage::Lisp,
                "(begin (say \"first\") (say \"second\"))",
                ExecutionEffect::Pure,
            ),
            sink,
        )
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);

    let events = receiver.try_iter().collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].execution_id, outcome.execution_id);
    assert_eq!(events[0].effect.sequence, 0);
    assert_eq!(events[1].effect.sequence, 1);
    assert!(matches!(
        &events[0].effect.event,
        crate::vm::HostSideEffect::Emit { text } if text == "first"
    ));
    assert!(matches!(
        &events[1].effect.event,
        crate::vm::HostSideEffect::Emit { text } if text == "second"
    ));
}

#[tokio::test]
async fn typed_effect_sink_observes_an_awaited_request_before_approval() {
    let runtime = ProgramRuntime::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink: TypedEffectSink = {
        let events = Arc::clone(&events);
        Arc::new(move |effect| events.lock().unwrap().push(effect))
    };

    let outcome = runtime
        .submit_with_typed_effect_sink(
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            ),
            sink,
        )
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
    assert!(matches!(
        events.lock().unwrap().as_slice(),
        [VmEffectEnvelope {
            effect: VmSideEffect {
                sequence: 0,
                event: crate::vm::HostSideEffect::Request { .. },
                output,
                ..
            },
            ..
        }] if output == &vec![Type::Bytes]
    ));
}

#[tokio::test]
async fn typed_suspension_can_be_inspected_and_cancelled_without_committing() {
    let runtime = ProgramRuntime::new();
    let yielded = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(begin (say \"before\") (yield) (say \"after\"))",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert!(matches!(
        runtime
            .pending_typed_execution(yielded.execution_id)
            .unwrap()
            .expect("yield should retain a daemon continuation")
            .reason,
        PendingTypedReason::Yielded
    ));
    let cancelled = runtime
        .cancel_typed_execution_with_outcome(yielded.execution_id)
        .unwrap()
        .expect("yielded run should produce a cancelled audit outcome");
    assert_eq!(cancelled.status, ExecutionStatus::Cancelled);
    assert_eq!(cancelled.output, "before");
    assert!(matches!(
        cancelled.effect_journal.as_slice(),
        [crate::vm::EffectJournalEntry {
            state: crate::vm::EffectJournalState::Acknowledged { values },
            ..
        }] if values.is_empty()
    ));
    assert!(runtime
        .resume_typed_execution(yielded.execution_id)
        .await
        .is_err());
    assert_eq!(runtime.revision(), yielded.input_revision);
}

#[tokio::test]
async fn pending_execution_capacity_cancels_the_new_run_without_eviction() {
    let runtime = ProgramRuntime::new();
    let mut retained_ids = Vec::with_capacity(MAX_PENDING_TYPED_EXECUTIONS);
    for _ in 0..MAX_PENDING_TYPED_EXECUTIONS {
        let yielded = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(yield)",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(yielded.status, ExecutionStatus::Suspended);
        retained_ids.push(yielded.execution_id);
    }
    assert_eq!(
        runtime.pending_typed_execution_count().unwrap(),
        MAX_PENDING_TYPED_EXECUTIONS
    );

    let rejected = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(yield)",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status, ExecutionStatus::Cancelled);
    assert!(rejected
        .diagnostics
        .iter()
        .any(|diagnostic| { diagnostic.contains("pending execution capacity 256 is exhausted") }));
    assert_eq!(
        runtime.pending_typed_execution_count().unwrap(),
        MAX_PENDING_TYPED_EXECUTIONS
    );
    assert!(runtime
        .pending_typed_execution(retained_ids[0])
        .unwrap()
        .is_some());
    assert!(runtime
        .pending_typed_execution(rejected.execution_id)
        .unwrap()
        .is_none());

    for execution_id in retained_ids {
        assert!(runtime.cancel_typed_execution(execution_id).unwrap());
    }
    assert_eq!(runtime.pending_typed_execution_count().unwrap(), 0);
}

#[tokio::test]
async fn yielded_execution_resumes_by_exact_id_until_completion() {
    let runtime = ProgramRuntime::new();
    let first = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(begin (say \"one\") (yield) (say \"two\") (yield) (say \"three\"))",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(first.status, ExecutionStatus::Suspended);
    assert_eq!(first.output, "one");
    assert!(matches!(
        runtime
            .pending_typed_execution(first.execution_id)
            .unwrap()
            .expect("first yield must persist its continuation")
            .reason,
        PendingTypedReason::Yielded
    ));

    let second = runtime
        .resume_typed_execution(first.execution_id)
        .await
        .unwrap();
    assert_eq!(second.status, ExecutionStatus::Suspended);
    assert_eq!(second.execution_id, first.execution_id);
    assert_eq!(second.output, "onetwo");
    assert!(matches!(
        runtime
            .pending_typed_execution(first.execution_id)
            .unwrap()
            .expect("second yield must replace the saved continuation")
            .reason,
        PendingTypedReason::Yielded
    ));

    let complete = runtime
        .resume_typed_execution(first.execution_id)
        .await
        .unwrap();
    assert_eq!(complete.status, ExecutionStatus::Completed);
    assert_eq!(complete.execution_id, first.execution_id);
    assert_eq!(complete.output, "onetwothree");
    assert_eq!(complete.output_chunks, ["one", "two", "three"]);
    assert!(runtime
        .pending_typed_execution(first.execution_id)
        .unwrap()
        .is_none());
    assert_eq!(runtime.revision(), first.input_revision + 1);
}

#[tokio::test]
async fn typed_yield_payload_is_visible_without_exposing_the_continuation() {
    let runtime = ProgramRuntime::new();
    let yielded = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "42 yield 7",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(yielded.status, ExecutionStatus::Suspended);

    let pending = runtime
        .pending_typed_execution(yielded.execution_id)
        .unwrap()
        .expect("typed yield must retain its continuation");
    assert_eq!(pending.reason, PendingTypedReason::Yielded);
    assert_eq!(pending.yielded_value, Some(ProgramValue::Int(42)));
    assert_eq!(pending.yielded_type, Some(Type::Int));

    let completed = runtime
        .resume_typed_execution(yielded.execution_id)
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(completed.values, vec![ProgramValue::Int(7)]);
}

#[tokio::test]
async fn typed_producer_values_cross_the_public_runtime_boundary() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(let ((numbers (defer (lambda () (begin (yield 2) (yield 3) 5))))) (list (fiber-next numbers) (fiber-next numbers) (fiber-next numbers)))",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();

    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert_eq!(
        outcome.values,
        vec![ProgramValue::List(vec![
            ProgramValue::Result {
                ok: true,
                value: Box::new(ProgramValue::Int(2)),
            },
            ProgramValue::Result {
                ok: true,
                value: Box::new(ProgramValue::Int(3)),
            },
            ProgramValue::Result {
                ok: false,
                value: Box::new(ProgramValue::Variant {
                    name: "end".into(),
                    value: Some(Box::new(ProgramValue::Int(5))),
                }),
            },
        ])]
    );
}

#[tokio::test]
async fn typed_fiber_handle_crosses_the_public_runtime_boundary() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(defer (lambda () (begin (yield 2) 5)))",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();

    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert!(matches!(
        outcome.values.as_slice(),
        [ProgramValue::Fiber {
            yield_type: Type::Int,
            result_type: Type::Int,
            ..
        }]
    ));
}

#[tokio::test]
async fn typed_capability_request_does_not_mutate_or_fallback() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(mem-store \"remember this\")",
            ExecutionEffect::VmWrite,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
    assert_eq!(outcome.output_revision, outcome.input_revision);
    assert_eq!(outcome.required_capabilities.len(), 1);
    assert_eq!(outcome.approval_prompts.len(), 1);
    assert_eq!(
        outcome.approval_prompts[0].exact,
        outcome.required_capabilities[0]
    );
    assert_eq!(
        outcome.required_capabilities[0].capability,
        crate::vm::CapabilityKind::MemoryWrite
    );
}

#[tokio::test]
async fn capability_ledger_restores_and_revokes_runtime_authority_by_id() {
    let requirement = crate::vm::CapabilityRequirement::file(
        crate::vm::FileOperation::Read,
        crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
    );
    let request = || {
        submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        )
    };
    let runtime = ProgramRuntime::new();
    let grant_id = runtime.grant_typed_capability(requirement.clone()).unwrap();
    let ledger = runtime.capability_ledger().unwrap();
    assert_eq!(ledger.grants.grants[0].id, grant_id);
    assert_eq!(ledger.audit.len(), 1);
    assert_eq!(
        runtime.submit(request()).await.unwrap().status,
        ExecutionStatus::Completed
    );

    let restored = ProgramRuntime::new();
    restored.restore_capability_ledger(ledger).unwrap();
    assert_eq!(
        restored.submit(request()).await.unwrap().status,
        ExecutionStatus::Completed
    );
    assert!(restored.revoke_typed_capability(grant_id).unwrap());
    let denied = restored.submit(request()).await.unwrap();
    assert_eq!(denied.status, ExecutionStatus::AuthorizationRequired);
    let ledger = restored.capability_ledger().unwrap();
    assert_eq!(ledger.audit.len(), 2);
    assert_eq!(
        ledger.audit[1].action,
        crate::vm::CapabilityAuditAction::Revoked
    );
}

#[test]
fn failed_authority_sink_rolls_back_a_new_grant() {
    let runtime = ProgramRuntime::new();
    runtime
        .set_authority_sink(Arc::new(|_| {
            Err(anyhow::anyhow!("simulated authority storage failure"))
        }))
        .unwrap();
    let requirement = CapabilityRequirement {
        capability: CapabilityKind::AgentSpawn,
        selector: crate::vm::ResourceSelector::None,
    };
    let error = runtime
        .issue_typed_capability(
            requirement.clone(),
            GrantScope::Session {
                session_id: runtime.capability_session_id(),
            },
            "test-user",
            None,
        )
        .expect_err("a grant must not survive failed durable policy storage");
    assert!(format!("{error:#}").contains("simulated authority storage failure"));
    assert!(runtime
        .capability_ledger()
        .unwrap()
        .grants
        .grants
        .is_empty());
    assert!(!runtime
        .typed
        .lock()
        .unwrap()
        .grants()
        .grants(&EffectSet::from_requirement(requirement)));
}

#[test]
fn process_grant_without_process_selector_has_no_authority_effects() {
    let runtime = ProgramRuntime::new();
    let sink_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    runtime
        .set_authority_sink({
            let sink_calls = Arc::clone(&sink_calls);
            Arc::new(move |_| {
                sink_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        })
        .unwrap();
    let before = runtime.capability_ledger().unwrap();

    let error = runtime
        .issue_typed_capability(
            CapabilityRequirement {
                capability: CapabilityKind::ProcessRun,
                selector: crate::vm::ResourceSelector::None,
            },
            GrantScope::Global,
            "test-user",
            None,
        )
        .expect_err("process-run authority must bind an exact process selector");

    assert!(format!("{error:#}").contains("typed process selector"));
    assert_eq!(runtime.capability_ledger().unwrap(), before);
    assert_eq!(sink_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn policy_change_revokes_obsolete_and_denied_grants_and_blocks_reissue() {
    let runtime = ProgramRuntime::new();
    let file_read = CapabilityRequirement::file(
        crate::vm::FileOperation::Read,
        crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
    );
    let agent_spawn = CapabilityRequirement {
        capability: CapabilityKind::AgentSpawn,
        selector: crate::vm::ResourceSelector::None,
    };
    let file_grant = runtime
        .issue_typed_capability(file_read.clone(), GrantScope::Global, "test-user", None)
        .unwrap();
    let agent_grant = runtime
        .issue_typed_capability(agent_spawn, GrantScope::Global, "test-user", None)
        .unwrap();

    let mut denied = std::collections::BTreeSet::new();
    denied.insert(CapabilityKind::FileRead);
    let revoked = runtime
        .apply_capability_policy(
            CapabilityPolicy {
                policy_hash: "finch-local-runtime-v2".into(),
                denied_capabilities: denied,
            },
            "policy-admin",
        )
        .unwrap();
    assert_eq!(revoked, vec![file_grant, agent_grant]);
    let ledger = runtime.capability_ledger().unwrap();
    assert!(ledger
        .grants
        .grants
        .iter()
        .find(|grant| grant.id == file_grant)
        .unwrap()
        .revoked_at_unix_ms
        .is_some());
    assert!(ledger
        .grants
        .grants
        .iter()
        .find(|grant| grant.id == agent_grant)
        .unwrap()
        .revoked_at_unix_ms
        .is_some());
    assert!(runtime
        .issue_typed_capability(file_read, GrantScope::Global, "test-user", None)
        .unwrap_err()
        .to_string()
        .contains("denied by policy"));
    assert!(runtime
        .apply_capability_policy(
            CapabilityPolicy {
                policy_hash: "finch-local-runtime-v2".into(),
                denied_capabilities: Default::default(),
            },
            "policy-admin",
        )
        .unwrap_err()
        .to_string()
        .contains("cannot be reused"));

    let replacement_agent_grant = runtime
        .issue_typed_capability(
            CapabilityRequirement {
                capability: CapabilityKind::AgentSpawn,
                selector: crate::vm::ResourceSelector::None,
            },
            GrantScope::Global,
            "test-user",
            None,
        )
        .unwrap();
    let revoked = runtime
        .apply_capability_policy(
            CapabilityPolicy {
                policy_hash: "finch-local-runtime-v3".into(),
                denied_capabilities: Default::default(),
            },
            "policy-admin",
        )
        .unwrap();
    assert_eq!(revoked, vec![replacement_agent_grant]);
    assert_eq!(
        runtime.capability_policy().unwrap().policy_hash,
        "finch-local-runtime-v3"
    );
}

#[test]
fn failed_authority_sink_rolls_back_policy_and_its_revocations() {
    let runtime = ProgramRuntime::new();
    let requirement = CapabilityRequirement {
        capability: CapabilityKind::AgentSpawn,
        selector: crate::vm::ResourceSelector::None,
    };
    let grant_id = runtime
        .issue_typed_capability(requirement, GrantScope::Global, "test-user", None)
        .unwrap();
    let previous_policy = runtime.capability_policy().unwrap();
    let previous_ledger = runtime.capability_ledger().unwrap();
    runtime
        .set_authority_sink(Arc::new(|_| {
            Err(anyhow::anyhow!("simulated policy storage failure"))
        }))
        .unwrap();

    let error = runtime
        .apply_capability_policy(
            CapabilityPolicy {
                policy_hash: "finch-local-runtime-v2".into(),
                denied_capabilities: Default::default(),
            },
            "policy-admin",
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("simulated policy storage failure"));
    assert_eq!(runtime.capability_policy().unwrap(), previous_policy);
    assert_eq!(runtime.capability_ledger().unwrap(), previous_ledger);
    assert!(runtime
        .capability_ledger()
        .unwrap()
        .grants
        .grants
        .iter()
        .find(|grant| grant.id == grant_id)
        .unwrap()
        .is_active(unix_time_ms()));
}

/// The production path for #276's `mem-store` deadlock, on the runtime
/// shape that would expose it: one worker.
///
/// `block_on_host` blocks its calling thread, so if that thread were a
/// runtime worker the write would wait for the MemTree loader while holding
/// the only thread the loader could run on. It is not a worker: both
/// `TypedHostHandler` drive sites go through `tokio::task::spawn_blocking`,
/// and this pins that the hop stays.
///
/// Written against the real submit API rather than a hand-built
/// `block_on_host` shape. An earlier version asserted on the hand-built one
/// and called it "the production boundary"; review showed production never
/// produces it, so the test was defending a shape no caller creates.
#[test]
fn typed_mem_store_completes_on_a_single_worker_runtime() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime");

    let database = tempfile::NamedTempFile::new().unwrap();
    let path = database.path().to_path_buf();

    // Seed enough rows that hydration actually has work. Without this the
    // store is empty, no loader is spawned, the write gate is already open,
    // and the test passes whether or not the deadlock is possible — which
    // is exactly what the first version of it did.
    {
        crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
            db_path: path.clone(),
            use_neural_embeddings: false,
            ..Default::default()
        })
        .expect("schema");
        let conn = rusqlite::Connection::open(&path).expect("open");
        let embedding: Vec<u8> = 0.5f32.to_le_bytes().repeat(8);
        let tx = conn.unchecked_transaction().expect("tx");
        for id in 0..2048i64 {
            tx.execute(
                "INSERT OR REPLACE INTO tree_nodes
                     (node_id, parent_id, text, embedding, level, created_at, importance)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
                rusqlite::params![
                    id,
                    if id == 0 { None } else { Some(0i64) },
                    format!("seeded node {id}"),
                    embedding,
                    if id == 0 { 0i64 } else { 1i64 },
                    id,
                ],
            )
            .expect("seed");
        }
        tx.commit().expect("commit");
    }

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    runtime.spawn(async move {
        let memory = Arc::new(
            crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
                db_path: path,
                use_neural_embeddings: false,
                ..Default::default()
            })
            .expect("memory"),
        );
        let program_runtime = ProgramRuntime::new();
        program_runtime.attach_memory(memory);
        program_runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::MemoryWrite,
                selector: crate::vm::ResourceSelector::Memory {
                    tree: "session".into(),
                    path: "**".into(),
                },
            })
            .expect("grant");
        let outcome = program_runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(mem-store \"a fact stored from a single-worker runtime\")",
                ExecutionEffect::VmWrite,
            ))
            .await;
        let _ = done_tx.send(outcome.map(|outcome| outcome.status));
    });

    // Bounded, and the runtime is torn down before asserting: a regression
    // here wedges the worker, and dropping the runtime would then block the
    // whole test binary — which in CI is an unattributed timeout rather
    // than a named failure.
    let outcome = done_rx.recv_timeout(std::time::Duration::from_secs(30));
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));

    let status = outcome
        .expect("mem-store deadlocked on a single-worker runtime")
        .expect("submit");
    assert_eq!(status, ExecutionStatus::Completed);
}

#[tokio::test]
async fn typed_memory_host_reads_and_writes_through_attached_memtree() {
    let database = tempfile::NamedTempFile::new().unwrap();
    let memory = Arc::new(
        crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
            db_path: database.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        })
        .unwrap(),
    );
    let runtime = ProgramRuntime::new();
    runtime.attach_memory(memory);
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::MemoryWrite,
            selector: crate::vm::ResourceSelector::Memory {
                tree: "session".into(),
                path: "**".into(),
            },
        })
        .unwrap();
    let stored = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(mem-store \"typed memory fact\")",
            ExecutionEffect::VmWrite,
        ))
        .await
        .unwrap();
    assert_eq!(stored.status, ExecutionStatus::Completed);
    assert!(matches!(
        stored.values.first(),
        Some(ProgramValue::Resource { kind, .. }) if kind == "memory-node"
    ));
}

/// `mem-recall` must refuse an unusable index rather than answer "nothing".
///
/// The typed surface returns a `List<String>`, which has nowhere to put a
/// caveat, so an empty list from an index that never loaded reads to the
/// calling program -- and to the model that wrote it -- as "no such
/// memory". With a `Failed` index the answer carries no information either
/// way, so an error is the only honest result and is something a program
/// can branch on (#275).
#[tokio::test]
async fn typed_mem_recall_refuses_an_unusable_index_instead_of_reporting_absence() {
    let database = tempfile::NamedTempFile::new().unwrap();
    let db_path = database.path().to_path_buf();
    {
        let memory = crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
            db_path: db_path.clone(),
            use_neural_embeddings: false,
            ..Default::default()
        })
        .unwrap();
        memory
            .insert_conversation("user", "the deploy key lives in 1Password", None, None)
            .await
            .unwrap();
    }
    // `level` is read as an i64, and non-numeric TEXT keeps its type under
    // INTEGER affinity, so no row parses and the index reaches `Failed`
    // with the memory still on disk.
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute("UPDATE tree_nodes SET level = 'unreadable'", [])
        .unwrap();

    let memory = Arc::new(
        crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
            db_path,
            use_neural_embeddings: false,
            ..Default::default()
        })
        .unwrap(),
    );
    assert!(
        matches!(
            memory.hydration_status(),
            crate::memory::HydrationStatus::Failed { .. }
        ),
        "fixture must actually break hydration, or this test cannot fail: {:?}",
        memory.hydration_status()
    );

    let runtime = ProgramRuntime::new();
    runtime.attach_memory(memory);
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::MemoryRead,
            selector: crate::vm::ResourceSelector::Memory {
                tree: "session".into(),
                path: "**".into(),
            },
        })
        .unwrap();

    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(mem-recall \"deploy key\")",
            ExecutionEffect::VmRead,
        ))
        .await
        .unwrap();

    assert_ne!(
        outcome.status,
        ExecutionStatus::Completed,
        "returned a list from an index it never read, which a program reads \
             as proof the memory does not exist: {:?}",
        outcome.values
    );
    // Assert the reason, not just the outcome. A denied capability, a
    // renamed word, a rejected effect or a detached memory binding would
    // all satisfy `!= Completed`, so without this the test could pass
    // without ever reaching the hydration check.
    assert!(
        outcome
            .diagnostics
            .iter()
            .any(|line| line.contains("memory index is unavailable")),
        "failed for some other reason than the unusable index: {:?}",
        outcome.diagnostics
    );
}

/// Every hydration state projects to the record it claims.
///
/// The two `submit` tests cover `Degraded` and `Failed`. Review of #308
/// mutation-tested the rest and found a hole: replacing the `Loading` arm
/// with `("ready", true, ..)` -- so a half-loaded index reports itself
/// complete -- left both of them green, as did blanking `Ready`'s counts.
/// `Loading` is the state #295 is actually about, and the one every
/// startup passes through, so that mutation is the exact lie this word
/// exists to prevent.
///
/// A table over all four states is the cheap fix. The `submit` tests stay
/// because they pin the production path end to end; this pins the
/// projection, which is where the states are enumerated.
#[test]
fn test_memory_index_status_projects_every_hydration_state() {
    use crate::memory::HydrationStatus;
    let origin = crate::vm::SourceOrigin::generated("mem-index-status");
    let int = |value: i64| ProgramValue::Option(Some(Box::new(ProgramValue::Int(value))));
    let text =
        |value: &str| ProgramValue::Option(Some(Box::new(ProgramValue::String(value.into()))));

    let cases = [
        (
            HydrationStatus::Ready { nodes: 7 },
            "ready",
            true,
            int(7),
            int(7),
            ProgramValue::Option(None),
        ),
        (
            HydrationStatus::Loading {
                loaded: 3,
                total: 9,
            },
            "loading",
            false,
            int(3),
            int(9),
            ProgramValue::Option(None),
        ),
        (
            HydrationStatus::Degraded {
                loaded: 4,
                total: 11,
                reason: "a batch would not read".into(),
            },
            "degraded",
            false,
            int(4),
            int(11),
            text("a batch would not read"),
        ),
        (
            HydrationStatus::Failed {
                reason: "the loader died".into(),
            },
            "failed",
            false,
            ProgramValue::Option(None),
            ProgramValue::Option(None),
            text("the loader died"),
        ),
    ];

    for (status, state, complete, loaded, total, reason) in cases {
        let value = typed_memory_index_status(status.clone(), &origin)
            .unwrap_or_else(|error| panic!("{status:?} must project: {error:?}"));
        let record = typed_value(value).expect("the record converts");
        assert_eq!(
            record,
            ProgramValue::Record(vec![
                ("state".into(), ProgramValue::String(state.into())),
                ("complete".into(), ProgramValue::Bool(complete)),
                ("loaded".into(), loaded),
                ("total".into(), total),
                ("reason".into(), reason),
            ]),
            "{status:?} projected wrongly"
        );
    }
}

/// A still-hydrating index reports itself incomplete, through `submit`.
///
/// This is the literal case #295 names -- "during startup it can run
/// against a fraction of the MemTree" -- and it is reached by holding the
/// production loader mid-hydration rather than by constructing a status,
/// so a change that stopped deriving `Loading` from the loader would fail
/// here rather than pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_mem_index_status_reports_a_still_loading_index_as_incomplete() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db_path = temp.path().to_path_buf();
    let config = crate::memory::MemoryConfig {
        db_path: db_path.clone(),
        use_neural_embeddings: false,
        ..Default::default()
    };
    drop(crate::memory::MemorySystem::new(config.clone()).unwrap());
    seed_nodes(&db_path, 4 * crate::memory::HYDRATION_BATCH as i64, None);

    // The loader itself announces that it has committed the first batch
    // and then waits. Polling the tree size cannot establish this window:
    // on a fast runner every batch can land before a polling task is
    // scheduled once.
    let (_registration, mut batch_reached, release) = crate::memory::register_hydration_batch_pause(
        db_path.clone(),
        crate::memory::HYDRATION_BATCH,
    );
    let memory = Arc::new(crate::memory::MemorySystem::new(config).unwrap());
    let hydrating = {
        let memory = Arc::clone(&memory);
        tokio::spawn(async move { memory.ensure_hydrated().await })
    };

    if !*batch_reached.borrow_and_update() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                batch_reached
                    .changed()
                    .await
                    .expect("the hydration pause sender disappeared");
                if *batch_reached.borrow_and_update() {
                    break;
                }
            }
        })
        .await
        .expect("the loader never reached the first batch");
    }

    let status = ask(&runtime_reading(Arc::clone(&memory)), "(mem-index-status)").await;
    assert_eq!(
        status_field(&status, "state"),
        &ProgramValue::String("loading".into()),
        "the loader is held mid-hydration: {status:?}"
    );
    assert_eq!(
        status_field(&status, "complete"),
        &ProgramValue::Bool(false),
        "a still-loading index must not report itself complete -- this is \
             the state #295 is about: {status:?}"
    );
    let (ProgramValue::Option(Some(loaded)), ProgramValue::Option(Some(total))) = (
        status_field(&status, "loaded"),
        status_field(&status, "total"),
    ) else {
        panic!("a loading index knows its counts: {status:?}");
    };
    let (ProgramValue::Int(loaded), ProgramValue::Int(total)) = (loaded.as_ref(), total.as_ref())
    else {
        panic!("counts must be integers: {status:?}");
    };
    assert!(
        *loaded > 0 && loaded < total,
        "held after one batch, so some but not all of {total} is loaded, got {loaded}"
    );

    let _ = release.send(true);
    let _ = hydrating.await;
}

/// Read one field out of the `mem-index-status` record.
fn status_field<'a>(record: &'a ProgramValue, name: &str) -> &'a ProgramValue {
    let ProgramValue::Record(fields) = record else {
        panic!("mem-index-status must return a record, got {record:?}");
    };
    fields
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("no `{name}` field in mem-index-status: {fields:?}"))
}

/// Write `count` linked nodes straight into the store, optionally leaving
/// invalid UTF-8 in one of them so its batch fails to read.
fn seed_nodes(db_path: &std::path::Path, count: i64, corrupt: Option<i64>) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let embedding: Vec<u8> = 0.5f32.to_le_bytes().repeat(8);
    let tx = conn.unchecked_transaction().unwrap();
    for id in 0..count {
        tx.execute(
            "INSERT OR REPLACE INTO tree_nodes
                 (node_id, parent_id, text, embedding, level, created_at, importance)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
            rusqlite::params![
                id,
                if id == 0 { None } else { Some(0i64) },
                format!("seeded node {id}"),
                embedding,
                if id == 0 { 0i64 } else { 1i64 },
                id,
            ],
        )
        .unwrap();
    }
    if let Some(id) = corrupt {
        tx.execute(
            "UPDATE tree_nodes SET text = ?1 WHERE node_id = ?2",
            rusqlite::params![vec![0xffu8, 0xfeu8], id],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

/// Build a memory whose hydration stops early but leaves what loaded
/// coherent, by way of the loader rather than by setting the flag.
///
/// An unreadable text blob in the second batch is the production route to
/// `Degraded`: the first batch lands, the second errors, and the tree that
/// remains is linked but incomplete. Reaching in and marking the status
/// directly would let a collapse of `Degraded` into `Ready` at the batch
/// arm keep this passing.
async fn degraded_memory(db_path: std::path::PathBuf) -> Arc<crate::memory::MemorySystem> {
    let config = crate::memory::MemoryConfig {
        db_path: db_path.clone(),
        use_neural_embeddings: false,
        ..Default::default()
    };
    drop(crate::memory::MemorySystem::new(config.clone()).unwrap());

    // Invalid UTF-8 in a node belonging to the *second* batch, so the first
    // has already committed when the read fails. That ordering is what
    // separates `Degraded` from `Failed`, and writing the id relative to
    // the batch size keeps it true if the batch size changes.
    seed_nodes(
        &db_path,
        2 * crate::memory::HYDRATION_BATCH as i64,
        Some(crate::memory::HYDRATION_BATCH as i64 + 4),
    );

    let memory = Arc::new(crate::memory::MemorySystem::new(config).unwrap());
    memory.ensure_hydrated().await.ok();
    assert!(
        matches!(
            memory.hydration_status(),
            crate::memory::HydrationStatus::Degraded { .. }
        ),
        "fixture must actually reach a partial index, or this test cannot \
             fail for the reason it exists: {:?}",
        memory.hydration_status()
    );
    memory
}

/// Run one Lisp source through `submit` and return its single value.
async fn ask(runtime: &ProgramRuntime, source: &str) -> ProgramValue {
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            source,
            ExecutionEffect::VmRead,
        ))
        .await
        .unwrap();
    assert_eq!(
        outcome.status,
        ExecutionStatus::Completed,
        "{source} did not complete: {:?}",
        outcome.diagnostics
    );
    // `values` is the VM stack, and it persists across submissions on the
    // same runtime: a second program's result is pushed onto the first's
    // rather than replacing it. Taking `first()` here silently returned
    // the *previous* program's value, which is how an earlier version of
    // this test read a `mem-recall` list where it expected a status
    // record.
    outcome.values.last().cloned().unwrap_or(ProgramValue::Nil)
}

/// Attach a memory to a runtime that may read it.
fn runtime_reading(memory: Arc<crate::memory::MemorySystem>) -> ProgramRuntime {
    let runtime = ProgramRuntime::new();
    runtime.attach_memory(memory);
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::MemoryRead,
            selector: crate::vm::ResourceSelector::Memory {
                tree: "session".into(),
                path: "**".into(),
            },
        })
        .unwrap();
    runtime
}

/// The residual #275 could not remove: an empty `mem-recall` from an index
/// that was only partly read, and an empty one from a complete index, are
/// the same value.
///
/// So the test first pins what the recall does *not* carry: the partial
/// index answers with a bare list, completing exactly as the complete
/// index does, with no error and no caveat anywhere in the value. Then it
/// asserts `mem-index-status` supplies the missing bit. Asserting only the
/// second half would leave the test passing if `mem-recall` started
/// refusing partial indexes outright, which would fix the ambiguity by
/// breaking working programs -- the outcome #295 explicitly declined, and
/// `ask`'s completion assertion is what catches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_mem_index_status_tells_a_partial_recall_from_a_complete_one() {
    let absent = "(mem-recall \"a phrase that was never stored\")";

    // A complete index with nothing in it, so its empty recall is a
    // genuine absence rather than an artifact of hydration.
    let whole_db = tempfile::NamedTempFile::new().unwrap();
    let whole = Arc::new(
        crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
            db_path: whole_db.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        })
        .unwrap(),
    );
    whole.ensure_hydrated().await.unwrap();
    assert!(
        matches!(
            whole.hydration_status(),
            crate::memory::HydrationStatus::Ready { .. }
        ),
        "control must be a complete index: {:?}",
        whole.hydration_status()
    );
    let whole_runtime = runtime_reading(whole);

    // An index that stopped early.
    let partial_db = tempfile::NamedTempFile::new().unwrap();
    let partial_runtime = runtime_reading(degraded_memory(partial_db.path().to_path_buf()).await);

    // The defect, stated as what is actually observable: the recall from a
    // partial index completes and hands back a bare `List<String>`, with
    // no error, no caveat and nothing in the value to say the index was
    // only fractionally read. It is the same kind of answer a complete
    // index gives.
    //
    // Note what this deliberately does *not* assert. An earlier version
    // claimed both recalls return an identical empty list; that is false,
    // and the test failed on it. Retrieval is nearest-neighbour, so a
    // query for a phrase that was never stored still returns the closest
    // nodes -- an empty list arises only from a genuinely empty tree, not
    // from a missing phrase. The ambiguity #295 names is real but it is
    // "this list was computed against a fraction of the index", not "empty
    // means absent".
    let from_partial = ask(&partial_runtime, absent).await;
    assert!(
        matches!(from_partial, ProgramValue::List(_)),
        "a partial index answers with a bare list carrying no caveat: {from_partial:?}"
    );
    let from_whole = ask(&whole_runtime, absent).await;
    assert_eq!(
        from_whole,
        ProgramValue::List(Vec::new()),
        "the control tree is empty, so this recall is a genuine absence"
    );
    // The fix: the status word separates them.
    let whole_status = ask(&whole_runtime, "(mem-index-status)").await;
    let partial_status = ask(&partial_runtime, "(mem-index-status)").await;

    assert_eq!(
        status_field(&whole_status, "state"),
        &ProgramValue::String("ready".into())
    );
    assert_eq!(
        status_field(&whole_status, "complete"),
        &ProgramValue::Bool(true),
        "a fully hydrated index must report complete, or an empty recall \
             stays unfalsifiable: {whole_status:?}"
    );

    assert_eq!(
        status_field(&partial_status, "state"),
        &ProgramValue::String("degraded".into())
    );
    assert_eq!(
        status_field(&partial_status, "complete"),
        &ProgramValue::Bool(false),
        "a partial index must not claim completeness: {partial_status:?}"
    );

    // The counts are the actionable part: a program that sees `false` still
    // needs to know how much was missed.
    let loaded = status_field(&partial_status, "loaded");
    let total = status_field(&partial_status, "total");
    let (ProgramValue::Option(Some(loaded)), ProgramValue::Option(Some(total))) = (loaded, total)
    else {
        panic!("a partial index knows its counts: {partial_status:?}");
    };
    let (ProgramValue::Int(loaded), ProgramValue::Int(total)) = (loaded.as_ref(), total.as_ref())
    else {
        panic!("counts must be integers: {partial_status:?}");
    };
    assert!(
        *loaded > 0 && loaded < total,
        "a degraded index read some but not all of {total}, got {loaded}"
    );
    assert!(
        matches!(
            status_field(&partial_status, "reason"),
            ProgramValue::Option(Some(_))
        ),
        "a degraded index carries why it stopped: {partial_status:?}"
    );
}

/// A failed index reports no counts rather than zero.
///
/// `HydrationStatus::Failed` deliberately carries no totals: hydration
/// ended without a trustworthy number. Projecting that as `0 of 0` would
/// publish a measurement nobody took. The record would still carry
/// `state: failed` and `complete: false` beside it, so it is not that the
/// record contradicts itself -- it is that one field would be invented,
/// and an invented count is exactly what a program reading `loaded` would
/// act on. `none` is the only honest projection.
#[tokio::test]
async fn typed_mem_index_status_reports_a_failed_index_without_inventing_counts() {
    let database = tempfile::NamedTempFile::new().unwrap();
    let db_path = database.path().to_path_buf();
    {
        let memory = crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
            db_path: db_path.clone(),
            use_neural_embeddings: false,
            ..Default::default()
        })
        .unwrap();
        memory
            .insert_conversation("user", "the deploy key lives in 1Password", None, None)
            .await
            .unwrap();
    }
    // Same route as the `mem-recall` refusal test: `level` is read as an
    // i64 and non-numeric TEXT keeps its type under INTEGER affinity, so
    // no row parses and the index reaches `Failed`.
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute("UPDATE tree_nodes SET level = 'unreadable'", [])
        .unwrap();

    let memory = Arc::new(
        crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
            db_path,
            use_neural_embeddings: false,
            ..Default::default()
        })
        .unwrap(),
    );
    assert!(
        matches!(
            memory.hydration_status(),
            crate::memory::HydrationStatus::Failed { .. }
        ),
        "fixture must actually break hydration: {:?}",
        memory.hydration_status()
    );

    let outcome = runtime_reading(memory)
        .submit(submission(
            ProgramLanguage::Lisp,
            "(mem-index-status)",
            ExecutionEffect::VmRead,
        ))
        .await
        .unwrap();
    assert_eq!(
        outcome.status,
        ExecutionStatus::Completed,
        "the status word must answer for an index mem-recall refuses -- \
             that is the case it exists for: {:?}",
        outcome.diagnostics
    );

    let status = outcome.values.last().expect("one record");
    assert_eq!(
        status_field(status, "state"),
        &ProgramValue::String("failed".into())
    );
    assert_eq!(status_field(status, "complete"), &ProgramValue::Bool(false));
    assert_eq!(
        status_field(status, "loaded"),
        &ProgramValue::Option(None),
        "a failed index must not report a count it does not have: {status:?}"
    );
    assert_eq!(
        status_field(status, "total"),
        &ProgramValue::Option(None),
        "a count here would be invented, not measured: {status:?}"
    );
    assert!(
        matches!(
            status_field(status, "reason"),
            ProgramValue::Option(Some(_))
        ),
        "a failed index carries why: {status:?}"
    );
}

/// The production entry point refuses the sheet, not just the helper.
///
/// `read_workbook_rows` backs `workbook-open`, `workbook-sheet-open`,
/// `workbook-range` and `workbook-summary` -- every typed spreadsheet word
/// -- and it is where the bound used to sit one step too late: the
/// `MAX_WORKBOOK_CELLS` check ran on the `Range` that `worksheet_range()`
/// had already allocated, so it bounded what Finch would iterate and never
/// what calamine would allocate (#282).
#[test]
fn read_workbook_rows_refuses_a_sheet_whose_box_cannot_be_allocated() {
    use std::io::Write;

    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&crate::workbook::fixtures::two_cells_spanning_the_whole_sheet())
        .unwrap();
    file.flush().unwrap();

    let started = std::time::Instant::now();
    let error = read_workbook_rows(file.as_file(), "hostile.xlsx", None)
        .expect_err("a 1.7e10-cell bounding box must be refused");
    let elapsed = started.elapsed();

    // Wall clock, not "it did not crash": filling 1.7e10 `Data` slots
    // cannot finish in a second on any host, so a fast refusal is positive
    // evidence the box was never built -- where a bare `is_err()` would
    // also pass if the allocation happened and something later complained.
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "took {elapsed:?}, long enough to have allocated the box"
    );
    assert!(
        error.contains("1048576 rows") && error.contains("16384 columns"),
        "the error must name the dimensions that made it too big: {error}"
    );
}

/// And the same at the boundary when the header lies.
///
/// The test above is caught by the constant-time check on the declared
/// extent, so it never exercises the streamed bound at the production
/// boundary. A file that declares `A1:B2` and holds a cell at
/// `XFD1048576` defeats a declared-only check entirely, and that is the
/// path this drives.
#[test]
fn read_workbook_rows_refuses_a_sheet_that_under_declares_its_extent() {
    use std::io::Write;

    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&crate::workbook::fixtures::a_sheet_that_under_declares_its_extent())
        .unwrap();
    file.flush().unwrap();

    let started = std::time::Instant::now();
    let error = read_workbook_rows(file.as_file(), "lying.xlsx", None)
        .expect_err("the declared extent is not evidence about the cells");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "slow enough to have allocated the box"
    );
    assert!(
        error.contains("actual extent"),
        "must be refused on the streamed cells, not the header it lied in: {error}"
    );
}

/// A chart-first workbook reads as empty at the production boundary.
///
/// This is the shipped path the regression was about: `sheet_names()`
/// lists chartsheets, so a workbook whose first sheet is a chart is the one
/// `read_workbook_rows` picks when no sheet is named. Propagating
/// calamine's `NotAWorksheet` made `workbook-open` and `workbook-summary`
/// fail outright where they used to yield zero rows.
#[test]
fn read_workbook_rows_reads_a_chart_first_workbook_as_empty() {
    use std::io::Write;

    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&crate::workbook::fixtures::chartsheet())
        .unwrap();
    file.flush().unwrap();

    let rows = read_workbook_rows(file.as_file(), "charts.xlsx", None)
        .expect("a chart sheet must not fail the read");
    assert!(rows.is_empty(), "{rows:?}");
}

/// A number too big or small for a decimal must not become one.
///
/// The whole-float arm cast to i64 unchecked, so 1e300 rendered as
/// 9223372036854775807 -- and simply removing the cast would have put a
/// 301-character cell in the grid and the TUI preview instead. A subnormal
/// already did exactly that: 5e-324 expanded to 326 characters.
#[test]
fn test_extreme_floats_use_exponent_notation_rather_than_expanding() {
    use calamine::Data;

    let float = |value: f64| workbook_cell_to_string(&Data::Float(value));

    assert_eq!(float(1.0e300), "1e300");
    assert_eq!(float(5.0e-324), "5e-324");
    assert!(
        float(1.0e300).len() < 10,
        "expanded a huge float into the grid"
    );
    // Ordinary spreadsheet numbers are untouched.
    assert_eq!(float(42.5), "42.5");
    assert_eq!(float(42.0), "42");
    assert_eq!(float(0.0), "0");
    assert_eq!(float(-7.0), "-7");
}

/// No path through this function can expand a number into the grid.
///
/// Asserted as a property over every arm, not as three examples, because
/// four consecutive review rounds each found a fix applied to one arm and
/// not its twin. The decimal-expansion blowup was removed from
/// `Data::Float` and left on the out-of-range `Data::DateTime` fallback
/// three arms above it -- same function, same match, same 301 characters,
/// reachable from the same `<v>` element and differing only in whether the
/// cell carried a date format.
#[test]
fn test_no_cell_renders_as_an_unbounded_decimal() {
    use calamine::{Data, ExcelDateTime, ExcelDateTimeType};

    let extremes = [
        1.0e300,
        -1.0e300,
        f64::MAX,
        f64::MIN,
        5.0e-324,
        -5.0e-324,
        1.0e12,
        -1.0e12,
    ];
    let mut cells: Vec<Data> = extremes.iter().map(|value| Data::Float(*value)).collect();
    for value in extremes {
        for kind in [ExcelDateTimeType::DateTime, ExcelDateTimeType::TimeDelta] {
            for epoch in [false, true] {
                cells.push(Data::DateTime(ExcelDateTime::new(value, kind, epoch)));
            }
        }
    }

    for cell in cells {
        let rendered = workbook_cell_to_string(&cell);
        assert!(
            rendered.len() <= 32,
            "{cell:?} rendered {} characters: {rendered}",
            rendered.len()
        );
    }
}

/// A crafted workbook must not take the process down.
///
/// `ExcelDateTime::as_duration` and `as_datetime` both compute
/// `Duration::milliseconds(ms.round() as i64)` before any `Option`, and a
/// float-to-int cast saturates -- so a serial past ~1.07e11, or an
/// infinity, which `<v>` parses happily, reaches chrono as `i64::MIN` and
/// panics. A `<v>-1e12</v>` cell in an otherwise ordinary `.xlsx` crashed
/// both `read_workbook_rows` and the TUI preview, and Finch has no
/// `catch_unwind`. This defect was introduced by this PR; `origin/main`
/// rendered the arm with a `to_string()` catch-all.
#[test]
fn test_an_out_of_range_serial_falls_back_instead_of_panicking() {
    use calamine::{Data, ExcelDateTime, ExcelDateTimeType};

    for kind in [ExcelDateTimeType::TimeDelta, ExcelDateTimeType::DateTime] {
        for serial in [-1.0e12, 1.0e12, f64::NEG_INFINITY, f64::INFINITY, f64::NAN] {
            let rendered =
                workbook_cell_to_string(&Data::DateTime(ExcelDateTime::new(serial, kind, false)));
            assert!(
                !rendered.is_empty(),
                "serial {serial} rendered nothing for {kind:?}"
            );
        }
    }
    // And a large-but-sane duration still renders, so the guard is not
    // refusing anything legitimate.
    assert_eq!(
        workbook_cell_to_string(&Data::DateTime(ExcelDateTime::new(
            1.0e9,
            ExcelDateTimeType::TimeDelta,
            false
        ))),
        "24000000000:00:00"
    );
}

/// The ISO duration parser must carry, keep the sign, and refuse the rest.
///
/// Its first version printed components verbatim, so the legal `PT90M` came
/// out as "0:90:00" -- an impossible clock reading beside the "1:30:00" an
/// XLSX cell of the same value gives, which falsifies the whole claim that
/// one logical cell reads the same whichever format it arrived in. It also
/// dropped the leading sign that `xs:duration` spells out, truncated
/// `PT1.5H` to "1:00:00", and accepted `PT`, `PT1M1M` and `PT30S45M13H` --
/// producing plausible answers where the documented behaviour is
/// passthrough.
#[test]
fn test_iso_durations_carry_keep_the_sign_and_refuse_what_they_cannot_read() {
    use calamine::Data;

    let iso = |value: &str| workbook_cell_to_string(&Data::DurationIso(value.into()));

    assert_eq!(iso("PT13H45M30S"), "13:45:30");
    // Carries, rather than printing an impossible clock.
    assert_eq!(iso("PT90M"), "1:30:00");
    assert_eq!(iso("PT13H45M75S"), "13:46:15");
    // The sign is part of the value here too.
    assert_eq!(iso("-PT01H00M00S"), "-1:00:00");
    // Strict `xs:duration` allows a fraction only on seconds; accepting it
    // on hours and minutes too is deliberate leniency, and better than the
    // silent truncation to "1:00:00" it replaces.
    assert_eq!(iso("PT1.5H"), "1:30:00");
    assert_eq!(iso("PT0.5H"), "0:30:00");
    // Rounding to zero drops the sign: "-0:00:00" is not a reading anyone
    // wants.
    assert_eq!(iso("-PT0.4S"), "0:00:00");
    assert_eq!(iso("-PT1H"), "-1:00:00");

    // Everything below is returned untouched, which is the contract.
    for unrecognised in [
        "PT",                      // no component at all
        "PT1M1M",                  // repeated designator
        "PT30S45M13H",             // descending order violated
        "PT99999999999999999999H", // would print a saturated i64
        "PT.5S",                   // no digit before the point
        "P1DT2H",                  // a day component this does not render
        "PTS",
        "pt1h",
        "",
    ] {
        assert_eq!(iso(unrecognised), unrecognised, "mangled {unrecognised:?}");
    }
}

/// An ISO datetime must normalise the way a serial one does.
#[test]
fn test_iso_datetimes_keep_their_offset_and_agree_at_midnight() {
    use calamine::Data;

    let iso = |value: &str| workbook_cell_to_string(&Data::DateTimeIso(value.into()));

    assert_eq!(iso("2026-09-02T13:45:30"), "2026-09-02 13:45:30");
    // `office:date-value` is `xs:dateTime`, which permits a timezone.
    // Preserved rather than dropped -- losing it silently changes what the
    // cell means -- but the `T` still goes, which is the artefact #281 is
    // about. A trailing `Z` is rewritten to an explicit offset before
    // either parser runs, in both letter cases.
    assert_eq!(iso("2026-09-02T13:45:30Z"), "2026-09-02 13:45:30+00:00");
    assert_eq!(
        iso("2026-09-02T13:45:30+01:00"),
        "2026-09-02 13:45:30+01:00"
    );
    // Minute precision with a bare `Z` too: RFC 3339 requires seconds, so
    // that one form slipped past the fallback the comment claimed covered
    // it.
    assert_eq!(iso("2026-09-02T13:45Z"), "2026-09-02 13:45:00+00:00");
    // Both letter cases, because `parse_from_rfc3339` accepts a lowercase
    // `z` -- so stripping only the uppercase one made second precision and
    // minute precision disagree, an asymmetry that did not exist before the
    // rewrite.
    assert_eq!(iso("2026-09-02T13:45z"), "2026-09-02 13:45:00+00:00");
    assert_eq!(iso("2026-09-02T13:45:30z"), "2026-09-02 13:45:30+00:00");
    // The separator's case too. Fixing only the offset letter left the
    // identical asymmetry one line below it -- `parse_from_rfc3339` accepts
    // a lowercase `t`, the fallback formats listed only `T`, so
    // "2026-09-02t13:45Z" went untouched while "2026-09-02t13:45:30Z"
    // normalised. Five review rounds have now found a fix applied to one
    // side of a pair and not the other, so both are asserted together.
    assert_eq!(iso("2026-09-02t13:45:30Z"), "2026-09-02 13:45:30+00:00");
    assert_eq!(iso("2026-09-02t13:45Z"), "2026-09-02 13:45:00+00:00");
    assert_eq!(iso("2026-09-02t13:45+01:00"), "2026-09-02 13:45:00+01:00");
    assert_eq!(iso("2026-09-02t00:00:00"), "2026-09-02");
    // Midnight drops its time when there is no offset to strand, as the
    // serial path does.
    assert_eq!(iso("2026-09-02T00:00:00"), "2026-09-02");
    // But not when an offset is present. Dropping the time glued the
    // offset onto the date and produced "2026-09-02-05:00", which parses
    // as nothing and reads as a date with garbage after it -- the exact
    // intersection of the two rules this function added.
    assert_eq!(
        iso("2026-09-02T00:00:00-05:00"),
        "2026-09-02 00:00:00-05:00"
    );
    assert_eq!(iso("2026-09-02T00:00:00Z"), "2026-09-02 00:00:00+00:00");
    assert_eq!(iso("2026-09-02"), "2026-09-02");
    assert_eq!(iso("not a date"), "not a date");
}

/// Elapsed time must not become a date in 1900.
///
/// A `[h]:mm:ss` cell holding 25 hours has serial 1.0416..., which looks
/// exactly like a date's -- so choosing the shape from the serial alone
/// rendered it "1900-01-01 01:00:00". That is the Excel epoch leaking into
/// the user's output, which is the failure this whole function exists to
/// remove, reintroduced by the first fix for it. Timesheets and run
/// durations are the ordinary reason to use `[h]:mm` at all.
#[test]
fn test_duration_cells_render_as_elapsed_time_not_as_dates() {
    use calamine::{Data, ExcelDateTime, ExcelDateTimeType};

    let duration = |serial| {
        workbook_cell_to_string(&Data::DateTime(ExcelDateTime::new(
            serial,
            ExcelDateTimeType::TimeDelta,
            false,
        )))
    };
    assert_eq!(duration(1.0416666666), "25:00:00");
    assert_eq!(duration(2.0), "48:00:00");
    assert_eq!(duration(0.5), "12:00:00");
    // The sign is part of the value. Treating a negative serial as a time
    // of day dropped it, so -36 hours read as "12:00:00".
    assert_eq!(duration(-1.5), "-36:00:00");
}

/// A negative serial is not a time of day.
#[test]
fn test_a_negative_serial_keeps_its_magnitude() {
    use calamine::{Data, ExcelDateTime, ExcelDateTimeType};

    let rendered = workbook_cell_to_string(&Data::DateTime(ExcelDateTime::new(
        -1.5,
        ExcelDateTimeType::DateTime,
        false,
    )));
    assert_eq!(rendered, "1899-12-29 12:00:00");
    assert_ne!(
        rendered, "12:00:00",
        "dropped the sign and the date with it"
    );
}

/// Out of chrono's range, the serial is a worse answer than a date and a
/// better one than silence.
#[test]
fn test_an_unrepresentable_serial_falls_back_to_the_number() {
    use calamine::{Data, ExcelDateTime, ExcelDateTimeType};

    assert_eq!(
        workbook_cell_to_string(&Data::DateTime(ExcelDateTime::new(
            1e9,
            ExcelDateTimeType::DateTime,
            false
        ))),
        "1000000000"
    );
}

/// The same logical cell must read the same whichever format it came from.
///
/// ODS carries ISO-8601 text rather than a serial, so a time arrived as
/// "PT13H45M30S" -- an encoding artefact of exactly the kind #281 is about,
/// beside an XLSX cell of the same value reading "13:45:30".
#[test]
fn test_iso_cells_normalize_to_the_same_shapes_as_serial_cells() {
    use calamine::Data;

    assert_eq!(
        workbook_cell_to_string(&Data::DateTimeIso("2026-09-02T13:45:30".into())),
        "2026-09-02 13:45:30"
    );
    assert_eq!(
        workbook_cell_to_string(&Data::DateTimeIso("2026-09-02".into())),
        "2026-09-02"
    );
    assert_eq!(
        workbook_cell_to_string(&Data::DurationIso("PT13H45M30S".into())),
        "13:45:30"
    );
    // Anything the normalisers do not recognise passes through rather than
    // being mangled or lost: a day component is legal ISO-8601 and is not
    // a shape this renders.
    assert_eq!(
        workbook_cell_to_string(&Data::DurationIso("P1DT2H".into())),
        "P1DT2H"
    );
    assert_eq!(
        workbook_cell_to_string(&Data::DateTimeIso("not a date".into())),
        "not a date"
    );
}

/// A date cell must read as a date, not as the serial underneath it.
///
/// `Data::DateTime`'s `Display` prints the raw f64, and
/// `workbook_cell_to_string`'s catch-all fell through to it -- so every
/// date in every spreadsheet Finch read came back as an opaque number.
/// 2026-09-02 was "46267", through `workbook-open`, `workbook-range` and
/// `workbook-summary` alike (#281).
///
/// The three shapes are asserted separately because the first fix rendered
/// them all the same way, which produced "2026-09-02 00:00:00" for a date
/// and "1899-12-31 13:45:30" for a bare time -- the second leaking the
/// Excel epoch into the user's output.
#[test]
fn read_workbook_rows_renders_dates_times_and_datetimes_as_text() {
    use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};

    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("Dates").unwrap();
    let date_format = Format::new().set_num_format("yyyy-mm-dd");
    let datetime_format = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    let time_format = Format::new().set_num_format("hh:mm:ss");
    sheet
        .write_datetime_with_format(
            0,
            0,
            &ExcelDateTime::from_ymd(2026, 9, 2).unwrap(),
            &date_format,
        )
        .unwrap();
    sheet
        .write_datetime_with_format(
            0,
            1,
            &ExcelDateTime::from_ymd(2026, 9, 2)
                .unwrap()
                .and_hms(13, 45, 30)
                .unwrap(),
            &datetime_format,
        )
        .unwrap();
    sheet
        .write_datetime_with_format(
            0,
            2,
            &ExcelDateTime::from_hms(13, 45, 30).unwrap(),
            &time_format,
        )
        .unwrap();
    // A plain number must keep reading as a number: the fix must not
    // reinterpret every float as a date.
    sheet.write_number(0, 3, 42.5).unwrap();
    sheet.write_string(0, 4, "plain").unwrap();

    // `tempfile::tempdir`, not a fixed name in the shared temp dir: two
    // concurrent `cargo test` runs on one machine would collide on it, and
    // one run's cleanup could delete the other's fixture mid-read.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dates.xlsx");
    workbook.save(&path).unwrap();
    let file = std::fs::File::open(&path).unwrap();

    let rows = read_workbook_rows(&file, "dates.xlsx", None).expect("must read");
    assert_eq!(
        rows,
        vec![vec![
            "2026-09-02".to_string(),
            "2026-09-02 13:45:30".to_string(),
            "13:45:30".to_string(),
            "42.5".to_string(),
            "plain".to_string(),
        ]],
        "a date read as its Excel serial, or a time carried the 1899 epoch"
    );
}

/// And an ordinary workbook still comes back whole.
#[test]
fn read_workbook_rows_still_reads_an_ordinary_sheet() {
    use std::io::Write;

    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&crate::workbook::fixtures::xlsx(
        "A1:B2",
        &[
            ("A1", "one"),
            ("B1", "two"),
            ("A2", "three"),
            ("B2", "four"),
        ],
    ))
    .unwrap();
    file.flush().unwrap();

    let rows = read_workbook_rows(file.as_file(), "ordinary.xlsx", None).expect("must read");
    assert_eq!(
        rows,
        vec![
            vec!["one".to_string(), "two".to_string()],
            vec!["three".to_string(), "four".to_string()],
        ]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn typed_mcp_call_uses_concrete_authority_and_managed_json() {
    let script = r#"
IFS= read -r discover
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"1"}}}}'
IFS= read -r list
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo_value","description":"untrusted fixture prose","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]},"outputSchema":{"type":"object","properties":{"ok":{"type":"boolean"},"typed":{"type":"boolean"},"forth":{"type":"boolean"}}}}]}}'
IFS= read -r call
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"fixture result"}],"structuredContent":{"ok":true},"isError":false}}'
IFS= read -r typed_call
printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"typed fixture result"}],"structuredContent":{"typed":true},"isError":false}}'
IFS= read -r forth_call
printf '%s\n' '{"jsonrpc":"2.0","id":5,"result":{"content":[{"type":"text","text":"forth fixture result"}],"structuredContent":{"forth":true},"isError":false}}'
"#;
    let config = std::collections::HashMap::from([(
        "fixture".to_string(),
        crate::tools::McpServerConfig {
            command: Some("sh".to_string()),
            args: vec!["-c".to_string(), script.to_string()],
            transport: crate::tools::TransportType::Stdio,
            url: None,
            env: std::collections::HashMap::new(),
            enabled: true,
            timeout_secs: 5,
        },
    )]);
    let client = Arc::new(crate::tools::McpClient::from_config(&config).await.unwrap());
    let runtime = ProgramRuntime::new();
    assert!(!runtime.has_mcp_client());
    assert!(runtime.bind_mcp_client(client).await.unwrap().is_empty());
    assert!(runtime.has_mcp_client());
    let state = runtime.inspect().await.unwrap();
    let namespaced = state
        .typed_vocabulary
        .iter()
        .find(|entry| entry.name == "mcp.fixture.echo_value")
        .expect("discovered MCP word");
    assert!(namespaced
        .signature
        .as_deref()
        .unwrap()
        .contains("record{value:string}"));
    assert!(namespaced
        .version
        .as_deref()
        .unwrap()
        .starts_with("sha256:"));
    assert!(namespaced
        .documentation
        .as_deref()
        .unwrap()
        .contains("Untrusted server description"));
    let requirement = CapabilityRequirement {
        capability: CapabilityKind::McpCall,
        selector: ResourceSelector::Mcp {
            server: "fixture".into(),
            tool: "echo_value".into(),
        },
    };
    runtime.grant_typed_capability(requirement).unwrap();

    let mut generic = submission(
        ProgramLanguage::Lisp,
        r#"(mcp-call "fixture" "echo_value" (result-unwrap (json-parse "{\"value\":\"hello\"}")))"#,
        ExecutionEffect::ExternalRead,
    );
    generic.manifest_generation = runtime.manifest_generation();
    let outcome = runtime.submit(generic).await.unwrap();

    assert_eq!(
        outcome.status,
        ExecutionStatus::Completed,
        "diagnostics: {:?}",
        outcome.diagnostics
    );
    assert!(matches!(
        outcome.values.as_slice(),
        [ProgramValue::Json(value)]
            if value["structuredContent"]["ok"] == serde_json::Value::Bool(true)
    ));

    let mut typed = submission(
        ProgramLanguage::Lisp,
        r#"(mcp.fixture.echo_value { :value "typed" })"#,
        ExecutionEffect::ExternalRead,
    );
    typed.manifest_generation = runtime.manifest_generation();
    let outcome = runtime.submit(typed).await.unwrap();
    assert_eq!(
        outcome.status,
        ExecutionStatus::Completed,
        "diagnostics: {:?}",
        outcome.diagnostics
    );
    assert!(
        matches!(
            outcome.values.last(),
            Some(ProgramValue::Json(value))
                if value["structuredContent"]["typed"] == serde_json::Value::Bool(true)
        ),
        "values: {:?}",
        outcome.values
    );

    let mut forth = submission(
        ProgramLanguage::Forth,
        r#"{ value: "forth" } mcp.fixture.echo_value"#,
        ExecutionEffect::ExternalRead,
    );
    forth.manifest_generation = runtime.manifest_generation();
    let outcome = runtime.submit(forth).await.unwrap();
    assert_eq!(
        outcome.status,
        ExecutionStatus::Completed,
        "diagnostics: {:?}",
        outcome.diagnostics
    );
    assert!(matches!(
        outcome.values.last(),
        Some(ProgramValue::Json(value))
            if value["structuredContent"]["forth"] == serde_json::Value::Bool(true)
    ));
}

#[tokio::test]
async fn inspection_exposes_typed_stack_vocabulary_and_grants() {
    let runtime = ProgramRuntime::new();
    runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(+ 20 22)",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    let state = runtime.inspect().await.unwrap();
    assert_eq!(state.typed_stack.len(), 1);
    assert_eq!(state.typed_stack[0].value, TypedValue::Int(42));
    assert!(state.typed_vocabulary.iter().any(|word| word.name == "say"));
    assert!(state
        .granted_capabilities
        .iter()
        .any(|grant| grant.capability == crate::vm::CapabilityKind::SessionEmit));
}

#[tokio::test]
async fn inspection_exposes_typed_definition_documentation() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(define (double (n : int)) : int \"Return twice n.\" (* n 2))",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);

    let state = runtime.inspect().await.unwrap();
    let double = state
        .typed_vocabulary
        .iter()
        .find(|word| word.name == "double")
        .expect("persisted typed definition");
    assert_eq!(double.documentation.as_deref(), Some("Return twice n."));
}

#[tokio::test]
async fn compiler_context_projects_promoted_functions_into_program_corpus_contract() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(define (double (n : int)) : int (* n 2))",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(
        outcome.status,
        ExecutionStatus::Completed,
        "the definition must commit before compiler context is captured; diagnostics={:?}",
        outcome.diagnostics
    );

    let context = runtime.compiler_context().unwrap();
    assert_eq!(context.manifest_generation, runtime.manifest_generation());
    assert_eq!(context.revision, runtime.revision());
    assert!(
        context.functions.contains_key("double"),
        "the runtime projection must carry promoted definitions for source-only corpus replay; functions={:?}",
        context.functions.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn typed_vm_can_introspect_its_vocabulary() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(vm-vocabulary)",
            ExecutionEffect::VmRead,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);
    let Some(ProgramValue::String(manifest)) = outcome.values.first() else {
        panic!("expected serialized vocabulary");
    };
    assert!(manifest.contains("vm-vocabulary"));
    assert!(manifest.contains("file-read"));
}

/// Installs the snapshot-write hook for one executable and hands back a
/// guard that clears it, so a panicking test cannot leave a hook armed for
/// the rest of the process.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
struct SnapshotWriteHookGuard(String);

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
impl Drop for SnapshotWriteHookGuard {
    fn drop(&mut self) {
        SNAPSHOT_WRITE_HOOK
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Copies `/usr/bin/printf` somewhere private and returns its canonical
/// path. Each test keys its hook on its own copy: the hook is global and
/// matched by path, so sharing `/usr/bin/printf` would fire one test's
/// callback inside another's `process-run`.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn private_printf(directory: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let executable = directory.join("tool");
    std::fs::copy("/usr/bin/printf", &executable).expect("copy printf");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("make the copy executable");
    std::fs::canonicalize(executable).expect("canonicalize the copy")
}

/// #287: an exec the kernel refuses with `ETXTBSY` is retried, not
/// reported to the caller as a failure.
///
/// Every `process-run` writes a private snapshot of the authorized
/// executable and then execs it, and the kernel refuses to exec a file any
/// process holds open for writing. `fork()` copies the whole descriptor
/// table, so a second `process-run` forking anywhere inside the first
/// one's write window hands its child an inherited write descriptor and
/// the first one's exec fails. In CI this surfaced as
/// `approved_typed_process_runs_without_a_shell` failing intermittently on
/// ubuntu with "Text file busy (os error 26)" -- and, on one commit,
/// failing on attempt 1 and passing on attempt 2 of the same run.
///
/// Reproducing it does not need a second process. The kernel's check is
/// per-inode, so a second write descriptor opened here blocks the exec
/// exactly the same way, on demand rather than at CI's whim.
///
/// Before the fix this fails at the status assertion: the first refusal is
/// returned to the caller verbatim.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn process_run_retries_a_transient_text_file_busy() {
    let directory = tempfile::tempdir().unwrap();
    let executable = private_printf(directory.path());
    let key = executable.to_string_lossy().into_owned();
    let _guard = SnapshotWriteHookGuard(key.clone());

    // Every writer is kept, not just the newest. One approved execution
    // calls `open_process_executable` more than once -- validation, grant
    // normalization, then the exec -- and each call snapshots to a fresh
    // temporary file. Holding only the latest descriptor leaves whichever
    // snapshot is actually exec'd unblocked whenever another call follows
    // it, which is how the first CI run reached `Completed` with a
    // descriptor supposedly held throughout.
    // One slot per firing, not one shared vector. Every firing spawns its
    // own releaser with its own deadline; if they all cleared the same
    // vector, an early firing timing out could drop the exec-time writer
    // and the test would fail at the refusal guard blaming the hook for
    // not holding long enough.
    type Slot = Arc<Mutex<Option<std::fs::File>>>;
    let held: Arc<Mutex<Vec<Slot>>> = Arc::new(Mutex::new(Vec::new()));
    let installer = Arc::clone(&held);
    let refused = key.clone();
    SNAPSHOT_WRITE_HOOK
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            key.clone(),
            Box::new(move |snapshot: &Path| {
                let writer = std::fs::OpenOptions::new()
                    .write(true)
                    .open(snapshot)
                    .expect("open the snapshot for writing while it is still named");
                let slot: Slot = Arc::new(Mutex::new(Some(writer)));
                installer.lock().unwrap().push(Arc::clone(&slot));

                // Release once the exec has actually been refused, rather than
                // after a fixed delay. A timer would make the test vacuous on
                // a loaded runner: if the work before the spawn outlasts it the
                // descriptor is gone before the first exec, everything below
                // still passes, and nothing was retried.
                let watched = refused.clone();
                std::thread::spawn(move || {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                    while text_file_busy_refusals(&watched) == 0
                        && std::time::Instant::now() < deadline
                    {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    slot.lock().unwrap().take();
                });
            }),
        );

    let source = format!(
        "(process-run {} (list \"ok\"))",
        serde_json::to_string(&executable.to_string_lossy()).unwrap()
    );
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            &source,
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(
        pending.status,
        ExecutionStatus::AuthorizationRequired,
        "an unapproved typed process must stop at its capability boundary; outcome={pending:#?}"
    );
    let completed = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowOnce,
            "test-user",
        )
        .await
        .unwrap();

    let retries = text_file_busy_refusals(&key);
    assert!(
        retries > 0,
        "the blocking descriptor was released before the exec was ever \
             refused, so this run proves nothing about the retry; make the \
             hook hold it longer. retries={retries}; outcome={completed:#?}"
    );
    assert_eq!(
        completed.status,
        ExecutionStatus::Completed,
        "a transient `Text file busy` must be retried rather than \
             surfaced as a failed execution (#287); it was refused {retries} \
             time(s); outcome={completed:#?}"
    );
    assert_eq!(completed.values, vec![ProgramValue::String("ok".into())]);
    assert!(
        held.lock()
            .unwrap()
            .iter()
            .all(|slot| slot.lock().unwrap().is_none()),
        "every writer the hook opened must have been released"
    );
}

/// A descriptor that never closes must fail, bounded, with the kernel's
/// own diagnosis -- not spin until the caller gives up.
///
/// This is the other half of the retry: it must not turn a permanent
/// condition into a hang, and the message must still say what happened.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn process_run_reports_a_permanent_text_file_busy_without_hanging() {
    let directory = tempfile::tempdir().unwrap();
    let executable = private_printf(directory.path());
    let key = executable.to_string_lossy().into_owned();
    let _guard = SnapshotWriteHookGuard(key.clone());

    // Keep every writer, for the same reason as the transient test: one
    // approved execution snapshots more than once, so the descriptor that
    // matters is not necessarily the newest.
    let held: Arc<Mutex<Vec<std::fs::File>>> = Arc::new(Mutex::new(Vec::new()));
    let installer = Arc::clone(&held);
    SNAPSHOT_WRITE_HOOK
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            key.clone(),
            Box::new(move |snapshot: &Path| {
                let writer = std::fs::OpenOptions::new()
                    .write(true)
                    .open(snapshot)
                    .expect("open the snapshot for writing");
                installer.lock().unwrap().push(writer);
            }),
        );

    let source = format!(
        "(process-run {} (list \"ok\"))",
        serde_json::to_string(&executable.to_string_lossy()).unwrap()
    );
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            &source,
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);

    let started = std::time::Instant::now();
    let outcome = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowOnce,
            "test-user",
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(
        outcome.status,
        ExecutionStatus::Failed,
        "a descriptor held open for writing throughout must fail the \
             execution, not succeed; outcome={outcome:#?}"
    );
    assert!(
        outcome
            .diagnostics
            .iter()
            .any(|message| message.contains("Text file busy")),
        "the failure must still name the kernel's condition after the \
             retries are spent; diagnostics={:#?}; outcome={outcome:#?}",
        outcome.diagnostics
    );
    let refusals = text_file_busy_refusals(&key);
    assert_eq!(
        refusals, TEXT_FILE_BUSY_ATTEMPTS as usize,
        "a permanently blocked exec must spend the whole budget and then \
             stop; it was refused {refusals} time(s) against a budget of {}",
        TEXT_FILE_BUSY_ATTEMPTS
    );
    // That comparison alone is tautological: every attempt is refused
    // here, so the count mirrors the constant whatever the constant is,
    // and this test passed unchanged with the budget set to 1. It catches
    // an off-by-one in the loop bounds and nothing more. What binds this
    // test to the budget's actual value is the pair below.
    assert_eq!(
        TEXT_FILE_BUSY_ATTEMPTS, 8,
        "this pin, not the elapsed bound below, is what catches a changed \
             budget; the bound is derived from this constant and moves with \
             it"
    );
    // What this catches, precisely: a loop that stops sleeping. Deleting
    // the `sleep` from the retry arm makes a fully refused exec return in
    // ~86ms against this 140ms bound, and CI run 33960900348 confirms it
    // fails there. It does NOT catch a reduced budget -- `backoff` is
    // derived from `TEXT_FILE_BUSY_ATTEMPTS`, so a smaller budget shrinks
    // this bound in lockstep, and at a budget of 1 it degrades to
    // `elapsed >= 0`. The pin above is what catches that.
    //
    // The margin is thinner than it looks. That ~86ms is ambient work in
    // `resolve_typed_approval`, not backoff, so only ~54ms of the bound is
    // actually doing discriminating work. It can never fail spuriously --
    // noise only pushes `elapsed` up -- but on a slow enough runner, or if
    // approval grows another `open_process_executable` call, it would stop
    // detecting the mutation silently.
    let backoff: std::time::Duration = (1..TEXT_FILE_BUSY_ATTEMPTS)
        .map(|attempt| TEXT_FILE_BUSY_BACKOFF * attempt)
        .sum();
    // Pins `TEXT_FILE_BUSY_BACKOFF` as well as the attempt count. Without
    // it, halving the backoff fails nothing here and quietly stales the
    // 140ms in the production doc comment -- a number this branch has
    // already had to correct once.
    assert_eq!(
        backoff,
        std::time::Duration::from_millis(140),
        "the documented backoff is 140ms; the constants now sum to \
             {backoff:?}, so the doc comment on TEXT_FILE_BUSY_ATTEMPTS and \
             this test both need revisiting"
    );
    assert!(
        elapsed >= backoff,
        "a fully refused exec must actually spend its backoff; the loop \
             is documented to wait {backoff:?} but gave up after {elapsed:?}"
    );
    // Deliberately loose. The lower bound above is what ties this test to
    // the budget; this one only says "not a hang", and tightening it buys
    // little against a runner executing ~2950 tests in parallel.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the retry budget must stay bounded; giving up took {elapsed:?}"
    );

    // #287 introduces a second way for an approved `process-run` to fail
    // after its approval resolved, so it needs the invariant
    // `kernel_rejected_launch_does_not_consume_or_audit_once_grant`
    // protects: a launch the kernel refused is not a use of the authority.
    let ledger = runtime.capability_ledger().unwrap();
    let once = ledger
        .grants
        .grants
        .last()
        .expect("once grant remains visible");
    assert!(
        matches!(once.scope, GrantScope::Once { .. }),
        "the grant under test must be the once grant; grant={once:#?}"
    );
    assert!(
        once.consumed_at_unix_ms.is_none(),
        "an exec the kernel never performed must not consume the once \
             grant; grant={once:#?}"
    );
    assert!(
        ledger.authorization_audit.is_empty(),
        "a refused exec must not audit a host use; audit={:#?}",
        ledger.authorization_audit
    );
    assert!(
        !ledger
            .audit
            .iter()
            .any(|entry| entry.action == crate::vm::CapabilityAuditAction::Consumed),
        "a refused exec must not record consumption; audit={:#?}",
        ledger.audit
    );
    held.lock().unwrap().clear();
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn approved_typed_process_runs_without_a_shell() {
    let runtime = ProgramRuntime::new();
    let request = submission(
        ProgramLanguage::Lisp,
        "(process-run \"/usr/bin/printf\" (list \"ok\"))",
        ExecutionEffect::ExternalWrite,
    );
    let pending = runtime.submit(request.clone()).await.unwrap();
    assert_eq!(
        pending.status,
        ExecutionStatus::AuthorizationRequired,
        "an unapproved typed process must stop at its capability boundary; outcome={pending:#?}"
    );
    let ResourceSelector::Process { executables } = &pending.required_capabilities[0].selector
    else {
        panic!("process approval must expose a stable executable identity");
    };
    let approved_identity = ProcessExecutableIdentity::decode(&executables[0]).unwrap();
    assert_eq!(approved_identity.path, "/usr/bin/printf");
    runtime
        .grant_typed_capability(pending.required_capabilities[0].clone())
        .unwrap();
    let approved = runtime.submit(request).await.unwrap();
    assert_eq!(
            approved.status,
            ExecutionStatus::Completed,
            "the approved typed process must execute the authorized object without a shell; approved_identity={approved_identity:?}; outcome={approved:#?}"
        );
    assert_eq!(approved.values, vec![ProgramValue::String("ok".into())]);
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn process_run_rejects_bare_and_relative_commands_without_consulting_path() {
    let runtime = ProgramRuntime::new();
    for command in ["printf", "./printf"] {
        let source = format!(
            "(process-run {} (list \"unused\"))",
            serde_json::to_string(command).unwrap()
        );
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                &source,
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Failed);
        assert!(outcome.required_capabilities.is_empty());
        assert!(outcome
            .diagnostics
            .iter()
            .any(|message| message.contains("PATH and relative lookup are forbidden")));
    }
    assert!(runtime
        .capability_ledger()
        .unwrap()
        .authorization_audit
        .is_empty());
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn process_run_rejects_symlinks_even_after_retargeting() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let link = directory.path().join("tool");
    symlink("/usr/bin/true", &link).unwrap();
    let source = format!(
        "(process-run {} (list \"unused\"))",
        serde_json::to_string(&link.to_string_lossy()).unwrap()
    );
    let runtime = ProgramRuntime::new();
    let first = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            &source,
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(first.status, ExecutionStatus::Failed);
    std::fs::remove_file(&link).unwrap();
    symlink("/usr/bin/false", &link).unwrap();
    let second = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            &source,
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(second.status, ExecutionStatus::Failed);
    assert!(runtime
        .capability_ledger()
        .unwrap()
        .authorization_audit
        .is_empty());
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn replaced_process_executable_does_not_consume_once_grant() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("tool");
    std::fs::copy("/usr/bin/true", &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = std::fs::canonicalize(executable).unwrap();
    let source = format!(
        "(process-run {} (list \"unused\"))",
        serde_json::to_string(&executable.to_string_lossy()).unwrap()
    );
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            &source,
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(
        pending.status,
        ExecutionStatus::AuthorizationRequired,
        "diagnostics: {:?}",
        pending.diagnostics
    );

    std::fs::copy("/usr/bin/false", &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let failed = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowOnce,
            "test-user",
        )
        .await
        .unwrap();
    assert_eq!(failed.status, ExecutionStatus::Failed);
    let ledger = runtime.capability_ledger().unwrap();
    let once = ledger
        .grants
        .grants
        .last()
        .expect("once grant was recorded");
    assert!(matches!(once.scope, GrantScope::Once { .. }));
    assert!(once.consumed_at_unix_ms.is_none());
    assert!(ledger.authorization_audit.is_empty());
    assert!(!ledger
        .audit
        .iter()
        .any(|entry| entry.action == crate::vm::CapabilityAuditAction::Consumed));
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn atomic_path_replacement_executes_the_authorized_open_object() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("tool");
    let replacement = directory.path().join("replacement");
    std::fs::copy("/usr/bin/printf", &executable).unwrap();
    std::fs::copy("/usr/bin/false", &replacement).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = std::fs::canonicalize(executable).unwrap();
    let source = format!(
        "(process-run {} (list \"opened-object\"))",
        serde_json::to_string(&executable.to_string_lossy()).unwrap()
    );
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            &source,
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);

    let expected = executable.to_string_lossy().into_owned();
    *PROCESS_BEFORE_EXEC_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = Some((
        expected,
        Box::new(move || std::fs::rename(replacement, executable).unwrap()),
    ));
    let completed = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowOnce,
            "test-user",
        )
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(
        completed.values,
        vec![ProgramValue::String("opened-object".into())]
    );
    let ledger = runtime.capability_ledger().unwrap();
    assert!(ledger
        .grants
        .grants
        .last()
        .unwrap()
        .consumed_at_unix_ms
        .is_some());
    assert!(matches!(
        ledger
            .authorization_audit
            .last()
            .map(|entry| &entry.decision),
        Some(AuthorizationDecision::Allowed { .. })
    ));
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn same_inode_mutation_cannot_change_the_snapshotted_process_invocation() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("tool");
    std::fs::copy("/usr/bin/printf", &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = std::fs::canonicalize(executable).unwrap();
    let source = format!(
        "(process-run {} (list \"same-inode\"))",
        serde_json::to_string(&executable.to_string_lossy()).unwrap()
    );
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            &source,
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let ResourceSelector::Process { executables } = &pending.required_capabilities[0].selector
    else {
        panic!("process requirement must be concrete")
    };
    let identity = ProcessExecutableIdentity::decode(&executables[0]).unwrap();
    assert_eq!(identity.arguments, vec!["same-inode"]);
    assert_eq!(
        identity.environment_sha256,
        format!("{:x}", Sha256::digest(b""))
    );
    assert_eq!(
        identity.cwd_path,
        std::env::current_dir().unwrap().to_string_lossy()
    );

    let expected_path = executable.to_string_lossy().into_owned();
    let original_inode = std::fs::metadata(&executable).unwrap().ino();
    let mutated = executable.clone();
    *PROCESS_BEFORE_EXEC_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = Some((
        expected_path,
        Box::new(move || {
            std::fs::copy("/usr/bin/false", &mutated).unwrap();
            std::fs::set_permissions(&mutated, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(std::fs::metadata(&mutated).unwrap().ino(), original_inode);
        }),
    ));
    let completed = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowOnce,
            "test-user",
        )
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(
        completed.values,
        vec![ProgramValue::String("same-inode".into())]
    );
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[test]
fn authority_restart_rejects_same_inode_process_mutation() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("tool");
    std::fs::copy("/usr/bin/printf", &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = std::fs::canonicalize(executable).unwrap();
    let original_inode = std::fs::metadata(&executable).unwrap().ino();
    let identity = open_process_executable(&executable.to_string_lossy(), &["approved".into()])
        .unwrap()
        .identity;
    let runtime = ProgramRuntime::new();
    let mut ledger = CapabilityLedger::default();
    ledger
        .issue(
            CapabilityRequirement {
                capability: CapabilityKind::ProcessRun,
                selector: ResourceSelector::Process {
                    executables: vec![identity.encode()],
                },
            },
            GrantScope::Global,
            runtime.capability_policy().unwrap().policy_hash,
            "test-user",
            unix_time_ms(),
            None,
        )
        .unwrap();
    std::fs::copy("/usr/bin/false", &executable).unwrap();
    assert_eq!(
        std::fs::metadata(&executable).unwrap().ino(),
        original_inode
    );
    let error = runtime
        .restore_capability_ledger(ledger)
        .expect_err("restart must revalidate executable bytes");
    assert!(format!("{error:#}").contains("identity changed since approval"));
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn owner_mode_other_execute_does_not_reach_authorization() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if nix::unistd::geteuid().as_raw() == 0 {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("tool");
    std::fs::copy("/usr/bin/true", &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o401)).unwrap();
    let executable = std::fs::canonicalize(executable).unwrap();
    assert_eq!(
        std::fs::metadata(&executable).unwrap().uid(),
        nix::unistd::geteuid().as_raw()
    );
    let source = format!(
        "(process-run {} (list \"unused\"))",
        serde_json::to_string(&executable.to_string_lossy()).unwrap()
    );
    let runtime = ProgramRuntime::new();
    let failed = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            &source,
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(failed.status, ExecutionStatus::Failed);
    assert!(failed
        .diagnostics
        .iter()
        .any(|message| message.contains("not executable by the effective user")));
    let ledger = runtime.capability_ledger().unwrap();
    assert!(ledger.grants.grants.is_empty());
    assert!(ledger.authorization_audit.is_empty());
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn kernel_rejected_launch_does_not_consume_or_audit_once_grant() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("invalid-executable");
    std::fs::write(&executable, b"not an executable image").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = std::fs::canonicalize(executable).unwrap();
    let source = format!(
        "(process-run {} (list \"unused\"))",
        serde_json::to_string(&executable.to_string_lossy()).unwrap()
    );
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            &source,
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);

    let failed = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowOnce,
            "test-user",
        )
        .await
        .unwrap();
    assert_eq!(failed.status, ExecutionStatus::Failed);
    let ledger = runtime.capability_ledger().unwrap();
    let once = ledger
        .grants
        .grants
        .last()
        .expect("once grant remains visible");
    assert!(matches!(once.scope, GrantScope::Once { .. }));
    assert!(once.consumed_at_unix_ms.is_none());
    assert!(ledger.authorization_audit.is_empty());
    assert!(!ledger
        .audit
        .iter()
        .any(|entry| entry.action == crate::vm::CapabilityAuditAction::Consumed));
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn wrong_process_origin_is_rejected_before_once_grant_issuance() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(process-run \"/usr/bin/true\" (list \"unused\"))",
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    {
        let mut pending_runs = runtime.pending_typed.lock().unwrap();
        let saved = pending_runs.get_mut(&pending.execution_id).unwrap();
        let forged = SourceOrigin::generated("legacy-process-run");
        saved.suspension.pending_host_call.as_mut().unwrap().origin = forged.clone();
        saved.suspension.event_journal.last_mut().unwrap().origin = forged;
    }
    let error = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowOnce,
            "test-user",
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("stale or forged"));
    let ledger = runtime.capability_ledger().unwrap();
    assert!(ledger.grants.grants.is_empty());
    assert!(ledger.authorization_audit.is_empty());
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
)))]
#[tokio::test]
async fn process_run_is_unavailable_without_stable_opened_object_execution() {
    let runtime = ProgramRuntime::new();
    let requirement = CapabilityRequirement {
        capability: CapabilityKind::ProcessRun,
        selector: ResourceSelector::Process {
            executables: Vec::new(),
        },
    };
    assert_eq!(
        runtime.capability_availability(&requirement),
        CapabilityAvailability::Unsupported
    );
    let outcome = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            "(process-run \"/usr/bin/true\" (list))",
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Failed);
    assert!(!outcome.diagnostics.is_empty());
    let ledger = runtime.capability_ledger().unwrap();
    assert!(ledger.grants.grants.is_empty());
    assert!(ledger.authorization_audit.is_empty());
}

#[tokio::test]
async fn typed_proposal_open_is_an_explicit_capability_and_returns_edited_artifact_data() {
    let runtime = ProgramRuntime::new();
    let request = submission(
        ProgramLanguage::Lisp,
        "(proposal-open \"python\" \"show an artifact\" \"print('ok')\")",
        ExecutionEffect::ExternalWrite,
    );
    let pending = runtime.submit(request.clone()).await.unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    assert_eq!(
        pending.required_capabilities,
        vec![crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProgramInvoke,
            selector: crate::vm::ResourceSelector::Program {
                languages: vec!["python".into()],
            },
        }]
    );

    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProgramInvoke,
            selector: crate::vm::ResourceSelector::Program {
                languages: vec!["python".into()],
            },
        })
        .unwrap();
    let accepted = runtime.submit(request).await.unwrap();
    assert_eq!(accepted.status, ExecutionStatus::Completed);
    assert_eq!(
        accepted.values,
        vec![ProgramValue::Option(Some(Box::new(ProgramValue::Result {
            ok: true,
            value: Box::new(ProgramValue::String("print('ok')".into())),
        })))]
    );
}

#[tokio::test]
async fn coforth_proposal_open_uses_the_same_typed_host_boundary() {
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProgramInvoke,
            selector: crate::vm::ResourceSelector::Program {
                languages: vec!["forth".into()],
            },
        })
        .unwrap();
    let accepted = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "s\"forth\" s\"show an artifact\" s\"1 2 +\" proposal-open",
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(accepted.status, ExecutionStatus::Completed);
    assert_eq!(
        accepted.values,
        vec![ProgramValue::Option(Some(Box::new(ProgramValue::Result {
            ok: true,
            value: Box::new(ProgramValue::String("1 2 +".into())),
        })))]
    );
}

#[tokio::test]
async fn proposal_open_can_suspend_for_an_external_editor_and_resume_once() {
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProgramInvoke,
            selector: crate::vm::ResourceSelector::Program {
                languages: vec!["python".into()],
            },
        })
        .unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let sink_observed = Arc::clone(&observed);
    let pending = runtime
        .submit_with_deferred_program_effects(
            submission(
                ProgramLanguage::Lisp,
                "(proposal-open \"python\" \"show an artifact\" \"print('original')\")",
                ExecutionEffect::ExternalWrite,
            ),
            Arc::new(move |effect| sink_observed.lock().unwrap().push(effect)),
        )
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::Suspended);
    let event = observed.lock().unwrap().pop().expect("proposal event");
    assert_eq!(event.execution_id, pending.execution_id);
    assert_eq!(
        event.handle(),
        VmEffectHandle {
            execution_id: pending.execution_id,
            sequence: event.effect.sequence,
        }
    );
    assert_eq!(
        event.effect.requirement.capability,
        crate::vm::CapabilityKind::ProgramInvoke
    );
    assert!(matches!(
        event.effect.event,
        crate::vm::HostSideEffect::Request { ref arguments }
            if matches!(arguments.as_slice(), [TypedValue::String(language), ..] if language == "python")
    ));
    let info = runtime
        .pending_typed_execution(pending.execution_id)
        .unwrap()
        .expect("proposal continuation");
    assert_eq!(info.resume_effect_sequence, Some(event.effect.sequence));
    assert!(matches!(
        info.reason,
        PendingTypedReason::AwaitingHostEffect { .. }
    ));
    assert!(matches!(
        pending.effect_journal.last().map(|entry| &entry.state),
        Some(crate::vm::EffectJournalState::AwaitingHostResult)
    ));

    let accepted = runtime
        .resume_typed_execution_with_effect_result(
            pending.execution_id,
            event.effect.sequence,
            vec![TypedValue::Option {
                inner_type: Type::Result(Box::new(Type::String), Box::new(Type::String)),
                value: Some(Box::new(TypedValue::Result {
                    ok_type: Type::String,
                    error_type: Type::String,
                    is_ok: true,
                    value: Box::new(TypedValue::String("print('edited')".into())),
                })),
            }],
        )
        .await
        .unwrap();
    assert_eq!(accepted.status, ExecutionStatus::Completed);
    assert_eq!(
        accepted.values,
        vec![ProgramValue::Option(Some(Box::new(ProgramValue::Result {
            ok: true,
            value: Box::new(ProgramValue::String("print('edited')".into())),
        })))]
    );
    assert!(matches!(
        accepted.effect_journal.last().map(|entry| &entry.state),
        Some(crate::vm::EffectJournalState::Acknowledged { values })
            if matches!(values.as_slice(), [TypedValue::Option { .. }])
    ));
}

#[tokio::test]
async fn portable_effect_channel_round_trips_a_deferred_proposal_resume() {
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProgramInvoke,
            selector: crate::vm::ResourceSelector::Program {
                languages: vec!["python".into()],
            },
        })
        .unwrap();
    let (sink, receiver) = typed_effect_channel();
    let pending = runtime
        .submit_with_deferred_program_effects(
            submission(
                ProgramLanguage::Lisp,
                "(proposal-open \"python\" \"show an artifact\" \"print('original')\")",
                ExecutionEffect::ExternalWrite,
            ),
            sink,
        )
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::Suspended);

    let envelope = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("portable effect envelope");
    assert_eq!(envelope.execution_id, pending.execution_id);
    assert_eq!(
        envelope.effect.requirement.capability,
        crate::vm::CapabilityKind::ProgramInvoke
    );

    let outcome = runtime
        .resume_vm_effect(VmResume {
            execution_id: envelope.execution_id,
            sequence: envelope.effect.sequence,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Option {
                    inner_type: Type::Result(Box::new(Type::String), Box::new(Type::String)),
                    value: Some(Box::new(TypedValue::Result {
                        ok_type: Type::String,
                        error_type: Type::String,
                        is_ok: true,
                        value: Box::new(TypedValue::String("print('edited')".into())),
                    })),
                }],
            },
        })
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert!(matches!(
        outcome.effect_journal.last().map(|entry| &entry.state),
        Some(crate::vm::EffectJournalState::Acknowledged { values })
            if matches!(values.as_slice(), [TypedValue::Option { .. }])
    ));
    assert!(
        receiver.try_recv().is_err(),
        "the VM must not redispatch the effect"
    );
}

#[tokio::test]
async fn portable_host_boundary_can_defer_a_file_read_without_touching_the_host() {
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
    let (sink, receiver) = typed_effect_channel();
    let pending = runtime
        .submit_with_deferred_host_effects(
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"does-not-need-to-exist.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ),
            sink,
        )
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::Suspended);

    let envelope = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("file-read effect envelope");
    assert_eq!(envelope.execution_id, pending.execution_id);
    assert_eq!(
        envelope.effect.requirement.capability,
        crate::vm::CapabilityKind::FileRead
    );
    assert_eq!(envelope.effect.output, vec![Type::Bytes]);

    let completed = runtime
        .resume_vm_effect(VmResume {
            execution_id: envelope.execution_id,
            sequence: envelope.effect.sequence,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Bytes(b"embedder bytes".to_vec())],
            },
        })
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(
        completed.values,
        vec![ProgramValue::Bytes(b"embedder bytes".to_vec())]
    );
}

#[test]
fn deferred_effect_sink_can_reenter_authority_without_deadlock() {
    let runtime = Arc::new(ProgramRuntime::new());
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let worker_runtime = Arc::clone(&runtime);
    let worker = std::thread::spawn(move || {
        let sink_runtime = Arc::clone(&worker_runtime);
        let sink: TypedEffectSink = Arc::new(move |_| {
            let ledger = sink_runtime
                .capability_ledger()
                .expect("deferred host sink can reenter public authority inspection");
            assert_eq!(ledger.authorization_audit.len(), 1);
        });
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = tokio.block_on(worker_runtime.submit_with_deferred_host_effects(
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"host-owned.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ),
            sink,
        ));
        result_tx.send(result).unwrap();
    });

    let pending = result_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("deferred host dispatch must not retain the ledger lock across its sink")
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::Suspended);
    worker.join().unwrap();
}

#[tokio::test]
async fn portable_host_boundary_retains_its_policy_across_multiple_resumes() {
    let runtime = ProgramRuntime::new();
    let grant_id = runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
    let (sink, receiver) = typed_effect_channel();
    let pending = runtime
        .submit_with_deferred_host_effects(
            submission(
                ProgramLanguage::Lisp,
                "(begin (file-read (path \"first.txt\")) (file-read (path \"second.txt\")))",
                ExecutionEffect::WorkspaceRead,
            ),
            sink,
        )
        .await
        .unwrap();
    let first = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("first file-read effect");
    assert_eq!(first.execution_id, pending.execution_id);

    let still_pending = runtime
        .resume_vm_effect(VmResume {
            execution_id: first.execution_id,
            sequence: first.effect.sequence,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Bytes(b"first".to_vec())],
            },
        })
        .await
        .unwrap();
    assert_eq!(still_pending.status, ExecutionStatus::Suspended);
    let second = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("second file-read effect");
    assert_eq!(second.execution_id, first.execution_id);
    assert_eq!(second.effect.sequence, first.effect.sequence + 1);

    let completed = runtime
        .resume_vm_effect(VmResume {
            execution_id: second.execution_id,
            sequence: second.effect.sequence,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Bytes(b"second".to_vec())],
            },
        })
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(
        completed.values,
        vec![ProgramValue::Bytes(b"second".to_vec())]
    );
    let ledger = runtime.capability_ledger().unwrap();
    assert_eq!(ledger.authorization_audit.len(), 2);
    assert_eq!(
        ledger
            .authorization_audit
            .iter()
            .map(|entry| entry.effect_sequence)
            .collect::<Vec<_>>(),
        vec![Some(first.effect.sequence), Some(second.effect.sequence)]
    );
    assert!(ledger.authorization_audit.iter().all(|entry| matches!(
        entry.decision,
        AuthorizationDecision::Allowed { grant_id: used } if used == grant_id
    )));
}

#[tokio::test]
async fn typed_effect_sink_projects_proposal_request() {
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProgramInvoke,
            selector: crate::vm::ResourceSelector::Program {
                languages: vec!["python".into()],
            },
        })
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = Arc::clone(&events);
    let outcome = runtime
        .submit_with_typed_effect_sink(
            submission(
                ProgramLanguage::Lisp,
                "(proposal-open \"python\" \"show an artifact\" \"print('ok')\")",
                ExecutionEffect::ExternalWrite,
            ),
            Arc::new(move |effect| sink_events.lock().unwrap().push(effect)),
        )
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert!(events.lock().unwrap().iter().any(|effect| {
        effect.effect.requirement.capability == crate::vm::CapabilityKind::ProgramInvoke
    }));
}

#[tokio::test]
async fn proposal_grant_cannot_be_reused_for_a_different_artifact_language() {
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProgramInvoke,
            selector: crate::vm::ResourceSelector::Program {
                languages: vec!["python".into()],
            },
        })
        .unwrap();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(proposal-open \"bash\" \"show an artifact\" \"echo nope\")",
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
    assert_eq!(
        outcome.required_capabilities,
        vec![crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProgramInvoke,
            selector: crate::vm::ResourceSelector::Program {
                languages: vec!["bash".into()],
            },
        }]
    );
}

#[tokio::test]
async fn proposal_open_rejects_an_unsupported_artifact_language_before_host_dispatch() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(proposal-open \"fortran\" \"show an artifact\" \"program x\")",
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Failed);
    assert!(outcome
        .vm_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "E-CAP-003"));
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn process_grant_cannot_be_reused_for_a_different_executable() {
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProcessRun,
            selector: crate::vm::ResourceSelector::Process {
                executables: vec!["/usr/bin/printf".into()],
            },
        })
        .unwrap();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(process-run \"/usr/bin/true\" (list \"unused\"))",
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
    assert_eq!(outcome.required_capabilities.len(), 1);
    let ResourceSelector::Process { executables } = &outcome.required_capabilities[0].selector
    else {
        panic!("process request must use a stable executable identity");
    };
    assert_eq!(
        ProcessExecutableIdentity::decode(&executables[0])
            .unwrap()
            .path,
        "/usr/bin/true"
    );
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
#[tokio::test]
async fn process_grant_cannot_be_reused_for_different_arguments() {
    let runtime = ProgramRuntime::new();
    let first = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(process-run \"/usr/bin/printf\" (list \"approved\"))",
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    runtime
        .grant_typed_capability(first.required_capabilities[0].clone())
        .unwrap();
    let mismatched = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(process-run \"/usr/bin/printf\" (list \"hostile\"))",
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(mismatched.status, ExecutionStatus::AuthorizationRequired);
    let ResourceSelector::Process { executables } = &mismatched.required_capabilities[0].selector
    else {
        panic!("process request must bind its exact arguments")
    };
    assert_eq!(
        ProcessExecutableIdentity::decode(&executables[0])
            .unwrap()
            .arguments,
        vec!["hostile"]
    );
}

#[tokio::test]
async fn approved_typed_network_connect_and_send_use_scoped_host_binding() {
    let listener = match std::net::TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(error) => panic!("failed to bind test listener: {error}"),
    };
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut input = [0; 4];
        std::io::Read::read_exact(&mut stream, &mut input).unwrap();
        assert_eq!(&input, b"ping");
        std::io::Write::write_all(&mut stream, b"pong").unwrap();
    });
    let runtime = ProgramRuntime::new();
    let source = format!("s\" 127.0.0.1\" {port} network-connect s\" ping\" bytes network-send");
    let request = submission(
        ProgramLanguage::Forth,
        &source,
        ExecutionEffect::ExternalWrite,
    );
    let pending = runtime.submit(request.clone()).await.unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::NetworkConnect,
            selector: crate::vm::ResourceSelector::Network {
                host: "127.0.0.1".into(),
                ports: vec![port],
            },
        })
        .unwrap();
    let approved = runtime.submit(request).await.unwrap();
    assert_eq!(approved.status, ExecutionStatus::Completed);
    assert_eq!(approved.values, vec![ProgramValue::Bytes(b"pong".to_vec())]);
    server.join().unwrap();
    assert!(
        runtime.network.lock().unwrap().is_empty(),
        "terminal ProgramRun must drop every owned socket"
    );
}

#[tokio::test]
async fn network_grant_cannot_be_reused_for_a_different_host() {
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::NetworkConnect,
            selector: crate::vm::ResourceSelector::Network {
                host: "127.0.0.1".into(),
                ports: vec![443],
            },
        })
        .unwrap();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "s\"example.test\" 443 network-connect",
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
    assert_eq!(outcome.required_capabilities.len(), 1);
    assert!(matches!(
        outcome.required_capabilities[0].selector,
        crate::vm::ResourceSelector::Network { ref host, ref ports }
            if host == "example.test" && ports == &[443]
    ));
}

#[tokio::test]
async fn same_run_revocation_before_network_send_prevents_payload_and_releases_socket() {
    let listener = match std::net::TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(error) => panic!("failed to bind test listener: {error}"),
    };
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(100)))
            .unwrap();
        let mut byte = [0; 1];
        match (&stream).read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Ok(size) => panic!("unexpected payload of {size} bytes"),
            Err(error) => panic!("unexpected socket read error: {error}"),
        }
    });
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            &format!("s\"127.0.0.1\" {port} network-connect s\"ping\" bytes network-send"),
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);

    // Both hooks target the same ProgramRun and capability. The first
    // permits connect; the second revokes its freshly issued session
    // grant at the send lease boundary, after the socket exists but
    // before any payload syscall can occur.
    let ledger = Arc::clone(&runtime.capability_ledger);
    let mut hooks = AUTHORIZATION_BEFORE_USE_HOOK
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap();
    hooks.push((
        pending.execution_id,
        CapabilityKind::NetworkConnect,
        Box::new(|| {}),
    ));
    hooks.push((
        pending.execution_id,
        CapabilityKind::NetworkConnect,
        Box::new(move || {
            let mut ledger = ledger.lock().unwrap();
            let grant_id = ledger
                .grants
                .grants
                .iter()
                .rev()
                .find(|grant| grant.revoked_at_unix_ms.is_none())
                .expect("session approval issued an active network grant")
                .id;
            assert!(ledger.revoke(grant_id, "same-run-revoker", unix_time_ms()));
        }),
    ));
    drop(hooks);

    let sent = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowSession,
            "test-user",
        )
        .await
        .unwrap();
    assert_eq!(sent.execution_id, pending.execution_id);
    assert_eq!(sent.status, ExecutionStatus::Failed);
    assert!(sent
        .diagnostics
        .iter()
        .any(|diagnostic| { diagnostic.contains("revoked") || diagnostic.contains("replaced") }));
    let ledger = runtime.capability_ledger().unwrap();
    assert_eq!(ledger.authorization_audit.len(), 1);
    server.join().unwrap();
    assert!(
        runtime.network.lock().unwrap().is_empty(),
        "failed terminal ProgramRun must drop its connected socket"
    );
}

#[tokio::test]
async fn typed_say_emits_stream_chunks_and_buffers_result() {
    let runtime = ProgramRuntime::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = Arc::clone(&events);
    let outcome = runtime
        .submit_with_typed_effect_sink(
            submission(
                ProgramLanguage::Forth,
                "s\" first\" say s\" second\" say",
                ExecutionEffect::VmRead,
            ),
            Arc::new(move |event| sink_events.lock().unwrap().push(event)),
        )
        .await
        .unwrap();
    assert_eq!(outcome.output, "firstsecond");
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].execution_id, outcome.execution_id);
    assert_eq!(events[0].effect.sequence, 0);
    assert_eq!(events[1].effect.sequence, 1);
    assert_eq!(
        events
            .iter()
            .map(|event| match &event.effect.event {
                crate::vm::HostSideEffect::Emit { text } => text.as_str(),
                other => panic!("expected emit event, found {other:?}"),
            })
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert_eq!(outcome.output_chunks, vec!["first", "second"]);
    assert_eq!(
        outcome.side_effects,
        vec![
            crate::vm::HostSideEffect::Emit {
                text: "first".into()
            },
            crate::vm::HostSideEffect::Emit {
                text: "second".into()
            }
        ]
    );
}

#[tokio::test]
async fn typed_forth_dot_quote_is_a_session_emit_shorthand() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            ".\" hello from standard Forth\"",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();

    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert_eq!(outcome.backend, ExecutionBackend::TypedVm);
    assert_eq!(outcome.output, "hello from standard Forth");
    assert_eq!(
        outcome.side_effects,
        vec![crate::vm::HostSideEffect::Emit {
            text: "hello from standard Forth".into(),
        }]
    );
}

#[tokio::test]
async fn typed_forth_s_quote_pushes_text_without_emitting_it() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "s\" retained value\"",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();

    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert_eq!(outcome.backend, ExecutionBackend::TypedVm);
    assert_eq!(
        outcome.values,
        vec![crate::programs::ProgramValue::String(
            "retained value".into()
        )]
    );
    assert!(outcome.output.is_empty());
    assert!(outcome.side_effects.is_empty());
    assert!(outcome.effect_journal.is_empty());
}

#[tokio::test]
async fn typed_forth_cr_is_an_explicit_session_emit_newline() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "s\" first\" say cr s\" second\" say",
            ExecutionEffect::VmRead,
        ))
        .await
        .unwrap();

    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert_eq!(outcome.output, "first\nsecond");
    assert_eq!(
        outcome.output_chunks,
        vec!["first".to_string(), "\n".to_string(), "second".to_string()]
    );
}

#[tokio::test]
async fn typed_forth_can_say_computed_values_progressively() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\"the result of 2+3 is \" say 2 3 + int-to-string space str-cat say s\"is that correct?\" say",
                ExecutionEffect::VmRead,
            ))
            .await
            .unwrap();
    assert_eq!(outcome.output, "the result of 2+3 is 5 is that correct?");
}

#[tokio::test]
async fn typed_output_handles_are_owned_by_their_program_run() {
    let runtime = ProgramRuntime::new();
    let opened = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "s\"build\" output-open",
            ExecutionEffect::VmRead,
        ))
        .await
        .unwrap();
    assert!(matches!(
        opened.values.as_slice(),
        [ProgramValue::Resource { kind, .. }] if kind == "output-handle"
    ));

    // A later submission may retain the opaque value on the persistent
    // VM stack, but it must not be able to update the previous run's
    // presentation resource.
    let update = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "s\"still working\" output-status",
            ExecutionEffect::VmRead,
        ))
        .await
        .unwrap();
    assert_eq!(update.status, ExecutionStatus::Failed);
    assert!(update
        .vm_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "E-OUTPUT-HANDLE-003"));
}

#[tokio::test]
async fn synchronous_output_open_projects_one_sequence_ordered_create_event() {
    let runtime = ProgramRuntime::new();
    let output_manager = Arc::new(crate::cli::OutputManager::default());
    output_manager.disable_stdout();
    let response = output_manager.start_work_unit("VM program output");
    response.set_program_output();
    let projection =
        crate::cli::VmOutputProjection::new(Arc::clone(&output_manager), Arc::clone(&response));
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink: TypedEffectSink = {
        let events = Arc::clone(&events);
        Arc::new(move |effect| events.lock().unwrap().push(effect))
    };

    let outcome = runtime
        .submit_with_typed_effect_sink(
            submission(
                ProgramLanguage::Lisp,
                "(let ((handle (output-open \"download\")))
                       (begin (output-status handle \"starting\")
                              (output-complete handle)))",
                ExecutionEffect::VmRead,
            ),
            sink,
        )
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);
    let events = events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .map(|envelope| envelope.effect.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "one visible event must exist for each UI sequence"
    );
    assert!(events.iter().all(|effect| !matches!(
        effect.effect.event,
        crate::vm::HostSideEffect::Request { .. }
    )));
    assert!(matches!(
        events.first().map(|effect| &effect.effect.event),
        Some(crate::vm::HostSideEffect::Ui {
            operation: crate::vm::UiOperation::Create,
            text: Some(title),
            target: Some(TypedValue::Resource { kind, .. }),
            ..
        }) if title == "download" && kind == "output-handle"
    ));
    assert!(matches!(
        events.last().map(|effect| &effect.effect.event),
        Some(crate::vm::HostSideEffect::Ui {
            operation: crate::vm::UiOperation::Complete,
            ..
        })
    ));
    for event in events.iter() {
        assert!(
            !projection.project_envelope(event.clone()).is_empty(),
            "the UI projection must not discard a same-sequence create event"
        );
    }
    let messages = output_manager.get_messages();
    assert_eq!(messages.len(), 2, "response port plus output handle");
    assert_eq!(
        messages[1].status(),
        crate::cli::messages::MessageStatus::Complete
    );
    assert!(messages[1]
        .format(&crate::theme::ColorScheme::default())
        .contains("download"));
}

#[tokio::test]
async fn typed_forth_output_handle_can_be_updated_in_its_creating_run() {
    let runtime = ProgramRuntime::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink: TypedEffectSink = {
        let events = Arc::clone(&events);
        Arc::new(move |effect| events.lock().unwrap().push(effect))
    };

    let outcome = runtime
            .submit_with_typed_effect_sink(
                submission(
                    ProgramLanguage::Forth,
                    "s\"download\" output-open dup s\"starting\" output-status dup 2 5 output-progress output-complete",
                    ExecutionEffect::VmRead,
                ),
                sink,
            )
            .await
            .unwrap();

    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert!(outcome.output.is_empty());
    let events = events.lock().unwrap();
    assert!(matches!(
        events.first().map(|effect| &effect.effect.event),
        Some(crate::vm::HostSideEffect::Ui {
            operation: crate::vm::UiOperation::Create,
            ..
        })
    ));
    assert!(events.iter().any(|effect| matches!(
        &effect.effect.event,
        crate::vm::HostSideEffect::Ui {
            operation: crate::vm::UiOperation::Create,
            text: Some(title),
            ..
        } if title == "download"
    )));
    assert!(events.iter().any(|effect| matches!(
        &effect.effect.event,
        crate::vm::HostSideEffect::Ui {
            operation: crate::vm::UiOperation::Status,
            text: Some(text),
            ..
        } if text == "starting"
    )));
    assert!(events.iter().any(|effect| matches!(
        &effect.effect.event,
        crate::vm::HostSideEffect::Ui {
            operation: crate::vm::UiOperation::Progress,
            progress: Some(crate::vm::UiProgress {
                completed: 2,
                total: Some(5),
            }),
            ..
        }
    )));
    assert!(matches!(
        events.last().map(|effect| &effect.effect.event),
        Some(crate::vm::HostSideEffect::Ui {
            operation: crate::vm::UiOperation::Complete,
            ..
        })
    ));
}

#[tokio::test]
async fn portable_output_open_registers_the_host_issued_handle_for_later_updates() {
    let runtime = ProgramRuntime::new();
    let (sink, receiver) = typed_effect_channel();
    let pending = runtime
        .submit_with_deferred_host_effects(
            submission(
                ProgramLanguage::Lisp,
                "(let ((handle (output-open \"download\"))) \
                       (begin (output-status handle \"starting\") \
                              (output-complete handle)))",
                ExecutionEffect::VmRead,
            ),
            sink,
        )
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::Suspended);

    let open = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("portable output-open request");
    assert_eq!(open.execution_id, pending.execution_id);
    assert!(matches!(
        &open.effect.event,
        crate::vm::HostSideEffect::Request { arguments }
            if matches!(arguments.as_slice(), [TypedValue::String(title)] if title == "download")
    ));

    let completed = runtime
        .resume_vm_effect(VmResume {
            execution_id: open.execution_id,
            sequence: open.effect.sequence,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Resource {
                    kind: "output-handle".into(),
                    handle: "portable-download".into(),
                    generation: 7,
                }],
            },
        })
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(completed.output, "");

    let updates = receiver.try_iter().collect::<Vec<_>>();
    assert_eq!(
        updates
            .iter()
            .map(|envelope| envelope.effect.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(matches!(
        updates.first().map(|envelope| &envelope.effect.event),
        Some(crate::vm::HostSideEffect::Ui {
            operation: crate::vm::UiOperation::Status,
            text: Some(text),
            target: Some(TypedValue::Resource { handle, generation, .. }),
            ..
        }) if text == "starting" && handle == "portable-download" && *generation == 7
    ));
    assert!(matches!(
        updates.last().map(|envelope| &envelope.effect.event),
        Some(crate::vm::HostSideEffect::Ui {
            operation: crate::vm::UiOperation::Complete,
            ..
        })
    ));
}

#[tokio::test]
async fn inspection_reports_ordered_stack_and_vocabulary() {
    let runtime = ProgramRuntime::new();
    runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "10 20",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    let state = runtime.inspect().await.unwrap();
    assert_eq!(state.revision, 1);
    assert_eq!(state.stack.len(), state.typed_stack.len());
    assert_eq!(state.stack[0].value, ProgramValue::Int(10));
    assert_eq!(state.stack[1].value, ProgramValue::Int(20));
    assert!(state.vocabulary.iter().any(|word| word.name == "+"));
    assert_eq!(state.vocabulary, state.typed_vocabulary);
}

#[tokio::test]
async fn revision_history_records_only_successful_commit_boundaries() {
    let runtime = ProgramRuntime::new();
    assert_eq!(runtime.revision_history().unwrap().len(), 1);
    runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "7",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    let suspended = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "unit yield 9",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(suspended.status, ExecutionStatus::Suspended);

    let history = runtime.revision_history().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].revision, 1);
    assert_eq!(history[1].stack, vec![TypedValue::Int(7)]);
    assert!(history[1].checkpoint.is_some());
}

#[tokio::test]
async fn newer_runner_checkpoint_hydrates_without_importing_authority() {
    let source = ProgramRuntime::new();
    let defined = source
        .submit(submission(
            ProgramLanguage::Lisp,
            "(define (double (n : int)) : int (* n 2))",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    let checkpoint = source
        .revision_history()
        .unwrap()
        .into_iter()
        .find(|snapshot| snapshot.revision == defined.output_revision)
        .and_then(|snapshot| snapshot.checkpoint)
        .unwrap();

    let runner = ProgramRuntime::new();
    let local_grant = crate::vm::CapabilityRequirement::file(
        crate::vm::FileOperation::Read,
        crate::vm::FileSelector::parse("./**").unwrap(),
    );
    runner.grant_typed_capability(local_grant.clone()).unwrap();
    let authority_before = runner.capability_ledger().unwrap();
    assert!(runner
        .hydrate_reducible_state_if_newer(checkpoint, defined.output_revision)
        .await
        .unwrap());
    assert_eq!(runner.revision(), defined.output_revision);
    assert_eq!(runner.capability_ledger().unwrap(), authority_before);

    let called = runner
        .submit(submission(
            ProgramLanguage::Forth,
            "21 double",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert!(matches!(called.values.as_slice(), [ProgramValue::Int(42)]));
}

#[tokio::test]
async fn switching_brains_replaces_an_unrelated_higher_revision_lineage() {
    let source = ProgramRuntime::new();
    let defined = source
        .submit(submission(
            ProgramLanguage::Lisp,
            "(define (double (n : int)) : int (* n 2))",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    let checkpoint = source
        .revision_history()
        .unwrap()
        .into_iter()
        .find(|snapshot| snapshot.revision == defined.output_revision)
        .and_then(|snapshot| snapshot.checkpoint)
        .unwrap();

    let runner = ProgramRuntime::new();
    runner
        .submit(submission(
            ProgramLanguage::Forth,
            "10",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    runner
        .submit(submission(
            ProgramLanguage::Forth,
            "20",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert!(runner.revision() > defined.output_revision);

    runner
        .replace_reducible_state(checkpoint, defined.output_revision)
        .await
        .unwrap();
    assert_eq!(runner.revision(), defined.output_revision);
    let called = runner
        .submit(submission(
            ProgramLanguage::Forth,
            "21 double",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert!(matches!(called.values.as_slice(), [ProgramValue::Int(42)]));
}

#[tokio::test]
async fn runner_hydration_never_replaces_pending_continuations() {
    let source = ProgramRuntime::new();
    let first = source
        .submit(submission(
            ProgramLanguage::Forth,
            "1",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    let second = source
        .submit(submission(
            ProgramLanguage::Forth,
            "2",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    let checkpoint = source
        .revision_history()
        .unwrap()
        .into_iter()
        .find(|snapshot| snapshot.revision == second.output_revision)
        .and_then(|snapshot| snapshot.checkpoint)
        .unwrap();

    let runner = ProgramRuntime::new();
    let pending = runner
        .submit(submission(
            ProgramLanguage::Forth,
            "unit yield",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::Suspended);
    let error = runner
        .hydrate_reducible_state_if_newer(checkpoint, first.output_revision + 1)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("continuations are pending"));
    assert_eq!(runner.revision(), 0);
    assert_eq!(runner.pending_typed_execution_count().unwrap(), 1);
}

#[tokio::test]
async fn revision_history_is_a_bounded_restorable_window() {
    let runtime = ProgramRuntime::new();
    let commit_count = MAX_RETAINED_VM_REVISIONS as u64 + 4;
    for _ in 0..commit_count {
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "1 drop",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);
    }

    let history = runtime.revision_history().unwrap();
    assert_eq!(history.len(), MAX_RETAINED_VM_REVISIONS);
    assert_eq!(history.first().unwrap().revision, 5);
    assert_eq!(history.last().unwrap().revision, commit_count);
    let archive = runtime.archive().unwrap();
    assert_eq!(archive.base_revision, 5);
    assert_eq!(archive.current_revision, commit_count);

    let mut mismatched = archive.clone();
    mismatched.base_revision = 4;
    assert!(ProgramRuntime::from_archive(mismatched)
        .err()
        .expect("mismatched base revision must fail")
        .to_string()
        .contains("not declared base revision 4"));

    let restored = ProgramRuntime::from_archive(archive.clone()).unwrap();
    assert_eq!(restored.revision(), commit_count);
    assert_eq!(
        restored.revision_history().unwrap().len(),
        MAX_RETAINED_VM_REVISIONS
    );
    let next = restored
        .submit(submission(
            ProgramLanguage::Forth,
            "1 drop",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(next.output_revision, commit_count + 1);
    assert_eq!(
        restored
            .revision_history()
            .unwrap()
            .first()
            .unwrap()
            .revision,
        6
    );

    // Version-1 archives written before `base_revision` existed remain
    // loadable even when their lineage began from an application-owned
    // nonzero checkpoint.
    let mut legacy_json = serde_json::to_value(archive).unwrap();
    legacy_json.as_object_mut().unwrap().remove("base_revision");
    ProgramRuntime::from_archive(serde_json::from_value(legacy_json).unwrap()).unwrap();
}

#[tokio::test]
async fn revision_history_checkpoint_restores_persisted_vocabulary() {
    let runtime = ProgramRuntime::new();
    let defined = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            ": square ( S int -- S int ! pure ) dup * ;",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(defined.status, ExecutionStatus::Completed);
    runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "7",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();

    let history = runtime.revision_history().unwrap();
    let checkpoint = history
        .last()
        .and_then(|snapshot| snapshot.checkpoint.clone())
        .expect("pure revision exposes a restorable VM checkpoint");
    let mut restored = TypedRuntime::from_checkpoint(checkpoint).unwrap();
    let module = crate::language::compile_with_functions(
        ProgramLanguage::Forth,
        "restore.forth",
        "square",
        restored
            .stack()
            .iter()
            .map(crate::vm::TypedValue::value_type)
            .collect(),
        restored.vocabulary(),
        restored.functions(),
    )
    .expect("restored checkpoint must compile square through the language facade");
    let result = restored.execute(&module, 1_000);

    assert_eq!(result.status, TypedExecutionStatus::Completed);
    assert_eq!(restored.stack(), &[TypedValue::Int(49)]);
}

#[tokio::test]
async fn program_runtime_restarts_from_a_typed_checkpoint() {
    let runtime = ProgramRuntime::new();
    runtime
        .submit(submission(
            ProgramLanguage::Forth,
            ": square ( S int -- S int ! pure ) dup * ; 8",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    let checkpoint = runtime
        .revision_history()
        .unwrap()
        .last()
        .and_then(|snapshot| snapshot.checkpoint.clone())
        .unwrap();

    let restored = ProgramRuntime::from_checkpoint(checkpoint).unwrap();
    let result = restored
        .submit(submission(
            ProgramLanguage::Lisp,
            "(square 8)",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();

    assert_eq!(result.status, ExecutionStatus::Completed);
    assert_eq!(result.input_revision, 0);
    assert_eq!(result.output_revision, 1);
    assert_eq!(
        restored
            .inspect()
            .await
            .unwrap()
            .typed_stack
            .iter()
            .map(|cell| cell.value.clone())
            .collect::<Vec<_>>(),
        vec![TypedValue::Int(8), TypedValue::Int(64)]
    );
}

#[tokio::test]
async fn program_runtime_archive_restores_history_but_not_authority() {
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./src/**").unwrap(),
        ))
        .unwrap();
    runtime
        .submit(submission(
            ProgramLanguage::Forth,
            ": square ( S int -- S int ! pure ) dup * ; 8",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    let archive = runtime.archive().unwrap();
    assert_eq!(archive.current_revision, 1);
    assert_eq!(archive.revisions.len(), 2);
    let encoded = serde_json::to_string(&archive).unwrap();
    let restored = ProgramRuntime::from_archive(serde_json::from_str(&encoded).unwrap()).unwrap();

    assert!(restored
        .capability_ledger()
        .unwrap()
        .grants
        .grants
        .is_empty());
    assert!(!restored
        .inspect()
        .await
        .unwrap()
        .granted_capabilities
        .iter()
        .any(|requirement| requirement.capability == crate::vm::CapabilityKind::FileRead));
    let result = restored
        .submit(submission(
            ProgramLanguage::Lisp,
            "(square 8)",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(result.input_revision, 1);
    assert_eq!(result.output_revision, 2);
    assert_eq!(restored.revision_history().unwrap().len(), 3);
}

#[tokio::test]
async fn authority_state_restores_scoped_grants_beside_the_vm_archive() {
    let runtime = ProgramRuntime::new();
    runtime
        .issue_typed_capability(
            crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
            ),
            GrantScope::Session {
                session_id: runtime.capability_session_id(),
            },
            "test-user",
            None,
        )
        .unwrap();
    let authority: ProgramRuntimeAuthorityState =
        serde_json::from_str(&serde_json::to_string(&runtime.authority_state().unwrap()).unwrap())
            .unwrap();
    let restored =
        ProgramRuntime::from_archive_with_authority(runtime.archive().unwrap(), authority).unwrap();

    assert_eq!(
        restored.capability_session_id(),
        runtime.capability_session_id()
    );
    assert_eq!(
        restored.capability_project_id(),
        runtime.capability_project_id()
    );
    let read = restored
        .submit(submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(read.status, ExecutionStatus::Completed);
}

#[test]
fn authority_restore_rejects_active_grants_from_another_policy() {
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
        ))
        .unwrap();
    let mut state = runtime.authority_state().unwrap();
    state.ledger.grants.grants[0].policy_hash = "obsolete-policy".into();
    let mut restored = ProgramRuntime::new();
    assert!(restored.restore_authority_state(state).is_err());
    assert!(restored
        .capability_ledger()
        .unwrap()
        .grants
        .grants
        .is_empty());
}

#[test]
fn authority_restore_rejects_legacy_raw_path_process_grants() {
    let runtime = ProgramRuntime::new();
    let policy = runtime.capability_policy().unwrap();
    let mut ledger = CapabilityLedger::default();
    ledger
        .issue(
            CapabilityRequirement {
                capability: CapabilityKind::ProcessRun,
                selector: ResourceSelector::Process {
                    executables: vec!["/usr/bin/true".into()],
                },
            },
            GrantScope::Global,
            policy.policy_hash.clone(),
            "legacy-state",
            unix_time_ms(),
            None,
        )
        .unwrap();

    let error = runtime
        .restore_capability_ledger(ledger.clone())
        .expect_err("raw-path authority must not be restored");
    assert!(format!("{error:#}").contains("legacy process capability grant"));
    assert!(runtime
        .capability_ledger()
        .unwrap()
        .grants
        .grants
        .is_empty());

    let mut restored = ProgramRuntime::new();
    let error = restored
        .restore_authority_state(ProgramRuntimeAuthorityState {
            format_version: PROGRAM_RUNTIME_AUTHORITY_STATE_VERSION,
            session_id: uuid::Uuid::new_v4(),
            project_id: "legacy-project".into(),
            policy,
            ledger,
            resource_roots: Vec::new(),
            resource_root_audit: Vec::new(),
        })
        .expect_err("raw-path authority state must require explicit reapproval");
    assert!(format!("{error:#}").contains("stable v3 invocation identity"));
    assert!(restored
        .capability_ledger()
        .unwrap()
        .grants
        .grants
        .is_empty());
}

#[test]
fn capability_availability_is_separate_from_grants_and_selector_aware() {
    let runtime = ProgramRuntime::new();
    let workspace_read = crate::vm::CapabilityRequirement::file(
        crate::vm::FileOperation::Read,
        crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
    );
    assert_eq!(
        runtime.capability_availability(&workspace_read),
        crate::vm::CapabilityAvailability::Available
    );
    assert!(runtime
        .capability_ledger()
        .unwrap()
        .grants
        .grants
        .is_empty());

    let root = tempfile::tempdir().unwrap();
    let host_read = crate::vm::CapabilityRequirement::file(
        crate::vm::FileOperation::Read,
        crate::vm::FileSelector {
            root: crate::vm::ResourceRoot::HostMachine,
            pattern: "**".into(),
        },
    );
    assert_eq!(
        runtime.capability_availability(&host_read),
        crate::vm::CapabilityAvailability::Disabled
    );
    runtime.bind_host_machine_root(root.path()).unwrap();
    assert_eq!(
        runtime.capability_availability(&host_read),
        crate::vm::CapabilityAvailability::Available
    );
    assert_eq!(
        runtime.capability_availability(&crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::VmWrite,
            selector: crate::vm::ResourceSelector::None,
        }),
        crate::vm::CapabilityAvailability::Unsupported
    );
    assert_eq!(
        runtime.capability_availability(&crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ScheduleCreate,
            selector: crate::vm::ResourceSelector::Schedule { policy: None },
        }),
        crate::vm::CapabilityAvailability::Disabled,
        "a bare runtime must not expose a second local schedule store"
    );
}

#[tokio::test]
async fn public_runtime_preserves_one_typed_stack_across_lisp_and_forth_turns() {
    let runtime = ProgramRuntime::new();
    let lisp = runtime
        .submit(submission(
            ProgramLanguage::Lisp,
            "(+ 2 3)",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(lisp.status, ExecutionStatus::Completed);
    assert_eq!(lisp.output_revision, 1);

    let forth = runtime
        .submit(ProgramSubmission {
            expected_revision: Some(lisp.output_revision),
            ..submission(ProgramLanguage::Forth, "2 *", ExecutionEffect::Pure)
        })
        .await
        .unwrap();
    assert_eq!(forth.status, ExecutionStatus::Completed);
    assert_eq!(forth.output_revision, 2);

    let state = runtime.inspect().await.unwrap();
    assert_eq!(state.typed_stack.len(), 1);
    assert_eq!(state.typed_stack[0].value, TypedValue::Int(10));
}

#[tokio::test]
async fn submission_source_id_is_preserved_in_effect_origins() {
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(CapabilityRequirement {
            capability: crate::vm::CapabilityKind::SessionEmit,
            selector: crate::vm::ResourceSelector::None,
        })
        .unwrap();
    let outcome = runtime
        .submit(ProgramSubmission {
            source_id: Some("scripts/demo.lisp".into()),
            ..submission(
                ProgramLanguage::Lisp,
                "(say \"source aware\")",
                ExecutionEffect::Pure,
            )
        })
        .await
        .unwrap();

    assert_eq!(outcome.status, ExecutionStatus::Completed);
    assert_eq!(outcome.vm_side_effects.len(), 1);
    assert_eq!(
        outcome.vm_side_effects[0]
            .origin
            .span
            .as_ref()
            .unwrap()
            .source_id,
        "scripts/demo.lisp"
    );
}

#[tokio::test]
async fn rejects_stale_vm_revision() {
    let runtime = ProgramRuntime::new();
    runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "1",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    let mut request = submission(ProgramLanguage::Forth, "2 +", ExecutionEffect::VmWrite);
    request.expected_revision = Some(0);
    let error = runtime.submit(request).await.unwrap_err();
    assert!(error.to_string().contains("stale VM revision"));
}

#[tokio::test]
async fn suspended_runs_keep_private_state_and_reject_a_losing_commit() {
    let runtime = ProgramRuntime::new();
    let suspended = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "s\" before conflict\" say unit yield 1",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(suspended.status, ExecutionStatus::Suspended);
    assert!(runtime.inspect().await.unwrap().typed_stack.is_empty());

    let winner = runtime
        .submit(submission(
            ProgramLanguage::Forth,
            "2",
            ExecutionEffect::Pure,
        ))
        .await
        .unwrap();
    assert_eq!(winner.status, ExecutionStatus::Completed);
    assert_eq!(winner.output_revision, 1);

    let rejected = runtime
        .resume_typed_execution(suspended.execution_id)
        .await
        .expect("a continuation conflict is reported as a typed outcome");
    assert_eq!(rejected.status, ExecutionStatus::Failed);
    assert!(rejected
        .diagnostics
        .iter()
        .any(|message| message.contains("input revision 0; current revision is 1")));
    assert_eq!(rejected.output, "before conflict");
    assert!(!rejected.effect_journal.is_empty());
    let state = runtime.inspect().await.unwrap();
    assert_eq!(state.revision, 1);
    assert_eq!(
        state
            .typed_stack
            .iter()
            .map(|cell| &cell.value)
            .collect::<Vec<_>>(),
        vec![&TypedValue::Int(2)]
    );
}

#[cfg(unix)]
#[test]
fn workspace_path_rejects_a_stable_symlink_escape_before_host_io() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    let link = workspace.path().join("outside-link");
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    let selector = crate::vm::FileSelector::parse("./**").unwrap();

    let root = resource_root_binding_record(
        crate::vm::ResourceRoot::Workspace,
        workspace.path(),
        1,
        false,
        1,
    )
    .unwrap();
    assert!(
        open_resource_beneath_mode(&root, &selector, "outside-link", SecureOpenMode::ReadFile,)
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn descriptor_relative_open_rejects_final_component_symlink_swap() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().unwrap();
    let victim = workspace.path().join("victim");
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(&victim, b"inside").unwrap();
    std::fs::write(outside.path(), b"outside").unwrap();
    let binding = resource_root_binding_record(
        crate::vm::ResourceRoot::Workspace,
        workspace.path(),
        1,
        false,
        1,
    )
    .unwrap();
    let selector = crate::vm::FileSelector::parse("./**").unwrap();
    let outside_path = outside.path().to_path_buf();
    RESOURCE_BEFORE_FINAL_OPEN_HOOK
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .push((
            "victim".into(),
            Box::new(move || {
                std::fs::remove_file(&victim).unwrap();
                symlink(outside_path, victim).unwrap();
            }),
        ));
    assert!(
        open_resource_beneath_mode(&binding, &selector, "victim", SecureOpenMode::ReadFile,)
            .is_err()
    );
    assert_eq!(std::fs::read(outside.path()).unwrap(), b"outside");
}

#[cfg(unix)]
#[test]
fn descriptor_relative_open_keeps_the_opened_parent_after_path_replacement() {
    let workspace = tempfile::tempdir().unwrap();
    let directory = workspace.path().join("dir");
    let replacement = workspace.path().join("replacement");
    let displaced = workspace.path().join("displaced");
    std::fs::create_dir(&directory).unwrap();
    std::fs::create_dir(&replacement).unwrap();
    std::fs::write(directory.join("value"), b"authorized").unwrap();
    std::fs::write(replacement.join("value"), b"replacement").unwrap();
    let binding = resource_root_binding_record(
        crate::vm::ResourceRoot::Workspace,
        workspace.path(),
        1,
        false,
        1,
    )
    .unwrap();
    let selector = crate::vm::FileSelector::parse("./**").unwrap();
    let replacement_target = workspace.path().join("dir");
    RESOURCE_BEFORE_FINAL_OPEN_HOOK
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .push((
            "dir/value".into(),
            Box::new(move || {
                std::fs::rename(directory, displaced).unwrap();
                std::fs::rename(replacement, replacement_target).unwrap();
            }),
        ));
    let mut file =
        open_resource_beneath_mode(&binding, &selector, "dir/value", SecureOpenMode::ReadFile)
            .unwrap();
    let mut value = String::new();
    file.read_to_string(&mut value).unwrap();
    assert_eq!(value, "authorized");
}

#[test]
fn generic_resource_resolution_does_not_assign_host_roots_to_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let selector = crate::vm::FileSelector::parse("${host-machine}/etc/**").unwrap();
    std::fs::create_dir(workspace.path().join("etc")).unwrap();
    std::fs::write(workspace.path().join("etc/hosts"), b"local").unwrap();

    // Root selection happens in `TypedHostHandler`; the generic canonical
    // check only proves that a child remains under the root selected by
    // the host binding.
    let root = resource_root_binding_record(
        crate::vm::ResourceRoot::HostMachine,
        workspace.path(),
        1,
        false,
        1,
    )
    .unwrap();
    let mut file =
        open_resource_beneath_mode(&root, &selector, "etc/hosts", SecureOpenMode::ReadFile)
            .unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "local");
}

#[tokio::test]
async fn host_file_read_requires_an_explicit_host_binding_and_host_grant() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("note.txt"), b"host-only").unwrap();
    let runtime = ProgramRuntime::new();
    runtime.bind_host_machine_root(root.path()).unwrap();

    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            "s\" note.txt\" host-path host-file-read",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let request = &pending.approval_prompts[0].request;
    assert!(matches!(
        request.arguments.as_slice(),
        [TypedValue::Path { selector, relative }]
            if selector.root == crate::vm::ResourceRoot::HostMachine && relative == "note.txt"
    ));
    let sequence = request.effect_sequence.unwrap();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("${host-machine}/**").unwrap(),
        ))
        .unwrap();
    let completed = runtime
        .resume_typed_execution_for_effect(pending.execution_id, sequence)
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(
        completed.values,
        vec![ProgramValue::Bytes(b"host-only".to_vec())]
    );
}

#[tokio::test]
async fn project_and_task_output_roots_are_typed_independent_bindings() {
    let project = tempfile::tempdir().unwrap();
    let task_output = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("input.txt"), b"project-data").unwrap();
    let runtime = ProgramRuntime::new();
    runtime.bind_project_root(project.path()).unwrap();
    runtime.bind_task_output_root(task_output.path()).unwrap();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("${project}/**").unwrap(),
        ))
        .unwrap();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Write,
            crate::vm::FileSelector::parse("${task.output}/**").unwrap(),
        ))
        .unwrap();

    let read = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            "(project-file-read (project-path \"input.txt\"))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(read.status, ExecutionStatus::Completed);
    assert_eq!(
        read.values,
        vec![ProgramValue::Bytes(b"project-data".to_vec())]
    );

    let write = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            "s\" result.txt\" task-output-path s\" task-data\" bytes task-output-file-write",
            ExecutionEffect::WorkspaceWrite,
        ))
        .await
        .unwrap();
    assert_eq!(write.status, ExecutionStatus::Completed);
    assert_eq!(
        std::fs::read(task_output.path().join("result.txt")).unwrap(),
        b"task-data"
    );

    let crossed = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            "s\" input.txt\" project-path task-output-file-read",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(crossed.status, ExecutionStatus::Failed);
    assert!(crossed.diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("project") && diagnostic.contains("task.output")
    }));
}

#[tokio::test]
async fn host_file_read_fails_when_the_host_binding_is_not_installed() {
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            "s\" note.txt\" host-path host-file-read",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    let sequence = pending.approval_prompts[0].request.effect_sequence.unwrap();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("${host-machine}/**").unwrap(),
        ))
        .unwrap();
    let completed = runtime
        .resume_typed_execution_for_effect(pending.execution_id, sequence)
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Failed);
    assert!(completed
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("host-machine root is not installed")));
}

#[tokio::test]
async fn workspace_file_word_cannot_consume_a_host_path() {
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            "s\" note.txt\" host-path file-read",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Failed);
    assert!(outcome.diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("host-machine") && diagnostic.contains("workspace")
    }));
}

#[tokio::test]
async fn host_file_write_uses_the_same_explicit_binding_and_grant_boundary() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ProgramRuntime::new();
    runtime.bind_host_machine_root(root.path()).unwrap();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Forth,
            "s\" created.txt\" host-path s\" host-write\" bytes host-file-write",
            ExecutionEffect::WorkspaceWrite,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let sequence = pending.approval_prompts[0].request.effect_sequence.unwrap();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Write,
            crate::vm::FileSelector::parse("${host-machine}/**").unwrap(),
        ))
        .unwrap();
    let completed = runtime
        .resume_typed_execution_for_effect(pending.execution_id, sequence)
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(completed.values, vec![ProgramValue::Nil]);
    assert_eq!(
        std::fs::read(root.path().join("created.txt")).unwrap(),
        b"host-write"
    );
}

#[tokio::test]
async fn public_revocation_winning_before_host_use_prevents_file_mutation() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("note.txt"), b"before").unwrap();
    let runtime = Arc::new(ProgramRuntime::new());
    runtime.bind_host_machine_root(root.path()).unwrap();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            "(host-file-write (host-path \"note.txt\") (bytes \"after\"))",
            ExecutionEffect::ExternalWrite,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let sequence = pending.approval_prompts[0].request.effect_sequence.unwrap();
    let grant_id = runtime
        .grant_typed_capability(pending.required_capabilities[0].clone())
        .unwrap();
    let (revoke_tx, revoke_rx) = std::sync::mpsc::channel();
    let (revoked_tx, revoked_rx) = std::sync::mpsc::channel::<bool>();
    AUTHORIZATION_BEFORE_LEASE_HOOK
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .push((
            pending.execution_id,
            CapabilityKind::FileWrite,
            Box::new(move || {
                revoke_tx.send(()).unwrap();
                assert!(revoked_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("public revocation must finish before the shared use lease begins"));
            }),
        ));
    let revoker_runtime = Arc::clone(&runtime);
    let revoker = std::thread::spawn(move || {
        revoke_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("host use reached the before-lease boundary");
        let revoked = revoker_runtime.revoke_typed_capability(grant_id).unwrap();
        revoked_tx.send(revoked).unwrap();
    });
    let outcome = runtime
        .resume_typed_execution_for_effect(pending.execution_id, sequence)
        .await
        .unwrap();
    revoker.join().unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Failed);
    assert_eq!(
        std::fs::read(root.path().join("note.txt")).unwrap(),
        b"before"
    );
    let ledger = runtime.capability_ledger().unwrap();
    assert!(ledger.authorization_audit.is_empty());
    assert!(ledger
        .audit
        .iter()
        .any(|entry| entry.action == crate::vm::CapabilityAuditAction::Revoked));
}

#[test]
fn in_flight_deferred_use_blocks_public_revoke_and_root_mutation() {
    let project = tempfile::tempdir().unwrap();
    let runtime = Arc::new(ProgramRuntime::new());
    runtime.bind_project_root(project.path()).unwrap();
    let grant_id = runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("${project}/**").unwrap(),
        ))
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let (outcome_tx, outcome_rx) = std::sync::mpsc::channel();
    let worker_runtime = Arc::clone(&runtime);
    let worker = std::thread::spawn(move || {
        let release_rx = Arc::clone(&release_rx);
        let sink: TypedEffectSink = Arc::new(move |_| {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("test releases the in-flight deferred host use");
        });
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let outcome = tokio.block_on(worker_runtime.submit_with_deferred_host_effects(
            submission(
                ProgramLanguage::Lisp,
                "(project-file-read (project-path \"note.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ),
            sink,
        ));
        outcome_tx.send(outcome).unwrap();
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("deferred sink reached the externally visible use boundary");

    let (revoker_started_tx, revoker_started_rx) = std::sync::mpsc::channel();
    let (revoked_tx, revoked_rx) = std::sync::mpsc::channel();
    let revoker_runtime = Arc::clone(&runtime);
    let revoker = std::thread::spawn(move || {
        revoker_started_tx.send(()).unwrap();
        revoked_tx
            .send(revoker_runtime.revoke_typed_capability(grant_id))
            .unwrap();
    });
    let (clearer_started_tx, clearer_started_rx) = std::sync::mpsc::channel();
    let (cleared_tx, cleared_rx) = std::sync::mpsc::channel();
    let clearer_runtime = Arc::clone(&runtime);
    let clearer = std::thread::spawn(move || {
        clearer_started_tx.send(()).unwrap();
        cleared_tx
            .send(clearer_runtime.clear_project_root())
            .unwrap();
    });
    revoker_started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("public revocation thread starts while the host use is in flight");
    clearer_started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("public root mutation thread starts while the host use is in flight");
    assert!(
        revoked_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "public revocation must wait for the in-flight shared use lease"
    );
    assert!(
        cleared_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "public root mutation must wait for the in-flight shared use lease"
    );

    release_tx.send(()).unwrap();
    let pending = outcome_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("deferred dispatch completes after its sink returns")
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::Suspended);
    assert_eq!(
        revoked_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("public revocation completes after the shared use lease")
            .unwrap(),
        true
    );
    cleared_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("public root mutation completes after the shared use lease")
        .unwrap();
    worker.join().unwrap();
    revoker.join().unwrap();
    clearer.join().unwrap();
    assert!(!runtime
        .authority_state()
        .unwrap()
        .resource_roots
        .iter()
        .any(|binding| binding.root == crate::vm::ResourceRoot::Project));
}

#[tokio::test]
async fn failed_file_open_rolls_back_once_use_and_restart_state() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ProgramRuntime::new();
    runtime.bind_host_machine_root(root.path()).unwrap();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            "(host-file-read (host-path \"missing.txt\"))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
    let failed = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowOnce,
            "test-user",
        )
        .await
        .unwrap();
    assert_eq!(failed.status, ExecutionStatus::Failed);
    let ledger = runtime.capability_ledger().unwrap();
    let grant = ledger.grants.grants.last().expect("once grant remains");
    assert!(matches!(grant.scope, GrantScope::Once { .. }));
    assert!(grant.consumed_at_unix_ms.is_none());
    assert!(ledger.authorization_audit.is_empty());
    assert!(!ledger
        .audit
        .iter()
        .any(|entry| entry.action == crate::vm::CapabilityAuditAction::Consumed));

    let encoded = serde_json::to_vec(&runtime.authority_state().unwrap()).unwrap();
    let state: ProgramRuntimeAuthorityState = serde_json::from_slice(&encoded).unwrap();
    let mut restored = ProgramRuntime::new();
    restored.restore_authority_state(state).unwrap();
    let restored = restored.capability_ledger().unwrap();
    assert!(restored
        .grants
        .grants
        .last()
        .unwrap()
        .consumed_at_unix_ms
        .is_none());
    assert!(restored.authorization_audit.is_empty());
}

#[tokio::test]
async fn rollback_sink_failure_retains_the_last_durable_authorization_state() {
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

    let root = tempfile::tempdir().unwrap();
    let runtime = ProgramRuntime::new();
    runtime.bind_host_machine_root(root.path()).unwrap();
    let saw_authorized = Arc::new(AtomicBool::new(false));
    let durable = Arc::new(Mutex::new(None::<ProgramRuntimeAuthorityState>));
    let sink_saw_authorized = Arc::clone(&saw_authorized);
    let sink_durable = Arc::clone(&durable);
    runtime
        .set_authority_sink(Arc::new(move |state| {
            if !state.ledger.authorization_audit.is_empty() {
                sink_saw_authorized.store(true, AtomicOrdering::SeqCst);
            } else if sink_saw_authorized.load(AtomicOrdering::SeqCst)
                && state.ledger.grants.grants.iter().any(|grant| {
                    matches!(grant.scope, GrantScope::Once { .. })
                        && grant.consumed_at_unix_ms.is_none()
                })
            {
                anyhow::bail!("injected rollback persistence failure");
            }
            *sink_durable.lock().unwrap() = Some(state);
            Ok(())
        }))
        .unwrap();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            "(host-file-read (host-path \"missing.txt\"))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    let failed = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowOnce,
            "test-user",
        )
        .await
        .unwrap();
    assert_eq!(failed.status, ExecutionStatus::Failed);
    assert!(failed
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("persist unused host authorization rollback")));
    let ledger = runtime.capability_ledger().unwrap();
    assert_eq!(ledger.authorization_audit.len(), 1);
    assert!(ledger
        .grants
        .grants
        .last()
        .unwrap()
        .consumed_at_unix_ms
        .is_some());
    assert_eq!(
        durable.lock().unwrap().as_ref().unwrap().ledger,
        ledger,
        "memory must remain at the last successfully persisted authority state"
    );
}

#[tokio::test]
async fn unused_authorization_rollback_preserves_an_unrelated_concurrent_grant() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ProgramRuntime::new();
    runtime.bind_host_machine_root(root.path()).unwrap();
    let pending = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            "(host-file-read (host-path \"missing.txt\"))",
            ExecutionEffect::WorkspaceRead,
        ))
        .await
        .unwrap();
    let ledger = Arc::clone(&runtime.capability_ledger);
    let policy_hash = runtime.capability_policy().unwrap().policy_hash;
    AUTHORIZATION_BEFORE_USE_HOOK
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .push((
            pending.execution_id,
            CapabilityKind::FileRead,
            Box::new(move || {
                ledger
                    .lock()
                    .unwrap()
                    .issue(
                        CapabilityRequirement {
                            capability: CapabilityKind::MemoryRead,
                            selector: ResourceSelector::Memory {
                                tree: "session".into(),
                                path: "concurrent".into(),
                            },
                        },
                        GrantScope::Global,
                        policy_hash,
                        "concurrent-user",
                        unix_time_ms(),
                        None,
                    )
                    .unwrap();
            }),
        ));
    let failed = runtime
        .resolve_typed_approval(
            &pending.approval_prompts[0],
            ApprovalChoice::AllowOnce,
            "test-user",
        )
        .await
        .unwrap();
    assert_eq!(failed.status, ExecutionStatus::Failed);
    let ledger = runtime.capability_ledger().unwrap();
    assert!(ledger.grants.grants.iter().any(|grant| {
        grant.requirement.capability == CapabilityKind::MemoryRead
            && grant.created_by == "concurrent-user"
            && grant.is_active(unix_time_ms())
    }));
    assert!(ledger.authorization_audit.is_empty());
}

#[test]
fn resource_root_bind_revoke_and_whole_machine_lifecycle_is_durable() {
    let runtime = ProgramRuntime::new();
    let persisted = Arc::new(Mutex::new(Vec::new()));
    let persisted_sink = Arc::clone(&persisted);
    runtime
        .set_authority_sink(Arc::new(move |state| {
            persisted_sink.lock().unwrap().push(state);
            Ok(())
        }))
        .unwrap();
    let project = tempfile::tempdir().unwrap();
    runtime.bind_project_root(project.path()).unwrap();
    runtime.clear_project_root().unwrap();
    assert!(runtime.bind_host_machine_root(PathBuf::from("/")).is_err());
    runtime.bind_whole_machine_root().unwrap();
    runtime.clear_host_machine_root().unwrap();

    let states = persisted.lock().unwrap();
    assert_eq!(states.len(), 4);
    assert!(states
        .iter()
        .all(|state| state.format_version == PROGRAM_RUNTIME_AUTHORITY_STATE_VERSION));
    let final_state = states.last().unwrap();
    assert!(final_state
        .resource_root_audit
        .iter()
        .any(|entry| entry.root == crate::vm::ResourceRoot::Project
            && entry.action == ResourceRootAuditAction::Bound));
    assert!(final_state
        .resource_root_audit
        .iter()
        .any(|entry| entry.root == crate::vm::ResourceRoot::Project
            && entry.action == ResourceRootAuditAction::Revoked));
    assert!(final_state
        .resource_root_audit
        .iter()
        .any(|entry| entry.whole_machine && entry.action == ResourceRootAuditAction::Bound));
    assert!(!final_state
        .resource_roots
        .iter()
        .any(|binding| binding.root == crate::vm::ResourceRoot::HostMachine));
}

#[test]
fn failed_authority_sink_rolls_back_resource_root_bind_and_revoke() {
    let runtime = ProgramRuntime::new();
    let project = tempfile::tempdir().unwrap();
    let initial = runtime.authority_state().unwrap();
    runtime
        .set_authority_sink(Arc::new(|_| anyhow::bail!("injected root sink failure")))
        .unwrap();
    assert!(runtime.bind_project_root(project.path()).is_err());
    assert_eq!(runtime.authority_state().unwrap(), initial);

    runtime.clear_authority_sink().unwrap();
    runtime.bind_project_root(project.path()).unwrap();
    let bound = runtime.authority_state().unwrap();
    runtime
        .set_authority_sink(Arc::new(|_| anyhow::bail!("injected root sink failure")))
        .unwrap();
    assert!(runtime.clear_project_root().is_err());
    assert_eq!(runtime.authority_state().unwrap(), bound);
}

#[test]
fn authority_restart_rejects_replaced_resource_root_identity() {
    let runtime = ProgramRuntime::new();
    let parent = tempfile::tempdir().unwrap();
    let project = parent.path().join("project");
    let displaced = parent.path().join("displaced");
    std::fs::create_dir(&project).unwrap();
    runtime.bind_project_root(&project).unwrap();
    let state = runtime.authority_state().unwrap();
    std::fs::rename(&project, &displaced).unwrap();
    std::fs::create_dir(&project).unwrap();

    let mut restored = ProgramRuntime::new();
    let error = restored
        .restore_authority_state(state)
        .expect_err("replacement directory must not inherit root authority");
    assert!(format!("{error:#}").contains("identity changed since approval"));
    assert_eq!(
        restored.capability_availability(&CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("${project}/**").unwrap(),
        )),
        CapabilityAvailability::Disabled
    );
}

#[test]
fn authority_restart_rejects_incomplete_resource_root_lifecycle_audit() {
    let runtime = ProgramRuntime::new();
    let project = tempfile::tempdir().unwrap();
    runtime.bind_project_root(project.path()).unwrap();
    runtime.clear_project_root().unwrap();
    let mut state = runtime.authority_state().unwrap();
    let removed = state.resource_root_audit.pop().unwrap();
    assert_eq!(removed.action, ResourceRootAuditAction::Revoked);

    let mut restored = ProgramRuntime::new();
    let error = restored
        .restore_authority_state(state)
        .expect_err("audit replay must detect an omitted revocation");
    assert!(format!("{error:#}").contains("replayed audit state"));
}

/// Tests that the capability inversion made possible.
///
/// Before the runtime took its scheduler as `AgentSpawning`, exercising any of this meant building
/// a real `AgentScheduler`, which needs a provider resolver, a generator, and a Brain client. None
/// of that is required to check what the runtime does with a child-agent request, so none of it is
/// here: the fake below records what it was asked and answers immediately.
#[cfg(test)]
mod agent_capability {
    use super::*;
    use crate::runtime::agents::{
        AgentIdentity, AgentSpawning, AgentTaskResult, AgentTaskSnapshot, AgentTaskSpec,
        AgentTaskStatus,
    };
    use std::sync::Mutex as StdMutex;

    /// Answers every request without running anything, and remembers what it was asked.
    struct RecordingSpawner {
        spawned: StdMutex<Vec<String>>,
        cancelled: StdMutex<Vec<uuid::Uuid>>,
        authorized: StdMutex<Vec<(uuid::Uuid, Option<uuid::Uuid>)>>,
    }

    impl RecordingSpawner {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                spawned: StdMutex::new(Vec::new()),
                cancelled: StdMutex::new(Vec::new()),
                authorized: StdMutex::new(Vec::new()),
            })
        }

        fn identity(task_id: uuid::Uuid) -> AgentIdentity {
            AgentIdentity {
                agent_id: uuid::Uuid::new_v4(),
                task_id,
                parent_agent_id: None,
                root_agent_id: uuid::Uuid::new_v4(),
                depth: 0,
                provider_model: "fake/model".into(),
                vm_revision: 0,
                manifest_generation: 0,
                starting_context_hash: String::new(),
                grant_ceiling: Default::default(),
                brain_run_id: None,
            }
        }
    }

    #[async_trait::async_trait]
    impl AgentSpawning for RecordingSpawner {
        async fn spawn(
            &self,
            spec: AgentTaskSpec,
            _parent: Option<&AgentIdentity>,
        ) -> Result<AgentIdentity> {
            self.spawned.lock().unwrap().push(spec.task.clone());
            Ok(Self::identity(uuid::Uuid::new_v4()))
        }

        async fn authorize(
            &self,
            task_id: uuid::Uuid,
            parent: Option<&AgentIdentity>,
        ) -> Result<()> {
            self.authorized
                .lock()
                .unwrap()
                .push((task_id, parent.map(|identity| identity.agent_id)));
            Ok(())
        }

        async fn poll(&self, task_id: uuid::Uuid) -> Result<AgentTaskSnapshot> {
            Ok(AgentTaskSnapshot {
                identity: Self::identity(task_id),
                task: "recorded".into(),
                role: Default::default(),
                status: AgentTaskStatus::Running,
                result: None,
            })
        }

        async fn wait(&self, task_id: uuid::Uuid) -> Result<AgentTaskResult> {
            Ok(AgentTaskResult {
                identity: Self::identity(task_id),
                status: AgentTaskStatus::Completed,
                final_message: "done".into(),
                diagnostics: Vec::new(),
                turns: 1,
                elapsed_ms: 0,
            })
        }

        async fn cancel(&self, task_id: uuid::Uuid) -> Result<()> {
            self.cancelled.lock().unwrap().push(task_id);
            Ok(())
        }
    }

    #[tokio::test]
    async fn an_unattached_runtime_refuses_a_child_agent_and_says_why() {
        // The empty slot is a real implementation, not an `Option` every caller unwraps, so the
        // refusal carries a reason instead of a panic or a silent `None`.
        let runtime = ProgramRuntime::new();
        let binding = runtime.agent_binding_for_test(None);
        assert!(
            binding.is_none(),
            "a runtime with no spawner attached must not hand out an agent binding"
        );
    }

    #[tokio::test]
    async fn the_runtime_routes_a_spawn_to_whatever_was_attached() {
        // The point of the inversion: this exercises the runtime's agent path with no provider,
        // no generator and no Brain client anywhere in the test.
        let runtime = Arc::new(ProgramRuntime::new());
        let spawner = RecordingSpawner::new();
        runtime.attach_agent_scheduler(&spawner);

        let binding = runtime
            .agent_binding_for_test(None)
            .expect("an attached spawner must produce a binding");
        let identity = binding
            .spawn("summarise the log".into())
            .await
            .expect("the fake spawner accepts every task");

        assert_eq!(
            vec!["summarise the log".to_string()],
            spawner.spawned.lock().unwrap().clone(),
            "the runtime must pass the task through unchanged"
        );
        assert_eq!(
            "fake/model", identity.provider_model,
            "the identity returned is the spawner's, not one the runtime invented"
        );
    }

    #[tokio::test]
    async fn test_attaching_a_second_spawner_replaces_the_first() {
        let runtime = Arc::new(ProgramRuntime::new());
        let first = RecordingSpawner::new();
        let second = RecordingSpawner::new();
        runtime.attach_agent_scheduler(&first);
        runtime.attach_agent_scheduler(&second);

        runtime
            .agent_binding_for_test(None)
            .expect("the replacement spawner must remain attached")
            .spawn("replacement work".into())
            .await
            .expect("the replacement spawner accepts every task");

        assert!(
            first.spawned.lock().unwrap().is_empty(),
            "a replaced spawner must receive no later tasks; first={:?}",
            first.spawned.lock().unwrap()
        );
        assert_eq!(
            second.spawned.lock().unwrap().as_slice(),
            ["replacement work"],
            "the most recently attached spawner must receive the task"
        );
    }

    #[tokio::test]
    async fn cancelling_authorizes_before_it_cancels() {
        // Order matters: a task the caller does not own must be refused before anything changes.
        let runtime = Arc::new(ProgramRuntime::new());
        let spawner = RecordingSpawner::new();
        runtime.attach_agent_scheduler(&spawner);
        let binding = runtime.agent_binding_for_test(None).expect("binding");

        let identity = binding.spawn("work".into()).await.expect("spawn");
        binding.cancel(identity.task_id).await.expect("cancel");

        assert_eq!(
            vec![identity.task_id],
            spawner.cancelled.lock().unwrap().clone(),
            "the cancel must reach the spawner"
        );
        assert!(
            spawner
                .authorized
                .lock()
                .unwrap()
                .iter()
                .any(|(task, _)| *task == identity.task_id),
            "cancel must authorize the task first; authorized={:?}",
            spawner.authorized.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn a_dropped_spawner_is_reported_rather_than_panicking() {
        // The runtime holds a `Weak`, so a host that shuts its scheduler down mid-session must
        // produce an error the caller can act on.
        let runtime = Arc::new(ProgramRuntime::new());
        let spawner = RecordingSpawner::new();
        runtime.attach_agent_scheduler(&spawner);
        let binding = runtime.agent_binding_for_test(None).expect("binding");
        drop(spawner);

        let refused = binding.spawn("work".into()).await;
        let message = refused
            .expect_err("a dropped spawner cannot accept work")
            .to_string();
        assert!(
            message.contains("unavailable"),
            "the error must name the cause, got {message:?}"
        );
    }
}

#[test]
fn runtime_application_abi_json_records_are_byte_stable() {
    assert_eq!(crate::vm::RUNTIME_APPLICATION_ABI_VERSION, 1);
    let execution_id = uuid::Uuid::nil();
    let run = ProgramRun::new(execution_id);
    assert_eq!(
        serde_json::to_string(&run).unwrap(),
        r#"{"abi_version":1,"execution_id":"00000000-0000-0000-0000-000000000000"}"#
    );
    let resume = VmResume {
        execution_id,
        sequence: 0,
        response: VmResumeResponse::Cancelled {
            reason: Some("timeout".into()),
        },
    };
    assert_eq!(
        serde_json::to_string(&resume).unwrap(),
        r#"{"execution_id":"00000000-0000-0000-0000-000000000000","sequence":0,"response":{"kind":"cancelled","reason":"timeout"}}"#
    );
    let handle = OutputHandleRef::new(execution_id, "download", 4);
    assert_eq!(
        serde_json::to_string(&handle).unwrap(),
        r#"{"execution_id":"00000000-0000-0000-0000-000000000000","handle":"download","generation":4}"#
    );
}

#[tokio::test]
async fn delivery_log_observes_awaited_effect_before_local_resume_and_rejects_stale_result() {
    let directory = tempfile::tempdir().unwrap();
    let brain = uuid::Uuid::new_v4();
    let client = DeliveryConsumerIdentity::new(brain, uuid::Uuid::new_v4());
    let log = Arc::new(Mutex::new(
        VmEffectDeliveryLog::open_bound(directory.path().join("effects.jsonl"), brain).unwrap(),
    ));
    let (live, receiver) = typed_effect_channel();
    let sink = bind_delivery_log(Arc::clone(&log), Some(live));
    let runtime = ProgramRuntime::new();
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();

    let pending = runtime
        .submit_with_deferred_host_effects(
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"does-not-need-to-exist.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ),
            sink,
        )
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::Suspended);
    assert_eq!(pending.program_run().execution_id, pending.execution_id);

    let envelope = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("observer must receive the awaited effect");
    assert_eq!(envelope.effect.output, vec![Type::Bytes]);
    {
        let mut log = log.lock().unwrap();
        assert_eq!(
            log.get(envelope.handle()).cloned().as_ref(),
            Some(&envelope),
            "durable delivery must precede observer projection"
        );
        assert!(log
            .acknowledge_identity(
                client,
                DeliveryCursor::through(envelope.execution_id, envelope.effect.sequence)
            )
            .unwrap());
        assert!(log.pending_for(&client).is_empty());
    }

    let stale = runtime
        .resume_vm_effect(VmResume {
            execution_id: envelope.execution_id,
            sequence: envelope.effect.sequence + 1,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Bytes(b"stale".to_vec())],
            },
        })
        .await;
    assert!(
        stale
            .unwrap_err()
            .to_string()
            .contains("stale typed effect resume"),
        "a mismatched sequence must not consume the continuation"
    );
    assert!(runtime
        .pending_typed_execution(pending.execution_id)
        .unwrap()
        .is_some());

    let completed = runtime
        .resume_vm_effect(VmResume {
            execution_id: envelope.execution_id,
            sequence: envelope.effect.sequence,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Bytes(b"embedder bytes".to_vec())],
            },
        })
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(
        completed.values,
        vec![crate::programs::ProgramValue::Bytes(
            b"embedder bytes".to_vec()
        )]
    );
    assert!(runtime
        .pending_typed_execution(pending.execution_id)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn delivery_bound_concurrent_output_handles_survive_disconnect() {
    let directory = tempfile::tempdir().unwrap();
    let brain = uuid::Uuid::new_v4();
    let client = DeliveryConsumerIdentity::new(brain, uuid::Uuid::new_v4());
    let path = directory.path().join("effects.jsonl");
    let log = Arc::new(Mutex::new(
        VmEffectDeliveryLog::open_bound(&path, brain).unwrap(),
    ));
    let (live, receiver) = typed_effect_channel();
    let sink = bind_delivery_log(Arc::clone(&log), Some(live));
    let runtime = ProgramRuntime::new();
    let outcome = runtime
        .submit_with_typed_effect_sink(
            submission(
                ProgramLanguage::Lisp,
                "(let ((download (output-open \"download\")) (log (output-open \"log\")))
                       (begin (output-status download \"starting\")
                              (output-status log \"line\")
                              (output-complete download)
                              (output-complete log)))",
                ExecutionEffect::VmRead,
            ),
            sink,
        )
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);
    let events = receiver.try_iter().collect::<Vec<_>>();
    let sequences: Vec<_> = events
        .iter()
        .map(|envelope| envelope.effect.sequence)
        .collect();
    assert_eq!(
        sequences,
        (0..events.len() as u64).collect::<Vec<_>>(),
        "delivery sequences must be contiguous before ack; events={events:?}"
    );
    let created: Vec<(String, String)> = events
        .iter()
        .filter_map(|envelope| match &envelope.effect.event {
            crate::vm::HostSideEffect::Ui {
                operation: crate::vm::UiOperation::Create,
                text: Some(title),
                target: Some(TypedValue::Resource { handle, .. }),
                ..
            } => Some((title.clone(), handle.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        created.len(),
        2,
        "one Create per output handle; events={events:?}"
    );
    let download = created
        .iter()
        .find(|(title, _)| title == "download")
        .map(|(_, handle)| handle.clone())
        .expect("download handle");
    let log_handle = created
        .iter()
        .find(|(title, _)| title == "log")
        .map(|(_, handle)| handle.clone())
        .expect("log handle");
    let pairs: Vec<_> = events
        .iter()
        .map(|envelope| match &envelope.effect.event {
            crate::vm::HostSideEffect::Ui {
                operation,
                target: Some(TypedValue::Resource { handle, .. }),
                ..
            } => (handle.clone(), *operation),
            other => panic!("expected UI output-handle event, got {other:?}"),
        })
        .collect();
    assert_eq!(
        pairs,
        vec![
            (download.clone(), crate::vm::UiOperation::Create),
            (log_handle.clone(), crate::vm::UiOperation::Create),
            (download.clone(), crate::vm::UiOperation::Status),
            (log_handle.clone(), crate::vm::UiOperation::Status),
            (download, crate::vm::UiOperation::Complete),
            (log_handle, crate::vm::UiOperation::Complete),
        ]
    );
    drop(log);

    let mut reopened = VmEffectDeliveryLog::open_bound(&path, brain).unwrap();
    let handles = reopened.output_handles(outcome.execution_id);
    assert_eq!(
        handles.len(),
        2,
        "concurrent output handles must survive restart; handles={handles:?}"
    );
    let pending = reopened.pending_for(&client);
    assert_eq!(pending.len(), events.len());
    assert!(reopened
        .acknowledge_identity(
            client,
            DeliveryCursor::through(outcome.execution_id, events.last().unwrap().effect.sequence),
        )
        .unwrap());
    assert!(reopened.pending_for(&client).is_empty());
}

fn grant_workspace_file_read(runtime: &ProgramRuntime) {
    runtime
        .grant_typed_capability(crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        ))
        .unwrap();
}

#[tokio::test]
async fn delivery_conflict_fails_observation_instead_of_suspending_unobserved() {
    let directory = tempfile::tempdir().unwrap();
    let brain = uuid::Uuid::new_v4();
    let path = directory.path().join("effects.jsonl");
    let log = Arc::new(Mutex::new(
        VmEffectDeliveryLog::open_bound(&path, brain).unwrap(),
    ));
    let (live, receiver) = typed_effect_channel();
    let bound = bind_delivery_log(Arc::clone(&log), Some(live));
    let sink: TypedEffectSink = {
        let log = Arc::clone(&log);
        Arc::new(move |envelope| {
            let mut conflict = envelope.clone();
            conflict.effect.event = crate::vm::HostSideEffect::Request {
                arguments: vec![TypedValue::String("forged".into())],
            };
            log.lock()
                .unwrap()
                .append(conflict)
                .expect("planted conflict must persist");
            bound(envelope);
        })
    };
    let runtime = ProgramRuntime::new();
    grant_workspace_file_read(&runtime);
    let error = runtime
        .submit_with_deferred_host_effects(
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"does-not-need-to-exist.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ),
            sink,
        )
        .await
        .expect_err("conflict must fail the host observation path");
    assert!(
        error.to_string().contains("conflicting")
            || error.to_string().contains("protocol violation")
            || error.to_string().contains("panicked"),
        "conflict must surface from observation, got {error:#}"
    );
    assert!(
        receiver.try_recv().is_err(),
        "the live observer must not receive a conflicting dispatch"
    );
    let planted = log.lock().unwrap().pending("observer");
    assert_eq!(planted.len(), 1, "only the planted conflict row is durable");
    assert!(
        runtime
            .pending_typed_execution(planted[0].execution_id)
            .unwrap()
            .is_none(),
        "a persist/conflict failure must not leave a suspended run"
    );
}

#[tokio::test]
async fn bound_log_cancel_after_persist_marks_the_journalled_effect() {
    let directory = tempfile::tempdir().unwrap();
    let brain = uuid::Uuid::new_v4();
    let log = Arc::new(Mutex::new(
        VmEffectDeliveryLog::open_bound(directory.path().join("effects.jsonl"), brain).unwrap(),
    ));
    let (live, receiver) = typed_effect_channel();
    let sink = bind_delivery_log(Arc::clone(&log), Some(live));
    let runtime = ProgramRuntime::new();
    grant_workspace_file_read(&runtime);
    let pending = runtime
        .submit_with_deferred_host_effects(
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"does-not-need-to-exist.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ),
            sink,
        )
        .await
        .unwrap();
    let envelope = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("observer must receive the awaited effect before cancel");
    assert_eq!(
        log.lock().unwrap().get(envelope.handle()).cloned().as_ref(),
        Some(&envelope)
    );
    let cancelled = runtime
        .resume_vm_effect(VmResume {
            execution_id: envelope.execution_id,
            sequence: envelope.effect.sequence,
            response: VmResumeResponse::Cancelled {
                reason: Some("timeout".into()),
            },
        })
        .await
        .unwrap();
    assert_eq!(cancelled.status, ExecutionStatus::Cancelled);
    assert!(matches!(
        cancelled.effect_journal.last(),
        Some(crate::vm::EffectJournalEntry {
            state: crate::vm::EffectJournalState::Cancelled,
            ..
        })
    ));
    assert!(runtime
        .pending_typed_execution(pending.execution_id)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn late_vm_resume_after_observer_disconnect_replays_from_reopened_log() {
    let directory = tempfile::tempdir().unwrap();
    let brain = uuid::Uuid::new_v4();
    let path = directory.path().join("effects.jsonl");
    let log = Arc::new(Mutex::new(
        VmEffectDeliveryLog::open_bound(&path, brain).unwrap(),
    ));
    let (live, receiver) = typed_effect_channel();
    let sink = bind_delivery_log(Arc::clone(&log), Some(live));
    let runtime = ProgramRuntime::new();
    grant_workspace_file_read(&runtime);
    let pending = runtime
        .submit_with_deferred_host_effects(
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"does-not-need-to-exist.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ),
            sink,
        )
        .await
        .unwrap();
    let envelope = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("observer must receive the awaited effect");
    drop(receiver);
    drop(log);
    let reopened = VmEffectDeliveryLog::open_bound(&path, brain).unwrap();
    assert_eq!(
        reopened.get(envelope.handle()).cloned().as_ref(),
        Some(&envelope),
        "reopening the log must replay the persisted envelope after observer drop"
    );
    let completed = runtime
        .resume_vm_effect(VmResume {
            execution_id: envelope.execution_id,
            sequence: envelope.effect.sequence,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Bytes(b"late resume".to_vec())],
            },
        })
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    assert_eq!(
        completed.values,
        vec![crate::programs::ProgramValue::Bytes(
            b"late resume".to_vec()
        )]
    );
    assert!(runtime
        .pending_typed_execution(pending.execution_id)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn bound_log_all_awaited_output_open_registers_the_resumed_handle() {
    let directory = tempfile::tempdir().unwrap();
    let brain = uuid::Uuid::new_v4();
    let log = Arc::new(Mutex::new(
        VmEffectDeliveryLog::open_bound(directory.path().join("effects.jsonl"), brain).unwrap(),
    ));
    let (live, receiver) = typed_effect_channel();
    let sink = bind_delivery_log(Arc::clone(&log), Some(live));
    let runtime = ProgramRuntime::new();
    let pending = runtime
        .submit_with_deferred_host_effects(
            submission(
                ProgramLanguage::Lisp,
                "(let ((handle (output-open \"download\"))) \
                       (begin (output-status handle \"starting\") \
                              (output-complete handle)))",
                ExecutionEffect::VmRead,
            ),
            sink,
        )
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::Suspended);
    let open = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("portable output-open request");
    assert_eq!(
        log.lock().unwrap().get(open.handle()).cloned().as_ref(),
        Some(&open)
    );
    let completed = runtime
        .resume_vm_effect(VmResume {
            execution_id: open.execution_id,
            sequence: open.effect.sequence,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Resource {
                    kind: "output-handle".into(),
                    handle: "portable-download".into(),
                    generation: 7,
                }],
            },
        })
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
    let updates = receiver.try_iter().collect::<Vec<_>>();
    assert_eq!(
        updates
            .iter()
            .map(|envelope| envelope.effect.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let handles = log.lock().unwrap().output_handles(open.execution_id);
    assert_eq!(
        handles,
        vec![OutputHandleRef::new(
            open.execution_id,
            "portable-download",
            7
        )]
    );
}

#[tokio::test]
async fn program_runtime_bound_log_is_the_production_observation_sink() {
    let directory = tempfile::tempdir().unwrap();
    let brain = uuid::Uuid::new_v4();
    let client = DeliveryConsumerIdentity::new(brain, uuid::Uuid::new_v4());
    let log = Arc::new(Mutex::new(
        VmEffectDeliveryLog::open_bound(directory.path().join("effects.jsonl"), brain).unwrap(),
    ));
    let (live, receiver) = typed_effect_channel();
    let runtime = ProgramRuntime::new();
    runtime.bind_effect_delivery_log(Arc::clone(&log)).unwrap();
    grant_workspace_file_read(&runtime);
    let pending = runtime
        .submit_with_deferred_host_effects(
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"does-not-need-to-exist.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ),
            live,
        )
        .await
        .unwrap();
    assert_eq!(pending.status, ExecutionStatus::Suspended);
    let envelope = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("production bound log must project after persist");
    assert_eq!(
        log.lock().unwrap().get(envelope.handle()).cloned().as_ref(),
        Some(&envelope),
        "ProgramRuntime must persist before the live observer"
    );
    assert_eq!(envelope.effect.output, vec![Type::Bytes]);
    {
        let mut log = log.lock().unwrap();
        assert!(log
            .acknowledge_identity(
                client,
                DeliveryCursor::through(envelope.execution_id, envelope.effect.sequence)
            )
            .unwrap());
        assert!(log.pending_for(&client).is_empty());
    }
    let stale = runtime
        .resume_vm_effect(VmResume {
            execution_id: envelope.execution_id,
            sequence: envelope.effect.sequence + 1,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Bytes(b"stale".to_vec())],
            },
        })
        .await;
    assert!(
        stale
            .unwrap_err()
            .to_string()
            .contains("stale typed effect resume"),
        "a mismatched sequence must not consume the continuation"
    );
    let completed = runtime
        .resume_vm_effect(VmResume {
            execution_id: envelope.execution_id,
            sequence: envelope.effect.sequence,
            response: VmResumeResponse::Result {
                values: vec![TypedValue::Bytes(b"embedder bytes".to_vec())],
            },
        })
        .await
        .unwrap();
    assert_eq!(completed.status, ExecutionStatus::Completed);
}

#[tokio::test]
async fn program_runtime_bound_log_persists_when_caller_omits_a_sink() {
    let directory = tempfile::tempdir().unwrap();
    let brain = uuid::Uuid::new_v4();
    let log = Arc::new(Mutex::new(
        VmEffectDeliveryLog::open_bound(directory.path().join("effects.jsonl"), brain).unwrap(),
    ));
    let runtime = ProgramRuntime::new();
    runtime.bind_effect_delivery_log(Arc::clone(&log)).unwrap();
    let outcome = runtime
        .submit_typed_only(submission(
            ProgramLanguage::Lisp,
            "(let ((handle (output-open \"download\"))) (output-complete handle))",
            ExecutionEffect::VmRead,
        ))
        .await
        .unwrap();
    assert_eq!(outcome.status, ExecutionStatus::Completed);
    let pending = log.lock().unwrap().pending("observer");
    assert!(
        pending
            .iter()
            .any(|envelope| envelope.execution_id == outcome.execution_id),
        "bound production log must persist even without a caller sink; pending={pending:?}"
    );
}

#[test]
fn runtime_facade_keeps_child_modules_private() {
    let facade = include_str!("mod.rs");
    let published = facade
        .lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//"))
        .filter(|line| line.starts_with("pub mod "))
        .collect::<Vec<_>>();
    assert!(
        published.is_empty(),
        "runtime facade must keep child modules private; found: {published:?}"
    );
}

#[test]
fn runtime_callers_use_facade_not_child_modules() {
    let children = [
        "abi",
        "agent_vm",
        "agents",
        "archive_store",
        "automation",
        "context",
        "effect_audit",
        "effect_log",
        "host",
        "hostio",
        "mcp",
        "outcome",
    ];
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runtime = root.join("src/runtime");
    let mut hits = Vec::new();
    for tree in ["src", "tests"] {
        collect_runtime_child_imports(&root.join(tree), &root, &runtime, &children, &mut hits);
    }
    assert!(
        hits.is_empty(),
        "callers outside src/runtime must use crate::runtime::Item, not child modules; found: {hits:?}"
    );
}

#[test]
fn runtime_facade_reexports_caller_types() {
    let _ = std::any::type_name::<ExecutionStatus>();
    let _ = std::any::type_name::<ExecutionOutcome>();
    let _ = std::any::type_name::<ExecutionBackend>();
    let _ = std::any::type_name::<EffectAuditIdentity>();
    let _ = std::any::type_name::<ProgramRuntimeAuthorityStore>();
    let _ = std::any::type_name::<AutomationBroker>();
    let _ = std::any::type_name::<AgentTaskSpec>();
    let _ = std::any::type_name::<RunnerEffectAuditControl>();
    let _ = std::any::type_name::<ProgramRun>();
    let _ = std::any::type_name::<DeliveryConsumerIdentity>();
    let _ = std::any::type_name::<DeliveryCursor>();
    let _ = std::any::type_name::<OutputHandleRef>();
    let _ = std::any::type_name::<RuntimeApplicationMessage>();
    let _ = MAX_ACTIVE_EFFECT_AUDITS_PER_RUN;
    let _ = permission_context_key();
}

fn collect_runtime_child_imports(
    dir: &Path,
    root: &Path,
    runtime: &Path,
    children: &[&str],
    hits: &mut Vec<String>,
) {
    if dir.starts_with(runtime) {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            hits.push(format!("failed to read {}: {error}", dir.display()));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_runtime_child_imports(&path, root, runtime, children, hits);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            hits.push(format!("failed to read {}", path.display()));
            continue;
        };
        for (index, line) in source.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for child in children {
                for prefix in ["crate::runtime::", "finch::runtime::"] {
                    let needle = [prefix, child, "::"].concat();
                    if line.contains(&needle) {
                        let rel = path.strip_prefix(root).unwrap_or(&path);
                        hits.push(format!("{}:{}", rel.display(), index + 1));
                    }
                }
            }
        }
    }
}
