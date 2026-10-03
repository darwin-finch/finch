## Summary
The "Settings" step contains heavy developer jargon ("~/.finch/debug.log", "HuggingFace", "Daemon-only mode", "REPL") that violates constraints.

## Reproduction
1. Proceed to the "Settings" step of the Setup Wizard.
2. Read the options list.

## Expected behavior
Advanced options should either be hidden behind an "Advanced Settings" toggle or explained in plain English.

## Actual behavior
Options explicitly reference Unix paths, daemon modes, and REPLs.

## Impact
Intimidating wall of text for nontechnical users.

## Acceptance criteria
- Advanced technical settings are either hidden or rewritten in plain English.

## Solution Contract
**Accepted outcome:** Advanced technical settings are either hidden behind a toggle or rewritten in plain English.
**Acceptance criteria:**
- Advanced technical settings are either hidden or rewritten in plain English.

**Owner:** auto
**Base revision:** HEAD

**Risk tier:** UX feature update
**Model lane:** default
**Required proof:** Settings screen renders without intimidating jargon.

**Blocked-by dependencies:** None
**Justification:** Improves settings screen usability for nontechnical users.

value: 3
cost: 2
certainty: 4
unblocking: 1
files: src/cli/setup_wizard/render.rs, src/cli/setup_wizard/state.rs, src/cli/setup_wizard/apply.rs
owner: auto
