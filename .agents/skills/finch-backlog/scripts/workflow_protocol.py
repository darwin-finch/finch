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
_BOOL_FIELDS = {
    "nonauthorizing", "zero_ledger", "clean_review", "fresh_clean_pass",
    "merged_current_main", "all_successors_resolved", "issue_closed", "claim_terminal",
    "artifact_identity", "user_visible_proof", "cleanup_safe", "frontier_recomputed",
    "authority_valid", "evidence_matches", "owner_disposition", "claim_predates_cutover",
    "collision_recheck", "exact_tip_review", "independently_verified", "current_main_proof",
    "active_claim", "terminal_claim", "ownership_valid", "merge_proof",
    "recovery_evidence", "gate_evidence", "artifact_evidence", "reached_ready_to_merge",
    "parent_repair_event", "active_successor_claims", "claim_terminal_after_parent_event",
    "issue_remains_open", "atomic_transfer", "replacement_packet", "new_epoch",
}
_INT_FIELDS = {"issue", "revision", "contract_revision", "round_number", "pull_request"}
_LIST_FIELDS = {
    "sections", "selected_perspectives", "skipped_perspectives", "finding_event_ids",
    "affected_gate_ids", "successor_finding_ids", "child_claim_ids", "expected_child_claim_ids",
    "expected_child_workers", "delegated_scopes", "expected_gates", "scope_items",
    "retained_scope_items", "parent_scope_items",
}
_ALIASES = {
    "finch-solution-contract:v1": {"owner_github_actor": "owner_actor"},
    "finch-solution-contract-approval:v1": {
        "contract_revision": "revision", "reviewer_github_actor": "reviewer_actor",
        "review_output_url": "review_output",
    },
    "finch-review-round:v1": {"gate_evidence_url": "gate_evidence"},
    "finch-review-finding:v1": {"scenario_evidence_url": "scenario_evidence"},
}


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


@dataclass(frozen=True)
class ReviewReduction:
    safe_to_merge: bool
    diagnostics: tuple[str, ...] = ()


def admit_claim(record: Mapping[str, Any], expected_issue: int) -> tuple[dict[str, Any] | None, str | None]:
    required = ("claim_id", "worker", "github_actor", "branch", "base", "scope", "timestamp", "url")
    if (record.get("schema") != "finch-work-claim:v1" or record.get("event") != "claim"
            or record.get("issue") != expected_issue or not _immutable(record)
            or not _has(record, *required)):
        return None, "claim is malformed, foreign, mutable, or incomplete"
    if record.get("author") != record.get("github_actor"):
        return None, "claim GitHub actor does not match immutable comment author"
    admitted = dict(record)
    admitted["active"] = True
    return admitted, None


def _coerce(key: str, value: str) -> Any:
    if key in _LIST_FIELDS:
        if value in {"", "none"}:
            return []
        return [part.strip() for part in value.split(",") if part.strip()]
    if value == "none":
        return None
    if key in _BOOL_FIELDS:
        if value not in {"true", "false"}:
            return value
        return value == "true"
    if key in _INT_FIELDS:
        try:
            return int(value)
        except ValueError:
            return value
    if key == "gate_map":
        entries = []
        for item in value.split(","):
            match = re.fullmatch(r"([^=]+)=([^@]+)@(.+)", item.strip())
            if not match:
                return value
            entries.append({"gate_id": match.group(1), "child_claim_id": match.group(2),
                            "proof_path": match.group(3)})
        return entries
    if key == "expected_children":
        children = []
        for item in value.split(";"):
            parts = [part.strip() for part in item.split("|")]
            if len(parts) != 7:
                return value
            children.append({"claim_id": parts[0], "worker": parts[1], "github_actor": parts[2],
                             "contract_id": parts[3], "branch": parts[4], "worktree": parts[5],
                             "scope_items": [part for part in parts[6].split("+") if part]})
        return children
    return value


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
            record[key] = _coerce(key, value.strip())
        if malformed:
            diagnostics.append(f"ignored malformed {record['schema']} block {block_order}")
            continue
        for source, destination in _ALIASES.get(record["schema"], {}).items():
            if source in record:
                record[destination] = record[source]
        if observation.get("url"):
            record["url"] = observation["url"]
        for trusted_key in ("issue", "pull_request"):
            if trusted_key in observation:
                record[trusted_key] = observation[trusted_key]
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
    def present(value: Any) -> bool:
        if isinstance(value, bool):
            return value
        return value not in (None, "", [], {})
    return all(present(record.get(name)) for name in names)


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
    poster = approval.get("author", approval.get("poster_actor"))
    direct = poster == approval["reviewer_actor"]
    delegated = (approval.get("delegation_valid") is True
                 and approval.get("delegation_reviewer_worker") == approval["reviewer_worker"]
                 and approval.get("delegation_substitute_actor") == poster
                 and approval.get("delegation_contract_id") == approval["contract_id"]
                 and approval.get("delegation_verdict") == approval["verdict"])
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


