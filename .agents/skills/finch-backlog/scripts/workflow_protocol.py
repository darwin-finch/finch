#!/usr/bin/env python3
"""Pure reducers for Finch's append-only workflow records.

The status reporter owns GitHub/git discovery and immutable-metadata verification.  This
module accepts already-observed records plus authenticated identities, validates records
before admitting them, and reduces only admitted history.  It performs no I/O or mutation.
"""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass
from typing import Any, Iterable, Mapping, Sequence


PRIMARY = {
    "DRAFT", "NEEDS_SPECIFICATION", "READY", "IN_PROGRESS",
    "REPAIR_IN_PROGRESS", "READY_TO_MERGE", "COMPLETE",
}
SIDE = {"BLOCKED_EXTERNAL", "INFEASIBLE", "DECLINED", "SUPERSEDED"}
STATES = PRIMARY | SIDE
ACTIVE = {"IN_PROGRESS", "REPAIR_IN_PROGRESS", "READY_TO_MERGE"}
_BLOCK = re.compile(r"<!--\s*([a-z0-9-]+:v[0-9]+)\s*\n(.*?)\n\s*-->", re.DOTALL)


@dataclass(frozen=True)
class Reduction:
    state: str | None
    admitted_ids: tuple[str, ...] = ()
    diagnostics: tuple[str, ...] = ()
    indeterminate: bool = False
    pairing_required: bool = False


@dataclass(frozen=True)
class FindingReduction:
    current: Mapping[str, Mapping[str, Any]]
    diagnostics: tuple[str, ...] = ()
    indeterminate: bool = False


def parse_event_blocks(raw_body: str, observation: Mapping[str, Any]) -> tuple[list[dict[str, Any]], list[str]]:
    """Parse versioned HTML event blocks and attach trusted immutable observations."""
    records: list[dict[str, Any]] = []
    diagnostics: list[str] = []
    immutable = (observation.get("last_edited_at") is None
                 if "last_edited_at" in observation
                 else observation.get("updated_at") == observation.get("created_at"))
    digest = hashlib.sha256(raw_body.encode("utf-8")).hexdigest()
    if observation.get("saved_digest") not in (None, digest):
        return [], ["saved raw-body digest changed"]
    for block_order, match in enumerate(_BLOCK.finditer(raw_body)):
        record: dict[str, Any] = {
            "schema": match.group(1), "block_order": block_order,
            "created_at": observation.get("created_at"),
            "comment_id": observation.get("comment_id"), "author": observation.get("author"),
            "immutable": immutable, "body_digest": digest,
        }
        malformed = False
        for line in match.group(2).splitlines():
            if not line.strip():
                continue
            if ":" not in line:
                malformed = True
                break
            key, value = line.split(":", 1)
            key = key.strip().replace("-", "_")
            if not key or key in record:
                malformed = True
                break
            record[key] = value.strip()
        if malformed:
            diagnostics.append(f"ignored malformed {record['schema']} block {block_order}")
            continue
        records.append(record)
    return records, diagnostics


def _ordered(records: Iterable[Mapping[str, Any]]) -> list[Mapping[str, Any]]:
    return sorted(records, key=lambda r: (
        r.get("created_at", ""), int(r.get("comment_id", 0)),
        int(r.get("block_order", 0)),
    ))


def _immutable(record: Mapping[str, Any]) -> bool:
    return bool(record.get("immutable", False)) and bool(record.get("body_digest"))


def _has(record: Mapping[str, Any], *names: str) -> bool:
    return all(record.get(name) not in (None, "", [], {}) for name in names)


