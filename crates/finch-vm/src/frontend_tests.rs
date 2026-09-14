use crate::interpreter::{DenyCapabilities, Interpreter};
use crate::*;
use finch_language::{compile_forth, compile_lisp};

#[test]
fn wire_failure_classifier_covers_every_stable_class() {
    let cases = [
        (
            "Hello there",
            "E-WIRE-001: prose",
            WireFailureClass::RawProse,
        ),
        (
            "```lisp",
            "E-WIRE-002: fenced",
            WireFailureClass::MarkdownFence,
        ),
        (
            "invented",
            "E-LINK-002: unknown word",
            WireFailureClass::InventedWord,
        ),
        (
            "1 +",
            "E-STACK-001: underflow",
            WireFailureClass::StackOrType,
        ),
        (
            "(say \"hi\")",
            "E-LINK-002: expected Co-Forth",
            WireFailureClass::WrongLanguageDispatch,
        ),
        (
            "read-file",
            "E-CAP-003: denied",
            WireFailureClass::Capability,
        ),
        ("anything", "E-RUNTIME-001: failed", WireFailureClass::Other),
    ];
    for (source, diagnostic, expected) in cases {
        assert_eq!(
            classify_wire_failure(source, diagnostic),
            expected,
            "wire failure classification changed for source={source:?}, diagnostic={diagnostic:?}"
        );
    }
}

#[test]
fn wire_failure_class_serialization_remains_snake_case() {
    let variants = [
        (WireFailureClass::RawProse, "raw_prose"),
        (WireFailureClass::MarkdownFence, "markdown_fence"),
        (WireFailureClass::InventedWord, "invented_word"),
        (WireFailureClass::StackOrType, "stack_or_type"),
        (
            WireFailureClass::WrongLanguageDispatch,
            "wrong_language_dispatch",
        ),
        (
            WireFailureClass::MissingOutputEffect,
            "missing_output_effect",
        ),
        (WireFailureClass::Capability, "capability"),
        (WireFailureClass::Other, "other"),
    ];
    for (class, encoded) in variants {
        assert_eq!(
            serde_json::to_string(&class).unwrap(),
            format!("\"{encoded}\"")
        );
        assert_eq!(
            serde_json::from_str::<WireFailureClass>(&format!("\"{encoded}\"")).unwrap(),
            class,
            "wire failure class {encoded} must remain round-trippable"
        );
    }
}

mod forth {
    use super::*;