def _roles_for(author: str | None, authority: Mapping[str, Any]) -> set[str]:
    return set(authority.get("roles_by_actor", {}).get(author, ()))


def _matching_packet(event: Mapping[str, Any], evidence: Mapping[str, Mapping[str, Any]]) -> Mapping[str, Any] | None:
    packet = evidence.get(event.get("evidence_url"))
    if not packet or packet.get("schema") != "finch-workflow-evidence:v1" or not _immutable(packet):
        return None
    if (packet.get("body_digest") != event.get("evidence_digest")
            or packet.get("kind") != event.get("evidence_kind")
            or packet.get("issue") != event.get("issue")):
        return None
    return packet


def prepare_readiness_event(event: Mapping[str, Any], context: Mapping[str, Any]) -> dict[str, Any]:
    """Bind one parsed readiness event to authenticated identities and typed evidence."""
    bound = dict(event)
    roles = _roles_for(event.get("author"), context)
    rule = _TRANSITIONS.get((event.get("prior_state"), event.get("new_state")))
    permitted = set() if rule is None else rule[0]
    direct_roles = roles & permitted
    bound["authenticated_role"] = sorted(direct_roles)[0] if direct_roles else None
    bound["proxy_valid"] = bool(context.get("valid_proxies", {}).get(event.get("event_id")))
    packet = _matching_packet(event, context.get("evidence_by_url", {}))
    identity_fields = ("contract_id", "contract_digest", "contract_revision",
                       "implementation_base", "claim_id", "ledger_id", "round_id",
                       "exact_tip", "integration_base")
    bound["identity_valid"] = (
        event.get("issue") == context.get("issue")
        and event.get("owner") == context.get("owner")
        and packet is not None
        and all(context.get(field) is None or event.get(field) == context.get(field)
                for field in identity_fields)
    )
    if packet:
        for key, value in packet.items():
            if key not in bound:
                bound[key] = value

    contract = context.get("contracts_by_url", {}).get(event.get("contract_url"))
    approval = context.get("approvals_by_url", {}).get(event.get("approval_url"))
    contract_ok = bool(contract and approval and not validate_contract(contract, approval, context))
    contract_ok = contract_ok and all((
        event.get("contract_id") == contract.get("contract_id"),
        event.get("contract_digest") == contract.get("body_digest"),
        event.get("contract_revision") == contract.get("revision"),
        event.get("implementation_base") == contract.get("implementation_base"),
        event.get("plan_reviewer") == approval.get("reviewer_worker"),
    ))
    bound["approved_contract"] = contract_ok

    claim = context.get("claims_by_url", {}).get(event.get("claim_url"))
    bound["active_claim"] = bool(
        claim and claim.get("active") is True and claim.get("issue") == event.get("issue")
        and claim.get("claim_id") == event.get("claim_id")
    )
    terminal = context.get("terminals_by_url", {}).get(event.get("claim_url"))
    if terminal:
        bound["terminal_pair"] = {
            "disposition": terminal.get("disposition"), "claim_id": terminal.get("claim_id"),
            "authority_valid": terminal_event_valid(terminal),
            "evidence_matches": terminal.get("evidence_digest") == event.get("evidence_digest"),
        }
        bound["terminal_pair_status"] = terminal_pair_status(terminal, {
            "new_state": event.get("new_state"), "claim_id": event.get("claim_id"),
            "terminal_url": event.get("claim_url"), "evidence_digest": event.get("evidence_digest"),
            "issue": event.get("issue"), "created_at": event.get("created_at"),
            "comment_id": event.get("comment_id"), "block_order": event.get("block_order", 0),
        })

    review = context.get("review_reduction")
    if review and review.safe_to_merge:
        bound["zero_ledger"] = True
        bound["clean_review"] = True
    if event.get("new_state") == "COMPLETE":
        bound["completion_packet"] = packet if packet and completion_allowed(packet) else None
    if event.get("evidence_kind") == "legacy-bootstrap":
        bound["legacy_bootstrap"] = packet
    return bound


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
        if pair and event.get("terminal_pair_status") not in (None, "ACCEPTED"):
            errors.append("terminal/readiness pair is not structurally accepted")
    if destination == "COMPLETE" and not completion_allowed(event.get("completion_packet") or {}):
        errors.append("COMPLETE packet does not prove every current-main outcome axis")
    if destination == "READY_TO_MERGE" and not (
            event.get("zero_ledger") is True and event.get("clean_review") is True):
        errors.append("READY_TO_MERGE lacks a bound zero-ledger fresh clean review")
    return errors


