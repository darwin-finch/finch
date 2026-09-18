// Declared post-edit diagnostics sources (issue #757).
//
// The only diagnostics source Finch recognises is one the user declared here.
// Nothing is inferred from project files. An LSP-server source is a recorded
// follow-up; unknown keys fail closed at parse time so a surface this build
// cannot execute is never silently accepted.

use serde::{Deserialize, Serialize};

fn default_timeout_secs() -> u64 {
    10
}

fn default_max_output_chars() -> usize {
    4000
}

/// One declared check command and the file extensions it covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckCommandSource {
    /// File extensions (without the leading dot, case-insensitive) whose edits
    /// this source diagnoses, e.g. `["rs"]`.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Simple argv to execute when a covered file is edited. No shell is
    /// involved: the string is split on whitespace and executed directly, so
    /// shell operators are rejected at validation rather than mis-executed.
    pub command: String,
}

/// Declared post-edit diagnostics sources ([issue #757](https://github.com/darwin-finch/finch/issues/757)).
///
/// Empty by default: no diagnostics run and edit results are unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsConfig {
    /// Declared check-command sources.
    #[serde(default)]
    pub check: Vec<CheckCommandSource>,
    /// Per-run bound for one check command. A slow, absent, or erroring source
    /// never blocks or fails the edit beyond this bound.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Cap on the diagnostics text excerpt appended to an edit result.
    #[serde(default = "default_max_output_chars")]
    pub max_output_chars: usize,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            check: Vec::new(),
            timeout_secs: default_timeout_secs(),
            max_output_chars: default_max_output_chars(),
        }
    }
}

impl DiagnosticsConfig {
    /// True when no source is declared, so the post-edit hook is inert.
    pub fn is_inert(&self) -> bool {
        self.check.is_empty()
    }