def validate_contract(contract: Mapping[str, Any], approval: Mapping[str, Any],
                      authority: Mapping[str, Any]) -> list[str]:
    """Return diagnostics; an empty result means the contract may support READY."""
    errors: list[str] = []
    contract_fields = (
        "event_id", "contract_id", "issue", "revision", "implementation_base",
        "owner_worker", "owner_actor", "scope", "created_at", "comment_id",
        "body_digest", "sections",
    )
    approval_fields = (
        "event_id", "contract_id", "contract_url", "contract_digest", "revision",
        "implementation_base", "reviewer_worker", "reviewer_actor", "verdict",
        "review_output", "created_at", "comment_id", "body_digest",
    )
    if not _has(contract, *contract_fields) or not _immutable(contract):
        errors.append("contract is malformed, incomplete, or not immutable")
        return errors
    required_sections = {
        "observed_failure", "intended_behavior", "non_goals", "invariants",
        "boundaries", "regression_proof", "integration_proof", "reversion_plan",
        "hostile_cases", "gate_ownership",
    }
    if not required_sections <= set(contract["sections"]):
        errors.append("contract omits a required solution section")
    if not _has(approval, *approval_fields) or not _immutable(approval):
        errors.append("approval is malformed, incomplete, or not immutable")
        return errors
    for left, right in (("contract_id", "contract_id"), ("revision", "revision"),
                        ("implementation_base", "implementation_base")):
        if contract[left] != approval[right]:
            errors.append(f"approval {right} does not bind the contract")
    if contract["body_digest"] != approval["contract_digest"]:
        errors.append("approval digest does not bind the immutable contract body")
    if approval["verdict"] != "APPROVE":
        errors.append("contract does not have an APPROVE verdict")
    forbidden_workers = set(authority.get("author_workers", ())) | {
        authority.get("coordinator_worker"), authority.get("implementer_worker")
    }
    if approval["reviewer_worker"] in forbidden_workers:
        errors.append("plan reviewer is not independent from author/coordinator/implementer")
    direct = approval.get("poster_actor") == approval["reviewer_actor"]
    delegated = approval.get("delegation_valid") is True
    if not (direct or delegated):
        errors.append("approval poster lacks direct or prior operation-specific authority")
    return errors


# Each edge names acceptable authenticated roles and typed destination evidence.
_TRANSITIONS: dict[tuple[str | None, str], tuple[set[str], tuple[str, ...]]] = {
    (None, "DRAFT"): ({"issue_author", "coordinator"}, ("outcome", "issue_author", "next_clarification")),
    (None, "NEEDS_SPECIFICATION"): ({"issue_author", "coordinator"}, ("discovery_packet", "decision_owner")),
    (None, "READY"): ({"contract_owner", "coordinator"}, ("approved_contract",)),
    ("DRAFT", "NEEDS_SPECIFICATION"): ({"issue_author", "coordinator"}, ("discovery_packet",)),
    ("DRAFT", "READY"): ({"contract_owner", "coordinator"}, ("approved_contract",)),
    ("DRAFT", "DECLINED"): ({"issue_author"}, ("decision_packet",)),
    ("DRAFT", "SUPERSEDED"): ({"issue_author"}, ("replacement_packet",)),
    ("NEEDS_SPECIFICATION", "READY"): ({"contract_owner", "coordinator"}, ("approved_contract",)),
    ("NEEDS_SPECIFICATION", "BLOCKED_EXTERNAL"): ({"decision_owner", "coordinator"}, ("external_packet",)),
    ("NEEDS_SPECIFICATION", "INFEASIBLE"): ({"contract_owner"}, ("infeasibility_packet", "owner_disposition")),
    ("NEEDS_SPECIFICATION", "DECLINED"): ({"contract_owner"}, ("decision_packet",)),
    ("NEEDS_SPECIFICATION", "SUPERSEDED"): ({"contract_owner"}, ("replacement_packet",)),
    ("READY", "IN_PROGRESS"): ({"coordinator", "claim_worker"}, ("approved_contract", "active_claim")),
    ("READY", "NEEDS_SPECIFICATION"): ({"contract_owner", "coordinator"}, ("invalidated_contract", "question")),
    ("READY", "BLOCKED_EXTERNAL"): ({"contract_owner", "coordinator"}, ("external_packet",)),
    ("READY", "INFEASIBLE"): ({"contract_owner"}, ("infeasibility_packet", "owner_disposition")),
    ("READY", "DECLINED"): ({"contract_owner"}, ("decision_packet",)),
    ("READY", "SUPERSEDED"): ({"contract_owner"}, ("replacement_packet",)),
    ("IN_PROGRESS", "REPAIR_IN_PROGRESS"): ({"coordinator", "claim_worker"}, ("review_round", "ledger", "exact_tip", "correction_owner")),
    ("IN_PROGRESS", "READY_TO_MERGE"): ({"coordinator"}, ("zero_ledger", "clean_review", "integration_base", "exact_tip", "gate_evidence", "artifact_evidence")),
    ("REPAIR_IN_PROGRESS", "READY_TO_MERGE"): ({"coordinator"}, ("zero_ledger", "clean_review", "integration_base", "exact_tip", "gate_evidence", "artifact_evidence")),
    ("REPAIR_IN_PROGRESS", "IN_PROGRESS"): ({"coordinator", "claim_worker"}, ("approved_contract", "active_claim", "new_epoch")),
    ("READY_TO_MERGE", "REPAIR_IN_PROGRESS"): ({"coordinator"}, ("repair_reason", "review_round", "ledger", "exact_tip", "correction_owner")),
    ("READY_TO_MERGE", "COMPLETE"): ({"contract_owner", "coordinator"}, ("completion_packet",)),
    ("BLOCKED_EXTERNAL", "NEEDS_SPECIFICATION"): ({"decision_owner", "coordinator"}, ("changed_question",)),
    ("BLOCKED_EXTERNAL", "READY"): ({"contract_owner", "coordinator"}, ("resumption_proof", "approved_contract")),
    ("BLOCKED_EXTERNAL", "INFEASIBLE"): ({"contract_owner"}, ("infeasibility_packet", "owner_disposition")),
    ("BLOCKED_EXTERNAL", "DECLINED"): ({"contract_owner"}, ("decision_packet",)),
    ("BLOCKED_EXTERNAL", "SUPERSEDED"): ({"contract_owner"}, ("replacement_packet",)),
    ("INFEASIBLE", "NEEDS_SPECIFICATION"): ({"contract_owner"}, ("revised_constraints", "question")),
    ("INFEASIBLE", "DECLINED"): ({"contract_owner"}, ("decision_packet",)),
    ("INFEASIBLE", "SUPERSEDED"): ({"contract_owner"}, ("replacement_packet",)),
    ("DECLINED", "NEEDS_SPECIFICATION"): ({"contract_owner"}, ("resumed_intent", "question")),
    ("DECLINED", "SUPERSEDED"): ({"contract_owner"}, ("replacement_packet",)),
}

