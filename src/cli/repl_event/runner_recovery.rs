//! Human runner-recovery state for leftover daemons, lease gaps, and mismatches.
//!
//! The interactive header, queued-run rows, and `/brain runner status` share
//! this classification so a leftover daemon or environment mismatch cannot be
//! presented as a mute `queuedforenvironment` driver.

use crate::brain::BrainRunStatus;
use crate::ipc::leftover_daemon_message;

/// Why this frontend is not the live environment runner, and what to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerRecovery {
    Reconnecting,
    NoLiveLease,
    OtherOwner {
        subject: String,
    },
    MachineMismatch {
        expected: String,
        found: String,
    },
    WorkspaceMismatch {
        expected: String,
        found: String,
    },
    ProtocolMismatch {
        frontend: u32,
        daemon: u32,
        uptime_seconds: Option<u64>,
    },
    DaemonIpcUnavailable {
        detail: String,
    },
    HandoffRequired,
    Other {
        detail: String,
    },
}

impl RunnerRecovery {
    /// Classify a register/reconnect/IPC failure without exposing secrets.
    pub fn from_error(error: &str) -> Self {
        if let Some(recovery) = parse_protocol_mismatch(error) {
            return recovery;
        }
        if let Some(recovery) = parse_machine_mismatch(error) {
            return recovery;
        }
        if let Some(recovery) = parse_workspace_mismatch(error) {
            return recovery;
        }
        if let Some(subject) = parse_other_owner(error) {
            return Self::OtherOwner { subject };
        }
        if error.contains("handed off") {
            return Self::HandoffRequired;
        }
        if error.contains("Cap'n Proto")
            || error.contains("daemon connection unavailable")
            || error.contains("check local daemon IPC")
            || error.contains("reconnect local daemon IPC")
        {
            return Self::DaemonIpcUnavailable {
                detail: error.to_string(),
            };
        }
        if error.contains("no runner lease") {
            return Self::NoLiveLease;
        }
        if error.contains("lease expired") || error.contains("lease reacquired") {
            return Self::Reconnecting;
        }
        Self::Other {
            detail: error.to_string(),
        }
    }

    /// One-line explanation with the exact next action.
    pub fn human_message(&self) -> String {
        match self {
            Self::Reconnecting => "Queued — no runner connected · reconnecting…".into(),
            Self::NoLiveLease => {
                "No runner is connected. Run this Brain here with: /brain runner claim".into()
            }
            Self::OtherOwner { subject } => {
                format!("Queued — runner belongs to {subject} · request handoff")
            }
            Self::MachineMismatch { expected, found } => {
                format!(
                    "This console's machine is {found}; the Brain runs on {expected}. That is not a protocol mismatch."
                )
            }
            Self::WorkspaceMismatch { expected, found } => {
                format!(
                    "This console's workspace is {found}; the Brain runs in {expected}. That is not a protocol mismatch."
                )
            }
            Self::ProtocolMismatch {
                frontend,
                daemon,
                uptime_seconds,
            } => leftover_daemon_message(*frontend, *daemon, *uptime_seconds),
            Self::DaemonIpcUnavailable { detail } => {
                format!("Daemon IPC unavailable: {detail}")
            }
            Self::HandoffRequired => "Runner lease was handed off to another frontend".into(),
            Self::Other { detail } => detail.clone(),
        }
    }

    /// Compact header/status suffix, without the `◆ brain:` prefix.
    pub fn header_suffix(&self) -> String {
        match self {
            Self::Reconnecting => "reconnecting runner".into(),
            Self::NoLiveLease => "no runner · /brain runner claim".into(),
            Self::OtherOwner { subject } => format!("runner belongs to {subject}"),
            Self::MachineMismatch { .. } => "machine mismatch".into(),
            Self::WorkspaceMismatch { .. } => "workspace mismatch".into(),
            Self::ProtocolMismatch { .. } => "leftover daemon · finch daemon-stop".into(),
            Self::DaemonIpcUnavailable { .. } => "daemon IPC unavailable".into(),
            Self::HandoffRequired => "runner handed off".into(),
            Self::Other { .. } => "runner unavailable".into(),
        }
    }