def reduce_readiness(records: Sequence[Mapping[str, Any]], context: Mapping[str, Any] | None = None) -> Reduction:
    admitted: list[str] = []
    diagnostics: list[str] = []
    state: str | None = None
    accepted_successor: dict[str, str] = {}
    accepted_by_id: dict[str, Mapping[str, Any]] = {}
    pairing_required = False

    for raw_event in _ordered(records):
        event = prepare_readiness_event(raw_event, context) if context is not None else raw_event
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
    authority_ok = terminal.get("authority_valid") is True or terminal_event_valid(terminal)
    if expected is None or not authority_ok:
        return "INVALID"
    if readiness is None:
        return "PAIRING_REQUIRED"
    if (readiness.get("new_state") != expected
            or readiness.get("claim_id") != terminal.get("claim_id")
            or readiness.get("terminal_url") != terminal.get("url")
            or readiness.get("evidence_digest") != terminal.get("evidence_digest")
            or readiness.get("issue") != terminal.get("issue")
            or (terminal.get("created_at"), int(terminal.get("comment_id", 0)), int(terminal.get("block_order", 0)))
               >= (readiness.get("created_at"), int(readiness.get("comment_id", 0)), int(readiness.get("block_order", 0)))):
        return "INVALID"
    return "ACCEPTED"