for _source in ACTIVE:
    _TRANSITIONS[(_source, "READY")] = ({"coordinator"}, ("terminal_pair", "recovery_evidence"))
    _TRANSITIONS[(_source, "NEEDS_SPECIFICATION")] = ({"contract_owner", "coordinator"}, ("terminal_pair", "invalidated_contract", "recovery_evidence"))
    _TRANSITIONS[(_source, "BLOCKED_EXTERNAL")] = ({"contract_owner", "coordinator"}, ("terminal_pair", "external_packet"))
    _TRANSITIONS[(_source, "INFEASIBLE")] = ({"contract_owner"}, ("terminal_pair", "infeasibility_packet", "owner_disposition"))
    _TRANSITIONS[(_source, "DECLINED")] = ({"contract_owner"}, ("terminal_pair", "decision_packet"))
    _TRANSITIONS[(_source, "SUPERSEDED")] = ({"contract_owner"}, ("atomic_transfer", "replacement_packet"))

_TRANSITIONS[("IN_PROGRESS", "IN_PROGRESS")] = ({"coordinator", "claim_worker"}, ("handoff_activation", "active_claim", "approved_contract"))
_TRANSITIONS[("REPAIR_IN_PROGRESS", "REPAIR_IN_PROGRESS")] = ({"coordinator", "claim_worker"}, ("handoff_activation", "active_claim", "ledger", "correction_owner"))

_PAIR_DISPOSITION = {
    "READY": "return-ready", "NEEDS_SPECIFICATION": "needs-specification",
    "BLOCKED_EXTERNAL": "blocked-external", "INFEASIBLE": "infeasible",
    "DECLINED": "declined",
}


def readiness_matrix() -> Mapping[tuple[str | None, str], tuple[set[str], tuple[str, ...]]]:
    return _TRANSITIONS


def validate_readiness_attempt(prior_state: str | None, event: Mapping[str, Any]) -> list[str]:
    errors: list[str] = []
    if event.get("identity_valid") is False:
        errors.append("issue/contract/claim/evidence identity is mismatched")
    rule = _TRANSITIONS.get((prior_state, event.get("new_state")))
    if event.get("new_state") not in STATES or rule is None:
        return [f"invalid readiness transition {prior_state}->{event.get('new_state')}"]
    roles, required = rule
    if event.get("authenticated_role") not in roles and not event.get("proxy_valid"):
        errors.append("poster lacks authenticated direct or operation-specific proxy authority")
    if not _has(event, *required):
        errors.append("destination is missing typed evidence")
    if event.get("legacy_bootstrap") is not None and not legacy_bootstrap_valid(event["legacy_bootstrap"]):
        errors.append("legacy bootstrap inventory/cutover evidence is incomplete")
    destination = event.get("new_state")
    if destination in _PAIR_DISPOSITION:
        pair = event.get("terminal_pair", {})
        if pair and (pair.get("disposition") != _PAIR_DISPOSITION[destination]
                     or pair.get("claim_id") != event.get("claim_id")
                     or not pair.get("authority_valid")
                     or not pair.get("evidence_matches")):
            errors.append("terminal/readiness pairing is mismatched")
    return errors


