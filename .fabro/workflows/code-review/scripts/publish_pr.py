#!/usr/bin/env python3
"""Deterministic PR publisher for the Fabro code-review workflow.

Posts a completed review's findings to the reviewed GitHub PR in two steps:

- ``plan`` is pure: canonical bundle + git diff arithmetic + routing
  configuration -> a publication plan (JSON). No network, no credentials.
  Every placement decision, comment body, batch, and summary body is in the
  plan and is byte-deterministic.
- ``apply`` executes a plan against the GitHub API and writes an outcome
  file. All environmental nondeterminism (HTTP failures, retries, existing
  PR state) is confined here. The plan file is untrusted input: apply
  re-validates it before any write.

A third subcommand, ``history``, is the token-holding sibling of apply
for incremental re-review (P3): it snapshots the PR's prior-review state
(our earlier comments and the sticky summary's identity tags) into a
validated ``pr-history.json`` that ``plan`` consumes as a file.

The canonical ``lithoscomputer/code-review`` source repository keeps the
requirements registers at ``.ai/plans/p1-pr-publisher-requirements.md``
(R-numbers) and ``.ai/plans/p3-incremental-re-review-requirements.md``
(P-numbers), with the executable specifications at
``tests/test_pr_publisher.py`` and ``tests/test_incremental_rereview.py``.
Packaged workflow installs do not need to copy those development files.

Python 3.9-compatible. Standard library only.
"""

from __future__ import annotations

import argparse
import base64
import functools
import hashlib
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Mapping, NoReturn, Optional, Sequence, Set, Tuple

sys.path.insert(0, str(Path(__file__).resolve().parent))

import render_report as renderer  # noqa: E402  (the bundle validators)
from review_contract import FINDING_ID_RE  # noqa: E402


PLAN_VERSION = 2
HISTORY_VERSION = 1
SUMMARY_MARKER = "<!-- fabro-code-review-summary -->"
COMMENT_TAG_PREFIX = "fabro-code-review-comment"
RUN_TAG_PREFIX = "fabro-code-review-run"
COMPLETED_TAG_PREFIX = "fabro-code-review-completed"
HEAD_TAG_PREFIX = "fabro-code-review-head"
BASE_TAG_PREFIX = "fabro-code-review-base"
META_TAG_PREFIX = "fabro-code-review-meta"
DEFAULT_BATCH_SIZE = 50
# OCR parity: the incremental overlap threshold defaults to 0.6 and must be
# strictly inside (0, 1) -- the predicate is IoU > threshold and an identical
# span has IoU exactly 1.0, so a threshold of 1.0 could never match anything.
DEFAULT_OVERLAP_THRESHOLD = 0.6
# Skipped-placement reasons (the plan's third placement kind, P3 item 3/5).
SKIP_REASONS = ("overlap", "duplicate-of-posted")
# Partial-span mapping policy (P3 item 4, decided Q6): a history comment
# whose span was partly rewritten by a later push is treated as covering the
# rewritten site's replacement lines and still suppresses -- the aggressive
# choice, recorded here as an explicit constant.
PARTIAL_MAPPED_SPANS_SUPPRESS = True
# Only canonical GitHub comment URLs are carried into plan details.
GITHUB_URL_PREFIX = "https://github.com/"
# The encoded meta-tag payload is bounded; the short summary is truncated
# deterministically until the encoding fits.
META_TAG_PAYLOAD_CAP = 1000
# GitHub caps a comment body at 65,536 characters; the summary is assembled
# under this budget so a write can never fail on size (R19).
SUMMARY_BUDGET = 65000
GITHUB_BODY_CAP = 65536
SEVERITY_RANK = {"LOW": 0, "MEDIUM": 1, "HIGH": 2}
SEVERITY_EMOJI = {"LOW": "🟡", "MEDIUM": "🟠", "HIGH": "🔴"}
CANONICAL_FILE_NAMES = (
    "review-manifest.json",
    "candidate-ledger.jsonl",
    "findings.json",
    "coverage.json",
    "votes.jsonl",
)

REVIEW_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$")
REPO_RE = re.compile(
    r"^[A-Za-z0-9][A-Za-z0-9._-]{0,99}/[A-Za-z0-9][A-Za-z0-9._-]{0,99}$"
)
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
HUNK_HEADER_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")
HUNK_PAIR_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
COMMENT_TAG_RE = re.compile(
    r"^<!-- "
    + re.escape(COMMENT_TAG_PREFIX)
    + r":([A-Za-z0-9][A-Za-z0-9_.:-]{0,127}):(R[0-9]+) -->"
)
META_TAG_RE = re.compile(
    r"<!-- " + re.escape(META_TAG_PREFIX) + r":([A-Za-z0-9_=-]{1,1000}) -->"
)
PLAUSIBLE_WARNING = (
    "> **Needs confirmation:** The verifier could not fully confirm this "
    "finding from the available evidence."
)


class PublishError(RuntimeError):
    """A refused plan or apply request."""


def fail(message: str) -> NoReturn:
    raise PublishError(message)


def comment_tag(review_id: str, finding_id: str) -> str:
    return f"{COMMENT_TAG_PREFIX}:{review_id}:{finding_id}"


def run_tag_for(review_id: str) -> str:
    return f"<!-- {RUN_TAG_PREFIX}:{review_id} -->"


def completed_tag_for(completed_at: str) -> str:
    return f"<!-- {COMPLETED_TAG_PREFIX}:{completed_at} -->"


def head_tag_for(head: str) -> str:
    return f"<!-- {HEAD_TAG_PREFIX}:{head} -->"


def base_tag_for(base: str) -> str:
    return f"<!-- {BASE_TAG_PREFIX}:{base} -->"


def tagged_sha(body: str, prefix: str) -> Optional[str]:
    match = re.search(
        r"<!-- " + re.escape(prefix) + r":([0-9a-f]{40}) -->", body
    )
    return match.group(1) if match else None


def meta_tag_for(finding: Mapping[str, Any]) -> str:
    """The hidden metadata tag an inline comment carries (P3 item 5).

    Base64url keeps the payload safe inside an HTML comment: its alphabet
    cannot produce the ``-->`` terminator, so a short summary containing
    it (or any markup) cannot break out of the tag. The short summary is
    truncated deterministically until the encoding fits the cap.
    """
    short_summary = str(finding["short_summary"])
    while True:
        payload = json.dumps(
            {
                "category": finding["category"],
                "severity": finding["severity"],
                "short_summary": short_summary,
            },
            ensure_ascii=True,
            sort_keys=True,
            separators=(",", ":"),
        )
        encoded = base64.urlsafe_b64encode(payload.encode("utf-8")).decode(
            "ascii"
        )
        if len(encoded) <= META_TAG_PAYLOAD_CAP or not short_summary:
            return f"<!-- {META_TAG_PREFIX}:{encoded} -->"
        short_summary = short_summary[:-10]


def parse_meta_tag(body: str) -> Optional[Dict[str, str]]:
    match = META_TAG_RE.search(body)
    if not match:
        return None
    try:
        payload = json.loads(
            base64.urlsafe_b64decode(match.group(1).encode("ascii"))
        )
    except (ValueError, UnicodeError):
        return None
    if not isinstance(payload, dict):
        return None
    category = payload.get("category")
    severity = payload.get("severity")
    short_summary = payload.get("short_summary")
    if (
        category not in renderer.CATEGORIES
        or severity not in SEVERITY_RANK
        or not isinstance(short_summary, str)
    ):
        return None
    return {
        "category": category,
        "severity": severity,
        "short_summary": short_summary,
    }


def parse_comment_identity(body: str) -> Optional[Tuple[str, str]]:
    """(review_id, finding_id) from a body's leading identity tag."""
    match = COMMENT_TAG_RE.match(body)
    return (match.group(1), match.group(2)) if match else None


# --- Git arithmetic (plan) ---------------------------------------------------


def run_git(*arguments: str) -> subprocess.CompletedProcess:
    environment = os.environ.copy()
    environment.update(
        {
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_PAGER": "cat",
            "PAGER": "cat",
        }
    )
    try:
        return subprocess.run(
            ["git", "-c", "core.quotePath=false", *arguments],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
            check=False,
        )
    except OSError as error:
        fail(f"could not run Git: {error}")


def resolve_commit(token: str, field: str) -> str:
    result = run_git("rev-parse", "--verify", "--quiet", token + "^{commit}")
    resolved = result.stdout.decode("utf-8", "replace").strip()
    if result.returncode != 0 or not SHA_RE.fullmatch(resolved):
        fail(
            f"{field} {token!r} does not resolve to a commit in this "
            "repository; plan must run inside the reviewed checkout"
        )
    return resolved


def resolve_diff_base(token: str) -> str:
    """The diff base as a commit, or a bare tree for a root commit.

    A root commit's reviewed range starts at the empty tree, which is
    tree-ish but not a commit; ``git diff`` accepts it as a base.
    """
    result = run_git("rev-parse", "--verify", "--quiet", token + "^{commit}")
    resolved = result.stdout.decode("utf-8", "replace").strip()
    if result.returncode == 0 and SHA_RE.fullmatch(resolved):
        return resolved
    result = run_git("rev-parse", "--verify", "--quiet", token + "^{tree}")
    resolved = result.stdout.decode("utf-8", "replace").strip()
    if result.returncode == 0 and SHA_RE.fullmatch(resolved):
        return resolved
    fail(
        f"range base {token!r} does not resolve to a commit or tree in "
        "this repository; plan must run inside the reviewed checkout"
    )