    #[test]
    fn compiles_and_executes_user_forth_text() {
        let module =
            compile_forth("input.forth", "3 4 2 * +", Vec::new(), &core_vocabulary()).unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(11)]);
    }

    #[test]
    fn result_question_mark_returns_error_without_running_the_rest_of_a_word() {
        let module = compile_forth(
            "try.forth",
            ": fail-fast ( S -- S result<dynamic,string> ! pure ) \
             s\" no\" err ? drop s\" unreachable\" err ; fail-fast",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("typed result propagation compiles");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("error result is an ordinary return, not a VM failure");
        assert_eq!(
            stack,
            vec![TypedValue::Result {
                ok_type: Type::Dynamic,
                error_type: Type::String,
                is_ok: false,
                value: Box::new(TypedValue::String("no".into())),
            }]
        );
    }

    #[test]
    fn result_question_mark_continues_with_the_ok_payload() {
        let module = compile_forth(
            "try-ok.forth",
            ": keep-going ( S -- S result<int,dynamic> ! pure ) \
             7 ok ? 1 + ok ; keep-going",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("successful result propagation compiles");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert!(matches!(
            stack.as_slice(),
            [TypedValue::Result { is_ok: true, value, .. }] if **value == TypedValue::Int(8)
        ));
    }

    #[test]
    fn string_literals_are_streamable_through_explicit_say() {
        let module = compile_forth(
            "input.forth",
            "s\"Hello \\\"世界\\\"\" say 3 5 + int-to-string say s\"! \" say",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        #[derive(Default)]
        struct EmitHandler(String);
        impl crate::interpreter::CapabilityHandler for EmitHandler {
            fn request(
                &mut self,
                requirement: &CapabilityRequirement,
                arguments: Vec<TypedValue>,
                _origin: &SourceOrigin,
            ) -> Result<Vec<TypedValue>, VmDiagnostic> {
                assert_eq!(requirement.capability, CapabilityKind::SessionEmit);
                let [TypedValue::String(text)] = arguments.as_slice() else {
                    panic!("expected string emission");
                };
                self.0.push_str(text);
                Ok(vec![TypedValue::Unit])
            }
            fn output(&self) -> String {
                self.0.clone()
            }
        }
        let mut handler = EmitHandler::default();
        Interpreter::new(
            &module,
            &mut handler,
            InterpreterConfig {
                fuel: 100_000,
                grants: EffectSet::from_requirement(CapabilityRequirement {
                    capability: CapabilityKind::SessionEmit,
                    selector: ResourceSelector::None,
                }),
            },
        )
        .execute(&mut stack)
        .unwrap();
        assert_eq!(handler.output(), "Hello \"世界\"8! ");
    }

    #[test]
    fn bare_and_standard_forth_string_literals_push_the_same_typed_value() {
        for source in ["\"hello there\"", "s\" hello there\""] {
            let module = compile_forth("strings.forth", source, Vec::new(), &core_vocabulary())
                .expect("typed string literal should compile");
            let mut stack = Vec::new();
            Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
                .execute(&mut stack)
                .expect("typed string literal should execute");
            assert_eq!(stack, vec![TypedValue::String("hello there".into())]);
        }
    }

    #[test]
    fn typed_frontend_lowers_standard_output_literal_to_say() {
        let module = compile_forth(
            "input.forth",
            ".\" legacy output\"",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("typed Co-Forth accepts standard output literal");
        #[derive(Default)]
        struct EmitHandler(String);
        impl crate::interpreter::CapabilityHandler for EmitHandler {
            fn request(
                &mut self,
                requirement: &CapabilityRequirement,
                arguments: Vec<TypedValue>,
                _origin: &SourceOrigin,
            ) -> Result<Vec<TypedValue>, VmDiagnostic> {
                assert_eq!(requirement.capability, CapabilityKind::SessionEmit);
                let [TypedValue::String(text)] = arguments.as_slice() else {
                    panic!("expected string emission");
                };
                self.0.push_str(text);
                Ok(vec![TypedValue::Unit])
            }
            fn output(&self) -> String {
                self.0.clone()
            }
        }
        let mut stack = Vec::new();
        let mut handler = EmitHandler::default();
        Interpreter::new(
            &module,
            &mut handler,
            InterpreterConfig {
                fuel: 100_000,
                grants: EffectSet::from_requirement(CapabilityRequirement {
                    capability: CapabilityKind::SessionEmit,
                    selector: ResourceSelector::None,
                }),
            },
        )
        .execute(&mut stack)
        .unwrap();
        assert_eq!(handler.output(), "legacy output");
    }

    #[test]
    fn named_break_and_continue_lower_to_typed_loop_edges() {
        let break_module = compile_forth(
            "break.forth",
            "0 begin: search dup 3 < while 1 + dup 2 = if break search then repeat",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut break_stack = Vec::new();
        Interpreter::new(
            &break_module,
            DenyCapabilities,
            InterpreterConfig::default(),
        )
        .execute(&mut break_stack)
        .unwrap();
        assert_eq!(break_stack, vec![TypedValue::Int(2)]);

        let continue_module = compile_forth(
            "continue.forth",
            "0 begin: count dup 3 < while 1 + dup 2 = if continue count then repeat",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut continue_stack = Vec::new();
        Interpreter::new(
            &continue_module,
            DenyCapabilities,
            InterpreterConfig::default(),
        )
        .execute(&mut continue_stack)
        .unwrap();
        assert_eq!(continue_stack, vec![TypedValue::Int(3)]);
    }

    #[test]
    fn if_ok_binds_each_result_payload_on_its_selected_edge() {
        let ok_module = compile_forth(
            "ok.forth",
            "5 ok if-ok drop else drop then",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let err_module = compile_forth(
            "err.forth",
            "s\"bad\" err if-ok drop else drop then",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        for module in [ok_module, err_module] {
            let mut stack = Vec::new();
            Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
                .execute(&mut stack)
                .unwrap();
            assert!(stack.is_empty());
        }
    }

    #[test]
    fn typed_integer_case_has_no_fallthrough_and_requires_compatible_arms() {
        let selected = compile_forth(
            "case-selected.forth",
            "2 case 1 of 10 endof 2 of 20 endof otherwise 30 endcase",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("integer case should compile");
        let defaulted = compile_forth(
            "case-default.forth",
            "3 case 1 of 10 endof 2 of 20 endof otherwise 30 endcase",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("integer case with otherwise should compile");
        for (module, expected) in [(selected, 20), (defaulted, 30)] {
            let mut stack = Vec::new();
            Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
                .execute(&mut stack)
                .unwrap();
            assert_eq!(stack, vec![TypedValue::Int(expected)]);
        }

        let effect_only = compile_forth(
            "case-effect.forth",
            "1 case 1 of endof endcase",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("a case without otherwise may leave no values on every path");
        let mut stack = Vec::new();
        Interpreter::new(&effect_only, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert!(stack.is_empty());

        let mismatch = compile_forth(
            "case-mismatch.forth",
            "1 case 1 of 10 endof otherwise endcase",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("every case path must leave the same stack row");
        assert_eq!(mismatch[0].code, "E-STACK-004");

        let non_integer = compile_forth(
            "case-string.forth",
            "s\" selector\" case 1 of 10 endof otherwise 20 endcase",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("case selectors are intentionally integer-only in version 1");
        assert_eq!(non_integer[0].code, "E-TYPE-002");
    }

    #[test]
    fn constructs_and_projects_heterogeneous_typed_records() {
        let module = compile_forth(
            "record.forth",
            "{ name: \"Ada\" age: 37 } \"name\" record-get unwrap",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("record literal should compile");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("record projection should execute");
        assert_eq!(stack, vec![TypedValue::String("Ada".into())]);

        let invalid = compile_forth(
            "record-invalid.forth",
            "{ name: \"Ada\" } \"age\" record-get",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("record fields are statically known");
        assert_eq!(invalid[0].code, "E-RECORD-005");

        let updated = compile_forth(
            "record-update.forth",
            "{ name: \"Ada\" age: 37 } 38 \"age\" record-set \"age\" record-get unwrap",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("record update should compile");
        let mut stack = Vec::new();
        Interpreter::new(&updated, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("record update should execute");
        assert_eq!(stack, vec![TypedValue::Int(38)]);

        let closure_field = compile_forth(
            "record-closure.forth",
            ": increment ( S int -- S int ! pure ) 1 + ; { run: ['] increment } \"run\" record-get unwrap 41 swap execute",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("record closure should compile");
        let mut stack = Vec::new();
        Interpreter::new(
            &closure_field,
            DenyCapabilities,
            InterpreterConfig::default(),
        )
        .execute(&mut stack)
        .expect("record closure should execute");
        assert_eq!(stack, vec![TypedValue::Int(42)]);
    }

    #[test]
    fn result_error_projects_the_error_of_a_heterogeneous_result() {
        let module = compile_forth(
            "result-error.forth",
            "s\"bad\" err result-error",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::String("bad".into())]);
    }

    #[test]
    fn compiles_against_preexisting_typed_stack() {
        let module =
            compile_forth("input.forth", "2 *", vec![Type::Int], &core_vocabulary()).unwrap();
        let mut stack = vec![TypedValue::Int(9)];
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(18)]);
    }

    #[test]
    fn typed_forth_named_signature_inputs_lower_to_explicit_frame_operations() {
        let module = compile_forth(
            "named-signature.forth",
            ": area ( S width:int height:int -- S int ! pure ) width height * ; 4 3 area",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let area = &module.module.functions["area"];
        assert_eq!(area.locals, vec![Type::Int, Type::Int]);
        let instructions = &area.blocks[&area.entry].instructions;
        assert!(matches!(
            instructions[0].instruction,
            Instruction::LocalSet { index: 1 }
        ));
        assert!(matches!(
            instructions[1].instruction,
            Instruction::LocalSet { index: 0 }
        ));
        assert!(matches!(
            instructions[2].instruction,
            Instruction::LocalGet { index: 0 }
        ));
        assert!(matches!(
            instructions[3].instruction,
            Instruction::LocalGet { index: 1 }
        ));

        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(12)]);
    }

    #[test]
    fn typed_forth_definitions_can_recursively_call_themselves() {
        let module = compile_forth(
            "factorial.forth",
            r#"
: factorial ( S n:int -- S int ! pure )
  n 1 <= if
    1
  else
    n n 1 - factorial *
  then ;
6 factorial
"#,
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("a declared-pure Co-Forth word should be able to recurse");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("recursive Co-Forth program should execute");
        assert_eq!(stack, vec![TypedValue::Int(720)]);
    }

    #[test]
    fn typed_forth_quotation_executes_a_persistent_word() {
        let module = compile_forth(
            "quotation.forth",
            ": square ( S int -- S int ! pure ) dup * ; 9 ['] square execute",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("a typed Co-Forth quotation should link to its definition");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("typed Co-Forth execute should call its quotation");
        assert_eq!(stack, vec![TypedValue::Int(81)]);
    }

    #[test]
    fn anonymous_quotation_lowers_to_the_shared_closure_ir() {
        let module = compile_forth(
            "anonymous-quotation.forth",
            "41 [ int -- int ! pure | 1 + ] execute",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("typed anonymous quotation should compile");
        let quote = module
            .module
            .functions
            .values()
            .find(|function| function.name.starts_with("quote$"))
            .expect("quotation lowering creates a typed hidden function");
        assert!(quote.captures.is_empty());
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("anonymous quotation should execute");
        assert_eq!(stack, vec![TypedValue::Int(42)]);
    }

    #[test]
    fn anonymous_quotation_captures_immutable_forth_locals() {
        let source = ": add-offset ( S offset:int value:int -- S int ! pure ) \
            value [ int -- int ! pure | offset + ] execute ; \
            3 39 add-offset";
        let module = compile_forth(
            "captured-quotation.forth",
            source,
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("quotation should capture its enclosing typed locals");
        let quote = module
            .module
            .functions
            .values()
            .find(|function| function.name.starts_with("quote$"))
            .expect("captured quotation hidden function");
        assert_eq!(quote.captures, vec![Type::Int, Type::Int]);
        assert!(quote.blocks.values().any(|block| block
            .instructions
            .iter()
            .any(|located| matches!(located.instruction, Instruction::CaptureGet { index: 0 }))));
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("captured quotation should execute");
        assert_eq!(stack, vec![TypedValue::Int(42)]);
    }

    #[test]
    fn captured_anonymous_quotation_can_escape_its_defining_frame() {
        let source = ": make-adder ( S offset:int -- S fn<int,int> ! pure ) \
            [ int -- int ! pure | offset + ] ; \
            39 3 make-adder execute";
        let module = compile_forth(
            "escaping-quotation.forth",
            source,
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("a declared function type should let a closure escape its frame");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("escaped closure owns its immutable capture");
        assert_eq!(stack, vec![TypedValue::Int(42)]);
    }

    #[test]
    fn pure_is_the_only_pure_effect_annotation() {
        let module = compile_forth(
            "pure.forth",
            ": preferred ( S int -- S int ! pure ) 1 + ; 41 preferred",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("pure effect annotation should compile");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("pure word should execute");
        assert_eq!(stack, vec![TypedValue::Int(42)]);

        let errors = compile_forth(
            "pure.forth",
            ": obsolete ( S int -- S int ! {} ) 1 + ;",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("braces are no longer an effect annotation");
        assert_eq!(errors[0].code, "E-FORTH-SIG-001");
    }

    #[test]
    fn constructs_closed_variant_values() {
        let module = compile_forth(
            "variant.forth",
            "42 variant<variant{none|some(int)},some>",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("payload variant constructor should compile");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("payload variant constructor should execute");
        assert_eq!(
            stack,
            vec![TypedValue::Variant {
                name: "some".into(),
                value: Some(Box::new(TypedValue::Int(42))),
            }]
        );

        let module = compile_forth(
            "variant-unit.forth",
            "variant<variant{none|some(int)},none>",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("payload-free variant constructor should compile");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("payload-free variant constructor should execute");
        assert_eq!(
            stack,
            vec![TypedValue::Variant {
                name: "none".into(),
                value: None,
            }]
        );
    }

    #[test]
    fn safely_projects_closed_variant_tags() {
        let module = compile_forth(
            "variant-get.forth",
            "42 variant<variant{none|some(int)},some> variant-get<some> unwrap",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("variant projection should compile");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("variant projection should execute");
        assert_eq!(stack, vec![TypedValue::Int(42)]);

        let module = compile_forth(
            "variant-miss.forth",
            "42 variant<variant{none|some(int)},some> variant-get<none> is-some",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("variant miss should compile");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("variant miss should execute");
        assert_eq!(stack, vec![TypedValue::Bool(false)]);
    }

    #[test]
    fn compiles_begin_until_loop() {
        let module = compile_forth(
            "input.forth",
            "begin true until 7",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(7)]);
    }

    #[test]
    fn infinite_loop_exhausts_fuel_and_rolls_back() {
        let module = compile_forth(
            "input.forth",
            "begin false until",
            vec![Type::Int],
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = vec![TypedValue::Int(42)];
        let error = Interpreter::new(
            &module,
            DenyCapabilities,
            InterpreterConfig {
                fuel: 10,
                ..InterpreterConfig::default()
            },
        )
        .execute(&mut stack)
        .unwrap_err();
        assert_eq!(error.code, "E-LIMIT-001");
        assert_eq!(stack, vec![TypedValue::Int(42)]);
    }

    #[test]
    fn compiles_typed_if_else_then() {
        let module = compile_forth(
            "input.forth",
            "true if 10 else 20 then",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(10)]);
    }

    #[test]
    fn quoted_word_produces_a_typed_symbol_value() {
        let module = compile_forth("input.forth", "'bash", Vec::new(), &core_vocabulary()).unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Symbol("bash".into())]);
    }

    #[test]
    fn constructs_and_uses_typed_map_literals() {
        let module = compile_forth(
            "input.forth",
            "map{ s\" answer\" 42 s\" other\" 7 }map s\" answer\" map-get unwrap",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(42)]);
    }

    #[test]
    fn constructs_and_appends_typed_list_literals() {
        let module = compile_forth(
            "input.forth",
            "[ 1 2 ] 3 list-append 2 list-get",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(3)]);
    }

    #[test]
    fn accepts_comma_separated_lists_and_pasted_json_objects() {
        let list = compile_forth(
            "input.forth",
            "[1, 2, 3] 2 list-get",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&list, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(3)]);

        let json = compile_forth(
            "input.forth",
            "{\"first name\":\"Ada\",\"age\":37} \"first name\" json-get unwrap json-as-string unwrap",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&json, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::String("Ada".into())]);
    }

    #[test]
    fn constructs_an_explicitly_typed_empty_map() {
        let module = compile_forth(
            "input.forth",
            "empty-map<string,int> s\" answer\" 42 map-set s\" answer\" map-get unwrap",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(42)]);
    }

    #[test]
    fn retains_nested_type_arguments_in_empty_collections() {
        let module = compile_forth(
            "input.forth",
            "empty-list<resource<capability-grant>>",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(
            stack,
            vec![TypedValue::List {
                element_type: Type::Resource("capability-grant".into()),
                values: Vec::new(),
            }]
        );
    }

    #[test]
    fn raw_string_literal_preserves_quotes_and_newlines_without_escaping() {
        let module = compile_forth(
            "input.forth",
            "s\"\"\"The user said \"hello\".\nSecond line.\"\"\"",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(
            stack,
            vec![TypedValue::String(
                "The user said \"hello\".\nSecond line.".into()
            )]
        );
    }

    #[test]
    fn bare_raw_string_literal_preserves_quotes_and_newlines_without_escaping() {
        let module = compile_forth(
            "input.forth",
            "\"\"\"The user said \"hello\".\nSecond line.\"\"\"",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(
            stack,
            vec![TypedValue::String(
                "The user said \"hello\".\nSecond line.".into()
            )]
        );
    }

    #[test]
    fn reads_json_object_fields_through_the_shared_typed_vocabulary() {
        let module = compile_forth(
            "input.forth",
            "s\" {\\\"answer\\\":42}\" json-parse result-unwrap s\" answer\" json-get unwrap json-as-int unwrap",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("typed Co-Forth compiles JSON field access");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("typed Co-Forth executes JSON field access");
        assert_eq!(stack, vec![TypedValue::Int(42)]);

        let float = compile_forth(
            "input.forth",
            "s\" 3.5\" json-parse result-unwrap json-as-float unwrap",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("typed Co-Forth compiles JSON float access");
        let mut stack = Vec::new();
        Interpreter::new(&float, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("typed Co-Forth executes JSON float access");
        assert_eq!(stack, vec![TypedValue::Float(3.5)]);
    }
}

mod lisp {
    use super::*;

    fn run(source: &str) -> Result<Vec<TypedValue>, Vec<VmDiagnostic>> {
        let module = compile_lisp("input.lisp", source, Vec::new(), &core_vocabulary())?;
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .map_err(|error| vec![error])?;
        Ok(stack)
    }

    #[test]
    fn lowers_nested_lisp_without_generating_forth_text() {
        assert_eq!(run("(+ 3 (* 4 2))").unwrap(), vec![TypedValue::Int(11)]);
    }

    #[test]
    fn lowers_lexical_let_to_typed_locals() {
        assert_eq!(
            run("(let ((a 10) (b 5)) (- a b))").unwrap(),
            vec![TypedValue::Int(5)]
        );
    }

    #[test]
    fn lowers_variadic_lisp_str_cat_to_binary_calls() {
        assert_eq!(
            run("(str-cat \"one\" \"-\" \"two\" \"-\" \"three\")").unwrap(),
            vec![TypedValue::String("one-two-three".into())]
        );
    }

    #[test]
    fn lowers_typed_closure_with_captured_environment() {
        assert_eq!(
            run("(let ((n 10)) ((lambda ((x : int)) (+ x n)) 5))").unwrap(),
            vec![TypedValue::Int(15)]
        );
    }

    #[test]
    fn expands_bounded_capture_free_syntax_templates_before_type_checking() {
        assert_eq!(
            run("(define-syntax (when test body) (if test body 0)) (when true 42)").unwrap(),
            vec![TypedValue::Int(42)]
        );
    }

    #[test]
    fn syntax_templates_compose_with_a_bounded_expansion_budget() {
        assert_eq!(
            run("(define-syntax (inc value) (+ value 1)) \
                 (define-syntax (twice value) (inc (inc value))) \
                 (twice 40)")
            .unwrap(),
            vec![TypedValue::Int(42)]
        );
    }

    #[test]
    fn compiles_a_recursively_typed_function() {
        assert_eq!(
            run("(define (factorial (n : int)) : int \
                   (if (<= n 1) 1 (* n (factorial (- n 1))))) \
                 (factorial 6)")
            .unwrap(),
            vec![TypedValue::Int(720)]
        );
    }

    #[test]
    fn top_level_begin_can_group_a_definition_and_its_first_use() {
        assert_eq!(
            run("(begin
                    (define (factorial (n : int)) : int
                      (if (<= n 1) 1 (* n (factorial (- n 1)))))
                    (factorial 6))")
            .unwrap(),
            vec![TypedValue::Int(720)]
        );
    }

    #[test]
    fn retains_lisp_definition_docstrings_as_non_executable_ir_metadata() {
        let module = compile_lisp(
            "input.lisp",
            "(define (double (n : int)) : int \"Return twice n.\" (* n 2)) (double 21)",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        assert_eq!(
            module.module.functions["double"].documentation.as_deref(),
            Some("Return twice n.")
        );

        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(42)]);
    }

    #[test]
    fn constructs_and_uses_homogeneous_typed_lists() {
        assert_eq!(
            run("(list-get (list 4 8 15 16) 2)").unwrap(),
            vec![TypedValue::Int(15)]
        );
        assert_eq!(
            run("(list-length (list \"a\" \"b\"))").unwrap(),
            vec![TypedValue::Int(2)]
        );
        assert_eq!(
            run("(unwrap (record-get (unwrap (list-uncons (list 4 8))) \"head\"))").unwrap(),
            vec![TypedValue::Int(4)]
        );
        assert_eq!(
            run("(list-length (unwrap (record-get (unwrap (list-uncons (list 4 8))) \"tail\")))")
                .unwrap(),
            vec![TypedValue::Int(1)]
        );
        assert_eq!(
            run("(is-some (list-uncons (empty-list int)))").unwrap(),
            vec![TypedValue::Bool(false)]
        );
        assert_eq!(
            run("(match (list 4 8) (empty 0) (cons head tail (+ head (list-length tail))))")
                .unwrap(),
            vec![TypedValue::Int(5)]
        );
        assert_eq!(
            run("(match (empty-list int) (empty 42) (cons head tail (begin tail head)))").unwrap(),
            vec![TypedValue::Int(42)]
        );

        let mismatched_arms = compile_lisp(
            "list-pattern.lisp",
            "(match (list 1) (empty \"empty\") (cons head tail head))",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("list pattern arms must merge to one type");
        assert!(mismatched_arms
            .iter()
            .any(|error| error.code == "E-TYPE-002"));
    }

    #[test]
    fn try_returns_an_err_from_the_enclosing_typed_definition() {
        let stack = run("(define (fail-fast) : result<dynamic,string> \
                (begin (try (err \"no\")) (err \"unreachable\"))) \
             (fail-fast)")
        .expect("try must compile as typed result propagation");
        assert_eq!(
            stack,
            vec![TypedValue::Result {
                ok_type: Type::Dynamic,
                error_type: Type::String,
                is_ok: false,
                value: Box::new(TypedValue::String("no".into())),
            }]
        );
    }

    #[test]
    fn try_continues_with_an_ok_payload() {
        let source = "(define (keep-going) : result<int,string> \
                      (begin (try (ok 7)) (ok 8))) \
                      (keep-going)";
        let module = compile_lisp("try-ok.lisp", source, Vec::new(), &core_vocabulary())
            .expect("successful try must leave the unwrapped payload for the next expression");
        assert_eq!(
            module.module.functions["keep-going"]
                .signature
                .output
                .values,
            vec![Type::result(Type::Int, Type::String)],
            "the declared result contract remains visible to callers"
        );
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert!(matches!(
            stack.as_slice(),
            [TypedValue::Result { is_ok: true, value, .. }] if **value == TypedValue::Int(8)
        ));
    }

    #[test]
    fn constructs_and_uses_immutable_typed_maps() {
        assert_eq!(
            run("(unwrap (map-get (map \"answer\" 42 \"other\" 7) \"answer\"))").unwrap(),
            vec![TypedValue::Int(42)]
        );
        assert_eq!(
            run("(unwrap (map-get (map-set (map \"answer\" 42) \"answer\" 99) \"answer\"))")
                .unwrap(),
            vec![TypedValue::Int(99)]
        );
        assert_eq!(
            run("(map-length (map \"a\" 1 \"a\" 2))").unwrap(),
            vec![TypedValue::Int(1)]
        );
        assert_eq!(
            run("(unwrap (map-get (map-set (empty-map string int) \"answer\" 42) \"answer\"))")
                .unwrap(),
            vec![TypedValue::Int(42)]
        );
    }

    #[test]
    fn constructs_and_projects_heterogeneous_typed_records() {
        assert_eq!(
            run("(unwrap (record-get { :name \"Ada\" :age 37 } \"age\"))").unwrap(),
            vec![TypedValue::Int(37)]
        );
        let missing = compile_lisp(
            "record.lisp",
            "(record-get { :name \"Ada\" } \"age\")",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("record fields are statically known");
        assert_eq!(missing[0].code, "E-RECORD-005");
        assert_eq!(
            run("(unwrap (record-get (record-set { :name \"Ada\" :age 37 } \"age\" 38) \"age\"))")
                .unwrap(),
            vec![TypedValue::Int(38)]
        );
        assert_eq!(
            run(
                "(let ((object { :run (lambda ((x : int)) (+ x 1)) })) ((unwrap (record-get object \"run\")) 41))",
            )
            .unwrap(),
            vec![TypedValue::Int(42)]
        );
        assert_eq!(
            run("(match { :name \"Ada\" :age 37 } \
                   (record ((name who) (age years)) \
                     (begin who (+ years 5))))",)
            .unwrap(),
            vec![TypedValue::Int(42)]
        );

        let missing_pattern = compile_lisp(
            "record-pattern.lisp",
            "(match { :name \"Ada\" } (record ((age years)) years))",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("record patterns may only project statically present fields");
        assert_eq!(missing_pattern[0].code, "E-RECORD-005");
    }

    #[test]
    fn reads_json_object_fields_through_typed_option_boundaries() {
        assert_eq!(
            run(
                "(unwrap (json-as-int (unwrap (json-get (result-unwrap (json-parse \"{\\\"answer\\\":42}\")) \"answer\"))))"
            )
            .unwrap(),
            vec![TypedValue::Int(42)]
        );
        assert_eq!(
            run("(is-some (json-get (result-unwrap (json-parse \"{}\")) \"missing\"))").unwrap(),
            vec![TypedValue::Bool(false)]
        );
        assert_eq!(
            run(
                "(unwrap (json-as-string (unwrap (json-index (result-unwrap (json-parse \"[0,\\\"one\\\"]\")) 1))))"
            )
            .unwrap(),
            vec![TypedValue::String("one".into())]
        );
        assert_eq!(
            run("(unwrap (json-as-float (result-unwrap (json-parse \"3.5\"))))").unwrap(),
            vec![TypedValue::Float(3.5)]
        );
        assert_eq!(
            run("(list-length (json-keys (result-unwrap (json-parse \"{\\\"a\\\":1,\\\"b\\\":2}\"))))")
                .unwrap(),
            vec![TypedValue::Int(2)]
        );
    }

    #[test]
    fn matches_typed_result_payloads_without_unsafe_projection() {
        assert_eq!(
            run("(match-result (ok 5) (ok value (+ value 1)) (err problem (begin problem 0)))")
                .unwrap(),
            vec![TypedValue::Int(6)]
        );
        assert_eq!(
            run("(match-result (err \"bad\") (ok value (begin value 0)) (err problem (begin problem 3)))").unwrap(),
            vec![TypedValue::Int(3)]
        );
    }

    #[test]
    fn generic_match_selects_the_existing_typed_tagged_lowering() {
        assert_eq!(
            run("(match (some 5) (some value (+ value 1)) (none 0))").unwrap(),
            vec![TypedValue::Int(6)]
        );
        assert_eq!(
            run("(match (err \"bad\") (ok value (begin value 0)) (err problem (begin problem 3)))")
                .unwrap(),
            vec![TypedValue::Int(3)]
        );
    }

    #[test]
    fn generic_match_supports_total_boolean_and_integer_literals() {
        assert_eq!(
            run("(match true (true 42) (false 0))").unwrap(),
            vec![TypedValue::Int(42)]
        );
        assert_eq!(
            run("(match 2 (0 100) (2 42) (_ 0))").unwrap(),
            vec![TypedValue::Int(42)]
        );
        assert_eq!(
            run("(match 9 (0 100) (2 42) (_ 0))").unwrap(),
            vec![TypedValue::Int(0)]
        );
    }

    #[test]
    fn constructs_closed_variant_values() {
        assert_eq!(
            run("(variant variant{none|some(int)} :some 42)").unwrap(),
            vec![TypedValue::Variant {
                name: "some".into(),
                value: Some(Box::new(TypedValue::Int(42))),
            }]
        );
        assert_eq!(
            run("(variant variant{none|some(int)} :none)").unwrap(),
            vec![TypedValue::Variant {
                name: "none".into(),
                value: None,
            }]
        );
        for source in [
            "(variant variant{none|some(int)} :missing)",
            "(variant variant{none|some(int)} :some)",
            "(variant variant{none|some(int)} :none 1)",
        ] {
            assert!(compile_lisp("variant.lisp", source, Vec::new(), &core_vocabulary()).is_err());
        }
    }

    #[test]
    fn safely_projects_closed_variant_tags() {
        assert_eq!(
            run("(unwrap (variant-get (variant variant{none|some(int)} :some 42) :some))").unwrap(),
            vec![TypedValue::Int(42)]
        );
        assert_eq!(
            run("(is-some (variant-get (variant variant{none|some(int)} :some 42) :none))")
                .unwrap(),
            vec![TypedValue::Bool(false)]
        );
        assert_eq!(
            run("(is-some (variant-get (variant variant{none|some(int)} :none) :none))").unwrap(),
            vec![TypedValue::Bool(true)]
        );
    }

    #[test]
    fn exhaustively_matches_closed_variants() {
        assert_eq!(
            run("(match (variant variant{none|some(int)} :some 41) \
                         (none 0) \
                         (some value (+ value 1)))")
            .unwrap(),
            vec![TypedValue::Int(42)]
        );
        assert_eq!(
            run("(match (variant variant{idle|busy(string)} :idle) \
                         (idle 7) \
                         (busy message (begin message 0)))")
            .unwrap(),
            vec![TypedValue::Int(7)]
        );
        assert_eq!(
            run("(match-variant (variant variant{some(int)|none} :some 21) \
                                 (some value (* value 2)) \
                                 (none 0))")
            .unwrap(),
            vec![TypedValue::Int(42)]
        );
    }

    #[test]
    fn accepts_parameterized_return_annotations() {
        assert_eq!(
            run(
                "(define (singleton (value : int)) : list<int> (list value)) \
                 (list-get (singleton 7) 0)"
            )
            .unwrap(),
            vec![TypedValue::Int(7)]
        );
    }

    #[test]
    fn accepts_fixed_record_return_annotations() {
        assert_eq!(
            run("(define (person) : record{name:string,age:int} \
                 { :name \"Ada\" :age 37 }) \
                 (unwrap (record-get (person) \"age\"))")
            .unwrap(),
            vec![TypedValue::Int(37)]
        );
    }

    #[test]
    fn quote_produces_a_typed_symbol_value() {
        assert_eq!(
            run("(quote bash)").unwrap(),
            vec![TypedValue::Symbol("bash".into())]
        );
        assert_eq!(
            run("'bash").unwrap(),
            vec![TypedValue::Symbol("bash".into())]
        );
    }

    #[test]
    fn named_break_and_continue_lower_to_typed_loop_edges() {
        assert_eq!(
            run("(while :label outer true (break outer))").unwrap(),
            Vec::<TypedValue>::new()
        );
        let module = compile_lisp(
            "continue.lisp",
            "(while :label outer false (continue outer))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        assert!(module.module.functions["main"]
            .blocks
            .values()
            .any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|located| matches!(located.instruction, Instruction::Jump { .. }))
            }));
    }
}

#[test]
fn public_colisp_facade_retains_unicode_comment_and_reader_sugar_span() {
    let source = "; π lead comment\n'λ";
    let module = compile_lisp("reader-span.lisp", source, Vec::new(), &core_vocabulary())
        .expect("CoLisp reader sugar must compile through the VM facade");
    let quote = module.module.functions["main"]
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find(|located| {
            matches!(
                located.instruction,
                Instruction::Constant {
                    value: TypedValue::Symbol(ref symbol)
                } if symbol == "λ"
            )
        })
        .expect("reader sugar must lower to the quoted symbol constant");
    let span = quote
        .origin
        .span
        .as_ref()
        .expect("quoted symbol must retain its source origin");
    assert_eq!(
        &source[span.start_byte..span.end_byte],
        "'λ",
        "reader-sugar origin must retain the original Unicode source slice: source={source:?}, span={span:?}"
    );
    assert_eq!(
        (span.start_line, span.start_column),
        (2, 1),
        "reader-sugar origin must account for the preceding comment and Unicode bytes: source={source:?}, span={span:?}"
    );
}

#[test]
fn public_colisp_facade_retains_typed_record_failure_span() {
    let source = "; π typed record\n(record-get { :name \"Ada\" } \"age\")";
    let diagnostics = compile_lisp("record-span.lisp", source, Vec::new(), &core_vocabulary())
        .expect_err("missing typed-record fields must fail through the VM facade");
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E-RECORD-005")
        .unwrap_or_else(|| {
            panic!("typed-record failure must retain its specific diagnostic: {diagnostics:?}")
        });
    let span = diagnostic
        .primary
        .as_ref()
        .and_then(|origin| origin.span.as_ref())
        .expect("typed-record failure must retain its source origin");
    let start = source
        .find("(record-get")
        .expect("typed-record expression must be present in its source");
    assert_eq!(
        &source[span.start_byte..span.end_byte],
        &source[start..],
        "typed-record diagnostic must retain the original expression span: source={source:?}, diagnostic={diagnostic:?}"
    );
    assert_eq!(
        (span.start_line, span.start_column),
        (2, 1),
        "typed-record diagnostic must retain line/column after the Unicode comment: source={source:?}, diagnostic={diagnostic:?}"
    );
}

#[test]
fn public_frontend_facade_executes_equivalent_typed_records() {
    let vocabulary = core_vocabulary();
    let forth_source = "{ name: \"Ada\" age: 37 } \"age\" record-get unwrap";
    let lisp_source = "(unwrap (record-get { :name \"Ada\" :age 37 } \"age\"))";
    let forth = compile_forth(
        "record-equivalent.forth",
        forth_source,
        Vec::new(),
        &vocabulary,
    )
    .unwrap_or_else(|diagnostics| {
        panic!("Co-Forth typed-record fixture must compile: source={forth_source:?}, diagnostics={diagnostics:?}")
    });
    let lisp = compile_lisp(
        "record-equivalent.lisp",
        lisp_source,
        Vec::new(),
        &vocabulary,
    )
    .unwrap_or_else(|diagnostics| {
        panic!("CoLisp typed-record fixture must compile: source={lisp_source:?}, diagnostics={diagnostics:?}")
    });

    let mut forth_stack = Vec::new();
    Interpreter::new(&forth, DenyCapabilities, InterpreterConfig::default())
        .execute(&mut forth_stack)
        .unwrap_or_else(|error| {
            panic!("Co-Forth typed-record fixture must execute: source={forth_source:?}, error={error:?}")
        });
    let mut lisp_stack = Vec::new();
    Interpreter::new(&lisp, DenyCapabilities, InterpreterConfig::default())
        .execute(&mut lisp_stack)
        .unwrap_or_else(|error| {
            panic!(
                "CoLisp typed-record fixture must execute: source={lisp_source:?}, error={error:?}"
            )
        });

    assert_eq!(
        forth_stack,
        vec![TypedValue::Int(37)],
        "Co-Forth typed-record fixture must produce the expected value: source={forth_source:?}, stack={forth_stack:?}"
    );
    assert_eq!(
        lisp_stack, forth_stack,
        "equivalent typed-record syntax must produce the same facade result: forth_source={forth_source:?}, lisp_source={lisp_source:?}, forth_stack={forth_stack:?}, lisp_stack={lisp_stack:?}"
    );
}