def reduce_readiness_with_terminals(readiness: Sequence[Mapping[str, Any]],
                                    terminals: Sequence[Mapping[str, Any]],
                                    context: Mapping[str, Any]) -> Reduction:
    enriched = dict(context)
    enriched["terminals_by_url"] = {event.get("url"): event for event in terminals if event.get("url")}
    result = reduce_readiness(readiness, enriched)
    admitted = set(result.admitted_ids)
    paired_urls = {event.get("claim_url") for event in readiness
                   if event.get("event_id") in admitted}
    unmatched = [event for event in terminals
                 if terminal_event_valid(event) and event.get("url") not in paired_urls]
    if unmatched and not result.indeterminate:
        return Reduction(result.state, result.admitted_ids,
                         result.diagnostics + ("valid terminal awaits matching readiness event",),
                         pairing_required=True)
    return result


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
                    "scenario_evidence", "owner", "issue", "pull_request", "claim_id",
                    "contract_id")
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
            if not item.get("current_main_proof") or item.get("successor_finding_ids") or item.get("child_claim_ids"):
                return False
            gates = item.get("affected_gate_ids", ())
            if len(gates) != len(set(gates)) or any(gate in seen_gates for gate in gates):
                return False
            seen_gates.update(gates)
            return True
        if item.get("state") == "REPLACED-BY":
            successors = item.get("successor_finding_ids", ())
            if item.get("child_claim_ids"):
                return False
        elif item.get("state") == "SPLIT-TO":
            successors = item.get("child_claim_ids", ())
            if item.get("successor_finding_ids"):
                return False
        else:
            return False
        if not successors or len(successors) != len(set(successors)):
            return False
        visiting.add(node)
        for successor in successors:
            if item.get("state") == "SPLIT-TO":
                child = children.get(successor)
                if child is None or not child.get("ownership_valid"):
                    visiting.remove(node)
                    return False
                valid_terminal = (child.get("terminal_claim") is True
                                  and child.get("disposition") == "slice-complete"
                                  and child.get("merge_proof") is True)
                if not valid_terminal or child.get("current_main_proof") is not True:
                    visiting.remove(node)
                    return False
                gates = child.get("affected_gate_ids", ())
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
    original = findings.get(root, {}).get("affected_gate_ids", ())
    if len(original) != len(set(original)):
        return False
    expected = set(original)
    return ok and seen_gates == expected


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
    if kind == "handoff" and len(expected) != 1:
        return False
    scopes: set[str] = set(transaction.get("retained_scope_items", ()))
    if len(scopes) != len(transaction.get("retained_scope_items", ())):
        return False
    for child in actual:
        wanted = expected.get(child.get("claim_id"))
        if wanted != child or not child.get("nonauthorizing_reservation"):
            return False
        child_scope = child.get("scope_items", ())
        if not child_scope or len(child_scope) != len(set(child_scope)) or scopes & set(child_scope):
            return False
        scopes.update(child_scope)
    if scopes != set(transaction.get("parent_scope_items", ())):
        return False
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


def reduce_atomic_transfer(records: Sequence[Mapping[str, Any]], parent_claim: Mapping[str, Any],
                           kind: str, approved_child_contracts: set[str]) -> bool:
    reserve_name = f"{kind}-reserve"
    child_name = f"{kind}-child-reserve"
    activate_name = f"{kind}-activate"
    ordered = _ordered(records)
    reserves = [r for r in ordered if r.get("event") == reserve_name]
    activations = [r for r in ordered if r.get("event") == activate_name]
    if len(reserves) != 1 or len(activations) != 1:
        return False
    reserve, activation = reserves[0], activations[0]
    def order_key(item: Mapping[str, Any]) -> tuple[str, int, int]:
        return (item.get("created_at", ""), int(item.get("comment_id", 0)), int(item.get("block_order", 0)))
    if (reserve.get("author") != parent_claim.get("author")
            or activation.get("author") != parent_claim.get("author")
            or reserve.get("worker") != parent_claim.get("worker")
            or activation.get("worker") != parent_claim.get("worker")
            or reserve.get("claim_id") != parent_claim.get("claim_id")
            or activation.get("claim_id") != parent_claim.get("claim_id")
            or reserve.get("transfer_id") != activation.get("transfer_id")
            or reserve.get("contract_id") != parent_claim.get("contract_id")
            or activation.get("contract_id") != parent_claim.get("contract_id")
            or not order_key(reserve) < order_key(activation)
            or not _immutable(reserve) or not _immutable(activation)):
        return False
    expected = reserve.get("expected_children")
    if not isinstance(expected, list):
        return False
    if {child.get("contract_id") for child in expected} != approved_child_contracts:
        return False
    actual = []
    for child in [r for r in ordered if r.get("event") == child_name]:
        if (child.get("transfer_id") != reserve.get("transfer_id")
                or not _immutable(child) or child.get("nonauthorizing") is not True
                or not (order_key(reserve) < order_key(child) < order_key(activation))):
            return False
        actual.append({"claim_id": child.get("claim_id"), "worker": child.get("worker"),
                       "github_actor": child.get("author"), "contract_id": child.get("contract_id"), "branch": child.get("branch"),
                       "worktree": child.get("worktree"), "scope_items": child.get("scope_items"),
                       "nonauthorizing_reservation": True})
    normalized_expected = [dict(child, nonauthorizing_reservation=True) for child in expected]
    return atomic_transfer({
        "parent_authorized": True, "expected_children": normalized_expected,
        "child_reservations": actual, "activation": True,
        "retained_scope_items": reserve.get("retained_scope_items", ()),
        "parent_scope_items": parent_claim.get("scope_items", ()),
        "expected_gates": reserve.get("expected_gates", ()),
        "gate_map": reserve.get("gate_map", ()),
    }, kind)


