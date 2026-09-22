use finch_language::{compile_forth, compile_lisp};
use finch_vm::{core_vocabulary, TypedExecutionStatus, TypedRuntime, TypedValue};

fn run_pure(module: &finch_vm::ModuleVerified) -> Vec<TypedValue> {
    let mut runtime = TypedRuntime::new();
    let execution = runtime.execute(module, 100);
    assert_eq!(
        execution.status,
        TypedExecutionStatus::Completed,
        "pure facade fixture must complete through the public runtime entry, observed status {status:?} with diagnostics {diagnostics:?}",
        status = execution.status,
        diagnostics = execution.diagnostics,
    );
    execution.values
}

#[test]
fn test_external_frontends_compile_verify_and_execute_equivalently_through_facade() {
    let vocabulary = core_vocabulary();
    let forth = compile_forth("equivalent.forth", "41 1 +", Vec::new(), &vocabulary)
        .expect("Co-Forth fixture must compile through the language facade");
    let lisp = compile_lisp("equivalent.lisp", "(+ 41 1)", Vec::new(), &vocabulary)
        .expect("Co-Lisp fixture must compile through the language facade");

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