def reduce_readiness(records: Sequence[Mapping[str, Any]]) -> Reduction:
    admitted: list[str] = []
    diagnostics: list[str] = []
    state: str | None = None
    accepted_successor: dict[str, str] = {}
    accepted_by_id: dict[str, Mapping[str, Any]] = {}
    pairing_required = False

    for event in _ordered(records):
        event_id = event.get("event_id")
        if not event_id or not _immutable(event):
            diagnostics.append("ignored malformed or mutable readiness attempt")
            continue
        if not _has(event, "issue", "owner", "evidence_kind", "evidence_digest"):
            diagnostics.append(f"ignored incomplete readiness event {event_id}")
            continue
        if event.get("previously_accepted_changed") or event.get("authoritative_retrieval_incomplete"):
            return Reduction("INDETERMINATE", tuple(admitted), tuple(diagnostics + ["accepted history integrity is unprovable"]), True)
        if event_id in accepted_by_id:
            diagnostics.append(f"ignored duplicate event id {event_id}")
            continue
        prior_id = event.get("prior_event_id")
        prior_state = event.get("prior_state")
        new_state = event.get("new_state")
        predecessor_key = prior_id or "<root>"
        predecessor = accepted_by_id.get(prior_id) if prior_id else None
        actual_prior_state = predecessor.get("new_state") if predecessor else None
        if prior_state != actual_prior_state:
            diagnostics.append(f"ignored noncontiguous readiness attempt {event_id}")
            continue
        attempt_errors = validate_readiness_attempt(prior_state, event)
        if attempt_errors:
            diagnostics.extend(f"ignored {event_id}: {error}" for error in attempt_errors)
            pairing_required = any("pairing" in error for error in attempt_errors)
            continue
        if predecessor_key in accepted_successor:
            return Reduction("INDETERMINATE", tuple(admitted), tuple(diagnostics + ["multiple valid successors from one predecessor"]), True)
        if prior_id is not None and (not admitted or prior_id != admitted[-1]):
            diagnostics.append(f"ignored stale predecessor for readiness event {event_id}")
            continue
        accepted_successor[predecessor_key] = event_id
        accepted_by_id[event_id] = event
        admitted.append(event_id)
        state = new_state
        pairing_required = False
    return Reduction(state, tuple(admitted), tuple(diagnostics), pairing_required=pairing_required)


def terminal_pair_status(terminal: Mapping[str, Any], readiness: Mapping[str, Any] | None) -> str:
    """Return ACCEPTED, PAIRING_REQUIRED, or INVALID for post-cutover pairs."""
    disposition_to_state = {value: key for key, value in _PAIR_DISPOSITION.items()}
    expected = disposition_to_state.get(terminal.get("disposition"))
    if expected is None or not terminal.get("authority_valid"):
        return "INVALID"
    if readiness is None:
        return "PAIRING_REQUIRED"
    if (readiness.get("new_state") != expected
            or readiness.get("claim_id") != terminal.get("claim_id")
            or readiness.get("terminal_url") != terminal.get("url")
            or readiness.get("evidence_digest") != terminal.get("evidence_digest")):
        return "INVALID"
    return "ACCEPTED"


def finding_blocks(finding: Mapping[str, Any]) -> bool:
    return (finding.get("confidence") == "CONFIRMED"
            and finding.get("locality") == "SAME_CONTRACT"
            and finding.get("obligation") in {"BLOCKER", "REGRESSION_DEBT"}
            and finding.get("state") != "RESOLVED")


