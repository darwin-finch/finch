use super::diagnostic::SourceOrigin;
use super::effects::{CapabilityRequirement, FileSelector, ResourceSelector};
use super::types::TypedValue;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum GrantScope {
    Once { request_id: Uuid },
    Task { task_id: Uuid },
    Session { session_id: Uuid },
    Project { project_id: String },
    Global,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityGrant {
    pub id: Uuid,
    pub requirement: CapabilityRequirement,
    pub scope: GrantScope,
    pub policy_hash: String,
    pub created_by: String,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: Option<u64>,
    pub revoked_at_unix_ms: Option<u64>,
    /// Set only for a successfully authorized `once` grant. Consumption is
    /// distinct from revocation so audit consumers can tell normal use from a
    /// policy withdrawal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at_unix_ms: Option<u64>,
}

impl CapabilityGrant {
    pub fn is_active(&self, now_unix_ms: u64) -> bool {
        self.revoked_at_unix_ms.is_none()
            && self.consumed_at_unix_ms.is_none()
            && self
                .expires_at_unix_ms
                .is_none_or(|expires| now_unix_ms < expires)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityRequest {
    pub id: Uuid,
    pub execution_id: Uuid,
    /// The portable VM effect sequence this approval may resume. `None` is
    /// reserved for static/preflight requests that have not yet reached a
    /// concrete runtime effect boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_sequence: Option<u64>,
    pub requirement: CapabilityRequirement,
    pub arguments: Vec<TypedValue>,
    pub reason: String,
    pub origin: SourceOrigin,
    pub agent_ancestry: Vec<Uuid>,
    pub program_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationContext {
    pub now_unix_ms: u64,
    pub task_id: Option<Uuid>,
    pub session_id: Uuid,
    pub project_id: Option<String>,
    pub policy_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAvailability {
    Disabled,
    Unsupported,
    PermissionRequired,
    Available,
    Degraded { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum AuthorizationDecision {
    Allowed { grant_id: Uuid },
    ApprovalRequired,
    Denied { reason: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantSet {
    pub grants: Vec<CapabilityGrant>,
}

impl GrantSet {
    pub fn active_global_requirements(
        &self,
        now_unix_ms: u64,
    ) -> impl Iterator<Item = &CapabilityRequirement> {
        self.grants
            .iter()
            .filter(move |grant| {
                grant.is_active(now_unix_ms) && matches!(grant.scope, GrantScope::Global)
            })
            .map(|grant| &grant.requirement)
    }

    /// Active reusable authority applicable to one ProgramRun. Exact
    /// `once` grants are deliberately excluded: they are consumed against a
    /// concrete request ID and resume only that already-pending host call.
    pub fn active_requirements_for<'a>(
        &'a self,
        context: &'a AuthorizationContext,
    ) -> impl Iterator<Item = &'a CapabilityRequirement> + 'a {
        self.grants
            .iter()
            .filter(move |grant| {
                grant.is_active(context.now_unix_ms)
                    && grant.policy_hash == context.policy_hash
                    && match &grant.scope {
                        GrantScope::Once { .. } => false,
                        GrantScope::Task { task_id } => context.task_id == Some(*task_id),
                        GrantScope::Session { session_id } => *session_id == context.session_id,
                        GrantScope::Project { project_id } => {
                            context.project_id.as_ref() == Some(project_id)
                        }
                        GrantScope::Global => true,
                    }
            })
            .map(|grant| &grant.requirement)
    }

    pub fn authorize(
        &self,
        request: &CapabilityRequest,
        context: &AuthorizationContext,
    ) -> AuthorizationDecision {
        if let Some(grant) = self.grants.iter().find(|grant| {
            grant.is_active(context.now_unix_ms)
                && grant.policy_hash == context.policy_hash
                && scope_applies(&grant.scope, request.id, context)
                && grant.requirement.covers(&request.requirement)
        }) {
            return AuthorizationDecision::Allowed { grant_id: grant.id };
        }
        AuthorizationDecision::ApprovalRequired
    }

    pub fn revoke(&mut self, grant_id: Uuid, now_unix_ms: u64) -> bool {
        let Some(grant) = self.grants.iter_mut().find(|grant| grant.id == grant_id) else {
            return false;
        };
        grant.revoked_at_unix_ms = Some(now_unix_ms);
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAuditAction {
    Granted,
    Revoked,
    Consumed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityAuditEntry {
    pub sequence: u64,
    pub grant_id: Uuid,
    pub action: CapabilityAuditAction,
    pub requirement: CapabilityRequirement,
    pub at_unix_ms: u64,
    pub actor: String,
}

/// Source-free record of one authorization decision. Request arguments and
/// source text remain in the effect/program journal; this ledger retains only
/// the stable correlation and structured requirement needed for security
/// review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityAuthorizationAuditEntry {
    pub sequence: u64,
    pub request_id: Uuid,
    pub execution_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_sequence: Option<u64>,
    pub requirement: CapabilityRequirement,
    pub decision: AuthorizationDecision,
    pub at_unix_ms: u64,
    pub actor: String,
}

/// Application-owned authority records. This ledger is serializable beside a
/// Brain or session event log, but deliberately never enters a VM checkpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityLedger {
    pub grants: GrantSet,
    pub audit: Vec<CapabilityAuditEntry>,
    #[serde(default)]
    pub authorization_audit: Vec<CapabilityAuthorizationAuditEntry>,
    #[serde(default)]
    next_sequence: u64,
}

impl CapabilityLedger {
    pub fn issue(
        &mut self,
        requirement: CapabilityRequirement,
        scope: GrantScope,
        policy_hash: impl Into<String>,
        actor: impl Into<String>,
        now_unix_ms: u64,
        expires_at_unix_ms: Option<u64>,
    ) -> Result<Uuid, String> {
        if expires_at_unix_ms.is_some_and(|expires| expires <= now_unix_ms) {
            return Err("capability grant expiry must be after its creation time".into());
        }
        if matches!(&scope, GrantScope::Project { project_id } if project_id.trim().is_empty()) {
            return Err("project-scoped capability grant requires a project identity".into());
        }
        let policy_hash = policy_hash.into();
        if policy_hash.trim().is_empty() {
            return Err("capability grant requires a policy hash".into());
        }
        let id = Uuid::new_v4();
        let actor = actor.into();
        self.grants.grants.push(CapabilityGrant {
            id,
            requirement: requirement.clone(),
            scope,
            policy_hash,
            created_by: actor.clone(),
            created_at_unix_ms: now_unix_ms,
            expires_at_unix_ms,
            revoked_at_unix_ms: None,
            consumed_at_unix_ms: None,
        });
        self.record(
            id,
            CapabilityAuditAction::Granted,
            requirement,
            now_unix_ms,
            actor,
        );
        Ok(id)
    }

    pub fn grant_global(
        &mut self,
        requirement: CapabilityRequirement,
        policy_hash: impl Into<String>,
        actor: impl Into<String>,
        now_unix_ms: u64,
    ) -> Uuid {
        self.issue(
            requirement,
            GrantScope::Global,
            policy_hash,
            actor,
            now_unix_ms,
            None,
        )
        .expect("a global grant with a non-empty built-in policy is valid")
    }

    /// Authorize and audit one concrete request. A matching `once` grant is
    /// consumed atomically with this decision, so it cannot authorize a later
    /// effect even when the selector is identical.
    pub fn authorize(
        &mut self,
        request: &CapabilityRequest,
        context: &AuthorizationContext,
        actor: impl Into<String>,
    ) -> AuthorizationDecision {
        let actor = actor.into();
        let decision = self.grants.authorize(request, context);
        if let AuthorizationDecision::Allowed { grant_id } = decision {
            let consumed_requirement = self
                .grants
                .grants
                .iter_mut()
                .find(|grant| grant.id == grant_id)
                .filter(|grant| matches!(grant.scope, GrantScope::Once { .. }))
                .map(|grant| {
                    grant.consumed_at_unix_ms = Some(context.now_unix_ms);
                    grant.requirement.clone()
                });
            if let Some(requirement) = consumed_requirement {
                self.record(
                    grant_id,
                    CapabilityAuditAction::Consumed,
                    requirement,
                    context.now_unix_ms,
                    actor.clone(),
                );
            }
        }
        let sequence = self.take_sequence();
        self.authorization_audit
            .push(CapabilityAuthorizationAuditEntry {
                sequence,
                request_id: request.id,
                execution_id: request.execution_id,
                effect_sequence: request.effect_sequence,
                requirement: request.requirement.clone(),
                decision: decision.clone(),
                at_unix_ms: context.now_unix_ms,
                actor,
            });
        decision
    }

    pub fn revoke(&mut self, grant_id: Uuid, actor: impl Into<String>, now_unix_ms: u64) -> bool {
        let requirement = self
            .grants
            .grants
            .iter()
            .find(|grant| grant.id == grant_id)
            .map(|grant| grant.requirement.clone());
        if !self.grants.revoke(grant_id, now_unix_ms) {
            return false;
        }
        self.record(
            grant_id,
            CapabilityAuditAction::Revoked,
            requirement.expect("a revoked grant was found before mutation"),
            now_unix_ms,
            actor.into(),
        );
        true
    }

    fn record(
        &mut self,
        grant_id: Uuid,
        action: CapabilityAuditAction,
        requirement: CapabilityRequirement,
        at_unix_ms: u64,
        actor: String,
    ) {
        let sequence = self.take_sequence();
        self.audit.push(CapabilityAuditEntry {
            sequence,
            grant_id,
            action,
            requirement,
            at_unix_ms,
            actor,
        });
    }

    fn take_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        sequence
    }
}

fn scope_applies(scope: &GrantScope, request_id: Uuid, context: &AuthorizationContext) -> bool {
    match scope {
        GrantScope::Once {
            request_id: granted,
        } => *granted == request_id,
        GrantScope::Task { task_id } => context.task_id == Some(*task_id),
        GrantScope::Session { session_id } => *session_id == context.session_id,
        GrantScope::Project { project_id } => context.project_id.as_ref() == Some(project_id),
        GrantScope::Global => true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "choice", rename_all = "snake_case")]
pub enum ApprovalChoice {
    Deny,
    AllowOnce,
    AllowTask,
    AllowSession,
    AllowProjectExact,
    AllowProjectPattern { requirement: CapabilityRequirement },
    AllowGlobal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalPrompt {
    pub request: CapabilityRequest,
    pub exact: CapabilityRequirement,
    pub suggested_patterns: Vec<CapabilityRequirement>,
    pub broad_scope_warning: bool,
}

impl ApprovalPrompt {
    pub fn for_request(request: CapabilityRequest) -> Self {
        let exact = request.requirement.clone();
        let mut suggested_patterns = Vec::new();
        let mut broad_scope_warning = false;
        if let ResourceSelector::File { selector } = &request.requirement.selector {
            if selector.pattern == "**" {
                broad_scope_warning = true;
            } else if !selector.pattern.contains('*') {
                if let Some((parent, _)) = selector.pattern.rsplit_once('/') {
                    let suggestion = CapabilityRequirement {
                        capability: request.requirement.capability.clone(),
                        selector: ResourceSelector::File {
                            selector: FileSelector {
                                root: selector.root.clone(),
                                pattern: format!("{parent}/**"),
                            },
                        },
                    };
                    suggested_patterns.push(suggestion);
                }
            }
        }
        Self {
            request,
            exact,
            suggested_patterns,
            broad_scope_warning,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::effects::{FileOperation, FileSelector};

    fn request(requirement: CapabilityRequirement) -> CapabilityRequest {
        CapabilityRequest {
            id: Uuid::new_v4(),
            execution_id: Uuid::new_v4(),
            effect_sequence: None,
            requirement,
            arguments: Vec::new(),
            reason: "test".into(),
            origin: SourceOrigin::generated("test"),
            agent_ancestry: Vec::new(),
            program_hash: "program".into(),
        }
    }

    #[test]
    fn project_grant_covers_only_narrower_resources_in_same_project() {
        let project = "project-1".to_string();
        let broad = CapabilityRequirement::file(
            FileOperation::Write,
            FileSelector::parse("./generated/**").unwrap(),
        );
        let narrow = CapabilityRequirement::file(
            FileOperation::Write,
            FileSelector::parse("./generated/report.md").unwrap(),
        );
        let request = request(narrow);
        let grant_id = Uuid::new_v4();
        let grants = GrantSet {
            grants: vec![CapabilityGrant {
                id: grant_id,
                requirement: broad,
                scope: GrantScope::Project {
                    project_id: project.clone(),
                },
                policy_hash: "policy".into(),
                created_by: "user".into(),
                created_at_unix_ms: 0,
                expires_at_unix_ms: None,
                revoked_at_unix_ms: None,
                consumed_at_unix_ms: None,
            }],
        };
        let context = AuthorizationContext {
            now_unix_ms: 1,
            task_id: None,
            session_id: Uuid::new_v4(),
            project_id: Some(project),
            policy_hash: "policy".into(),
        };
        assert_eq!(
            grants.authorize(&request, &context),
            AuthorizationDecision::Allowed { grant_id }
        );
    }

    #[test]
    fn revocation_takes_effect_without_recompiling_program() {
        let requirement = CapabilityRequirement::file(
            FileOperation::Read,
            FileSelector::parse("./src/**").unwrap(),
        );
        let request = request(requirement.clone());
        let session_id = Uuid::new_v4();
        let grant_id = Uuid::new_v4();
        let mut grants = GrantSet {
            grants: vec![CapabilityGrant {
                id: grant_id,
                requirement,
                scope: GrantScope::Session { session_id },
                policy_hash: "policy".into(),
                created_by: "user".into(),
                created_at_unix_ms: 0,
                expires_at_unix_ms: None,
                revoked_at_unix_ms: None,
                consumed_at_unix_ms: None,
            }],
        };
        let context = AuthorizationContext {
            now_unix_ms: 1,
            task_id: None,
            session_id,
            project_id: None,
            policy_hash: "policy".into(),
        };
        assert!(matches!(
            grants.authorize(&request, &context),
            AuthorizationDecision::Allowed { .. }
        ));
        grants.revoke(grant_id, 2);
        assert_eq!(
            grants.authorize(&request, &context),
            AuthorizationDecision::ApprovalRequired
        );
    }

    #[test]
    fn dialog_suggests_parent_pattern_without_changing_exact_request() {
        let requirement = CapabilityRequirement::file(
            FileOperation::Write,
            FileSelector::parse("./generated/report.md").unwrap(),
        );
        let prompt = ApprovalPrompt::for_request(request(requirement.clone()));
        assert_eq!(prompt.exact, requirement);
        assert_eq!(prompt.suggested_patterns.len(), 1);
    }

    #[test]
    fn ledger_persists_grant_identity_revocation_and_audit_order() {
        let requirement = CapabilityRequirement::file(
            FileOperation::Read,
            FileSelector::parse("./src/**").unwrap(),
        );
        let mut ledger = CapabilityLedger::default();
        let id = ledger.grant_global(requirement.clone(), "policy", "user", 10);
        ledger.grants.grants.push(CapabilityGrant {
            id: Uuid::new_v4(),
            requirement: CapabilityRequirement::file(
                FileOperation::Write,
                FileSelector::parse("./project-only/**").unwrap(),
            ),
            scope: GrantScope::Project {
                project_id: "project".into(),
            },
            policy_hash: "policy".into(),
            created_by: "user".into(),
            created_at_unix_ms: 10,
            expires_at_unix_ms: None,
            revoked_at_unix_ms: None,
            consumed_at_unix_ms: None,
        });
        assert_eq!(
            ledger
                .grants
                .active_global_requirements(11)
                .cloned()
                .collect::<Vec<_>>(),
            vec![requirement]
        );
        assert!(ledger.revoke(id, "user", 12));
        assert!(ledger
            .grants
            .active_global_requirements(13)
            .next()
            .is_none());
        assert_eq!(ledger.audit.len(), 2);
        assert_eq!(ledger.audit[0].sequence, 0);
        assert_eq!(ledger.audit[1].sequence, 1);
        assert_eq!(ledger.audit[1].action, CapabilityAuditAction::Revoked);

        let encoded = serde_json::to_string(&ledger).unwrap();
        assert_eq!(
            serde_json::from_str::<CapabilityLedger>(&encoded).unwrap(),
            ledger
        );
    }

    #[test]
    fn ledger_issues_validated_scoped_and_expiring_grants() {
        let requirement = CapabilityRequirement::file(
            FileOperation::Read,
            FileSelector::parse("./reports/**").unwrap(),
        );
        let mut ledger = CapabilityLedger::default();
        let session_id = Uuid::new_v4();
        let grant_id = ledger
            .issue(
                requirement.clone(),
                GrantScope::Session { session_id },
                "policy-v1",
                "user",
                10,
                Some(20),
            )
            .unwrap();
        let grant = ledger
            .grants
            .grants
            .iter()
            .find(|grant| grant.id == grant_id)
            .unwrap();
        assert_eq!(grant.scope, GrantScope::Session { session_id });
        assert!(grant.is_active(19));
        assert!(!grant.is_active(20));
        assert!(ledger
            .issue(
                requirement.clone(),
                GrantScope::Project {
                    project_id: " ".into(),
                },
                "policy-v1",
                "user",
                10,
                None,
            )
            .unwrap_err()
            .contains("project identity"));
        assert!(ledger
            .issue(
                requirement,
                GrantScope::Global,
                "policy-v1",
                "user",
                10,
                Some(10),
            )
            .unwrap_err()
            .contains("expiry"));
    }

    #[test]
    fn authorization_audit_consumes_allow_once_exactly_once() {
        let requirement = CapabilityRequirement::file(
            FileOperation::Write,
            FileSelector::parse("./generated/report.md").unwrap(),
        );
        let request = request(requirement.clone());
        let mut ledger = CapabilityLedger::default();
        let grant_id = ledger
            .issue(
                requirement,
                GrantScope::Once {
                    request_id: request.id,
                },
                "policy-v1",
                "user",
                10,
                None,
            )
            .unwrap();
        let context = AuthorizationContext {
            now_unix_ms: 11,
            task_id: None,
            session_id: Uuid::new_v4(),
            project_id: None,
            policy_hash: "policy-v1".into(),
        };
        assert_eq!(
            ledger.authorize(&request, &context, "runtime"),
            AuthorizationDecision::Allowed { grant_id }
        );
        assert_eq!(
            ledger.authorize(&request, &context, "runtime"),
            AuthorizationDecision::ApprovalRequired
        );
        assert_eq!(ledger.audit.len(), 2);
        assert_eq!(ledger.audit[1].action, CapabilityAuditAction::Consumed);
        assert_eq!(ledger.authorization_audit.len(), 2);
        assert_eq!(ledger.authorization_audit[0].sequence, 2);
        assert_eq!(ledger.authorization_audit[1].sequence, 3);
        assert_eq!(
            ledger.authorization_audit[1].decision,
            AuthorizationDecision::ApprovalRequired
        );

        let encoded = serde_json::to_string(&ledger).unwrap();
        assert_eq!(
            serde_json::from_str::<CapabilityLedger>(&encoded).unwrap(),
            ledger
        );
    }
}