def terminal_event_valid(event: Mapping[str, Any]) -> bool:
    allowed = {"return-ready", "blocked-external", "needs-specification", "infeasible",
               "declined", "slice-complete", "outcome-superseded", "legacy"}
    event_dispositions = {
        "release": {"return-ready", "blocked-external", "needs-specification", "infeasible", "declined"},
        "complete": {"slice-complete"}, "supersede": {"outcome-superseded"},
    }
    if event.get("disposition") not in allowed:
        return False
    if event.get("post_cutover") and event.get("disposition") not in event_dispositions.get(event.get("event"), set()):
        return False
    poster = event.get("author", event.get("poster_actor"))
    if event.get("worker") != event.get("original_worker"):
        return False
    direct = (poster == event.get("original_author")
              and event.get("worker") == event.get("original_worker"))
    delegated = (event.get("delegation_prior") is True
                 and event.get("delegation_claim_id") == event.get("claim_id")
                 and event.get("delegation_operation") == event.get("event")
                 and event.get("delegation_substitute_actor") == poster
                 and event.get("delegation_disposition") == event.get("disposition"))
    if not (direct or delegated) or not event.get("evidence_url"):
        return False
    if event.get("disposition") in {"infeasible", "declined"} and not event.get("owner_disposition"):
        return False
    if event.get("disposition") == "slice-complete" and not (
            event.get("reached_ready_to_merge") is True
            and event.get("merge_proof") is True):
        return False
    if event.get("disposition") == "outcome-superseded" and not (
            event.get("atomic_transfer") is True and event.get("replacement_packet") is True):
        return False
    return True


def prepare_terminal_event(event: Mapping[str, Any], original_claim: Mapping[str, Any],
                           packet: Mapping[str, Any] | None = None,
                           delegation: Mapping[str, Any] | None = None,
                           post_cutover: bool = True) -> dict[str, Any]:
    bound = dict(event)
    bound.update({"original_author": original_claim.get("author"),
                  "original_worker": original_claim.get("worker"),
                  "post_cutover": post_cutover})
    if (packet and _immutable(packet) and packet.get("body_digest") == event.get("evidence_digest")
            and packet.get("url") == event.get("evidence_url")
            and packet.get("issue") == event.get("issue")):
        for key, value in packet.items():
            if key not in bound:
                bound[key] = value
    if delegation:
        bound.update({
            "delegation_prior": delegation.get("created_at", "") < event.get("created_at", ""),
            "delegation_claim_id": delegation.get("claim_id"),
            "delegation_operation": delegation.get("operation"),
            "delegation_substitute_actor": delegation.get("substitute_actor"),
            "delegation_disposition": delegation.get("disposition"),
        })
    return bound


