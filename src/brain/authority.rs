//! Executable least-privilege contract for named-Brain operations.
//!
//! This inventory is deliberately independent from HTTP route names and IPC
//! method numbers.  Transports must map an incoming operation to one row and
//! then satisfy that row's authority requirement; possessing IDs or resource
//! references is never one of those requirements.

use super::credential::BrainCredentialScope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrainTransport {
    LocalIpc,
    RemoteHttp,
    RemoteWebSocket,
    RunnerCallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrainAuthorityRequirement {
    /// A participant credential for the exact Brain identity and environment.
    Participant {
        scope: BrainCredentialScope,
        attachment_bound: bool,
    },
    /// Ephemeral authority owned by one authenticated local IPC connection.
    Connection,
    /// A node-wide administrator credential, intentionally not representable
    /// by `BrainCredentialScope` or by a participant invitation.
    NodeAdministrator,
    /// Authority held by the registered runner callback for an exact lease.
    RunnerLease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrainAuthorityCase {
    pub operation: &'static str,
    pub transport: BrainTransport,
    pub requirement: BrainAuthorityRequirement,
}

const fn participant(
    operation: &'static str,
    transport: BrainTransport,
    scope: BrainCredentialScope,
    attachment_bound: bool,
) -> BrainAuthorityCase {
    BrainAuthorityCase {
        operation,
        transport,
        requirement: BrainAuthorityRequirement::Participant {
            scope,
            attachment_bound,
        },
    }
}

const fn connection(operation: &'static str) -> BrainAuthorityCase {
    BrainAuthorityCase {
        operation,
        transport: BrainTransport::LocalIpc,
        requirement: BrainAuthorityRequirement::Connection,
    }
}

/// Current authority surface. Adding an operation without a row is a review
/// failure; changing a row changes the security contract and its generated
/// conformance tests.
pub const BRAIN_AUTHORITY_MATRIX: &[BrainAuthorityCase] = &[
    connection("events.watch"),
    connection("events.acknowledge"),
    connection("participant.attach"),
    connection("participant.detach"),
    connection("event.submit"),
    connection("approval.decide"),
    connection("run.cancel"),
    connection("schedule.create"),
    connection("schedule.cancel"),
    connection("environment.initialize"),
    connection("environment.effect.execute"),
    connection("compute.submit"),
    connection("runner.handoff.request"),
    connection("runner.handoff.cancel"),
    participant(
        "events.watch",
        BrainTransport::RemoteWebSocket,
        BrainCredentialScope::BrainRead,
        true,
    ),
    participant(
        "events.acknowledge",
        BrainTransport::RemoteWebSocket,
        BrainCredentialScope::BrainRead,
        true,
    ),
    participant(
        "participant.attach",
        BrainTransport::RemoteHttp,
        BrainCredentialScope::BrainAttach,
        false,
    ),
    participant(
        "participant.detach",
        BrainTransport::RemoteWebSocket,
        BrainCredentialScope::BrainDetach,
        true,
    ),
    participant(
        "event.submit",
        BrainTransport::RemoteWebSocket,
        BrainCredentialScope::BrainSubmit,
        true,
    ),
    participant(
        "approval.decide",
        BrainTransport::RemoteWebSocket,
        BrainCredentialScope::BrainApprove,
        true,
    ),
    participant(
        "run.cancel",
        BrainTransport::RemoteWebSocket,
        BrainCredentialScope::BrainSubmit,
        true,
    ),
    participant(
        "schedule.create",
        BrainTransport::RemoteWebSocket,
        BrainCredentialScope::BrainSubmit,
        true,
    ),
    participant(
        "schedule.cancel",
        BrainTransport::RemoteWebSocket,
        BrainCredentialScope::BrainSubmit,
        true,
    ),
    participant(
        "environment.initialize",
        BrainTransport::RemoteWebSocket,
        BrainCredentialScope::BrainSubmit,
        true,
    ),
    participant(
        "runner.handoff.request",
        BrainTransport::RemoteWebSocket,
        BrainCredentialScope::BrainControl,
        true,
    ),
    participant(
        "runner.handoff.cancel",
        BrainTransport::RemoteWebSocket,
        BrainCredentialScope::BrainControl,
        true,
    ),
    participant(
        "credential.delegate",
        BrainTransport::RemoteHttp,
        BrainCredentialScope::BrainControl,
        false,
    ),
    participant(
        "invitation.issue",
        BrainTransport::RemoteHttp,
        BrainCredentialScope::BrainControl,
        false,
    ),
    participant(
        "delegation.revoke",
        BrainTransport::RemoteHttp,
        BrainCredentialScope::BrainControl,
        false,
    ),
    participant(
        "environment.change",
        BrainTransport::RemoteHttp,
        BrainCredentialScope::EnvironmentAdmin,
        false,
    ),
    participant(
        "environment.effect.execute",
        BrainTransport::RemoteHttp,
        BrainCredentialScope::EnvironmentExecute,
        false,
    ),
    participant(
        "compute.submit",
        BrainTransport::RemoteHttp,
        BrainCredentialScope::ComputeSubmit,
        false,
    ),
    connection("runner.register"),
    BrainAuthorityCase {
        operation: "runner.callback",
        transport: BrainTransport::RunnerCallback,
        requirement: BrainAuthorityRequirement::RunnerLease,
    },
    BrainAuthorityCase {
        operation: "brain.create",
        transport: BrainTransport::RemoteHttp,
        requirement: BrainAuthorityRequirement::NodeAdministrator,
    },
    BrainAuthorityCase {
        operation: "brain.list",
        transport: BrainTransport::RemoteHttp,
        requirement: BrainAuthorityRequirement::NodeAdministrator,
    },
    BrainAuthorityCase {
        operation: "brain.archive",
        transport: BrainTransport::RemoteHttp,
        requirement: BrainAuthorityRequirement::NodeAdministrator,
    },
    BrainAuthorityCase {
        operation: "brain.delete",
        transport: BrainTransport::RemoteHttp,
        requirement: BrainAuthorityRequirement::NodeAdministrator,
    },
];

pub fn authority_case(
    operation: &str,
    transport: BrainTransport,
) -> Option<&'static BrainAuthorityCase> {
    BRAIN_AUTHORITY_MATRIX
        .iter()
        .find(|case| case.operation == operation && case.transport == transport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn matrix_keys_are_unique_and_transport_explicit() {
        let mut keys = BTreeSet::new();
        for case in BRAIN_AUTHORITY_MATRIX {
            assert!(keys.insert((case.operation, format!("{:?}", case.transport))));
        }
    }

    #[test]
    fn node_administration_is_never_participant_or_invitation_authority() {
        for operation in [
            "brain.create",
            "brain.list",
            "brain.archive",
            "brain.delete",
        ] {
            let case = authority_case(operation, BrainTransport::RemoteHttp).unwrap();
            assert_eq!(
                case.requirement,
                BrainAuthorityRequirement::NodeAdministrator
            );
            assert!(BRAIN_AUTHORITY_MATRIX.iter().all(|candidate| {
                candidate.operation != operation
                    || candidate.requirement == BrainAuthorityRequirement::NodeAdministrator
            }));
        }
    }

    #[test]
    fn administration_delegation_and_revocation_rows_are_unbound() {
        for operation in [
            "credential.delegate",
            "invitation.issue",
            "delegation.revoke",
            "environment.change",
        ] {
            let requirement = authority_case(operation, BrainTransport::RemoteHttp)
                .unwrap()
                .requirement;
            assert!(matches!(
                requirement,
                BrainAuthorityRequirement::Participant {
                    attachment_bound: false,
                    ..
                }
            ));
        }
    }
}