def reduce_findings(records: Sequence[Mapping[str, Any]]) -> FindingReduction:
    current: dict[str, Mapping[str, Any]] = {}
    event_ids: set[str] = set()
    diagnostics: list[str] = []
    for event in _ordered(records):
        required = ("event_id", "finding_id", "ledger_id", "round_id", "confidence",
                    "severity", "locality", "obligation", "state", "exact_tip",
                    "scenario", "proof", "owner")
        if not _immutable(event) or not _has(event, *required):
            diagnostics.append("ignored malformed finding event")
            continue
        if event["event_id"] in event_ids:
            diagnostics.append(f"ignored duplicate finding event {event['event_id']}")
            continue
        prior = current.get(event["finding_id"])
        expected = None if prior is None else prior["event_id"]
        if event.get("prior_event_id") != expected:
            predecessor_exists = (event.get("prior_event_id") is None or
                                  event.get("prior_event_id") in event_ids)
            if predecessor_exists:
                return FindingReduction(current, tuple(diagnostics + [f"forked finding history for {event['finding_id']}"]), True)
            diagnostics.append(f"ignored missing predecessor for {event['finding_id']}")
            continue
        if event["confidence"] not in {"CONFIRMED", "PLAUSIBLE"} or event["severity"] not in {"CRITICAL", "HIGH", "MEDIUM", "LOW"} or event["locality"] not in {"SAME_CONTRACT", "INDEPENDENT"} or event["obligation"] not in {"BLOCKER", "REGRESSION_DEBT", "NONBLOCKING"} or event["state"] not in {"OPEN", "RESOLVED", "REPLACED-BY", "SPLIT-TO"}:
            diagnostics.append(f"ignored unknown finding axis for {event['finding_id']}")
            continue
        if event["state"] == "RESOLVED" and not (
                event.get("independently_verified") is True
                and event.get("proof_tip") == event.get("exact_tip")
                and event.get("proof_integration_base")):
            diagnostics.append(f"ignored unproven RESOLVED event for {event['finding_id']}")
            continue
        event_ids.add(event["event_id"])
        current[event["finding_id"]] = event
    return FindingReduction(current, tuple(diagnostics))


def successor_obligation_complete(root: str, findings: Mapping[str, Mapping[str, Any]],
                                  children: Mapping[str, Mapping[str, Any]]) -> bool:
    visiting: set[str] = set()
    seen_gates: set[str] = set()

    def visit(node: str) -> bool:
        if node in visiting or node not in findings:
            return False
        item = findings[node]
        if item.get("state") == "RESOLVED":
            return bool(item.get("current_main_proof")) and not item.get("successors")
        if item.get("state") not in {"REPLACED-BY", "SPLIT-TO"}:
            return False
        successors = item.get("successors", ())
        if not successors or len(successors) != len(set(successors)):
            return False
        visiting.add(node)
        for successor in successors:
            child = children.get(successor)
            if child is not None:
                if not child.get("active_claim") or not child.get("current_main_proof"):
                    visiting.remove(node)
                    return False
                gates = child.get("gates", ())
                if not gates or any(gate in seen_gates for gate in gates):
                    visiting.remove(node)
                    return False
                seen_gates.update(gates)
            elif not visit(successor):
                visiting.remove(node)
                return False
        visiting.remove(node)
        return True

    ok = visit(root)
    expected = set(findings.get(root, {}).get("affected_gates", ()))
    return ok and (not expected or seen_gates == expected)


def atomic_transfer(transaction: Mapping[str, Any], kind: str) -> bool:
    """Validate a split/handoff reservation without partially activating it."""
    if kind not in {"split", "handoff"} or not transaction.get("parent_authorized"):
        return False
    reserved = transaction.get("expected_children", ())
    actual = transaction.get("child_reservations", ())
    if not transaction.get("activation") or len(reserved) != len(actual):
        return False
    expected = {c["claim_id"]: c for c in reserved}
    if len(expected) != len(reserved):
        return False
    scopes: set[str] = set()
    for child in actual:
        wanted = expected.get(child.get("claim_id"))
        if wanted != child or not child.get("nonauthorizing_reservation"):
            return False
        if child.get("scope") in scopes:
            return False
        scopes.add(child.get("scope"))
    gate_map = transaction.get("gate_map", ())
    expected_gates = transaction.get("expected_gates", ())
    if not gate_map or len(expected_gates) != len(set(expected_gates)):
        return False
    mapped_gates = [entry.get("gate_id") for entry in gate_map]
    child_ids = set(expected)
    if (len(mapped_gates) != len(set(mapped_gates))
            or set(mapped_gates) != set(expected_gates)):
        return False
    return all(entry.get("child_claim_id") in child_ids and entry.get("proof_path")
               for entry in gate_map)


