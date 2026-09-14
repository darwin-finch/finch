//! Compilation facade for Finch source languages.
//!
//! This crate selects a frontend, runs the shared semantic-construction
//! pipeline, and returns [`ModuleVerified`]. It owns no interpreter, host
//! policy, or checkpoint codec.

use finch_vm_core::{Function, ProgramLanguage, Type, VmDiagnostic, Vocabulary};
use std::collections::BTreeMap;

pub use finch_coforth::{compile_forth, compile_forth_with_functions};
pub use finch_colisp::{
    compile_lisp, compile_lisp_with_functions, parse_math, parse_str, parse_str_spanned,
    SpannedVal, Val,
};
pub use finch_vm_core::{
    certify_module, Elaborated, FunctionCertified, ModuleSealed, ModuleVerified, Parsed,
    SemanticBuilder, SEMANTIC_CONSTRUCTION_VERSION,
};

/// Compile source in `language` through the shared compiler pipeline.
pub fn compile(
    language: ProgramLanguage,
    source_id: &str,
    source: &str,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
) -> Result<ModuleVerified, Vec<VmDiagnostic>> {
    compile_with_functions(
        language,
        source_id,
        source,
        initial_stack,
        vocabulary,
        &BTreeMap::new(),
    )
}

/// Compile source with additional already-lowered functions available for calls.
pub fn compile_with_functions(
    language: ProgramLanguage,
    source_id: &str,
    source: &str,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
    linked_functions: &BTreeMap<String, Function>,
) -> Result<ModuleVerified, Vec<VmDiagnostic>> {
    match language {
        ProgramLanguage::Forth => compile_forth_with_functions(
            source_id,
            source,
            initial_stack,
            vocabulary,
            linked_functions,
        ),
        ProgramLanguage::Lisp => compile_lisp_with_functions(
            source_id,
            source,
            initial_stack,
            vocabulary,
            linked_functions,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use finch_vm_core::core_vocabulary;

    #[test]
    fn facade_selects_both_frontends_onto_one_verified_module_type() {
        let lisp = compile(
            ProgramLanguage::Lisp,
            "pair.lisp",
            "(+ 1 2)",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("CoLisp addition should compile through the shared facade");
        let forth = compile(
            ProgramLanguage::Forth,
            "pair.forth",
            "1 2 +",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("Co-Forth addition should compile through the shared facade");
        assert_eq!(lisp.module.entry, "main");
        assert_eq!(forth.module.entry, "main");
        assert_eq!(
            lisp.functions["main"].entry_stack, forth.functions["main"].entry_stack,
            "paired arithmetic must share the same verified entry stack through one pipeline"
        );
    }

    #[test]
    fn facade_does_not_mint_a_module_certificate_for_unverified_source() {
        let errors = compile(
            ProgramLanguage::Lisp,
            "broken.lisp",
            "(unknown-word)",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("unknown words must not become ModuleVerified");
        assert!(
            errors.iter().any(|error| error.code.starts_with("E-")),
            "construction failure must remain a diagnostic, not an executable module: {errors:?}"
        );
    }
}
