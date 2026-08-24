//! Report-only compatibility checks for source written for older Finch runtimes.

use crate::vm::frontend::forth::compile_forth;
use crate::vm::verifier::Vocabulary;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedForthRejection {
    pub source_id: String,
    pub diagnostic: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TypedForthAudit {
    pub total: usize,
    pub accepted: usize,
    pub missing: usize,
    pub rejected: Vec<TypedForthRejection>,
    pub rejection_codes: BTreeMap<String, usize>,
}

/// Compile legacy Co-Forth sources through the typed frontend without running
/// them or changing any persistent vocabulary. A missing source is measured
/// separately from a source rejected by the typed compiler.
pub fn audit_forth_sources<'a>(
    sources: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    vocabulary: &Vocabulary,
) -> TypedForthAudit {
    let mut audit = TypedForthAudit::default();

    for (source_id, source) in sources {
        audit.total += 1;
        let Some(source) = source else {
            audit.missing += 1;
            continue;
        };

        match compile_forth(source_id, source, Vec::new(), vocabulary) {
            Ok(_) => audit.accepted += 1,
            Err(diagnostics) => {
                let diagnostic = diagnostics
                    .first()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "E-UNKNOWN: typed compiler rejected the source".into());
                let code = diagnostics
                    .first()
                    .map(|diagnostic| diagnostic.code.clone())
                    .unwrap_or_else(|| "E-UNKNOWN".into());
                *audit.rejection_codes.entry(code).or_default() += 1;
                audit.rejected.push(TypedForthRejection {
                    source_id: source_id.into(),
                    diagnostic,
                });
            }
        }
    }

    audit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::core_vocabulary;

    #[test]
    fn audit_is_report_only_and_groups_stable_diagnostic_codes() {
        let vocabulary = core_vocabulary();
        let audit = audit_forth_sources(
            [
                ("valid", Some("2 3 +")),
                ("invalid", Some("missing-legacy-word")),
                ("absent", None),
            ],
            &vocabulary,
        );

        assert_eq!(audit.total, 3);
        assert_eq!(audit.accepted, 1);
        assert_eq!(audit.missing, 1);
        assert_eq!(audit.rejected.len(), 1);
        assert_eq!(audit.rejection_codes.get("E-LINK-002"), Some(&1));
        assert_eq!(audit.rejected[0].source_id, "invalid");
    }
}