def right_side_hunks(base: str, head: str) -> Dict[str, List[Tuple[int, int]]]:
    """RIGHT-side hunk line ranges of ``git diff -U3 base head`` (R2).

    Hunks include context lines; a pure-deletion hunk has no RIGHT-side
    lines and is skipped. A ``+++ `` target line counts as a file header
    only inside a file's preamble (between its ``diff --git`` boundary
    and its first hunk): an added body line whose content starts with
    ``++ `` renders as ``+++ `` but cannot reach the preamble, because
    every hunk body line carries a +/-/space marker while a real file
    boundary starts bare.
    """
    result = run_git(
        "diff", "--no-color", "--no-ext-diff", "--no-textconv",
        "--find-renames", "-U3",
        base, head,
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        fail(f"git diff over the reviewed range failed: {detail}")
    ranges: Dict[str, List[Tuple[int, int]]] = {}
    current: Optional[str] = None
    in_preamble = False
    for line in result.stdout.decode("utf-8", "replace").splitlines():
        if line.startswith("diff --git "):
            in_preamble = True
            current = None
        elif in_preamble and line.startswith("+++ "):
            target = line[4:]
            if target == "/dev/null" or target.startswith('"'):
                current = None
            elif target.startswith("b/"):
                current = target[2:]
            else:
                current = target
        elif line.startswith("@@ "):
            in_preamble = False
            if current is None:
                continue
            match = HUNK_HEADER_RE.match(line)
            if not match:
                continue
            start = int(match.group(1))
            count = 1 if match.group(2) is None else int(match.group(2))
            if count > 0:
                ranges.setdefault(current, []).append(
                    (start, start + count - 1)
                )
    return ranges


def has_diff_range(
    hunks: Mapping[str, Sequence[Tuple[int, int]]],
    path: str,
    start_line: int,
    end_line: int,
) -> bool:
    """True when one RIGHT-side hunk contains the complete range."""
    return any(
        hunk_start <= start_line <= end_line <= hunk_end
        for hunk_start, hunk_end in hunks.get(path, ())
    )


# --- Span mapping between commits (P3 item 4) --------------------------------
#
# Pure git arithmetic shared by `history` (mapping an outdated comment's
# original span onto the live head) and `apply` (forward-mapping a plan's
# spans onto a head that moved after the review started).


@functools.lru_cache(maxsize=64)
def pair_diff_map(
    old_rev: str, new_rev: str
) -> Mapping[str, Tuple[Optional[str], Tuple[Tuple[int, int, int, int], ...]]]:
    """Per old path: (new path, or None when deleted; -U0 hunk quadruples).

    Hunks are (old_start, old_count, new_start, new_count). A file absent
    from the mapping is unchanged between the revisions. Renames are
    followed. The same preamble discipline as ``right_side_hunks`` keeps a
    body line that renders as ``--- ``/``+++ `` from being read as a file
    boundary.
    """
    result = run_git(
        "diff", "--no-color", "--no-ext-diff", "--no-textconv",
        "--find-renames", "-U0",
        old_rev, new_rev,
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        fail(f"git diff for span mapping failed: {detail}")
    mapping: Dict[str, Tuple[Optional[str], Tuple[Tuple[int, int, int, int], ...]]] = {}
    old_path: Optional[str] = None
    new_path: Optional[str] = None
    hunks: List[Tuple[int, int, int, int]] = []
    in_preamble = False

    def flush() -> None:
        if old_path is not None:
            mapping[old_path] = (new_path, tuple(hunks))

    for line in result.stdout.decode("utf-8", "replace").splitlines():
        if line.startswith("diff --git "):
            flush()
            old_path = None
            new_path = None
            hunks = []
            in_preamble = True
        elif in_preamble and line.startswith("rename from "):
            # A pure rename has no ---/+++ lines at all; the rename
            # preamble is the only record of the path pair.
            token = line[len("rename from "):]
            old_path = None if token.startswith('"') else token
        elif in_preamble and line.startswith("rename to "):
            token = line[len("rename to "):]
            if token.startswith('"'):
                old_path = None
            else:
                new_path = token
        elif in_preamble and line.startswith("--- "):
            token = line[4:]
            if token == "/dev/null" or token.startswith('"'):
                old_path = None
            elif token.startswith("a/"):
                old_path = token[2:]
            else:
                old_path = token
        elif in_preamble and line.startswith("+++ "):
            token = line[4:]
            if token == "/dev/null":
                new_path = None
            elif token.startswith('"'):
                old_path = None
            elif token.startswith("b/"):
                new_path = token[2:]
            else:
                new_path = token
        elif line.startswith("@@ "):
            in_preamble = False
            match = HUNK_PAIR_RE.match(line)
            if not match:
                continue
            old_start = int(match.group(1))
            old_count = 1 if match.group(2) is None else int(match.group(2))
            new_start = int(match.group(3))
            new_count = 1 if match.group(4) is None else int(match.group(4))
            hunks.append((old_start, old_count, new_start, new_count))
    flush()
    return mapping


def map_line(
    hunks: Sequence[Tuple[int, int, int, int]], line: int
) -> Optional[int]:
    """The line's image in the new revision, or None inside a removal."""
    delta = 0
    for old_start, old_count, _new_start, new_count in hunks:
        # A pure insertion (-a,0) sits after old line a; line a itself is
        # untouched by it.
        boundary = old_start if old_count > 0 else old_start + 1
        if line < boundary:
            return line + delta
        if old_count > 0 and line <= old_start + old_count - 1:
            return None
        delta += new_count - old_count
    return line + delta


def map_span(
    old_rev: str, new_rev: str, path: str, start_line: int, end_line: int
) -> Optional[Dict[str, Any]]:
    """The span's image at new_rev: path, lines, and a ``partial`` flag.

    Both endpoints mapping cleanly is a full mapping. A span partly inside
    a rewritten region maps onto the rewrite's replacement lines and is
    flagged ``partial`` (policy: PARTIAL_MAPPED_SPANS_SUPPRESS). A deleted
    file or a fully deleted span has no image and returns None.
    """
    entry = pair_diff_map(old_rev, new_rev).get(path)
    if entry is None:
        return {
            "path": path,
            "start_line": start_line,
            "end_line": end_line,
            "partial": False,
        }
    new_path, hunks = entry
    if new_path is None:
        return None
    points: List[int] = []
    partial = False
    for line in (start_line, end_line):
        image = map_line(hunks, line)
        if image is None:
            partial = True
        else:
            points.append(image)
    # Any removal intersecting the span means span lines changed; the
    # replacement region is part of the image (the same site, edited).
    for old_start, old_count, new_start, new_count in hunks:
        if old_count < 1:
            continue
        if old_start > end_line or old_start + old_count - 1 < start_line:
            continue
        partial = True
        if new_count > 0:
            points.extend((new_start, new_start + new_count - 1))
    if not points:
        return None
    return {
        "path": new_path,
        "start_line": min(points),
        "end_line": max(points),
        "partial": partial,
    }


# --- Overlap predicate (P3 item 3, OCR parity) --------------------------------


def span_overlap_iou(
    new_start: int,
    new_end: int,
    old_start: int,
    old_end: int,
    threshold: float,
) -> Optional[float]:
    """The IoU when the spans match under OCR's predicate, else None.

    Single-line vs single-line matches on the same line (IoU 1.0);
    multi-line vs multi-line matches when overlap/union is strictly above
    the threshold; single vs multi never matches, in either direction.
    """
    new_single = new_start == new_end
    old_single = old_start == old_end
    if new_single != old_single:
        return None
    if new_single:
        return 1.0 if new_start == old_start else None
    overlap = min(new_end, old_end) - max(new_start, old_start) + 1
    if overlap <= 0:
        return None
    union = max(new_end, old_end) - min(new_start, old_start) + 1
    iou = overlap / union
    return iou if iou > threshold else None


# --- Routing configuration (R3-R5, fail-closed) ------------------------------


def parse_severity_threshold(raw: str) -> Optional[str]:
    text = (raw or "").strip().lower()
    if not text:
        return None
    if text.upper() not in SEVERITY_RANK:
        fail(
            f"route-severity-below must be one of high, medium, low "
            f"(or empty to disable); got {raw!r}"
        )
    return text.upper()


def parse_route_categories(raw: str) -> List[str]:
    text = (raw or "").strip()
    if not text:
        return []
    tokens: List[str] = []
    for piece in text.split(","):
        token = piece.strip().lower()
        if not token:
            continue
        if token not in renderer.CATEGORIES:
            fail(
                f"route-categories names an unknown category {token!r}; "
                f"known: {', '.join(renderer.CATEGORIES)}"
            )
        if token not in tokens:
            tokens.append(token)
    return tokens


def parse_batch_size(raw: str) -> int:
    try:
        value = int(str(raw).strip())
    except (TypeError, ValueError):
        return DEFAULT_BATCH_SIZE
    return value if value >= 1 else DEFAULT_BATCH_SIZE


def parse_pr_number(raw: str) -> int:
    text = str(raw).strip()
    if not text.isdigit() or int(text) < 1:
        fail(f"pr must be a positive integer, got {raw!r}")
    return int(text)


def parse_incremental_flag(raw: str) -> bool:
    """Fail-closed boolean for the incremental input."""
    text = str(raw or "").strip().lower()
    if text in ("", "false", "0", "no", "off"):
        return False
    if text in ("true", "1", "yes", "on"):
        return True
    fail(f"incremental must be true or false (or empty), got {raw!r}")


def parse_overlap_threshold(raw: str) -> float:
    """The overlap threshold, strictly inside (0, 1) (fail-closed).

    The predicate is strict ``IoU > threshold`` and an identical span has
    IoU exactly 1.0, so a threshold of 1.0 could never match anything;
    dead configuration is refused rather than accepted (unlike OCR, which
    documents (0, 1] and silently inherits the quirk).
    """
    text = str(raw or "").strip()
    if not text:
        return DEFAULT_OVERLAP_THRESHOLD
    try:
        value = float(text)
    except ValueError:
        fail(
            "incremental-overlap-threshold must be a number strictly "
            f"between 0 and 1, got {raw!r}"
        )
    if not (0.0 < value < 1.0):
        fail(
            "incremental-overlap-threshold must be strictly between 0 and "
            f"1 (exclusive: 1.0 could never match anything), got {raw!r}"
        )
    return value


def routing_detail(
    finding: Mapping[str, Any],
    threshold: Optional[str],
    categories: Sequence[str],
) -> Optional[str]:
    """The reason a finding routes to the summary, or None (R3, R4)."""
    reasons: List[str] = []
    if threshold is not None and (
        SEVERITY_RANK[finding["severity"]] <= SEVERITY_RANK[threshold]
    ):
        reasons.append(
            f"severity {finding['severity']} is at or below the "
            f"{threshold.lower()} threshold"
        )
    if finding["category"] in categories:
        reasons.append(f"category {finding['category']} is routed by policy")
    return "; ".join(reasons) if reasons else None


# --- Comment and summary rendering (R9, R11) ---------------------------------


def finding_detail_lines(finding: Mapping[str, Any]) -> List[str]:
    lines: List[str] = []
    if finding["summary"].strip() != finding["short_summary"].strip():
        lines.extend(["", renderer.escape_markdown(finding["summary"])])
    lines.extend(
        [
            "",
            "**Failure scenario.** "
            + renderer.escape_markdown(finding["failure_scenario"]),
        ]
    )
    if finding["verdict"] == "UNVERIFIED":
        lines.extend(["", renderer.UNVERIFIED_FINDING_ITALIC])
    else:
        reasoning = str(finding.get("verdict_reasoning") or "").strip()
        if reasoning:
            lines.extend(
                ["", "**Verifier.** " + renderer.escape_markdown(reasoning)]
            )
    return lines


def inline_more_lines(finding: Mapping[str, Any]) -> List[str]:
    verdict = str(finding["verdict"]).lower().capitalize()
    confidence = str(finding["confidence"]).lower().capitalize()
    lines = [
        "",
        "<details>",
        f"<summary>More · {verdict} · {confidence} confidence</summary>",
        "",
        "**Impact:** "
        + renderer.escape_markdown(finding["failure_scenario"]),
        "",
    ]
    reasoning = str(finding.get("verdict_reasoning") or "").strip()
    if reasoning:
        evidence = renderer.escape_markdown(reasoning)
    elif finding["verdict"] == "UNVERIFIED":
        evidence = "No independent verification ran at this effort level."
    else:
        evidence = "No verifier reasoning was recorded."
    lines.append("**Evidence:** " + evidence)

    metadata: List[str] = []
    reports = finding.get("reports")
    reporters = finding.get("reporters") or []
    if isinstance(reports, int) and reports > 1:
        report_text = f"- Reported by {reports} review passes"
        if reporters:
            report_text += ": " + renderer.escape_markdown(
                ", ".join(str(reporter) for reporter in reporters)
            )
        metadata.append(report_text)

    rule_ids = finding.get("rule_ids") or []
    if rule_ids:
        label = "Rule" if len(rule_ids) == 1 else "Rules"
        metadata.append(
            f"- {label}: "
            + ", ".join(renderer.code_span(rule_id) for rule_id in rule_ids)
        )

    for anchor in finding.get("anchors") or []:
        location = f"{anchor['file']}:{anchor['line']}"
        metadata.append(
            f"- Related location: {renderer.code_span(location)} "
            f"({renderer.escape_markdown(anchor['category'])}, "
            f"{renderer.escape_markdown(anchor['id'])})"
        )

    if metadata:
        lines.extend(["", *metadata])
    lines.extend(["", "</details>"])
    return lines


def inline_comment_body(finding: Mapping[str, Any], review_id: str) -> str:
    tag = comment_tag(review_id, finding["id"])
    issue_type = str(finding["issue_type"]).lower().capitalize()
    lines = [
        f"<!-- {tag} -->",
        meta_tag_for(finding),
        "",
        f"**{SEVERITY_EMOJI[finding['severity']]} {issue_type}** — "
        + renderer.escape_markdown(finding["short_summary"]),
    ]
    if finding["summary"].strip() != finding["short_summary"].strip():
        lines.extend(["", renderer.escape_markdown(finding["summary"])])
    if finding["verdict"] == "PLAUSIBLE":
        lines.extend(["", PLAUSIBLE_WARNING])
    elif finding["verdict"] == "UNVERIFIED":
        lines.extend(["", renderer.UNVERIFIED_FINDING_WARNING])
    suggestion = finding.get("suggestion")
    if isinstance(suggestion, dict):
        lines.extend(
            [
                "",
                *renderer.fenced_text(
                    suggestion["replacement_code"], "suggestion"
                ),
            ]
        )
    lines.extend(inline_more_lines(finding))
    return "\n".join(lines)


def summary_section(
    finding: Mapping[str, Any], reason_text: Optional[str]
) -> str:
    location_data = finding["location"]
    start_line = location_data["start_line"]
    end_line = location_data["end_line"]
    location_text = (
        f"{finding['file']}:{start_line}"
        if start_line == end_line
        else f"{finding['file']}:{start_line}-{end_line}"
    )
    location = renderer.code_span(location_text)
    facts = [location]
    if reason_text:
        facts.append(reason_text)
    facts.append(f"verdict {finding['verdict']}")
    facts.append(f"confidence {finding['confidence']}")
    rule_ids = finding.get("rule_ids") or []
    if rule_ids:
        facts.append(
            "rule "
            + ", ".join(renderer.code_span(rule_id) for rule_id in rule_ids)
        )
    lines = [
        f"### {finding['id']} · {finding['severity']} "
        f"{finding['issue_type']} / {finding['category']} — "
        + renderer.escape_markdown(finding["short_summary"]),
        "",
        " · ".join(facts),
        *finding_detail_lines(finding),
    ]
    excerpt = renderer.code_block(finding["code"])
    if excerpt:
        lines.extend(["", *excerpt])
    suggestion = finding.get("suggestion")
    if isinstance(suggestion, dict):
        lines.extend(
            [
                "",
                "<details><summary>Suggested change</summary>",
                "",
                "**Before:**",
                *renderer.fenced_text(location_data["existing_code"]),
                "",
                "**After:**",
                *renderer.fenced_text(suggestion["replacement_code"]),
                "",
                "</details>",
            ]
        )
    return "\n".join(lines)


def counts_line(
    total: int,
    inline: int,
    no_position: int,
    routed: int,
    failed: int,
    skipped: int = 0,
) -> str:
    if total == 0:
        return (
            "**No findings.** The review completed with nothing to report; "
            "this summary supersedes any earlier run."
        )
    text = (
        f"**{total} finding(s)** — posted inline: {inline} · "
        f"no diff position: {no_position} · routed by policy: {routed} · "
        f"could not be posted: {failed}"
    )
    # Skipped findings render as the count alone (P3, decided Q4); the
    # plan's skipped placements are the audit trail.
    if skipped:
        text += f" · already reported: {skipped}"
    return text


def rules_coverage_line(
    coverage: Mapping[str, Any], findings: Sequence[Mapping[str, Any]]
) -> Optional[str]:
    rules = coverage.get("rules")
    if not isinstance(rules, dict):
        return None
    effective = rules.get("effectiveChecksByFile")
    effective = effective if isinstance(effective, dict) else {}
    audited_files = sum(1 for check_ids in effective.values() if check_ids)
    distinct = {
        check_id
        for check_ids in effective.values()
        if isinstance(check_ids, list)
        for check_id in check_ids
    }
    counts = rules.get("counts") or {}
    rule_findings = sum(1 for finding in findings if finding.get("rule_ids"))
    return (
        f"Rules: audited {len(distinct)} check(s) "
        f"({counts.get('builtin_packs', 0)} built-in + "
        f"{counts.get('repo_packs', 0)} repository pack(s)) across "
        f"{audited_files} file(s); {rule_findings} rule violation(s) reported."
    )


def incremental_context_lines(manifest: Mapping[str, Any]) -> List[str]:
    """The sticky summary's incremental identity and degrade notes (P3)."""
    record = manifest.get("incremental")
    if not isinstance(record, dict) or not record.get("requested"):
        return []
    lines: List[str] = []
    delta_from = record.get("from")
    if isinstance(delta_from, str) and SHA_RE.fullmatch(delta_from):
        commits = record.get("commits")
        counted = (
            f"{commits} commit(s)"
            if isinstance(commits, int) and not isinstance(commits, bool)
            else "the commits"
        )
        lines.append(
            f"Incremental review: reviewed {counted} since "
            + renderer.code_span(delta_from[:12])
            + "; findings outside this delta were not re-reviewed."
        )
    reason = record.get("history_unavailable")
    if isinstance(reason, str) and reason.strip():
        lines.append(
            "History unavailable ("
            + renderer.escape_markdown(reason.strip())
            + "); earlier comments may be repeated."
        )
    return lines


def summary_context_lines(
    manifest: Mapping[str, Any],
    coverage: Mapping[str, Any],
    findings: Sequence[Mapping[str, Any]],
    reasons: Sequence[str],
    head: str,
    run_url: str,
) -> List[str]:
    lines: List[str] = []
    if reasons:
        lines.append(renderer.partial_review_warning(reasons))
    if manifest["effort"] == "low":
        lines.append(renderer.LOW_EFFORT_REVIEW_NOTE)
    lines.extend(incremental_context_lines(manifest))
    rules_text = rules_coverage_line(coverage, findings)
    if rules_text:
        lines.append(rules_text)
    lines.append(
        f"Review {renderer.code_span(manifest['review_id'])} · "
        f"effort {manifest['effort']} · mode {manifest['mode']} · "
        f"commit {renderer.code_span(head[:12])} · completed "
        + renderer.escape_markdown(manifest.get("completed_at"))
    )
    if run_url:
        lines.append(f"Run report: {run_url}")
    return lines


def elision_line(count: int, review_id: str, run_url: str) -> str:
    reference = f"see review {review_id}"
    if run_url:
        reference += f" and the run report: {run_url}"
    return f"_{count} finding(s) omitted from this summary; {reference}._"


def assemble_summary_body(
    marker: str,
    run_tag: str,
    completed_tag: str,
    counts_text: str,
    context_lines: Sequence[str],
    sections: Sequence[str],
    review_id: str,
    run_url: str,
    identity_tags: Sequence[str] = (),
) -> str:
    """Assemble the sticky summary under the size budget (R9, R19).

    Sections render in full in ranking order; when the next section would
    overflow the budget, it and every later section are replaced by one
    elision line. Elision affects rendering only, never counts.
    ``identity_tags`` carries the reviewed-head (and base) tags, written
    only into the final summary body -- never the anchor -- so a crash
    after the anchor write cannot leave a tag claiming this head was
    fully reviewed (P3 item 1).
    """
    head_parts = [marker, run_tag, completed_tag, *identity_tags, "",
                  "## Code review", "", counts_text]
    for line in context_lines:
        head_parts.extend(["", line])
    if sections:
        head_parts.extend(["", "### Findings not posted inline"])
    head = "\n".join(head_parts)
    for chosen in range(len(sections), 0, -1):
        omitted = len(sections) - chosen
        candidate = head + "".join(
            "\n\n" + section for section in sections[:chosen]
        )
        if omitted:
            candidate += "\n\n" + elision_line(omitted, review_id, run_url)
        if len(candidate) <= SUMMARY_BUDGET:
            return candidate
    body = head
    if sections:
        body += "\n\n" + elision_line(len(sections), review_id, run_url)
    return body


# --- PR history (P3 item 1) --------------------------------------------------


def optional_positive_line(value: Any, field: str) -> Optional[int]:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int) or value < 1:
        fail(f"history {field} must be a positive integer or null")
    return value


def validate_history_document(value: Any) -> Dict[str, Any]:
    """Validate a pr-history.json document (fail-closed, R5)."""
    if not isinstance(value, dict):
        fail("the history file is not a JSON object")
    if value.get("version") != HISTORY_VERSION:
        fail(
            "the history file has an unsupported version "
            f"(expected {HISTORY_VERSION}, got {value.get('version')!r})"
        )
    target = value.get("target")
    if (
        not isinstance(target, dict)
        or not isinstance(target.get("repo"), str)
        or not REPO_RE.fullmatch(target["repo"])
        or isinstance(target.get("pr"), bool)
        or not isinstance(target.get("pr"), int)
        or target["pr"] < 1
    ):
        fail("the history target is invalid")
    live_head = value.get("live_head")
    if not isinstance(live_head, str) or not SHA_RE.fullmatch(live_head):
        fail("the history live_head is not a commit SHA")
    summary = value.get("summary")
    if summary is not None:
        if not isinstance(summary, dict):
            fail("the history summary must be an object or null")
        if isinstance(summary.get("comment_id"), bool) or not isinstance(
            summary.get("comment_id"), int
        ):
            fail("the history summary.comment_id must be an integer")
        for field in ("head", "base"):
            sha = summary.get(field)
            if sha is not None and (
                not isinstance(sha, str) or not SHA_RE.fullmatch(sha)
            ):
                fail(f"the history summary.{field} must be a SHA or null")
    comments = value.get("comments")
    if not isinstance(comments, list):
        fail("the history comments must be an array")
    seen_ids: Set[int] = set()
    for index, comment in enumerate(comments):
        field = f"comments[{index}]"
        if not isinstance(comment, dict):
            fail(f"history {field} must be an object")
        comment_id = comment.get("id")
        if (
            isinstance(comment_id, bool)
            or not isinstance(comment_id, int)
            or comment_id < 1
            or comment_id in seen_ids
        ):
            fail(f"history {field}.id must be a unique positive integer")
        seen_ids.add(comment_id)
        try:
            renderer.safe_repo_path(comment.get("path"), f"history {field}.path")
        except renderer.RenderError as error:
            fail(str(error))
        if comment.get("side") not in ("LEFT", "RIGHT"):
            fail(f"history {field}.side is invalid")
        for line_field in (
            "line",
            "start_line",
            "original_line",
            "original_start_line",
        ):
            optional_positive_line(
                comment.get(line_field), f"{field}.{line_field}"
            )
        original_commit = comment.get("original_commit_id")
        if original_commit is not None and (
            not isinstance(original_commit, str)
            or not SHA_RE.fullmatch(original_commit)
        ):
            fail(f"history {field}.original_commit_id must be a SHA or null")
        span = comment.get("mapped_span")
        if span is not None:
            if not isinstance(span, dict):
                fail(f"history {field}.mapped_span must be an object or null")
            try:
                renderer.safe_repo_path(
                    span.get("path"), f"history {field}.mapped_span.path"
                )
            except renderer.RenderError as error:
                fail(str(error))
            start = optional_positive_line(
                span.get("start_line"), f"{field}.mapped_span.start_line"
            )
            end = optional_positive_line(
                span.get("end_line"), f"{field}.mapped_span.end_line"
            )
            if start is None or end is None or start > end:
                fail(f"history {field}.mapped_span lines are invalid")
            if not isinstance(span.get("partial"), bool):
                fail(f"history {field}.mapped_span.partial must be a boolean")
        meta = comment.get("meta")
        if meta is not None:
            if (
                not isinstance(meta, dict)
                or meta.get("category") not in renderer.CATEGORIES
                or meta.get("severity") not in SEVERITY_RANK
                or not isinstance(meta.get("short_summary"), str)
                or len(meta["short_summary"]) > 8000
            ):
                fail(f"history {field}.meta is invalid")
        html_url = comment.get("html_url")
        if html_url is not None and (
            not isinstance(html_url, str) or len(html_url) > 2048
        ):
            fail(f"history {field}.html_url is invalid")
    return value


def load_history_file(
    path: str, repo: str, pr: int, reviewed_head: str
) -> Tuple[Dict[str, Any], str]:
    """Read, validate, and digest a history file for this plan's target.

    The snapshot must describe exactly the reviewed head: a push landing
    between the checkout and the history fetch would make every decision
    built on it wrong from the outset, so a mismatch fails the plan.
    """
    try:
        raw = Path(path).read_bytes()
    except OSError as error:
        fail(f"could not read the history file: {error}")
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as error:
        fail(f"the history file is not valid JSON: {error}")
    history = validate_history_document(value)
    if history["target"] != {"repo": repo, "pr": pr}:
        fail("the history file's target does not match --repo/--pr")
    if history["live_head"] != reviewed_head:
        fail(
            f"the history snapshot describes head "
            f"{history['live_head'][:12]}, not the reviewed head "
            f"{reviewed_head[:12]}; the PR moved between the checkout and "
            "the history fetch -- re-run the review"
        )
    return history, hashlib.sha256(raw).hexdigest()


# --- plan --------------------------------------------------------------------


def bundle_digest(evidence_dir: str) -> str:
    hasher = hashlib.sha256()
    for name in CANONICAL_FILE_NAMES:
        path = Path(evidence_dir) / name
        try:
            raw = path.read_bytes()
        except OSError as error:
            fail(f"could not read canonical file {path}: {error}")
        hasher.update(name.encode("utf-8"))
        hasher.update(b"\x00")
        hasher.update(hashlib.sha256(raw).digest())
    return hasher.hexdigest()


def load_bundle(
    evidence_dir: str,
) -> Tuple[Dict[str, Any], List[Dict[str, Any]], Dict[str, Any], List[str]]:
    manifest = renderer.validate_manifest(
        renderer.read_json(evidence_dir, "review-manifest.json")
    )
    findings = renderer.validate_findings(
        renderer.read_json(evidence_dir, "findings.json")
    )
    ledger = renderer.validate_ledger(
        renderer.read_jsonl(evidence_dir, "candidate-ledger.jsonl")
    )
    votes = renderer.validate_votes(
        renderer.read_jsonl(evidence_dir, "votes.jsonl")
    )
    coverage = renderer.validate_coverage(
        renderer.read_json(evidence_dir, "coverage.json")
    )
    renderer.validate_relationships(
        manifest, findings, ledger, votes, coverage
    )
    reasons = renderer.partial_reasons(manifest, coverage)
    return manifest, findings, coverage, reasons


def resolve_reviewed_range(manifest: Mapping[str, Any]) -> Tuple[str, str, str]:
    """The reviewed (base, head, range) as local commits (R2, R20)."""
    if manifest["mode"] not in ("changes", "commit"):
        fail(
            "the publisher requires a ranged review (mode changes or "
            "commit); a files-mode bundle has no PR diff to anchor to"
        )
    range_text = manifest.get("range")
    if not isinstance(range_text, str) or not range_text.strip():
        fail("the manifest has no reviewed range")
    range_text = range_text.strip()
    revision = manifest.get("revision")
    if not isinstance(revision, dict) or not revision.get("versioned"):
        fail("the manifest has no versioned revision record")
    head = revision.get("commit")
    if not isinstance(head, str) or not SHA_RE.fullmatch(head):
        fail("the manifest revision does not name the reviewed head commit")
    if "..." in range_text:
        left_token = range_text.split("...", 1)[0]
        three_dot = True
    elif ".." in range_text:
        left_token = range_text.split("..", 1)[0]
        three_dot = False
    else:
        fail(f"the manifest range is not two-sided: {range_text!r}")
    if not left_token:
        fail(f"the manifest range has no base side: {range_text!r}")
    left_sha = resolve_diff_base(left_token)
    resolved_head = resolve_commit(head, "reviewed head")
    if resolved_head != head:
        fail("the reviewed head commit is not present in this repository")
    if three_dot:
        result = run_git("merge-base", left_sha, head)
        base = result.stdout.decode("utf-8", "replace").strip()
        if result.returncode != 0 or not SHA_RE.fullmatch(base):
            fail("the reviewed range endpoints have no merge base")
    else:
        base = left_sha
    return base, head, range_text


def duplicate_of_posted_detail(
    finding: Mapping[str, Any],
) -> Optional[Dict[str, Any]]:
    """The skipped-placement detail for a verifier-marked duplicate (item 5)."""
    record = finding.get("duplicate_of_posted")
    if not isinstance(record, dict):
        return None
    comment_id = record.get("comment_id")
    if isinstance(comment_id, bool) or not isinstance(comment_id, int):
        fail(
            f"finding {finding.get('id')} carries an invalid "
            "duplicate_of_posted record"
        )
    detail: Dict[str, Any] = {"comment_id": comment_id}
    html_url = record.get("html_url")
    if isinstance(html_url, str) and html_url.startswith(GITHUB_URL_PREFIX):
        detail["html_url"] = html_url
    return detail


def suppression_spans(
    history: Optional[Mapping[str, Any]],
) -> List[Dict[str, Any]]:
    """Our history comments' spans at the live head, for overlap tests.

    A comment suppresses only through a RIGHT-side mapped span (item 4
    supplies it: live API fields while the comment is live, git-mapped
    original fields once GitHub marks it outdated). A comment with no
    usable span never suppresses.
    """
    if history is None:
        return []
    spans: List[Dict[str, Any]] = []
    for comment in history["comments"]:
        if comment.get("side") != "RIGHT":
            continue
        span = comment.get("mapped_span")
        if not isinstance(span, dict):
            continue
        if span.get("partial") and not PARTIAL_MAPPED_SPANS_SUPPRESS:
            continue
        spans.append(
            {
                "comment_id": comment["id"],
                "review_id": comment.get("review_id"),
                "finding_id": comment.get("finding_id"),
                "html_url": comment.get("html_url"),
                "path": span["path"],
                "start_line": span["start_line"],
                "end_line": span["end_line"],
                "partial": bool(span.get("partial")),
            }
        )
    return spans


def overlap_match(
    finding: Mapping[str, Any],
    spans: Sequence[Mapping[str, Any]],
    threshold: float,
) -> Optional[Dict[str, Any]]:
    """The best history comment covering this finding's span, or None.

    Deterministic: the highest IoU wins; ties resolve to the lowest
    comment ID.
    """
    location = finding["location"]
    best: Optional[Dict[str, Any]] = None
    for span in spans:
        if span["path"] != finding["file"]:
            continue
        iou = span_overlap_iou(
            location["start_line"],
            location["end_line"],
            span["start_line"],
            span["end_line"],
            threshold,
        )
        if iou is None:
            continue
        if (
            best is None
            or iou > best["iou"]
            or (iou == best["iou"] and span["comment_id"] < best["comment_id"])
        ):
            best = {**span, "iou": iou}
    if best is None:
        return None
    detail: Dict[str, Any] = {
        "comment_id": best["comment_id"],
        "iou": round(best["iou"], 4),
        "mapped_span": {
            "path": best["path"],
            "start_line": best["start_line"],
            "end_line": best["end_line"],
            "partial": best["partial"],
        },
    }
    if isinstance(best.get("review_id"), str):
        detail["review_id"] = best["review_id"]
    if isinstance(best.get("finding_id"), str):
        detail["finding_id"] = best["finding_id"]
    html_url = best.get("html_url")
    if isinstance(html_url, str) and html_url.startswith(GITHUB_URL_PREFIX):
        detail["html_url"] = html_url
    return detail


def command_plan(args: argparse.Namespace) -> int:
    repo = args.repo.strip()
    if not REPO_RE.fullmatch(repo) or ".." in repo:
        fail(f"repo must look like owner/name, got {args.repo!r}")
    pr = parse_pr_number(args.pr)
    # Fail-closed routing policy (R5): a malformed configuration fails the
    # plan before anything can be posted.
    threshold = parse_severity_threshold(args.route_severity_below)
    categories = parse_route_categories(args.route_categories)
    batch_size = parse_batch_size(args.batch_size)
    incremental = parse_incremental_flag(args.incremental)
    overlap_threshold = parse_overlap_threshold(
        args.incremental_overlap_threshold
    )
    if args.history and not incremental:
        fail("--history requires --incremental true")
    run_url = (args.run_url or "").strip()
    if len(run_url) > 2048:
        fail("run-url exceeds 2048 characters")

    manifest, findings, coverage, reasons = load_bundle(args.evidence_dir)
    base, head, range_text = resolve_reviewed_range(manifest)
    review_id = str(manifest["review_id"])
    completed_at = manifest.get("completed_at")
    if not isinstance(completed_at, str) or not completed_at.strip():
        fail("the manifest has no completion timestamp")

    history: Optional[Dict[str, Any]] = None
    history_digest: Optional[str] = None
    if args.history:
        history, history_digest = load_history_file(
            args.history, repo, pr, head
        )
    spans = suppression_spans(history)

    hunks = right_side_hunks(base, head)

    # Exhaustive partition (R1): every finding gets exactly one placement.
    # A verifier-marked duplicate of a posted comment skips first (the
    # defect already has a visible comment, wherever it sits); then diff
    # position decides (R2); then overlap suppression (a finding already
    # posted must not be routed to the summary as new); routing applies
    # last, only to otherwise inline-eligible findings, so a finding
    # matching several policies carries the earliest reason and nothing
    # can hide a placement.
    placements: List[Dict[str, Any]] = []
    for finding in findings:
        location = finding["location"]
        start_line = location["start_line"]
        end_line = location["end_line"]
        base_entry = {
            "finding_id": finding["id"],
            "path": finding["file"],
            "line": end_line,
            "start_line": start_line,
            "end_line": end_line,
        }
        posted_duplicate = duplicate_of_posted_detail(finding)
        if posted_duplicate is not None:
            placements.append(
                {
                    **base_entry,
                    "placement": "skipped",
                    "reason": "duplicate-of-posted",
                    "detail": posted_duplicate,
                }
            )
            continue
        if not has_diff_range(
            hunks, finding["file"], start_line, end_line
        ):
            placements.append(
                {
                    **base_entry,
                    "placement": "summary",
                    "reason": "no-position",
                    "body": summary_section(
                        finding, "no diff position in the reviewed range"
                    ),
                }
            )
            continue
        overlap = overlap_match(finding, spans, overlap_threshold)
        if overlap is not None:
            placements.append(
                {
                    **base_entry,
                    "placement": "skipped",
                    "reason": "overlap",
                    "detail": overlap,
                }
            )
            continue
        detail = routing_detail(finding, threshold, categories)
        if detail is not None:
            placements.append(
                {
                    **base_entry,
                    "placement": "summary",
                    "reason": "routed",
                    "detail": detail,
                    "body": summary_section(
                        finding, f"routed to the summary by policy — {detail}"
                    ),
                }
            )
            continue
        placements.append(
            {
                **base_entry,
                "placement": "inline",
                "comment_id": comment_tag(review_id, finding["id"]),
                "body": inline_comment_body(finding, review_id),
                "section": summary_section(finding, None),
            }
        )

    inline_entries = [
        entry for entry in placements if entry["placement"] == "inline"
    ]
    no_position = sum(
        1
        for entry in placements
        if entry["placement"] == "summary" and entry["reason"] == "no-position"
    )
    routed = sum(
        1
        for entry in placements
        if entry["placement"] == "summary" and entry["reason"] == "routed"
    )
    skipped = sum(
        1 for entry in placements if entry["placement"] == "skipped"
    )

    # Deterministic batching (R13): sorted (path, line, finding ID), then
    # contiguous chunks of at most batch_size. Routed findings never enter
    # a batch (R6).
    ordered = sorted(
        inline_entries,
        key=lambda entry: (
            entry["path"],
            entry["line"],
            int(entry["finding_id"][1:]),
        ),
    )
    batches = [
        [entry["finding_id"] for entry in ordered[index:index + batch_size]]
        for index in range(0, len(ordered), batch_size)
    ]

    run_tag = run_tag_for(review_id)
    completed_tag = completed_tag_for(completed_at)
    head_tag = head_tag_for(head)
    base_tag = base_tag_for(base)
    context = summary_context_lines(
        manifest, coverage, findings, reasons, head, run_url
    )
    sections = [
        entry["body"] for entry in placements if entry["placement"] == "summary"
    ]
    summary_body = assemble_summary_body(
        SUMMARY_MARKER,
        run_tag,
        completed_tag,
        counts_line(
            len(findings),
            len(inline_entries),
            no_position,
            routed,
            0,
            skipped,
        ),
        context,
        sections,
        review_id,
        run_url,
        identity_tags=(head_tag, base_tag),
    )
    # The anchor never carries the head tag: a crash after the anchor
    # write must not leave a tag claiming this head was fully reviewed.
    anchor_body = "\n".join(
        [
            SUMMARY_MARKER,
            run_tag,
            completed_tag,
            "",
            "## Code review",
            "",
            "_Posting code review results…_",
        ]
    )

    incremental_record = manifest.get("incremental")
    history_unavailable = ""
    if isinstance(incremental_record, dict):
        reason = incremental_record.get("history_unavailable")
        if isinstance(reason, str):
            history_unavailable = reason.strip()

    plan = {
        "version": PLAN_VERSION,
        "review_id": review_id,
        "mode": manifest["mode"],
        "effort": manifest["effort"],
        "target": {"repo": repo, "pr": pr},
        "base": base,
        "head": head,
        "range": range_text,
        "bundle_digest": bundle_digest(args.evidence_dir),
        "history_digest": history_digest,
        "config": {
            "batch_size": batch_size,
            "route_severity_below": threshold.lower() if threshold else "",
            "route_categories": categories,
            "run_url": run_url,
            "incremental": incremental,
            "incremental_overlap_threshold": overlap_threshold,
            "history_unavailable": history_unavailable,
        },
        "placements": placements,
        "batches": batches,
        "summary": {
            "marker": SUMMARY_MARKER,
            "run_tag": run_tag,
            "completed_tag": completed_tag,
            "completed_at": completed_at,
            "head_tag": head_tag,
            "base_tag": base_tag,
            "anchor_body": anchor_body,
            "body": summary_body,
            "context_lines": context,
        },
        "counts": {
            "total": len(findings),
            "planned_inline": len(inline_entries),
            "no_position": no_position,
            "routed": routed,
            "skipped": skipped,
        },
    }
    output = Path(args.output)
    temporary = output.with_name(output.name + ".tmp")
    temporary.write_text(
        json.dumps(plan, ensure_ascii=True, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, output)
    print(
        f"Planned {len(findings)} placement(s): {len(inline_entries)} inline "
        f"in {len(batches)} batch(es), {no_position} without a diff "
        f"position, {routed} routed, {skipped} skipped (already reported)"
    )
    return 0


# --- Plan re-validation (apply-side, R18) ------------------------------------


def require_text(value: Any, field: str, limit: int) -> str:
    if not isinstance(value, str) or not value or len(value) > limit:
        fail(f"plan {field} must be a string of at most {limit} characters")
    return value


def validate_plan_document(
    value: Any, repo: str, pr: int
) -> Dict[str, Any]:
    """Re-validate the untrusted plan before any write (R18)."""
    if not isinstance(value, dict):
        fail("the plan is not a JSON object")
    if value.get("version") != PLAN_VERSION:
        fail(
            "the plan has an unsupported version: this apply understands "
            f"version {PLAN_VERSION}, the plan carries "
            f"{value.get('version')!r}"
        )
    review_id = value.get("review_id")
    if not isinstance(review_id, str) or not REVIEW_ID_RE.fullmatch(review_id):
        fail("the plan review_id is invalid")
    target = value.get("target")
    if not isinstance(target, dict) or target.get("repo") != repo or (
        target.get("pr") != pr
    ):
        fail(
            "the plan's embedded target does not match --repo/--pr; refusing"
        )
    for field in ("base", "head"):
        sha = value.get(field)
        if not isinstance(sha, str) or not SHA_RE.fullmatch(sha):
            fail(f"the plan {field} is not a commit SHA")
    history_digest = value.get("history_digest")
    if history_digest is not None and (
        not isinstance(history_digest, str)
        or not re.fullmatch(r"[0-9a-f]{64}", history_digest)
    ):
        fail("the plan history_digest is invalid")
    config = value.get("config")
    if not isinstance(config, dict):
        fail("the plan config is missing")
    run_url = config.get("run_url", "")
    if not isinstance(run_url, str) or len(run_url) > 2048:
        fail("the plan run_url is invalid")
    if not isinstance(config.get("incremental"), bool):
        fail("the plan config.incremental must be a boolean")
    overlap_threshold = config.get("incremental_overlap_threshold")
    if (
        isinstance(overlap_threshold, bool)
        or not isinstance(overlap_threshold, (int, float))
        or not (0.0 < float(overlap_threshold) < 1.0)
    ):
        fail(
            "the plan config.incremental_overlap_threshold must be a "
            "number strictly between 0 and 1"
        )
    if not isinstance(config.get("history_unavailable", ""), str):
        fail("the plan config.history_unavailable must be a string")

    placements = value.get("placements")
    if not isinstance(placements, list):
        fail("the plan placements must be an array")
    seen_ids: Set[str] = set()
    inline_ids: List[str] = []
    reason_counts = {"no-position": 0, "routed": 0}
    skipped_count = 0
    for index, entry in enumerate(placements):
        field = f"placements[{index}]"
        if not isinstance(entry, dict):
            fail(f"plan {field} must be an object")
        finding_id = entry.get("finding_id")
        if not isinstance(finding_id, str) or not FINDING_ID_RE.fullmatch(
            finding_id
        ):
            fail(f"plan {field}.finding_id is invalid")
        if finding_id in seen_ids:
            fail(f"plan {field} repeats finding {finding_id}")
        seen_ids.add(finding_id)
        try:
            renderer.safe_repo_path(entry.get("path"), f"plan {field}.path")
        except renderer.RenderError as error:
            fail(str(error))
        line = entry.get("line")
        if isinstance(line, bool) or not isinstance(line, int) or line < 1:
            fail(f"plan {field}.line must be a positive integer")
        start_line = entry.get("start_line")
        end_line = entry.get("end_line")
        if (
            isinstance(start_line, bool)
            or not isinstance(start_line, int)
            or start_line < 1
        ):
            fail(f"plan {field}.start_line must be a positive integer")
        if (
            isinstance(end_line, bool)
            or not isinstance(end_line, int)
            or end_line < start_line
            or end_line != line
        ):
            fail(
                f"plan {field}.end_line must end at its line and not precede "
                "start_line"
            )
        placement = entry.get("placement")
        if placement == "skipped":
            # The P3 placement kind: a finding already covered by one of
            # our earlier comments. It carries no body and never posts;
            # its detail names the covering comment (the audit trail).
            if entry.get("reason") not in SKIP_REASONS:
                fail(f"plan {field}.reason is invalid")
            skipped_count += 1
            detail = entry.get("detail")
            if not isinstance(detail, dict):
                fail(f"plan {field}.detail must name the covering comment")
            comment_id = detail.get("comment_id")
            if isinstance(comment_id, bool) or not isinstance(
                comment_id, int
            ):
                fail(f"plan {field}.detail.comment_id must be an integer")
            html_url = detail.get("html_url")
            if html_url is not None and (
                not isinstance(html_url, str)
                or len(html_url) > 2048
                or not html_url.startswith(GITHUB_URL_PREFIX)
            ):
                fail(
                    f"plan {field}.detail.html_url must be an "
                    "https://github.com/ URL or absent"
                )
            iou = detail.get("iou")
            if iou is not None and (
                isinstance(iou, bool)
                or not isinstance(iou, (int, float))
                or not (0.0 <= float(iou) <= 1.0)
            ):
                fail(f"plan {field}.detail.iou is invalid")
            if "body" in entry:
                fail(f"plan {field} is skipped and must carry no body")
            continue
        body = require_text(entry.get("body"), f"{field}.body", GITHUB_BODY_CAP)
        if placement == "inline":
            expected_tag = comment_tag(review_id, finding_id)
            if entry.get("comment_id") != expected_tag:
                fail(f"plan {field}.comment_id is not this review's tag")
            if f"<!-- {expected_tag} -->" not in body:
                fail(f"plan {field}.body does not embed its identity tag")
            if "section" in entry:
                require_text(
                    entry.get("section"), f"{field}.section", GITHUB_BODY_CAP
                )
            inline_ids.append(finding_id)
        elif placement == "summary":
            reason = entry.get("reason")
            if reason not in reason_counts:
                fail(f"plan {field}.reason is invalid")
            reason_counts[reason] += 1
            if "detail" in entry:
                require_text(entry.get("detail"), f"{field}.detail", 2000)
        else:
            fail(f"plan {field}.placement is invalid")

    batches = value.get("batches")
    if not isinstance(batches, list) or not all(
        isinstance(batch, list) and batch for batch in batches
    ):
        fail("the plan batches must be an array of non-empty arrays")
    batched = [finding_id for batch in batches for finding_id in batch]
    if len(batched) != len(set(batched)) or set(batched) != set(inline_ids):
        fail(
            "the plan batches do not partition exactly the inline "
            "placements (R6)"
        )

    summary = value.get("summary")
    if not isinstance(summary, dict):
        fail("the plan summary is missing")
    if summary.get("marker") != SUMMARY_MARKER:
        fail("the plan summary marker is not this workflow's marker")
    if summary.get("run_tag") != run_tag_for(review_id):
        fail("the plan summary run_tag is not this review's tag")
    completed_at = require_text(
        summary.get("completed_at"), "summary.completed_at", 64
    )
    if summary.get("completed_tag") != completed_tag_for(completed_at):
        fail("the plan summary completed_tag is inconsistent")
    if summary.get("head_tag") != head_tag_for(value["head"]):
        fail("the plan summary head_tag is not the reviewed head's tag")
    if summary.get("base_tag") != base_tag_for(value["base"]):
        fail("the plan summary base_tag is not the reviewed base's tag")
    for field in ("anchor_body", "body"):
        body = require_text(
            summary.get(field), f"summary.{field}", GITHUB_BODY_CAP
        )
        if SUMMARY_MARKER not in body:
            fail(f"the plan summary.{field} does not embed the marker")
    if summary["head_tag"] not in summary["body"]:
        fail("the plan summary.body does not embed the head tag")
    # The head tag claims this head was fully reviewed; only the final
    # summary body may carry it, never the anchor (P3 item 1).
    if HEAD_TAG_PREFIX in summary["anchor_body"]:
        fail("the plan summary.anchor_body must not carry a head tag")
    context = summary.get("context_lines")
    if not isinstance(context, list) or len(context) > 100 or not all(
        isinstance(line, str) and len(line) <= GITHUB_BODY_CAP
        for line in context
    ):
        fail("the plan summary.context_lines are invalid")

    counts = value.get("counts")
    if not isinstance(counts, dict):
        fail("the plan counts are missing")
    for field in ("total", "planned_inline", "no_position", "routed", "skipped"):
        entry = counts.get(field)
        if isinstance(entry, bool) or not isinstance(entry, int) or entry < 0:
            fail(f"plan counts.{field} must be a non-negative integer")
    if (
        counts["total"] != len(placements)
        or counts["planned_inline"] != len(inline_ids)
        or counts["no_position"] != reason_counts["no-position"]
        or counts["routed"] != reason_counts["routed"]
        or counts["skipped"] != skipped_count
    ):
        fail("the plan counts do not reconcile with its placements (R1)")
    return value


# --- GitHub client (apply) ---------------------------------------------------


class GitHubClient:
    def __init__(self, api_base: str, token: str) -> None:
        self.api_base = api_base.rstrip("/")
        self.token = token

    def request(
        self, method: str, path: str, payload: Optional[Dict[str, Any]] = None
    ) -> Tuple[Optional[int], Any]:
        """(status, parsed JSON); status None means a network failure."""
        data = (
            json.dumps(payload).encode("utf-8") if payload is not None else None
        )
        request = urllib.request.Request(
            self.api_base + path,
            data=data,
            method=method,
            headers={
                "Authorization": f"Bearer {self.token}",
                "Accept": "application/vnd.github+json",
                "Content-Type": "application/json",
                "User-Agent": "fabro-code-review-publisher",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read()
                status = response.status
        except urllib.error.HTTPError as error:
            raw = error.read()
            status = error.code
        except (urllib.error.URLError, OSError):
            return None, None
        try:
            return status, json.loads(raw) if raw else None
        except json.JSONDecodeError:
            return status, None

    def list_all(self, path: str, params: str = "") -> List[Dict[str, Any]]:
        results: List[Dict[str, Any]] = []
        suffix = f"&{params}" if params else ""
        for page in range(1, 51):
            status, value = self.request(
                "GET", f"{path}?per_page=100&page={page}{suffix}"
            )
            if status != 200 or not isinstance(value, list):
                fail(f"could not list {path} (HTTP {status})")
            results.extend(
                entry for entry in value if isinstance(entry, dict)
            )
            if len(value) < 100:
                break
        return results


def extract_posted_ids(
    comments: Sequence[Mapping[str, Any]], review_id: str
) -> Set[str]:
    pattern = re.compile(
        r"<!-- "
        + re.escape(f"{COMMENT_TAG_PREFIX}:{review_id}:")
        + r"(R[0-9]+) -->"
    )
    posted: Set[str] = set()
    for comment in comments:
        for match in pattern.finditer(str(comment.get("body") or "")):
            posted.add(match.group(1))
    return posted


# --- history (P3 item 1: the token-holding sibling of apply) -----------------


def resolve_history_login(
    client: GitHubClient, bot_login: str
) -> Optional[str]:
    """The login whose comments count as ours, for this read-only fetch.

    The bot_login input wins when set; otherwise GET /user works on
    PAT-mode servers. Apply's write-probe cannot be reused here -- the
    probe is a write -- so on App-mode servers (where /user returns 403)
    bot_login is effectively required for incremental to work at all.
    """
    login = (bot_login or "").strip()
    if login:
        return login
    status, user = client.request("GET", "/user")
    if status == 401:
        fail("GitHub rejected the token (HTTP 401)")
    if status == 200 and isinstance(user, dict) and user.get("login"):
        return str(user["login"])
    return None


def comment_line_field(comment: Mapping[str, Any], field: str) -> Optional[int]:
    value = comment.get(field)
    if isinstance(value, bool) or not isinstance(value, int) or value < 1:
        return None
    return value


def history_mapped_span(
    comment: Mapping[str, Any], live_head: str
) -> Optional[Dict[str, Any]]:
    """The comment's span at the live head (P3 item 4).

    While the comment is live its ``line``/``start_line`` already track
    the current diff -- the live head. Once a push outdates it those go
    null and the creation-time ``original_*`` fields are mapped forward
    from ``original_commit_id`` with pure git arithmetic. An original
    commit missing from local history (a force-push) cannot be mapped.
    """
    path = comment.get("path")
    if not isinstance(path, str) or not path:
        return None
    line = comment_line_field(comment, "line")
    if line is not None:
        start = comment_line_field(comment, "start_line") or line
        return {
            "path": path,
            "start_line": min(start, line),
            "end_line": line,
            "partial": False,
        }
    original_line = comment_line_field(comment, "original_line")
    original_commit = comment.get("original_commit_id")
    if original_line is None or not isinstance(original_commit, str) or not (
        SHA_RE.fullmatch(original_commit)
    ):
        return None
    result = run_git(
        "rev-parse", "--verify", "--quiet", original_commit + "^{commit}"
    )
    if result.returncode != 0:
        return None
    original_start = (
        comment_line_field(comment, "original_start_line") or original_line
    )
    try:
        return map_span(
            original_commit,
            live_head,
            path,
            min(original_start, original_line),
            original_line,
        )
    except PublishError:
        return None


def command_history(args: argparse.Namespace) -> int:
    token = os.environ.get("GITHUB_TOKEN", "")
    if not token:
        fail("history requires GITHUB_TOKEN in the environment, never argv")
    repo = args.repo.strip()
    if not REPO_RE.fullmatch(repo) or ".." in repo:
        fail(f"repo must look like owner/name, got {args.repo!r}")
    pr = parse_pr_number(args.pr)
    client = GitHubClient(args.api_base, token)

    status, pull = client.request("GET", f"/repos/{repo}/pulls/{pr}")
    if status != 200 or not isinstance(pull, dict):
        fail(f"could not read the PR (HTTP {status})")
    live_head = (pull.get("head") or {}).get("sha")
    if not isinstance(live_head, str) or not SHA_RE.fullmatch(live_head):
        fail("the PR head is not a commit SHA")

    login = resolve_history_login(client, args.bot_login)
    if login is None:
        # History drives range selection and suppression, so ownership
        # must be attributable; without an identity the run degrades to
        # no history (decided, Q2).
        fail(
            "no identity for the history fetch: GET /user is unavailable "
            "(a GitHub App installation token?) and the bot_login input "
            "is not set; degrading to no history"
        )

    review_comments = client.list_all(
        f"/repos/{repo}/pulls/{pr}/comments",
        params="sort=created&direction=desc",
    )
    issue_comments = client.list_all(f"/repos/{repo}/issues/{pr}/comments")

    # Only comments that carry our identity tag AND were authored by our
    # resolved login count: the tag alone is never sufficient -- a forged
    # tag from another author could shrink the delta or hide a finding.
    comments: List[Dict[str, Any]] = []
    for comment in review_comments:
        body = str(comment.get("body") or "")
        identity = parse_comment_identity(body)
        if identity is None:
            continue
        if (comment.get("user") or {}).get("login") != login:
            continue
        comment_id = comment.get("id")
        if isinstance(comment_id, bool) or not isinstance(comment_id, int):
            continue
        record: Dict[str, Any] = {
            "id": comment_id,
            "review_id": identity[0],
            "finding_id": identity[1],
            "path": comment.get("path"),
            "side": comment.get("side") or "RIGHT",
            "line": comment_line_field(comment, "line"),
            "start_line": comment_line_field(comment, "start_line"),
            "original_line": comment_line_field(comment, "original_line"),
            "original_start_line": comment_line_field(
                comment, "original_start_line"
            ),
            "original_commit_id": comment.get("original_commit_id"),
            "html_url": str(comment.get("html_url") or "")[:2048],
            "meta": parse_meta_tag(body),
            "mapped_span": history_mapped_span(comment, live_head),
        }
        comments.append(record)
    comments.sort(key=lambda record: record["id"])

    summary_record: Optional[Dict[str, Any]] = None
    owned_summaries = [
        comment
        for comment in issue_comments
        if SUMMARY_MARKER in str(comment.get("body") or "")
        and (comment.get("user") or {}).get("login") == login
        and isinstance(comment.get("id"), int)
    ]
    if owned_summaries:
        newest = max(owned_summaries, key=lambda comment: comment["id"])
        body = str(newest.get("body") or "")
        run_match = re.search(
            r"<!-- " + re.escape(RUN_TAG_PREFIX) + r":([^>]+) -->", body
        )
        summary_record = {
            "comment_id": newest["id"],
            "review_id": run_match.group(1).strip() if run_match else None,
            "completed_at": completed_stamp(body),
            "head": tagged_sha(body, HEAD_TAG_PREFIX),
            "base": tagged_sha(body, BASE_TAG_PREFIX),
        }

    document = {
        "version": HISTORY_VERSION,
        "target": {"repo": repo, "pr": pr},
        "live_head": live_head,
        "bot_login": login,
        "summary": summary_record,
        "comments": comments,
    }
    validate_history_document(document)
    output = Path(args.output)
    temporary = output.with_name(output.name + ".tmp")
    temporary.write_text(
        json.dumps(document, ensure_ascii=True, indent=2, sort_keys=True)
        + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, output)
    print(
        f"Fetched PR history: {len(comments)} owned inline comment(s), "
        + (
            "summary head "
            + (summary_record.get("head") or "untagged")[:12]
            if summary_record
            else "no owned summary"
        )
    )
    return 0


def completed_stamp(body: str) -> Optional[str]:
    match = re.search(
        r"<!-- " + re.escape(COMPLETED_TAG_PREFIX) + r":([^>]+) -->", body
    )
    return match.group(1).strip() if match else None


def is_newer_stamp(existing: Optional[str], ours: str) -> bool:
    if existing is None:
        return False
    try:
        return datetime.fromisoformat(existing) > datetime.fromisoformat(ours)
    except (ValueError, TypeError):
        return existing > ours


def strip_identity_tag(body: str) -> str:
    return re.sub(
        r"^(?:<!-- (?:"
        + re.escape(COMMENT_TAG_PREFIX)
        + r"|"
        + re.escape(META_TAG_PREFIX)
        + r"):[^>]+ -->\n*)+",
        "",
        body,
    )


# --- apply -------------------------------------------------------------------


class BatchPoster:
    """Posts inline batches with never-duplicate discipline (R13-R15)."""

    def __init__(
        self,
        client: GitHubClient,
        repo: str,
        pr: int,
        plan: Mapping[str, Any],
        already_posted: Set[str],
        commit_id: Optional[str] = None,
    ) -> None:
        self.client = client
        self.repo = repo
        self.pr = pr
        self.head = commit_id or plan["head"]
        self.by_id = {
            entry["finding_id"]: entry
            for entry in plan["placements"]
            if entry["placement"] == "inline"
        }
        self.review_id = plan["review_id"]
        self.posted: Set[str] = set(already_posted)
        self.failed: Dict[str, str] = {}
        self.batches_attempted = 0
        self.batches_succeeded = 0

    def post_review(self, finding_ids: Sequence[str]) -> Optional[int]:
        comments: List[Dict[str, Any]] = []
        for finding_id in finding_ids:
            entry = self.by_id[finding_id]
            comment: Dict[str, Any] = {
                "path": entry["path"],
                "line": entry["end_line"],
                "side": "RIGHT",
                "body": entry["body"],
            }
            if entry["start_line"] != entry["end_line"]:
                comment["start_line"] = entry["start_line"]
                comment["start_side"] = "RIGHT"
            comments.append(comment)
        # No review body: the run tag lives in the sticky summary and in
        # every inline comment's identity tag, so body text here would
        # only add a noise bubble to the PR timeline (R12).
        status, _ = self.client.request(
            "POST",
            f"/repos/{self.repo}/pulls/{self.pr}/reviews",
            {
                "commit_id": self.head,
                "event": "COMMENT",
                "comments": comments,
            },
        )
        return status

    def landed_ids(self) -> Optional[Set[str]]:
        """The finding IDs whose comments are on the PR, or None if the
        read failed (then nothing can be verified, R14)."""
        try:
            comments = self.client.list_all(
                f"/repos/{self.repo}/pulls/{self.pr}/comments"
            )
        except PublishError:
            return None
        return extract_posted_ids(comments, self.review_id)

    @staticmethod
    def is_server_failure(status: Optional[int]) -> bool:
        return status is None or status == 408 or (status >= 500)

    UNVERIFIED_DETAIL = (
        "a server error interrupted the write and the result could not "
        "be verified"
    )
    DROPPED_DETAIL = (
        "a server error dropped the write; the comment was verified "
        "missing and the retry also failed"
    )

    def mark_failed(self, finding_ids: Sequence[str], detail: str) -> None:
        for finding_id in finding_ids:
            self.failed[finding_id] = detail

    def reconcile(self, finding_ids: Sequence[str]) -> List[str]:
        """After a possibly-landed failure: absorb what landed, return what
        is verifiably missing; on an unverifiable read, mark failed and
        return nothing (never risk a duplicate, R14)."""
        landed = self.landed_ids()
        if landed is None:
            self.mark_failed(finding_ids, self.UNVERIFIED_DETAIL)
            return []
        self.posted.update(landed & set(self.by_id))
        return [fid for fid in finding_ids if fid not in landed]

    def post_individual_pass(self, finding_ids: Sequence[str]) -> List[str]:
        """Post once and return writes with an ambiguous server result."""
        ambiguous: List[str] = []
        for finding_id in finding_ids:
            status = self.post_review([finding_id])
            if status in (200, 201):
                self.posted.add(finding_id)
            elif status == 422:
                self.failed[finding_id] = (
                    "GitHub could not resolve the diff position (422)"
                )
            elif self.is_server_failure(status):
                ambiguous.append(finding_id)
            else:
                self.failed[finding_id] = f"GitHub refused the comment (HTTP {status})"
        return ambiguous

    def post_individually(self, finding_ids: Sequence[str]) -> None:
        """Per-comment fallback that isolates unpostable comments (R15).

        Reconcile ambiguous writes once per pass. This preserves duplicate
        safety without re-paginating the complete comment history for every
        failed comment.
        """
        ambiguous = self.post_individual_pass(finding_ids)
        if not ambiguous:
            return
        missing = self.reconcile(ambiguous)
        if not missing:
            return
        # Verified missing, so one retry cannot duplicate (R14).
        retry_ambiguous = self.post_individual_pass(missing)
        if retry_ambiguous:
            still_missing = self.reconcile(retry_ambiguous)
            if still_missing:
                self.mark_failed(still_missing, self.DROPPED_DETAIL)

    def post_batch(self, batch_ids: Sequence[str]) -> None:
        # A finding pre-marked failed (a span that did not survive drift
        # mapping) is never attempted; it routes to the summary (R15).
        to_send = [
            fid
            for fid in batch_ids
            if fid not in self.posted and fid not in self.failed
        ]
        if to_send:
            self.batches_attempted += 1
            status = self.post_review(to_send)
            if status in (200, 201):
                self.posted.update(to_send)
            elif status == 422:
                self.post_individually(to_send)
            elif self.is_server_failure(status):
                missing = self.reconcile(to_send)
                if missing:
                    retry_status = self.post_review(missing)
                    if retry_status in (200, 201):
                        self.posted.update(missing)
                    elif retry_status == 422:
                        self.post_individually(missing)
                    else:
                        still_missing = self.reconcile(missing)
                        if still_missing:
                            self.mark_failed(
                                still_missing, self.DROPPED_DETAIL
                            )
            else:
                for finding_id in to_send:
                    self.failed[finding_id] = (
                        f"GitHub refused the batch (HTTP {status})"
                    )
        if all(fid in self.posted for fid in batch_ids):
            self.batches_succeeded += 1


def ensure_commit_local(sha: str) -> bool:
    """True when the commit is available locally, fetching if needed."""
    check = run_git("rev-parse", "--verify", "--quiet", sha + "^{commit}")
    if check.returncode == 0:
        return True
    # The sandbox clone's HTTPS credentials are ambient; a fetch failure
    # falls back to the R20 refusal.
    fetched = run_git("fetch", "origin", sha)
    if fetched.returncode != 0:
        return False
    check = run_git("rev-parse", "--verify", "--quiet", sha + "^{commit}")
    return check.returncode == 0


def forward_map_placements(
    plan: Mapping[str, Any], live_head: str
) -> Dict[str, str]:
    """Map every planned inline span from plan.head onto the live head.

    Mutates the (validated) placement entries in place and returns the
    finding IDs whose span did not survive the mapping, with the reason;
    those route to the summary through the failed-comment path (R15).
    Raises PublishError when the drift cannot be trusted at all, and the
    caller falls back to the R20 refusal.
    """
    if not ensure_commit_local(live_head):
        fail(
            f"the live head {live_head[:12]} could not be fetched for "
            "drift mapping"
        )
    ancestry = run_git(
        "merge-base", "--is-ancestor", plan["head"], live_head
    )
    if ancestry.returncode != 0:
        fail(
            f"the plan head {plan['head'][:12]} is not an ancestor of the "
            f"live head {live_head[:12]} (a force-push?)"
        )
    unmapped: Dict[str, str] = {}
    for entry in plan["placements"]:
        if entry["placement"] != "inline":
            continue
        mapped = map_span(
            plan["head"],
            live_head,
            entry["path"],
            entry["start_line"],
            entry["end_line"],
        )
        if mapped is None or mapped["partial"]:
            unmapped[entry["finding_id"]] = (
                "the PR head moved past the reviewed head and this "
                "comment's lines did not survive the move"
            )
            continue
        entry["path"] = mapped["path"]
        entry["start_line"] = mapped["start_line"]
        entry["end_line"] = mapped["end_line"]
        entry["line"] = mapped["end_line"]
    return unmapped


def command_apply(args: argparse.Namespace) -> int:
    token = os.environ.get("GITHUB_TOKEN", "")
    if not token:
        fail("apply requires GITHUB_TOKEN in the environment, never argv")
    repo = args.repo.strip()
    if not REPO_RE.fullmatch(repo) or ".." in repo:
        fail(f"repo must look like owner/name, got {args.repo!r}")
    pr = parse_pr_number(args.pr)
    try:
        raw_plan = json.loads(Path(args.plan).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        fail(f"could not read the plan: {error}")
    plan = validate_plan_document(raw_plan, repo, pr)
    review_id = plan["review_id"]
    summary = plan["summary"]
    client = GitHubClient(args.api_base, token)

    # Head drift check before the first write (R20, revised by P3 item 4):
    # when the live head has moved past the plan's head, apply forward-maps
    # every planned span onto the live head instead of refusing; spans that
    # do not survive the mapping route to the summary (R15). When the
    # drift cannot be trusted (fetch failure, force-push, mapping error),
    # the original refusal stands: exit nonzero before the first write.
    status, pull = client.request("GET", f"/repos/{repo}/pulls/{pr}")
    if status != 200 or not isinstance(pull, dict):
        fail(f"could not read the PR (HTTP {status})")
    live_head = (pull.get("head") or {}).get("sha")
    drift_failures: Dict[str, str] = {}
    posting_head = plan["head"]
    if live_head != plan["head"]:
        refusal = (
            f"the live PR head {str(live_head)[:12]} does not match the "
            f"plan's reviewed head {plan['head'][:12]}; refusing to post "
            "against a drifted head"
        )
        if not isinstance(live_head, str) or not SHA_RE.fullmatch(live_head):
            fail(refusal)
        try:
            drift_failures = forward_map_placements(plan, live_head)
        except PublishError as error:
            fail(f"{refusal} ({error})")
        posting_head = live_head

    # Token identity (R7). The bot_login input names it directly when
    # set; otherwise a PAT answers /user, while a GitHub App installation
    # token gets 403 there and its login is learned from apply's own
    # first write (the anchor response) below.
    login: Optional[str] = (args.bot_login or "").strip() or None
    if login is None:
        status, user = client.request("GET", "/user")
        if status == 200 and isinstance(user, dict) and user.get("login"):
            login = str(user["login"])
        elif status == 401:
            fail("GitHub rejected the token (HTTP 401)")

    # Reconcile against comments already carrying this review's tags (R14).
    already_posted = extract_posted_ids(
        client.list_all(f"/repos/{repo}/pulls/{pr}/comments"), review_id
    )

    # Sticky-summary discovery (R7): only a marker comment authored by our
    # own token identity is ever updated; the newest owned one wins. While
    # the login is still unknown, no comment counts as owned.
    def find_owned_summary(
        comments: Sequence[Mapping[str, Any]],
    ) -> Optional[Dict[str, Any]]:
        if login is None:
            return None
        owned = [
            comment
            for comment in comments
            if SUMMARY_MARKER in str(comment.get("body") or "")
            and (comment.get("user") or {}).get("login") == login
            and isinstance(comment.get("id"), int)
        ]
        if not owned:
            return None
        return max(owned, key=lambda comment: comment["id"])

    prior_comments = client.list_all(f"/repos/{repo}/issues/{pr}/comments")
    summary_comment_id: Optional[int] = None
    summary_url = ""
    stale_skip = False
    anchor_failed = False

    # Take over an owned summary comment as the one to update -- unless it
    # carries a newer review's completed tag: the stale-run guard (R14)
    # never overwrites a newer review's summary.
    def adopt(existing: Mapping[str, Any]) -> None:
        nonlocal stale_skip, summary_comment_id, summary_url
        summary_url = str(existing.get("html_url") or "")
        stamp = completed_stamp(str(existing.get("body") or ""))
        if is_newer_stamp(stamp, summary["completed_at"]):
            stale_skip = True
            summary_comment_id = None
        else:
            summary_comment_id = existing["id"]

    existing = find_owned_summary(prior_comments)
    if existing is not None:
        adopt(existing)

    # Anchor before review on a cold start (R8). A failed anchor does not
    # abort the run; the final summary write settles the outcome (R16).
    # With an unknown login the anchor doubles as the identity probe: its
    # response names our author, and if an older owned sticky comment then
    # turns out to exist, the probe is deleted so that comment stays the
    # one summary (R7); if the delete fails, the probe is the newest owned
    # comment and later runs converge on it.
    if not stale_skip and summary_comment_id is None:
        status, created = client.request(
            "POST",
            f"/repos/{repo}/issues/{pr}/comments",
            {"body": summary["anchor_body"]},
        )
        if status in (200, 201) and isinstance(created, dict) and isinstance(
            created.get("id"), int
        ):
            summary_comment_id = created["id"]
            summary_url = str(created.get("html_url") or "")
            if login is None:
                login = (created.get("user") or {}).get("login")
                prior = find_owned_summary(prior_comments)
                if prior is not None:
                    delete_status, _ = client.request(
                        "DELETE",
                        f"/repos/{repo}/issues/comments/{summary_comment_id}",
                    )
                    deleted = delete_status in (200, 204)
                    if deleted or is_newer_stamp(
                        completed_stamp(str(prior.get("body") or "")),
                        summary["completed_at"],
                    ):
                        adopt(prior)
        else:
            anchor_failed = True
            print(
                f"warning: could not create the summary anchor (HTTP {status})",
                file=sys.stderr,
            )

    poster = BatchPoster(
        client, repo, pr, plan, already_posted, commit_id=posting_head
    )
    # A drift-unmapped span is failed before any write: it never enters a
    # posted batch and lands in the summary with its reason (R15).
    for finding_id, detail in drift_failures.items():
        if finding_id not in poster.posted:
            poster.failed[finding_id] = detail
    for batch in plan["batches"]:
        poster.post_batch(batch)

    inline_entries = [
        entry for entry in plan["placements"] if entry["placement"] == "inline"
    ]
    posted_inline = sum(
        1 for entry in inline_entries if entry["finding_id"] in poster.posted
    )
    outcome_counts = {
        "total": plan["counts"]["total"],
        "posted_inline": posted_inline,
        "failed_inline": len(poster.failed),
        "no_position": plan["counts"]["no_position"],
        "routed": plan["counts"]["routed"],
        "skipped": plan["counts"]["skipped"],
    }

    # Final summary: planned sections plus every failed finding with its
    # reason (R15), re-assembled under the same budget (R19).
    summary_failed = False
    if not stale_skip:
        sections = [
            entry["body"]
            for entry in plan["placements"]
            if entry["placement"] == "summary"
        ]
        for entry in inline_entries:
            detail = poster.failed.get(entry["finding_id"])
            if detail is None:
                continue
            section_text = entry.get("section") or strip_identity_tag(
                entry["body"]
            )
            sections.append(
                f"_This finding could not be posted inline: {detail}._"
                + "\n\n"
                + section_text
            )
        final_body = assemble_summary_body(
            SUMMARY_MARKER,
            summary["run_tag"],
            summary["completed_tag"],
            counts_line(
                outcome_counts["total"],
                outcome_counts["posted_inline"],
                outcome_counts["no_position"],
                outcome_counts["routed"],
                outcome_counts["failed_inline"],
                outcome_counts["skipped"],
            ),
            summary["context_lines"],
            sections,
            review_id,
            plan["config"].get("run_url") or "",
            identity_tags=(summary["head_tag"], summary["base_tag"]),
        )
        if summary_comment_id is None and anchor_failed and login is not None:
            # The anchor write may have landed despite its error; re-read
            # before choosing create over update (R14).
            try:
                landed = find_owned_summary(
                    client.list_all(f"/repos/{repo}/issues/{pr}/comments")
                )
            except PublishError:
                landed = None
            if landed is not None:
                summary_comment_id = landed["id"]
                summary_url = str(landed.get("html_url") or "")
        if summary_comment_id is not None:
            status, updated = client.request(
                "PATCH",
                f"/repos/{repo}/issues/comments/{summary_comment_id}",
                {"body": final_body},
            )
        else:
            status, updated = client.request(
                "POST",
                f"/repos/{repo}/issues/{pr}/comments",
                {"body": final_body},
            )
        if status in (200, 201) and isinstance(updated, dict):
            summary_url = str(updated.get("html_url") or summary_url)
        else:
            summary_failed = True
            summary_url = ""
            print(
                f"error: the summary could not be written (HTTP {status}); "
                "posted inline comments are kept",
                file=sys.stderr,
            )

    outcome: Dict[str, Any] = {
        "review_id": review_id,
        "counts": outcome_counts,
        "summary_url": summary_url,
        "batches": {
            "total": poster.batches_attempted,
            "succeeded": poster.batches_succeeded,
        },
        "failures": [
            {"finding_id": finding_id, "detail": detail}
            for finding_id, detail in sorted(poster.failed.items())
        ],
        # Skip telemetry (P3): which earlier comment each finding
        # deferred to, replayable from the plan artifact.
        "skipped": [
            {
                "finding_id": entry["finding_id"],
                "reason": entry["reason"],
                "detail": entry["detail"],
            }
            for entry in plan["placements"]
            if entry["placement"] == "skipped"
        ],
    }
    if posting_head != plan["head"]:
        outcome["head_drift"] = {
            "plan_head": plan["head"],
            "posted_against": posting_head,
            "unmapped": sorted(drift_failures),
        }
    if plan["config"].get("history_unavailable"):
        outcome["history_unavailable"] = plan["config"][
            "history_unavailable"
        ]
    if stale_skip:
        outcome["summary_skipped"] = (
            "a newer review's summary is already posted"
        )
    if summary_failed:
        outcome["summary_error"] = "the summary comment could not be written"
    outcome_path = Path(args.outcome)
    temporary = outcome_path.with_name(outcome_path.name + ".tmp")
    temporary.write_text(
        json.dumps(outcome, ensure_ascii=True, indent=2, sort_keys=True)
        + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, outcome_path)
    print(
        f"Applied: {posted_inline} inline comment(s) posted, "
        f"{len(poster.failed)} failed; summary "
        + (
            "skipped (newer review present)"
            if stale_skip
            else ("FAILED" if summary_failed else "written")
        )
    )
    return 1 if summary_failed else 0


# --- Entry point -------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="publish_pr.py",
        description="Deterministic PR publisher (plan, then apply)",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    plan = commands.add_parser("plan", help="compute a publication plan")
    plan.add_argument("--evidence-dir", required=True)
    plan.add_argument("--repo", required=True)
    plan.add_argument("--pr", required=True, type=int)
    plan.add_argument("--route-severity-below", default="")
    plan.add_argument("--route-categories", default="")
    plan.add_argument("--batch-size", default=str(DEFAULT_BATCH_SIZE))
    plan.add_argument("--run-url", default="")
    plan.add_argument("--incremental", default="")
    plan.add_argument("--incremental-overlap-threshold", default="")
    plan.add_argument("--history", default="")
    plan.add_argument("--output", required=True)
    plan.set_defaults(handler=command_plan)

    history = commands.add_parser(
        "history",
        help="fetch this PR's prior-review state into pr-history.json",
    )
    history.add_argument("--repo", required=True)
    history.add_argument("--pr", required=True, type=int)
    history.add_argument("--api-base", default="https://api.github.com")
    history.add_argument("--bot-login", default="")
    history.add_argument("--output", required=True)
    history.set_defaults(handler=command_history)

    apply_ = commands.add_parser("apply", help="execute a publication plan")
    apply_.add_argument("--plan", required=True)
    apply_.add_argument("--repo", required=True)
    apply_.add_argument("--pr", required=True, type=int)
    apply_.add_argument("--api-base", required=True)
    apply_.add_argument("--bot-login", default="")
    apply_.add_argument("--outcome", required=True)
    apply_.set_defaults(handler=command_apply)
    return parser


def main(argv: Sequence[str]) -> int:
    args = build_parser().parse_args(argv)
    try:
        return int(args.handler(args))
    except renderer.RenderError as error:
        print(f"publish_pr.py: invalid bundle: {error}", file=sys.stderr)
        return 2
    except PublishError as error:
        print(f"publish_pr.py: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
