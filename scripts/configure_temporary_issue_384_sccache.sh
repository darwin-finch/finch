#!/usr/bin/env bash
# TEMPORARY_FINCH_ISSUE_384_SCCACHE_AUTHORITY_BEGIN
# Delete this helper with the temporary issue 384 evidence workflow.

set -euo pipefail

expected_claim=5f155aeb-2a15-4837-8590-833b4a4daa06
if [[ "${FINCH_ISSUE_384_EVIDENCE:-}" != "${expected_claim}" || \
      "${GITHUB_REPOSITORY:-}" != "darwin-finch/finch" || \
      "${GITHUB_EVENT_NAME:-}" != "pull_request" || \
      "${GITHUB_BASE_REF:-}" != "main" || \
      "${GITHUB_HEAD_REF:-}" != "codex/issue-384-ci-cache-v3" || \
      "${GITHUB_ACTOR:-}" != "schancel" || \
      "${GITHUB_TRIGGERING_ACTOR:-}" != "schancel" ]]; then
  echo "temporary issue 384 compiler-cache authority check failed" >&2
  exit 1
fi

# The production helper itself remains main-only. Override its two internal
# process inputs only after this temporary helper has validated the real event.
GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/main \
  "${GITHUB_WORKSPACE}/scripts/configure_ci_sccache.sh"

# TEMPORARY_FINCH_ISSUE_384_SCCACHE_AUTHORITY_END
