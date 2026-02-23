// IMPCPD persona selection — deterministic keyword-based activation

/// Return the active critique personas for the given plan text.
///
/// Six personas are always active:
/// - Regression (existing code paths)
/// - Edge Cases (error handling, boundary conditions)
/// - Completeness (specificity, missing steps)
/// - Tests & Docs (every plan must include regression test steps AND doc updates)
/// - Repo Hygiene (no markdown dumped at root, no scratch files committed, right locations)
/// - Git Discipline (surgical staging by explicit path, commit message specified, pre-commit diff check)
///
/// Additional personas activate when the plan text contains relevant keywords:
/// - Security — when the plan touches auth/crypto/secrets/permissions
/// - Architecture — when the plan introduces modules/traits/dependencies/refactors
/// - Scope Creep — when the plan has more than 6 numbered steps
pub fn select_active_personas(plan_text: &str) -> Vec<&'static str> {
    let lower = plan_text.to_lowercase();

    // Tests & Docs is always active: every plan must include regression tests
    // and documentation update steps (CHANGELOG, CLAUDE.md, README if needed).
    // Repo Hygiene is always active: no markdown dumped at root, no scratch files
    // committed, generated output in .gitignore, etc.
    let mut personas = vec![
        "Regression",
        "Edge Cases",
        "Completeness",
        "Tests & Docs",
        "Repo Hygiene",
        "Git Discipline",
    ];

    // Security: activates on auth, crypto, and secrets-related keywords
    let security_keywords = [
        "auth", "jwt", "token", "secret", "api_key", "apikey", "password",
        "permission", "role", "bearer", "crypto", "encrypt", "decrypt", "tls",
        "ssl", "https", "certificate", "hash", "hmac", "session", "csrf",
        "cors", "oauth", "saml",
    ];
    if security_keywords.iter().any(|kw| lower.contains(kw)) {
        personas.push("Security");
    }

    // Architecture: activates on structural/module-level changes
    let arch_keywords = [
        "refactor", "module", "mod ", "pub mod", "crate", "dependency",
        "struct ", "trait ", "interface", "abstraction", "impl ", "pub ",
        "pub(crate)", "architecture", "layer", "separation",
    ];
    if arch_keywords.iter().any(|kw| lower.contains(kw)) {
        personas.push("Architecture");
    }

    // Scope Creep: activates when the plan has more than 6 numbered steps
    let step_count = count_numbered_steps(plan_text);
    if step_count > 6 {
        personas.push("Scope Creep");
    }

    personas
}

/// Count lines that begin with a digit (numbered steps like "1.", "2.", "10.")
fn count_numbered_steps(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            let t = line.trim_start();
            t.starts_with(|c: char| c.is_ascii_digit())
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_always_active_personas() {
        let personas = select_active_personas("1. Write a hello world function");
        assert!(personas.contains(&"Regression"));
        assert!(personas.contains(&"Edge Cases"));
        assert!(personas.contains(&"Completeness"));
        assert!(personas.contains(&"Tests & Docs"));
        assert!(personas.contains(&"Repo Hygiene"));
        assert!(personas.contains(&"Git Discipline"));
    }

    #[test]
    fn test_git_discipline_always_active() {
        // Git Discipline fires even on trivial plans — every commit step needs scrutiny
        let personas = select_active_personas("1. Fix a typo in README");
        assert!(personas.contains(&"Git Discipline"));
    }

    #[test]
    fn test_security_activates_on_auth() {
        let plan = "1. Add JWT token validation\n2. Check auth header";
        let personas = select_active_personas(plan);
        assert!(personas.contains(&"Security"));
    }

    #[test]
    fn test_security_activates_on_crypto() {
        let plan = "1. Hash the password using bcrypt\n2. Store in DB";
        let personas = select_active_personas(plan);
        assert!(personas.contains(&"Security"));
    }

    #[test]
    fn test_security_not_activated_without_keywords() {
        let plan = "1. Add a button\n2. Update the CSS\n3. Write a test";
        let personas = select_active_personas(plan);
        assert!(!personas.contains(&"Security"));
    }

    #[test]
    fn test_architecture_activates_on_module() {
        let plan = "1. Create a new module src/planning/mod.rs";
        let personas = select_active_personas(plan);
        assert!(personas.contains(&"Architecture"));
    }

    #[test]
    fn test_architecture_activates_on_refactor() {
        let plan = "1. Refactor the provider factory";
        let personas = select_active_personas(plan);
        assert!(personas.contains(&"Architecture"));
    }

    #[test]
    fn test_scope_creep_activates_at_7_steps() {
        let plan = (1..=7)
            .map(|i| format!("{}. Step {}", i, i))
            .collect::<Vec<_>>()
            .join("\n");
        let personas = select_active_personas(&plan);
        assert!(personas.contains(&"Scope Creep"));
    }

    #[test]
    fn test_scope_creep_not_activated_at_6_steps() {
        let plan = (1..=6)
            .map(|i| format!("{}. Step {}", i, i))
            .collect::<Vec<_>>()
            .join("\n");
        let personas = select_active_personas(&plan);
        assert!(!personas.contains(&"Scope Creep"));
    }

    #[test]
    fn test_repo_hygiene_always_active() {
        // Repo Hygiene is always active regardless of plan content
        let personas = select_active_personas("1. Run cargo fmt");
        assert!(personas.contains(&"Repo Hygiene"));
    }

    #[test]
    fn test_multiple_personas_can_activate() {
        let plan = "1. Add JWT auth middleware\n\
                    2. Refactor the provider struct\n\
                    3. Step 3\n4. Step 4\n5. Step 5\n6. Step 6\n7. Step 7";
        let personas = select_active_personas(plan);
        assert!(personas.contains(&"Security"));
        assert!(personas.contains(&"Architecture"));
        assert!(personas.contains(&"Scope Creep"));
    }
}