    /// Declared source whose extensions cover `path`, if any.
    pub fn source_for_file(&self, path: &str) -> Option<&CheckCommandSource> {
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())?
            .to_ascii_lowercase();
        self.check.iter().find(|source| {
            source.extensions.iter().any(|declared| {
                let declared = declared.trim().trim_start_matches('.').to_ascii_lowercase();
                declared == extension
            })
        })
    }

    /// Fail closed on declarations this build must not mis-execute.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.timeout_secs == 0 {
            anyhow::bail!("diagnostics.timeout_secs must be greater than 0");
        }
        if self.max_output_chars == 0 {
            anyhow::bail!("diagnostics.max_output_chars must be greater than 0");
        }
        for (index, source) in self.check.iter().enumerate() {
            let name = |detail: String| format!("diagnostics.check[{index}] ({detail})");
            let command = source.command.trim();
            if command.is_empty() {
                anyhow::bail!("{}: command must not be empty", name("command".into()));
            }
            for operator in [';', '|', '>', '<', '&', '`', '\n', '\r'] {
                if command.contains(operator) {
                    anyhow::bail!(
                        "{}: command must be a simple argv without shell operators \
                         (found {:?}); the declared check command is executed directly, \
                         not through a shell",
                        name("command".into()),
                        operator
                    );
                }
            }
            if command.contains("$(") {
                anyhow::bail!(
                    "{}: command substitution is not executed; declare a plain argv",
                    name("command".into())
                );
            }
            if source.extensions.is_empty() {
                anyhow::bail!(
                    "{}: extensions must not be empty",
                    name("extensions".into())
                );
            }
            for extension in &source.extensions {
                let cleaned = extension
                    .trim()
                    .trim_start_matches('.')
                    .to_ascii_lowercase();
                if cleaned.is_empty() || cleaned.contains(['/', '\\', '.']) {
                    anyhow::bail!(
                        "{}: extension {:?} is not a bare file extension",
                        name("extensions".into()),
                        extension
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(extensions: &[&str], command: &str) -> CheckCommandSource {
        CheckCommandSource {
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
            command: command.to_string(),
        }
    }

    #[test]
    fn test_default_config_is_inert() {
        let config = DiagnosticsConfig::default();
        assert!(
            config.is_inert(),
            "an empty [diagnostics] table must declare no sources, got {:?}",
            config.check
        );
        assert!(
            config.source_for_file("src/main.rs").is_none(),
            "an inert config must not match any file"
        );
    }

    #[test]
    fn test_diagnostics_config_parses_declared_check_sources() {
        let parsed: DiagnosticsConfig = toml::from_str(
            r#"
            timeout_secs = 30
            max_output_chars = 100

            [[check]]
            extensions = ["rs"]
            command = "cargo check --message-format=json"

            [[check]]
            extensions = [".py"]
            command = "mypy"
        "#,
        )
        .expect("declared sources must parse");
        assert_eq!(
            parsed.check.len(),
            2,
            "both declarations must parse: {parsed:?}"
        );
        assert_eq!(parsed.check[0].command, "cargo check --message-format=json");
        assert_eq!(
            parsed.timeout_secs, 30,
            "declared timeout must override the default"
        );
        assert_eq!(parsed.max_output_chars, 100);
        assert_eq!(
            parsed
                .source_for_file("src/lib.RS")
                .map(|s| s.command.as_str()),
            Some("cargo check --message-format=json"),
            "extension lookup must be case-insensitive; config: {parsed:?}"
        );
        assert_eq!(
            parsed
                .source_for_file("tool.py")
                .map(|s| s.command.as_str()),
            Some("mypy"),
            "a leading dot in a declared extension must still match; config: {parsed:?}"
        );
        assert!(
            parsed.source_for_file("notes.txt").is_none(),
            "a file with no declared extension must not match any source; config: {parsed:?}"
        );
        assert!(
            parsed.source_for_file("noextension").is_none(),
            "an extension-less path must not match any source; config: {parsed:?}"
        );
    }

    #[test]
    fn test_diagnostics_config_rejects_shell_operators() {
        let config = DiagnosticsConfig {
            check: vec![source(&["rs"], "cargo check 2>&1 | tee out")],
            ..DiagnosticsConfig::default()
        };
        let error = config
            .validate()
            .expect_err("shell operators must fail validation");
        assert!(
            error.to_string().contains("shell operators"),
            "the rejection must name the shell-operator rule, got: {error}"
        );
    }

    #[test]
    fn test_diagnostics_config_rejects_command_substitution() {
        let config = DiagnosticsConfig {
            check: vec![source(&["rs"], "echo $(whoami)")],
            ..DiagnosticsConfig::default()
        };
        let error = config
            .validate()
            .expect_err("command substitution must fail validation");
        assert!(
            error.to_string().contains("command substitution"),
            "the rejection must name the substitution rule, got: {error}"
        );
    }

    #[test]
    fn test_diagnostics_config_rejects_unknown_keys_fail_closed() {
        let error = toml::from_str::<DiagnosticsConfig>(
            r#"
            [lsp]
            command = "rust-analyzer"
        "#,
        )
        .expect_err("an LSP declaration is not executable by this build and must fail closed");
        assert!(
            error.to_string().contains("unknown field"),
            "the parse error must be an unknown-field rejection, got: {error}"
        );
    }

    #[test]
    fn test_diagnostics_config_rejects_empty_command_or_extensions() {
        let empty_command = DiagnosticsConfig {
            check: vec![source(&["rs"], "   ")],
            ..DiagnosticsConfig::default()
        };
        assert!(
            empty_command.validate().is_err(),
            "an empty command must fail validation"
        );
        let empty_extensions = DiagnosticsConfig {
            check: vec![source(&[], "cargo check")],
            ..DiagnosticsConfig::default()
        };
        let error = empty_extensions
            .validate()
            .expect_err("empty extensions must fail validation");
        assert!(
            error.to_string().contains("extensions"),
            "the rejection must name the extensions rule, got: {error}"
        );
    }

    #[test]
    fn test_diagnostics_config_rejects_zero_bounds() {
        let config = DiagnosticsConfig {
            check: vec![source(&["rs"], "cargo check")],
            timeout_secs: 0,
            ..DiagnosticsConfig::default()
        };
        let error = config
            .validate()
            .expect_err("a zero timeout is not a bounded run");
        assert!(error.to_string().contains("timeout_secs"), "got: {error}");

        let config = DiagnosticsConfig {
            check: vec![source(&["rs"], "cargo check")],
            max_output_chars: 0,
            ..DiagnosticsConfig::default()
        };
        let error = config
            .validate()
            .expect_err("a zero output cap is not a bounded excerpt");
        assert!(
            error.to_string().contains("max_output_chars"),
            "got: {error}"
        );
    }
}
