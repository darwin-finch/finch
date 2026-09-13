#[test]
fn test_vm_extraction_preserves_public_vm_and_lisp_paths() {
    let _config = finch::vm::InterpreterConfig::default();
    let value: finch::lisp::types::Val = finch::lisp::Val::Nil;
    let _: Vec<finch::lisp::Val> = finch::lisp::reader::parse_str("nil")
        .expect("the compatibility reader path must parse Lisp values");
    let _: Vec<finch::lisp::reader::SpannedVal> = finch::lisp::reader::parse_str_spanned("nil")
        .expect("the compatibility spanned-reader path must parse Lisp values");
    let _: finch::lisp::Val = finch::lisp::reader::parse_math("1 + 2")
        .expect("the compatibility math-reader path must parse an expression");
    assert_eq!(value, finch::lisp::Val::Nil);
}
