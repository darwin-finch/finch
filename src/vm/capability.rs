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
}

impl CapabilityGrant {
    pub fn is_active(&self, now_unix_ms: u64) -> bool {
        self.revoked_at_unix_ms.is_none()
            && self
                .expires_at_unix_ms
                .is_none_or(|expires| now_unix_ms < expires)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityRequest {
    pub id: Uuid,
    pub execution_id: Uuid,
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
}