def legacy_bootstrap_valid(packet: Mapping[str, Any]) -> bool:
    if not isinstance(packet, Mapping):
        return False
    identities = (
        "cutover_identity", "cutover_created_at", "claim_created_at", "preserved_claim",
        "preserved_pr", "preserved_branch", "preserved_worktree", "preserved_tip",
        "preserved_base", "approved_contract",
    )
    return (all(packet.get(field) not in (None, False, "", [], {})
                and not isinstance(packet.get(field), bool) for field in identities)
            and packet.get("claim_predates_cutover") is True
            and packet.get("claim_created_at") < packet.get("cutover_created_at")
            and packet.get("collision_recheck") is True
            and packet.get("exact_tip_review") is True)


def scope_revision_allowed(event: Mapping[str, Any]) -> bool:
    poster = event.get("author", event.get("poster_actor"))
    if event.get("worker") != event.get("original_worker"):
        return False
    direct = poster == event.get("original_author")
    delegated = (event.get("delegation_prior") is True
                 and event.get("delegation_operation") == "scope-revise"
                 and event.get("delegation_claim_id") == event.get("claim_id")
                 and event.get("delegation_substitute_actor") == poster
                 and event.get("delegation_scope") == event.get("new_scope")
                 and event.get("delegation_contract_revision") == event.get("contract_revision"))
    return bool((direct or delegated) and event.get("prior_scope_matches")
                and event.get("contract_approved") and event.get("collision_check_complete"))


def prepare_scope_revision(event: Mapping[str, Any], original_claim: Mapping[str, Any],
                           contract: Mapping[str, Any], approval: Mapping[str, Any],
                           authority: Mapping[str, Any], delegation: Mapping[str, Any] | None,
                           collision_check_complete: bool) -> dict[str, Any]:
    bound = dict(event)
    bound.update({
        "original_author": original_claim.get("author"),
        "original_worker": original_claim.get("worker"),
        "prior_scope_matches": event.get("prior_scope") == original_claim.get("scope"),
        "contract_approved": (not validate_contract(contract, approval, authority)
                              and event.get("contract_id") == contract.get("contract_id")
                              and event.get("contract_url") == contract.get("url")
                              and event.get("contract_revision") == contract.get("revision")),
        "collision_check_complete": collision_check_complete,
    })
    if delegation:
        bound.update({
            "delegation_prior": delegation.get("created_at", "") < event.get("created_at", ""),
            "delegation_operation": delegation.get("operation"),
            "delegation_claim_id": delegation.get("claim_id"),
            "delegation_substitute_actor": delegation.get("substitute_actor"),
            "delegation_scope": delegation.get("new_scope"),
            "delegation_contract_revision": delegation.get("contract_revision"),
        })
    return bound


def validate_review_round(record: Mapping[str, Any]) -> list[str]:
    fields = (
        "event_id", "round_id", "ledger_id", "issue", "pull_request", "claim_id",
        "contract_id", "contract_url", "contract_digest", "claim_base",
        "integration_base", "exact_tip", "round_number", "selected_perspectives",
        "skipped_perspectives", "status", "gate_evidence",
    )
    errors: list[str] = []
    if not _immutable(record) or not _has(record, *fields):
        errors.append("review round is malformed, incomplete, or mutable")
    if "finding_event_ids" not in record:
        errors.append("review round omits its exact finding-event set")
    if record.get("status") not in {"DISCOVERY", "VERIFICATION", "REPAIR_IN_PROGRESS", "CONVERGED"}:
        errors.append("review round has unknown status")
    if record.get("verdict") not in {None, "SAFE_TO_MERGE", "ESCALATE_WITH_EXECUTABLE_REPAIR_OR_SPLIT"}:
        errors.append("review round has unknown final verdict")
    status_verdicts = {
        "DISCOVERY": {None}, "VERIFICATION": {None},
        "REPAIR_IN_PROGRESS": {None, "ESCALATE_WITH_EXECUTABLE_REPAIR_OR_SPLIT"},
        "CONVERGED": {"SAFE_TO_MERGE"},
    }
    if record.get("verdict") not in status_verdicts.get(record.get("status"), set()):
        errors.append("review status and verdict are contradictory")
    if record.get("panel_diff_base") != record.get("integration_base"):
        errors.append("review panel was not derived from the integration base")
    if record.get("status") == "CONVERGED" and not (
            record.get("fresh_clean_pass") is True and record.get("zero_ledger") is True
            and record.get("prior_zero_ledger_round_id")):
        errors.append("CONVERGED round lacks zero-ledger fresh-clean-pass proof")
    return errors


