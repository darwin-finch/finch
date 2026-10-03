import subprocess
import json

issues = {
    1529: {
        "outcome": "The Validation Error overlay renders correctly with straight right borders and uniform padding.",
        "criteria": "- The right border of the Validation Error overlay renders as a straight vertical line.\n- The padding of lines within the overlay text block is uniform.",
        "files": "src/cli/setup_wizard/render.rs",
        "tier": "UI bug fix",
        "lane": "cheap",
        "proof": "Visual verification in Setup Wizard rendering test.",
        "value": 2, "cost": 1, "certainty": 5, "unblocking": 1,
        "justification": "Minor UI polish, improves perceived quality"
    },
    1530: {
        "outcome": "The Welcome Banner focuses on general assistance without assuming the user has software projects.",
        "criteria": "- The Welcome Banner explains Finch's purpose without using terms like 'software projects', focusing on general capabilities.",
        "files": "src/cli/setup_wizard/render.rs",
        "tier": "UX copy update",
        "lane": "cheap",
        "proof": "Welcome Banner copy updated in tests.",
        "value": 3, "cost": 1, "certainty": 5, "unblocking": 1,
        "justification": "Target audience alignment is important for adoption."
    },
    1531: {
        "outcome": "Helper text accurately describes how to add an API key and matches actual keyboard interactions.",
        "criteria": "- Helper text accurately describes how to add an API key.\n- Instructions match actual keyboard interactions available.",
        "files": "src/cli/setup_wizard/render.rs",
        "tier": "UX copy update",
        "lane": "cheap",
        "proof": "Helper text copy updated in tests.",
        "value": 3, "cost": 1, "certainty": 5, "unblocking": 1,
        "justification": "Usability fix to prevent user confusion."
    },
    1532: {
        "outcome": "The model helper text uses plain English and avoids exposing internal terminology.",
        "criteria": "- The model helper text uses plain English.\n- No exposure of internal terminology like 'bundled fallback snapshot'.",
        "files": "src/cli/setup_wizard/render.rs, src/cli/setup_wizard/tests.rs",
        "tier": "UX copy update",
        "lane": "cheap",
        "proof": "Helper text copy updated in tests.",
        "value": 3, "cost": 1, "certainty": 5, "unblocking": 1,
        "justification": "Prevents confusing nontechnical users with jargon."
    },
    1533: {
        "outcome": "The Local Helpers feature description avoids exposing AI architecture or file formats.",
        "criteria": "- The feature description avoids terms like 'embeddings', 'neural model', 'GGUF', and 'llama.cpp'.",
        "files": "src/cli/setup_wizard/render.rs, src/cli/setup_wizard/state.rs",
        "tier": "UX copy update",
        "lane": "cheap",
        "proof": "Feature description updated in tests.",
        "value": 3, "cost": 1, "certainty": 5, "unblocking": 1,
        "justification": "Aligns with Evelyn's mental model by removing extreme technical jargon."
    },
    1534: {
        "outcome": "Advanced technical settings are either hidden behind a toggle or rewritten in plain English.",
        "criteria": "- Advanced technical settings are either hidden or rewritten in plain English.",
        "files": "src/cli/setup_wizard/render.rs, src/cli/setup_wizard/state.rs, src/cli/setup_wizard/apply.rs",
        "tier": "UX feature update",
        "lane": "default",
        "proof": "Settings screen renders without intimidating jargon.",
        "value": 3, "cost": 2, "certainty": 4, "unblocking": 1,
        "justification": "Improves settings screen usability for nontechnical users."
    }
}

for issue_id, data in issues.items():
    res = subprocess.run(["gh", "issue", "view", str(issue_id), "--json", "body"], capture_output=True, text=True)
    body = json.loads(res.stdout)["body"]
    
    # Strip previous solution contract
    idx = body.find("## Solution Contract")
    if idx != -1:
        body = body[:idx].strip()
    
    contract = f"""

## Solution Contract
**Accepted outcome:** {data["outcome"]}
**Acceptance criteria:**
{data["criteria"]}

**Owner:** auto
**Base revision:** HEAD

**Risk tier:** {data["tier"]}
**Model lane:** {data["lane"]}
**Required proof:** {data["proof"]}

**Blocked-by dependencies:** None
**Justification:** {data["justification"]}

value: {data["value"]}
cost: {data["cost"]}
certainty: {data["certainty"]}
unblocking: {data["unblocking"]}
files: {data["files"]}
owner: auto
"""
    new_body = body + contract
    
    # Save to temp file and update
    with open("temp_body.md", "w") as f:
        f.write(new_body)
    
    subprocess.run(["gh", "issue", "edit", str(issue_id), "--body-file", "temp_body.md"])
    print(f"Updated issue {issue_id}")