    /// Queued-run row replacing raw `queuedforenvironment`.
    pub fn queued_label(&self) -> String {
        match self {
            Self::Reconnecting => "Queued — no runner connected · reconnecting…".into(),
            Self::NoLiveLease => {
                "Queued — no runner connected · /brain runner claim".into()
            }
            Self::OtherOwner { subject } => {
                format!("Queued — runner belongs to {subject} · request handoff")
            }
            Self::MachineMismatch { expected, found } => {
                format!("Queued — machine mismatch (expected {expected}, found {found})")
            }
            Self::WorkspaceMismatch { expected, found } => {
                format!("Queued — workspace mismatch (expected {expected}, found {found})")
            }
            Self::ProtocolMismatch {
                frontend,
                daemon,
                ..
            } => format!(
                "Queued — leftover daemon (protocol {daemon}, this Finch is {frontend}) · finch daemon-stop"
            ),
            Self::DaemonIpcUnavailable { .. } => {
                "Queued — daemon IPC unavailable · check local daemon".into()
            }
            Self::HandoffRequired => {
                "Queued — runner handed off · request handoff".into()
            }
            Self::Other { detail } => format!("Queued — no runner connected · {detail}"),
        }
    }

    /// Next recovery action for `/brain runner status`.
    pub fn next_action(&self) -> &'static str {
        match self {
            Self::Reconnecting => "wait for automatic reconnect, or /brain runner claim",
            Self::NoLiveLease => "/brain runner claim",
            Self::OtherOwner { .. } | Self::HandoffRequired => {
                "/brain handoff accept  (or ask the owner to /brain handoff <this identity>)"
            }
            Self::MachineMismatch { .. } | Self::WorkspaceMismatch { .. } => {
                "open Finch on the Brain's machine and workspace"
            }
            Self::ProtocolMismatch { .. } => "finch daemon-stop   then relaunch Finch",
            Self::DaemonIpcUnavailable { .. } => "finch daemon-stop   then relaunch Finch",
            Self::Other { .. } => "/brain runner status",
        }
    }

    /// Leftover/environment mismatches must not attach as a mute driver.
    pub fn blocks_driver_attach(&self) -> bool {
        matches!(
            self,
            Self::ProtocolMismatch { .. }
                | Self::MachineMismatch { .. }
                | Self::WorkspaceMismatch { .. }
        )
    }

    /// Bounded reconnect is only for a recoverable same-frontend gap.
    pub fn should_auto_reconnect(&self) -> bool {
        matches!(
            self,
            Self::Reconnecting | Self::DaemonIpcUnavailable { .. } | Self::Other { .. }
        )
    }
}

/// Label a durable run status, using recovery context for queued work.
pub fn brain_run_status_label(status: BrainRunStatus, recovery: Option<&RunnerRecovery>) -> String {
    if status == BrainRunStatus::QueuedForEnvironment {
        return recovery
            .map(RunnerRecovery::queued_label)
            .unwrap_or_else(|| {
                BrainRunStatus::QueuedForEnvironment
                    .human_label()
                    .to_string()
            });
    }
    status.human_label().to_string()
}

fn parse_protocol_mismatch(error: &str) -> Option<RunnerRecovery> {
    let frontend = capture_after(error, "This Finch speaks protocol ")
        .or_else(|| capture_after(error, "this frontend requires "))
        .or_else(|| capture_after(error, "requires "))?;
    let daemon = capture_after(error, "the running daemon speaks ")
        .or_else(|| capture_after(error, "uses IPC protocol "))
        .or_else(|| capture_after(error, "protocol "))?;
    let uptime_seconds = parse_uptime_from_message(error);
    Some(RunnerRecovery::ProtocolMismatch {
        frontend,
        daemon,
        uptime_seconds,
    })
}

fn parse_machine_mismatch(error: &str) -> Option<RunnerRecovery> {
    if !error.contains("machine does not match") {
        return None;
    }
    let (expected, found) = parse_expected_found(error)?;
    Some(RunnerRecovery::MachineMismatch { expected, found })
}

fn parse_workspace_mismatch(error: &str) -> Option<RunnerRecovery> {
    if !error.contains("workspace does not match") {
        return None;
    }
    let (expected, found) = parse_expected_found(error)?;
    Some(RunnerRecovery::WorkspaceMismatch { expected, found })
}

fn parse_other_owner(error: &str) -> Option<String> {
    let marker = "belongs to another subject (";
    let start = error.find(marker)? + marker.len();
    let end = error[start..].find(')')? + start;
    let subject = error[start..end].trim();
    if subject.is_empty() {
        None
    } else {
        Some(subject.to_string())
    }
}

fn parse_expected_found(error: &str) -> Option<(String, String)> {
    let expected_marker = "expected ";
    let found_marker = ", found ";
    let expected_at = error.find(expected_marker)? + expected_marker.len();
    let found_at = error[expected_at..].find(found_marker)? + expected_at;
    let expected = error[expected_at..found_at].trim().trim_end_matches(')');
    let found = error[found_at + found_marker.len()..]
        .trim()
        .trim_end_matches(')')
        .trim();
    if expected.is_empty() || found.is_empty() {
        None
    } else {
        Some((expected.to_string(), found.to_string()))
    }
}

