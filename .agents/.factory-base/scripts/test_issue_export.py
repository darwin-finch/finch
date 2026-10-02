#!/usr/bin/env python3
"""Pure-function checks for issue_export.parse_body. Does not call gh."""

from __future__ import annotations

from issue_export import from_gh_issues, parse_body


def test_parse_queue_fields() -> None:
    body = "owner: @a\n\n## Queue\nvalue: 4\ncost: 2\ncertainty: 5\nunblocking: 1\nfiles: install.sh other.md\n"
    row = parse_body(body)
    assert row["value"] == 4
    assert row["cost"] == 2
    assert row["certainty"] == 5
    assert row["unblocking"] == 1
    assert row["files"] == ["install.sh", "other.md"]


def test_parse_ignores_inline_digits() -> None:
    row = parse_body("the value: 9 is a joke\ncost is high\n")
    assert "value" not in row
    assert "cost" not in row
    assert row["files"] == []


def test_from_gh_issues() -> None:
    issues = from_gh_issues(
        [{"number": 1, "title": "t", "body": "value: 3\ncost: 1\ncertainty: 5\nunblocking: 2\n"}]
    )
    assert issues[0]["number"] == 1
    assert issues[0]["value"] == 3
    assert issues[0]["unblocking"] == 2


def main() -> int:
    test_parse_queue_fields()
    test_parse_ignores_inline_digits()
    test_from_gh_issues()
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