def terminal_event_valid(event: Mapping[str, Any]) -> bool:
    allowed = {"return-ready", "blocked-external", "needs-specification", "infeasible",
               "declined", "slice-complete", "outcome-superseded", "legacy"}
    if event.get("disposition") not in allowed:
        return False
    if event.get("post_cutover") and event.get("disposition") == "legacy":
        return False
    direct = (event.get("poster_actor") == event.get("original_author")
              and event.get("worker") == event.get("original_worker"))
    delegated = (event.get("delegation_prior") is True
                 and event.get("delegation_claim_id") == event.get("claim_id")
                 and event.get("delegation_operation") == event.get("event"))
    if not (direct or delegated) or not event.get("evidence_url"):
        return False
    if event.get("disposition") in {"infeasible", "declined"} and not event.get("owner_disposition"):
        return False
    return True


def legacy_bootstrap_valid(packet: Mapping[str, Any]) -> bool:
    required = (
        "cutover_identity", "claim_predates_cutover", "preserved_claim",
        "preserved_pr", "preserved_branch", "preserved_worktree", "preserved_tip",
        "preserved_base", "approved_contract", "collision_recheck", "exact_tip_review",
    )
    return all(packet.get(field) not in (None, False, "", [], {}) for field in required)


def scope_revision_allowed(event: Mapping[str, Any]) -> bool:
    direct = event.get("poster_actor") == event.get("original_author")
    delegated = (event.get("delegation_prior") is True
                 and event.get("delegation_operation") == "scope-revise"
                 and event.get("delegation_claim_id") == event.get("claim_id")
                 and event.get("delegation_scope") == event.get("new_scope")
                 and event.get("delegation_contract_revision") == event.get("contract_revision"))
    return bool((direct or delegated) and event.get("prior_scope_matches")
                and event.get("contract_approved") and event.get("collision_check_complete"))


def validate_review_round(record: Mapping[str, Any]) -> list[str]:
    fields = (
        "event_id", "round_id", "ledger_id", "issue", "pull_request", "claim_id",
        "contract_id", "contract_url", "contract_digest", "claim_base",
        "integration_base", "exact_tip", "round_number", "selected_perspectives",
        "skipped_perspectives", "status", "finding_event_ids", "gate_evidence",
    )
    errors: list[str] = []
    if not _immutable(record) or not _has(record, *fields):
        errors.append("review round is malformed, incomplete, or mutable")
    if record.get("status") not in {"DISCOVERY", "VERIFICATION", "REPAIR_IN_PROGRESS", "CONVERGED"}:
        errors.append("review round has unknown status")
    if record.get("verdict") not in {None, "SAFE_TO_MERGE", "ESCALATE_WITH_EXECUTABLE_REPAIR_OR_SPLIT"}:
        errors.append("review round has unknown final verdict")
    if record.get("panel_diff_base") != record.get("integration_base"):
        errors.append("review panel was not derived from the integration base")
    return errors


def policy_contradictions(text: str) -> list[str]:
    """Detect explicit legacy overrides; prose smoke is not the behavioral proof."""
    normalized = " ".join(text.lower().split())
    forbidden = {
        "pre-ready production": "production may begin before ready",
        "severity cancellation": "critical automatically cancels",
        "immediate successor discharge": "split-to immediately resolves",
        "proofless completion": "complete does not require user-visible proof",
    }
    return [name for name, phrase in forbidden.items() if phrase in normalized]


_COMPLETE_FIELDS = (
    "merged_current_main", "all_successors_resolved", "issue_closed",
    "claim_terminal", "artifact_identity", "user_visible_proof", "cleanup_safe",
    "frontier_recomputed",
)


def completion_allowed(packet: Mapping[str, Any]) -> bool:
    return all(packet.get(field) is True for field in _COMPLETE_FIELDS)


def scoped_slice_completion_allowed(packet: Mapping[str, Any]) -> bool:
    return all(packet.get(field) is True for field in (
        "reached_ready_to_merge", "merged_slice_proof", "parent_repair_event",
        "active_successor_claims", "claim_terminal_after_parent_event", "issue_remains_open",
    ))


def review_diff_base(round_record: Mapping[str, Any]) -> str | None:
    """Panel derivation always uses current integration base, never claim base."""
    return round_record.get("integration_base")
