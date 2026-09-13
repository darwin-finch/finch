use finch_vm::{
    compile_forth, compile_lisp, core_vocabulary, InterpreterConfig, TypedValue, VmStep,
    VmTrampoline,
};

fn run_pure(module: &finch_vm::VerifiedModule) -> Vec<TypedValue> {
    let trampoline = VmTrampoline::new(
        module,
        &InterpreterConfig {
            fuel: 100,
            ..InterpreterConfig::default()
        },
    );
    let continuation = trampoline
        .start(Vec::new())
        .expect("verified fixture must start through the public facade");
    match trampoline.run(continuation) {
        VmStep::Complete { stack } => stack,
        other => panic!("pure facade fixture must complete, observed {other:?}"),
    }
}

#[test]
fn test_external_frontends_compile_verify_and_execute_equivalently_through_facade() {
    let vocabulary = core_vocabulary();
    let forth = compile_forth("equivalent.forth", "41 1 +", Vec::new(), &vocabulary)
        .expect("Co-Forth fixture must compile and verify through finch-vm");
    let lisp = compile_lisp("equivalent.lisp", "(+ 41 1)", Vec::new(), &vocabulary)
        .expect("Co-Lisp fixture must compile and verify through finch-vm");

    let forth_stack = run_pure(&forth);
    let lisp_stack = run_pure(&lisp);
    assert_eq!(
        forth_stack,
        vec![TypedValue::Int(42)],
        "Co-Forth facade execution must produce the representative typed result"
    );
    assert_eq!(
        lisp_stack, forth_stack,
        "Co-Lisp and Co-Forth must retain equivalent compile/verify/execute behavior"
    );
}