fn capture_after(error: &str, marker: &str) -> Option<u32> {
    let start = error.find(marker)? + marker.len();
    let rest = error[start..].trim_start();
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn parse_uptime_from_message(error: &str) -> Option<u64> {
    let marker = "(up for ";
    let start = error.find(marker)? + marker.len();
    let rest = error[start..].split(')').next()?.trim();
    parse_human_uptime(rest)
}

fn parse_human_uptime(text: &str) -> Option<u64> {
    let mut total = 0_u64;
    let mut saw_unit = false;
    for token in text.split_whitespace() {
        let (number, unit) = token.split_at(token.find(|ch: char| !ch.is_ascii_digit())?);
        let value: u64 = number.parse().ok()?;
        total += match unit {
            "d" => value * 86_400,
            "h" => value * 3_600,
            "m" => value * 60,
            "s" => value,
            _ => return None,
        };
        saw_unit = true;
    }
    saw_unit.then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::IPC_PROTOCOL_VERSION;

    #[test]
    fn leftover_protocol_mismatch_names_both_generations_and_daemon_stop() {
        let recovery = RunnerRecovery::ProtocolMismatch {
            frontend: 9,
            daemon: 8,
            uptime_seconds: Some(7_200),
        };
        let message = recovery.human_message();
        assert!(
            message.contains("protocol 9") && message.contains("speaks 8"),
            "leftover error must name both generations; message={message}"
        );
        assert!(
            message.contains("finch daemon-stop"),
            "leftover error must name the kick command; message={message}"
        );
        assert!(
            message.contains("up for 2h"),
            "leftover error must include uptime; message={message}"
        );
        assert_eq!(
            recovery.queued_label(),
            "Queued — leftover daemon (protocol 8, this Finch is 9) · finch daemon-stop"
        );
        assert!(
            recovery.blocks_driver_attach(),
            "a leftover daemon must not attach as a mute driver; recovery={recovery:?}"
        );
        assert!(
            !recovery.should_auto_reconnect(),
            "kicking a leftover daemon requires an explicit stop, not silent reconnect; recovery={recovery:?}"
        );
    }

    #[test]
    fn workspace_and_machine_mismatch_are_not_protocol_mismatch() {
        let machine = RunnerRecovery::from_error(
            "frontend machine does not match the Brain environment (expected box.local, found other.local)",
        );
        match &machine {
            RunnerRecovery::MachineMismatch { expected, found } => {
                assert_eq!(expected, "box.local");
                assert_eq!(found, "other.local");
            }
            other => panic!("machine mismatch must not collapse into {other:?}"),
        }
        assert!(
            machine.human_message().contains("not a protocol mismatch"),
            "machine mismatch must say it is not a protocol mismatch; message={}",
            machine.human_message()
        );

        let workspace = RunnerRecovery::from_error(
            "frontend workspace does not match the Brain environment (expected /tmp/a, found /tmp/b)",
        );
        match &workspace {
            RunnerRecovery::WorkspaceMismatch { expected, found } => {
                assert_eq!(expected, "/tmp/a");
                assert_eq!(found, "/tmp/b");
            }
            other => panic!("workspace mismatch must not collapse into {other:?}"),
        }
        assert!(
            workspace.blocks_driver_attach(),
            "workspace mismatch must not attach as a mute driver; recovery={workspace:?}"
        );
    }

    #[test]
    fn other_owner_and_no_lease_are_distinct_from_leftover_daemon() {
        let owner = RunnerRecovery::from_error(
            "Brain runner lease belongs to another subject (alice@host/frontend)",
        );
        assert_eq!(
            owner.queued_label(),
            "Queued — runner belongs to alice@host/frontend · request handoff"
        );
        assert!(
            !owner.blocks_driver_attach(),
            "another owner is observable as a driver; recovery={owner:?}"
        );

        let queued = brain_run_status_label(BrainRunStatus::QueuedForEnvironment, None);
        assert_eq!(queued, "Queued — no runner connected");
        assert!(
            !queued.to_lowercase().contains("queuedforenvironment"),
            "raw Debug enum names must not reach the user; label={queued}"
        );
    }

    #[test]
    fn ipc_protocol_error_classifies_as_leftover_daemon() {
        let error = leftover_daemon_message(IPC_PROTOCOL_VERSION, 0, Some(120));
        let recovery = RunnerRecovery::from_error(&error);
        match recovery {
            RunnerRecovery::ProtocolMismatch {
                frontend,
                daemon,
                uptime_seconds,
            } => {
                assert_eq!(frontend, IPC_PROTOCOL_VERSION);
                assert_eq!(daemon, 0);
                assert_eq!(uptime_seconds, Some(120));
            }
            other => {
                panic!("IPC leftover message must classify as protocol mismatch; got {other:?}")
            }
        }
    }
}