def reduce_review_history(rounds: Sequence[Mapping[str, Any]], findings: Sequence[Mapping[str, Any]],
                          expected: Mapping[str, Any]) -> ReviewReduction:
    diagnostics: list[str] = []
    finding_result = reduce_findings(findings)
    if finding_result.indeterminate:
        return ReviewReduction(False, finding_result.diagnostics)
    diagnostics.extend(finding_result.diagnostics)
    ordered_rounds = _ordered(rounds)
    if not ordered_rounds:
        return ReviewReduction(False, ("no review round",))
    for round_record in ordered_rounds:
        diagnostics.extend(validate_review_round(round_record))
        for key in ("issue", "pull_request", "claim_id", "contract_id", "ledger_id", "exact_tip"):
            if round_record.get(key) != expected.get(key):
                diagnostics.append(f"review round has foreign {key}")
    latest = ordered_rounds[-1]
    rounds_by_id = {record.get("round_id"): record for record in ordered_rounds}
    prior_zero = rounds_by_id.get(latest.get("prior_zero_ledger_round_id"))
    prior_zero_valid = bool(prior_zero and prior_zero is not latest and prior_zero.get("zero_ledger") is True
                            and all(prior_zero.get(key) == latest.get(key) for key in
                                    ("issue", "pull_request", "claim_id", "contract_id", "ledger_id",
                                     "integration_base", "exact_tip")))
    blocking = []
    latest_event_ids = set(latest.get("finding_event_ids", ()))
    for finding in finding_result.current.values():
        if any(finding.get(key) != expected.get(key) for key in
               ("issue", "pull_request", "claim_id", "contract_id", "ledger_id", "exact_tip")):
            diagnostics.append(f"foreign finding {finding.get('finding_id')} entered review ledger")
            continue
        if finding_blocks(finding):
            blocking.append(finding["finding_id"])
        if finding.get("event_id") not in latest_event_ids:
            diagnostics.append(f"latest round omits finding event {finding.get('event_id')}")
    if latest_event_ids != {finding.get("event_id") for finding in finding_result.current.values()}:
        diagnostics.append("round finding-event set does not equal the reduced ledger")
    safe = (not diagnostics and not blocking and latest.get("status") == "CONVERGED"
            and latest.get("verdict") == "SAFE_TO_MERGE"
            and latest.get("fresh_clean_pass") is True
            and latest.get("zero_ledger") is True and prior_zero_valid)
    if latest.get("status") == "CONVERGED" and not prior_zero_valid:
        diagnostics.append("fresh clean pass does not follow a bound zero-ledger round")
    return ReviewReduction(safe, tuple(diagnostics + ([f"open blockers: {','.join(blocking)}"] if blocking else [])))


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
    return isinstance(packet, Mapping) and all(packet.get(field) is True for field in _COMPLETE_FIELDS)


def scoped_slice_completion_allowed(packet: Mapping[str, Any]) -> bool:
    return all(packet.get(field) is True for field in (
        "reached_ready_to_merge", "merged_slice_proof", "parent_repair_event",
        "active_successor_claims", "claim_terminal_after_parent_event", "issue_remains_open",
    ))


def review_diff_base(round_record: Mapping[str, Any]) -> str | None:
    """Panel derivation always uses current integration base, never claim base."""
    return round_record.get("integration_base")
