#!/usr/bin/env python3
"""Deterministic report renderer for the Fabro code-review workflow.

Validates the canonical bundle written by `code_review.py final-tally` and
derives every presentation artifact from it: the Markdown, HTML, JSONL, and
SARIF reports plus `metadata/revision.json`. No model output reaches a report
without passing this program's checks, and no finding text is ever placed
into markup -- the HTML report receives one escaped JSON payload and renders
it with `textContent`.

Python 3.9-compatible. Standard library only.
"""

from __future__ import annotations

import json
import hashlib
import os
import re
import sys
from pathlib import Path, PurePosixPath
from typing import Any, Dict, List, Mapping, NoReturn, Optional, Sequence, Tuple
from urllib.parse import quote

sys.path.insert(0, str(Path(__file__).resolve().parent))

from review_contract import (  # noqa: E402
    CATEGORIES,
    COMPILED_RULE_ID_RE,
    EFFORT_TIERS,
    FINDING_ID_RE,
    ISSUE_TYPES,
    MAX_RULE_IDS_PER_FINDING,
    REVIEW_MODES,
)


CANONICAL_SCHEMA_VERSION = 4
TEMPLATE_RELATIVE_PATH = ("..", "templates", "report.html")
PAYLOAD_PLACEHOLDER = "__CODE_REVIEW_PAYLOAD__"

SEVERITIES = ("HIGH", "MEDIUM", "LOW")
FINDING_VERDICTS = ("CONFIRMED", "PLAUSIBLE", "UNVERIFIED")
VOTE_VERDICTS = ("CONFIRMED", "PLAUSIBLE", "REFUTED")
DISPOSITIONS = (
    "reportable",
    "refuted",
    "verification-incomplete",
    "deferred-by-cap",
    "duplicate",
    # P3 item 5: kept in the bundle, but the verifier judged it the same
    # defect as a finding already posted on the reviewed PR; the
    # publisher places it as skipped instead of re-posting.
    "duplicate-of-posted",
)
VERIFICATION_STATUSES = ("complete", "partial", "skipped-low-effort")
COMPLETION_STATUSES = ("complete", "partial")
MAX_TEXT = 8000
MAX_LOCATION_LINES = 50
UNVERIFIED_FINDING_NOTE = (
    "This finding comes from a low-effort single-pass review and was not "
    "independently verified."
)
UNVERIFIED_FINDING_ITALIC = f"_{UNVERIFIED_FINDING_NOTE}_"
UNVERIFIED_FINDING_WARNING = (
    "> **Not verified:** " + UNVERIFIED_FINDING_NOTE
)
LOW_EFFORT_REVIEW_NOTE = (
    "This was a low-effort single-pass review: findings were not "
    "independently verified."
)


class RenderError(RuntimeError):
    """A canonical-bundle or rendering failure."""


def die(message: str) -> NoReturn:
    raise RenderError(message)


# --- Validation helpers ------------------------------------------------------


def as_map(value: object) -> Dict[str, Any]:
    if not isinstance(value, dict):
        die("expected a JSON object")
    return value


def safe_text(value: object, field: str, allow_empty: bool = True) -> str:
    if not isinstance(value, str):
        die(f"{field} must be a string")
    if len(value) > MAX_TEXT:
        die(f"{field} exceeds the {MAX_TEXT}-character limit")
    if any(
        character not in "\n\t" and ord(character) < 0x20
        for character in value
    ):
        die(f"{field} contains control characters")
    if not allow_empty and not value.strip():
        die(f"{field} is empty")
    return value


def non_negative_int(value: object, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        die(f"{field} must be a non-negative integer")
    return value


def positive_int(value: object, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 1:
        die(f"{field} must be a positive integer")
    return value


def safe_repo_path(value: object, field: str) -> str:
    text = safe_text(value, field, allow_empty=False)
    if "\n" in text or "\t" in text:
        die(f"{field} contains control characters")
    path = PurePosixPath(text.replace("\\", "/"))
    if path.is_absolute() or ".." in path.parts:
        die(f"{field} is not a safe repository path")
    return path.as_posix()


def read_json(directory: str, name: str) -> Any:
    path = Path(directory) / name
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        die(f"canonical file is missing: {path}")
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        die(f"could not read {path}: {error}")


def read_jsonl(directory: str, name: str) -> List[Dict[str, Any]]:
    path = Path(directory) / name
    try:
        raw = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        die(f"canonical file is missing: {path}")
    except (OSError, UnicodeError) as error:
        die(f"could not read {path}: {error}")
    records: List[Dict[str, Any]] = []
    for number, line in enumerate(raw.splitlines(), 1):
        if not line.strip():
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError as error:
            die(f"{path}:{number} is not valid JSON: {error}")
        if not isinstance(value, dict):
            die(f"{path}:{number} must be a JSON object")
        records.append(value)
    return records


# --- Canonical-bundle validation ---------------------------------------------


def validate_manifest(value: object) -> Dict[str, Any]:
    manifest = as_map(value)
    if manifest.get("schema_version") != CANONICAL_SCHEMA_VERSION:
        die("review-manifest.json has an unsupported schema_version")
    safe_text(manifest.get("review_id"), "manifest review_id", allow_empty=False)
    if manifest.get("mode") not in REVIEW_MODES:
        die("manifest mode is not a known review mode")
    if manifest.get("effort") not in EFFORT_TIERS:
        die("manifest effort is not a known tier")
    if manifest.get("guidance") is not None:
        safe_text(manifest.get("guidance"), "manifest guidance")
    counts = as_map(manifest.get("counts"))
    for field in ("raw", "deduplicated", "sweep", "kept", "reported"):
        non_negative_int(counts.get(field), f"manifest counts.{field}")
    if "duplicates" in counts:
        non_negative_int(counts.get("duplicates"), "manifest counts.duplicates")
    completion = as_map(manifest.get("completion"))
    if completion.get("status") not in COMPLETION_STATUSES:
        die("manifest completion.status is invalid")
    verification = as_map(manifest.get("verification"))
    if verification.get("status") not in VERIFICATION_STATUSES:
        die("manifest verification.status is invalid")
    incremental = manifest.get("incremental")
    if incremental is not None:
        incremental = as_map(incremental)
        delta_from = incremental.get("from")
        if delta_from is not None and (
            not isinstance(delta_from, str)
            or not re.fullmatch(r"[0-9a-f]{40}", delta_from)
        ):
            die("manifest incremental.from must be a commit SHA or null")
    rules = manifest.get("rules")
    if rules is not None:
        rules = as_map(rules)
        for field in ("configSha256", "builtinManifestSha256"):
            value = rules.get(field)
            if not isinstance(value, str) or not re.fullmatch(
                r"[0-9a-f]{64}", value
            ):
                die(f"manifest rules.{field} must be a SHA-256 hex digest")
        counts = as_map(rules.get("counts"))
        for field in (
            "builtin_packs",
            "repo_packs",
            "builtin_checks",
            "repo_checks",
        ):
            non_negative_int(counts.get(field), f"manifest rules.counts.{field}")
    return manifest


def validate_code(value: object, field: str) -> Dict[str, Any]:
    code = as_map(value)
    safe_text(code.get("language"), f"{field}.language", allow_empty=False)
    safe_text(code.get("label"), f"{field}.label", allow_empty=False)
    lines = code.get("lines")
    if not isinstance(lines, list):
        die(f"{field}.lines must be an array")
    highlighted = 0
    normalized: List[Dict[str, Any]] = []
    for index, entry in enumerate(lines):
        record = as_map(entry)
        number = positive_int(record.get("number"), f"{field}.lines[{index}]")
        text = safe_text(record.get("text"), f"{field}.lines[{index}].text")
        if "\n" in text:
            die(f"{field}.lines[{index}].text spans lines")
        line: Dict[str, Any] = {"number": number, "text": text}
        if record.get("highlight"):
            line["highlight"] = True
            highlighted += 1
        normalized.append(line)
    if normalized and highlighted < 1:
        die(f"{field} must highlight at least one line")
    return {
        "language": code["language"],
        "label": code["label"],
        "lines": normalized,
    }


def validate_finding(value: object, index: int) -> Dict[str, Any]:
    field = f"findings[{index}]"
    finding = as_map(value)
    display_id = safe_text(finding.get("id"), f"{field}.id", allow_empty=False)
    if not FINDING_ID_RE.fullmatch(display_id):
        die(f"{field}.id is not a valid display ID")
    path = safe_repo_path(finding.get("file"), f"{field}.file")
    line = positive_int(finding.get("line"), f"{field}.line")
    if finding.get("category") not in CATEGORIES:
        die(f"{field}.category is not in the closed list")
    if finding.get("issue_type") not in ISSUE_TYPES:
        die(f"{field}.issue_type is not in the closed list")
    if finding.get("severity") not in SEVERITIES:
        die(f"{field}.severity is invalid")
    if finding.get("confidence") not in SEVERITIES:
        die(f"{field}.confidence is invalid")
    if finding.get("verdict") not in FINDING_VERDICTS:
        die(f"{field}.verdict is invalid")
    location = as_map(finding.get("location"))
    start_line = positive_int(
        location.get("start_line"), f"{field}.location.start_line"
    )
    end_line = positive_int(
        location.get("end_line"), f"{field}.location.end_line"
    )
    if start_line > end_line:
        die(f"{field}.location starts after it ends")
    if end_line - start_line + 1 > MAX_LOCATION_LINES:
        die(f"{field}.location spans more than {MAX_LOCATION_LINES} lines")
    if end_line != line:
        die(f"{field}.line must equal location.end_line")
    normalized_location = {
        "start_line": start_line,
        "end_line": end_line,
        "existing_code": safe_text(
            location.get("existing_code"),
            f"{field}.location.existing_code",
        ),
    }
    suggestion = finding.get("suggestion")
    normalized_suggestion: Optional[Dict[str, str]] = None
    if suggestion is not None:
        suggestion_record = as_map(suggestion)
        replacement = safe_text(
            suggestion_record.get("replacement_code"),
            f"{field}.suggestion.replacement_code",
            allow_empty=False,
        )
        if not normalized_location["existing_code"]:
            die(f"{field}.suggestion has no exact existing code anchor")
        if replacement == normalized_location["existing_code"]:
            die(f"{field}.suggestion does not change the anchored code")
        normalized_suggestion = {"replacement_code": replacement}
    reporters = finding.get("reporters")
    if not isinstance(reporters, list) or not all(
        isinstance(item, str) for item in reporters
    ):
        die(f"{field}.reporters must be an array of strings")
    rule_ids = finding.get("rule_ids", [])
    if not isinstance(rule_ids, list) or len(rule_ids) > (
        MAX_RULE_IDS_PER_FINDING
    ):
        die(f"{field}.rule_ids must be a bounded array")
    for rule_id in rule_ids:
        if not isinstance(rule_id, str) or not COMPILED_RULE_ID_RE.fullmatch(
            rule_id
        ):
            die(f"{field}.rule_ids contains an invalid compiled check ID")
    if len(set(rule_ids)) != len(rule_ids):
        die(f"{field}.rule_ids repeats a check ID")
    anchors = finding.get("anchors", [])
    if not isinstance(anchors, list) or len(anchors) > MAX_RULE_IDS_PER_FINDING:
        die(f"{field}.anchors must be a bounded array")
    normalized_anchors: List[Dict[str, Any]] = []
    for index, anchor in enumerate(anchors):
        record = as_map(anchor)
        anchor_field = f"{field}.anchors[{index}]"
        if record.get("category") not in CATEGORIES:
            die(f"{anchor_field}.category is not in the closed list")
        normalized_anchors.append(
            {
                "id": safe_text(record.get("id"), f"{anchor_field}.id", allow_empty=False),
                "file": safe_repo_path(record.get("file"), f"{anchor_field}.file"),
                "line": positive_int(record.get("line"), f"{anchor_field}.line"),
                "category": record["category"],
            }
        )
    normalized_code = validate_code(finding.get("code"), f"{field}.code")
    if normalized_code["lines"]:
        highlighted = {
            entry["number"]
            for entry in normalized_code["lines"]
            if entry.get("highlight")
        }
        expected = set(range(start_line, end_line + 1))
        if highlighted != expected:
            die(f"{field}.code highlights do not match its location")
    normalized = {
        "id": display_id,
        "file": path,
        "line": line,
        "location": normalized_location,
        "summary": safe_text(
            finding.get("summary"), f"{field}.summary", allow_empty=False
        ),
        "short_summary": safe_text(
            finding.get("short_summary"),
            f"{field}.short_summary",
            allow_empty=False,
        ),
        "failure_scenario": safe_text(
            finding.get("failure_scenario"),
            f"{field}.failure_scenario",
            allow_empty=False,
        ),
        "category": finding["category"],
        "issue_type": finding["issue_type"],
        "severity": finding["severity"],
        "confidence": finding["confidence"],
        "reports": positive_int(finding.get("reports"), f"{field}.reports"),
        "reporters": [safe_text(item, f"{field}.reporters") for item in reporters],
        "rule_ids": list(rule_ids),
        "anchors": normalized_anchors,
        "source": safe_text(finding.get("source"), f"{field}.source"),
        "verdict": finding["verdict"],
        "verdict_reasoning": safe_text(
            finding.get("verdict_reasoning"), f"{field}.verdict_reasoning"
        ),
        "code": normalized_code,
    }
    if normalized_suggestion is not None:
        normalized["suggestion"] = normalized_suggestion
    duplicate_of_posted = finding.get("duplicate_of_posted")
    if duplicate_of_posted is not None:
        record = as_map(duplicate_of_posted)
        normalized["duplicate_of_posted"] = {
            "comment_id": positive_int(
                record.get("comment_id"),
                f"{field}.duplicate_of_posted.comment_id",
            ),
            "html_url": safe_text(
                record.get("html_url", ""),
                f"{field}.duplicate_of_posted.html_url",
            ),
        }
    return normalized


def validate_findings(value: object) -> List[Dict[str, Any]]:
    if not isinstance(value, list):
        die("findings.json must contain a JSON array")
    findings = [
        validate_finding(entry, index) for index, entry in enumerate(value)
    ]
    seen_ids = {finding["id"] for finding in findings}
    if len(seen_ids) != len(findings):
        die("findings.json repeats a display ID")
    return findings


def finding_key(record: Mapping[str, Any]) -> Tuple[str, int, str]:
    return (
        str(record.get("file")),
        int(record.get("line") or 0),
        str(record.get("category")),
    )


def validate_ledger(records: Sequence[Mapping[str, Any]]) -> List[Dict[str, Any]]:
    validated: List[Dict[str, Any]] = []
    for index, value in enumerate(records):
        field = f"ledger[{index}]"
        record = as_map(dict(value))
        safe_text(record.get("id"), f"{field}.id", allow_empty=False)
        safe_repo_path(record.get("file"), f"{field}.file")
        positive_int(record.get("line"), f"{field}.line")
        if record.get("category") not in CATEGORIES:
            die(f"{field}.category is not in the closed list")
        if record.get("issue_type") not in ISSUE_TYPES:
            die(f"{field}.issue_type is not in the closed list")
        if record.get("disposition") not in DISPOSITIONS:
            die(f"{field}.disposition is invalid")
        validated.append(dict(record))
    return validated


def validate_votes(records: Sequence[Mapping[str, Any]]) -> List[Dict[str, Any]]:
    validated: List[Dict[str, Any]] = []
    for index, value in enumerate(records):
        field = f"votes[{index}]"
        record = as_map(dict(value))
        completed = record.get("completed")
        if not isinstance(completed, bool):
            die(f"{field}.completed must be a boolean")
        if completed:
            if record.get("verdict") not in VOTE_VERDICTS:
                die(f"{field}.verdict is invalid")
            safe_text(record.get("reasoning"), f"{field}.reasoning")
            if "suggestion_valid" in record and not isinstance(
                record.get("suggestion_valid"), bool
            ):
                die(f"{field}.suggestion_valid must be a boolean")
        validated.append(dict(record))
    return validated


def validate_coverage(value: object) -> Dict[str, Any]:
    coverage = as_map(value)
    finders = as_map(coverage.get("finders"))
    non_negative_int(finders.get("dispatched"), "coverage.finders.dispatched")
    non_negative_int(finders.get("returned"), "coverage.finders.returned")
    verification = as_map(coverage.get("verification"))
    if verification.get("status") not in VERIFICATION_STATUSES:
        die("coverage.verification.status is invalid")
    non_negative_int(
        verification.get("votesDispatched"),
        "coverage.verification.votesDispatched",
    )
    non_negative_int(
        verification.get("votesCompleted"),
        "coverage.verification.votesCompleted",
    )
    rejected = coverage.get("rejectedFindingReports")
    if not isinstance(rejected, list) or not all(
        isinstance(item, str) for item in rejected
    ):
        die("coverage.rejectedFindingReports must be an array of strings")
    filtered = coverage.get("filteredFindingReports", [])
    if not isinstance(filtered, list) or not all(
        isinstance(item, str) for item in filtered
    ):
        die("coverage.filteredFindingReports must be an array of strings")
    rules = coverage.get("rules")
    if rules is not None:
        rules = as_map(rules)
        effective = as_map(rules.get("effectiveChecksByFile"))
        for path, check_ids in effective.items():
            safe_repo_path(path, "coverage.rules.effectiveChecksByFile key")
            if not isinstance(check_ids, list) or not all(
                isinstance(check_id, str)
                and COMPILED_RULE_ID_RE.fullmatch(check_id)
                for check_id in check_ids
            ):
                die(
                    "coverage.rules.effectiveChecksByFile values must be "
                    "arrays of compiled check IDs"
                )
        catalog = rules.get("checkCatalog")
        if catalog is not None:
            catalog = as_map(catalog)
            for check_id, entry in catalog.items():
                if not isinstance(
                    check_id, str
                ) or not COMPILED_RULE_ID_RE.fullmatch(check_id):
                    die(
                        "coverage.rules.checkCatalog keys must be compiled "
                        "check IDs"
                    )
                record = as_map(entry)
                if record.get("category") not in CATEGORIES:
                    die(
                        f"coverage.rules.checkCatalog[{check_id}].category "
                        "is not in the closed list"
                    )
                safe_text(
                    record.get("guidance"),
                    f"coverage.rules.checkCatalog[{check_id}].guidance",
                    allow_empty=False,
                )
            for check_ids in effective.values():
                for check_id in check_ids:
                    if check_id not in catalog:
                        die(
                            "coverage.rules.checkCatalog is missing an "
                            "effective check"
                        )
    return coverage


def validate_relationships(
    manifest: Mapping[str, Any],
    findings: Sequence[Mapping[str, Any]],
    ledger: Sequence[Mapping[str, Any]],
    votes: Sequence[Mapping[str, Any]],
    coverage: Mapping[str, Any],
) -> None:
    counts = manifest["counts"]
    if counts["reported"] != len(findings):
        die("manifest counts.reported does not match findings.json")
    # A duplicate-of-posted finding stays in findings.json (the bundle
    # never drops a finding); its ledger disposition names the earlier
    # comment it defers to.
    reportable = [
        record
        for record in ledger
        if record["disposition"] in ("reportable", "duplicate-of-posted")
    ]
    if len(reportable) != len(findings):
        die("reportable ledger records do not match findings.json")
    ledger_keys = {finding_key(record) for record in reportable}
    finding_keys = {finding_key(finding) for finding in findings}
    if ledger_keys != finding_keys:
        die("reportable ledger records and findings.json disagree")
    verification = as_map(coverage.get("verification"))
    if verification["status"] != manifest["verification"]["status"]:
        die("coverage and manifest verification statuses disagree")
    if verification["votesDispatched"] != len(votes):
        die("coverage vote counts do not match votes.jsonl")
    completed = sum(1 for vote in votes if vote.get("completed"))
    if verification["votesCompleted"] != completed:
        die("coverage completed-vote count does not match votes.jsonl")
    if manifest["effort"] == "low":
        if any(finding["verdict"] != "UNVERIFIED" for finding in findings):
            die("a low-effort review cannot carry verified verdicts")
    else:
        if any(finding["verdict"] == "UNVERIFIED" for finding in findings):
            die("a verified tier cannot report an UNVERIFIED finding")
    coverage_rules = coverage.get("rules")
    manifest_rules = manifest.get("rules")
    if (coverage_rules is None) != (manifest_rules is None):
        die("manifest and coverage disagree about rule compilation")
    if coverage_rules is not None:
        if as_map(coverage_rules).get("configSha256") != as_map(
            manifest_rules
        ).get("configSha256"):
            die("manifest and coverage rule configuration hashes disagree")
        effective = as_map(as_map(coverage_rules).get("effectiveChecksByFile"))
        for finding in findings:
            allowed = effective.get(finding["file"]) or []
            for rule_id in finding.get("rule_ids") or []:
                if rule_id not in allowed:
                    die(
                        "a reported finding names a rule check that is not "
                        "effective for its file"
                    )
    else:
        for finding in findings:
            if finding.get("rule_ids"):
                die(
                    "a reported finding names rule checks but the review "
                    "compiled no rules"
                )


def partial_reasons(
    manifest: Mapping[str, Any],
    coverage: Mapping[str, Any],
) -> List[str]:
    # A completed review carries no partial banner. Report-cap deferral at
    # the rule-mapped tiers is a completed policy selection: it stays
    # visible in the coverage section and the ledger, not here.
    if manifest["completion"]["status"] == "complete":
        return []
    reasons: List[str] = []
    finders = coverage["finders"]
    missing = finders["dispatched"] - finders["returned"]
    if missing > 0:
        reasons.append(
            f"{missing} finder angle(s) returned no usable result"
        )
    verification = coverage["verification"]
    incomplete = verification.get("incomplete")
    if isinstance(incomplete, int) and incomplete > 0:
        reasons.append(
            f"{incomplete} candidate(s) have no verdict and were not reported"
        )
    rejected = coverage.get("rejectedFindingReports") or []
    if rejected:
        reasons.append(
            f"{len(rejected)} reported finding(s) failed the finding contract"
        )
    sweep = coverage.get("sweep")
    if isinstance(sweep, dict) and sweep.get("planned") and not sweep.get(
        "returned"
    ):
        reasons.append("the planned gap-fill sweep returned no usable result")
    caps = coverage.get("caps")
    if isinstance(caps, dict):
        deferred = caps.get("verificationDeferred")
        if isinstance(deferred, int) and deferred > 0:
            reasons.append(
                f"{deferred} candidate(s) were deferred by the verification cap"
            )
        report_deferred = caps.get("reportDeferred")
        if isinstance(report_deferred, int) and report_deferred > 0:
            reasons.append(
                f"{report_deferred} kept finding(s) were cut by the report cap"
            )
    if manifest["completion"]["status"] == "partial" and not reasons:
        reasons.append("the review recorded a partial completion")
    return reasons


# --- Markdown rendering ------------------------------------------------------


MARKDOWN_ESCAPES = str.maketrans(
    {
        "\\": "\\\\",
        "`": "\\`",
        "*": "\\*",
        "_": "\\_",
        "[": "\\[",
        "]": "\\]",
        "<": "&lt;",
        ">": "&gt;",
        "|": "\\|",
        "#": "\\#",
        "~": "\\~",
    }
)


def escape_markdown(value: object) -> str:
    return str("" if value is None else value).translate(MARKDOWN_ESCAPES)


def code_span(value: object) -> str:
    text = str("" if value is None else value).replace("`", "'")
    return f"`{text}`"


def partial_review_warning(reasons: Sequence[str]) -> str:
    return "> **Partial review.** " + " ".join(
        escape_markdown(reason) + "." for reason in reasons
    )


def backtick_fence(texts: Sequence[str]) -> str:
    longest = 0
    for text in texts:
        for run in re.findall(r"`+", text):
            longest = max(longest, len(run))
    return "`" * max(4, longest + 1)


def code_block(code: Mapping[str, Any]) -> List[str]:
    lines = code.get("lines") or []
    if not lines:
        return []
    fence = backtick_fence([str(entry["text"]) for entry in lines])
    body: List[str] = [fence + "text"]
    width = max(len(str(entry["number"])) for entry in lines)
    for entry in lines:
        marker = ">" if entry.get("highlight") else " "
        body.append(
            f"{marker} {str(entry['number']).rjust(width)} | {entry['text']}"
        )
    body.append(fence)
    return body


def fenced_text(value: str, language: str = "text") -> List[str]:
    fence = backtick_fence([value])
    return [fence + language, value, fence]


def describe_target(manifest: Mapping[str, Any]) -> str:
    mode = str(manifest.get("mode"))
    if mode == "files":
        scope = manifest.get("scope") or []
        return f"files mode, {len(scope)} scope path(s)"
    range_text = manifest.get("range") or "(unknown range)"
    return f"{mode} mode, range {range_text}"


def revision_summary(manifest: Mapping[str, Any]) -> str:
    revision = manifest.get("revision")
    if not isinstance(revision, dict) or not revision.get("versioned"):
        return "unversioned tree"
    commit = str(revision.get("commit") or "")[:12] or "(unknown)"
    branch = revision.get("branch")
    return f"commit {commit}" + (f" on {branch}" if branch else "")


def finding_markdown(finding: Mapping[str, Any]) -> List[str]:
    location_data = finding["location"]
    start_line = location_data["start_line"]
    end_line = location_data["end_line"]
    location = (
        f"{finding['file']}:{start_line}"
        if start_line == end_line
        else f"{finding['file']}:{start_line}-{end_line}"
    )
    rule_ids = finding.get("rule_ids") or []
    lines = [
        f"### {finding['id']} · {finding['severity']} "
        f"{finding['issue_type']} / {finding['category']} — "
        f"{escape_markdown(finding['short_summary'])}",
        "",
        f"{code_span(location)} · verdict {finding['verdict']} · "
        f"confidence {finding['confidence']} · reported by "
        f"{finding['reports']} pass(es) "
        f"({escape_markdown(', '.join(finding['reporters']))})"
        + (
            " · rule " + ", ".join(code_span(item) for item in rule_ids)
            if rule_ids
            else ""
        ),
    ]
    anchors = finding.get("anchors") or []
    if anchors:
        lines.append(
            "Also reported at "
            + ", ".join(
                f"{code_span(anchor['file'] + ':' + str(anchor['line']))} "
                f"({anchor['category']}, {escape_markdown(anchor['id'])})"
                for anchor in anchors
            )
            + " -- judged the same defect and folded in."
        )
    duplicate_of_posted = finding.get("duplicate_of_posted")
    if isinstance(duplicate_of_posted, dict):
        url = str(duplicate_of_posted.get("html_url") or "").strip()
        lines.append(
            "Already reported on the pull request"
            + (f": {url}" if url else "")
            + " -- not re-posted."
        )
    if finding["summary"].strip() != finding["short_summary"].strip():
        lines.extend(["", escape_markdown(finding["summary"])])
    lines.extend(
        ["", f"**Failure scenario.** {escape_markdown(finding['failure_scenario'])}"]
    )
    reasoning = str(finding.get("verdict_reasoning") or "").strip()
    if reasoning:
        lines.extend(["", f"**Verifier.** {escape_markdown(reasoning)}"])
    suggestion = finding.get("suggestion")
    if isinstance(suggestion, dict):
        lines.extend(
            [
                "",
                "<details><summary>Suggested change</summary>",
                "",
                "**Before:**",
                *fenced_text(location_data["existing_code"]),
                "",
                "**After:**",
                *fenced_text(suggestion["replacement_code"]),
                "",
                "</details>",
            ]
        )
    excerpt = code_block(finding["code"])
    if excerpt:
        lines.extend(["", *excerpt])
    lines.append("")
    return lines


def render_markdown(
    manifest: Mapping[str, Any],
    findings: Sequence[Mapping[str, Any]],
    coverage: Mapping[str, Any],
    reasons: Sequence[str],
) -> str:
    correctness = sum(
        1 for finding in findings if finding["category"] == "correctness"
    )
    cleanup = len(findings) - correctness
    lines: List[str] = [
        "# Code review results",
        "",
        f"- **Target:** {escape_markdown(describe_target(manifest))}",
        f"- **Revision:** {escape_markdown(revision_summary(manifest))}",
        f"- **Effort:** {manifest['effort']}"
        + (
            f" · **Model:** {escape_markdown(manifest.get('model'))}"
            if manifest.get("model")
            else ""
        ),
        *(
            [f"- **Guidance:** {escape_markdown(manifest['guidance'])}"]
            if manifest.get("guidance")
            else []
        ),
        f"- **Completed:** {escape_markdown(manifest.get('completed_at'))}",
        f"- **Verification:** {manifest['verification']['status']} · "
        f"**Completion:** {manifest['completion']['status']}",
        "",
        f"**{len(findings)} finding(s) reported** "
        f"({correctness} correctness, {cleanup} cleanup).",
        "",
    ]
    if manifest["effort"] == "low":
        lines.extend([LOW_EFFORT_REVIEW_NOTE, ""])
    if reasons:
        lines.append(partial_review_warning(reasons))
        lines.append("")
    if findings:
        lines.append("## Findings")
        lines.append("")
        for finding in findings:
            lines.extend(finding_markdown(finding))
    else:
        lines.extend(["No findings survived review.", ""])
    finders = coverage["finders"]
    verification = coverage["verification"]
    lines.extend(
        [
            "## Coverage",
            "",
            f"- Finder jobs: {finders['returned']} of "
            f"{finders['dispatched']} returned a usable result.",
            f"- Verification: {verification['votesCompleted']} of "
            f"{verification['votesDispatched']} verdict(s) returned "
            f"({verification['status']}).",
        ]
    )
    rules = coverage.get("rules")
    if isinstance(rules, dict):
        effective = rules.get("effectiveChecksByFile")
        effective = effective if isinstance(effective, dict) else {}
        audited_files = sum(1 for ids in effective.values() if ids)
        distinct_checks = {
            check_id
            for ids in effective.values()
            if isinstance(ids, list)
            for check_id in ids
        }
        by_kind = (coverage.get("finders") or {}).get("byKind") or {}
        cells = by_kind.get("rule-audit")
        cells = cells if isinstance(cells, dict) else {}
        rule_findings = [
            finding for finding in findings if finding.get("rule_ids")
        ]
        filtered_count = len(coverage.get("filteredFindingReports") or [])
        folded = int((manifest.get("counts") or {}).get("duplicates") or 0)
        counts = rules.get("counts") or {}
        lines.append(
            f"- Rules: audited {len(distinct_checks)} check(s) "
            f"({counts.get('builtin_packs', 0)} built-in + "
            f"{counts.get('repo_packs', 0)} repository pack(s)) across "
            f"{audited_files} file(s) in {cells.get('returned', 0)} of "
            f"{cells.get('dispatched', 0)} audit cell(s); "
            f"{len(rule_findings)} violation(s) reported; "
            f"{filtered_count} filtered; {folded} folded."
        )
        if rule_findings:
            per_check: Dict[str, int] = {}
            for finding in rule_findings:
                for check_id in finding["rule_ids"]:
                    per_check[check_id] = per_check.get(check_id, 0) + 1
            lines.append("- Violations by check:")
            lines.extend(
                f"  - {code_span(check_id)} x{count}"
                for check_id, count in sorted(
                    per_check.items(), key=lambda item: (-item[1], item[0])
                )
            )
        failed_cells = rules.get("failedAuditCells") or []
        if failed_cells:
            lines.append(
                f"- Rule audits: {len(failed_cells)} cell(s) returned no "
                "usable result; their files and checks are recorded as "
                "uncovered."
            )
    caps = coverage.get("caps") or {}
    report_deferred = caps.get("reportDeferred")
    if isinstance(report_deferred, int) and report_deferred > 0:
        lines.append(
            f"- Report cap: {report_deferred} additional kept finding(s) "
            "are recorded in the candidate ledger."
        )
    rejected = coverage.get("rejectedFindingReports") or []
    if rejected:
        lines.append(
            f"- Rejected finding reports ({len(rejected)}):"
        )
        lines.extend(
            f"  - {escape_markdown(entry)}" for entry in rejected
        )
    filtered = coverage.get("filteredFindingReports") or []
    if filtered:
        lines.append(
            f"- Filtered by review policy ({len(filtered)}):"
        )
        lines.extend(
            f"  - {escape_markdown(entry)}" for entry in filtered
        )
    lines.append("")
    return "\n".join(lines)


# --- HTML and JSONL rendering ------------------------------------------------


def embed_json(value: object) -> str:
    text = json.dumps(value, ensure_ascii=True, separators=(",", ":"))
    return (
        text.replace("&", "\\u0026")
        .replace("<", "\\u003c")
        .replace(">", "\\u003e")
    )


def read_template() -> str:
    path = Path(__file__).resolve().parent.joinpath(*TEMPLATE_RELATIVE_PATH)
    try:
        template = path.read_text(encoding="utf-8")
    except OSError as error:
        die(f"could not read the HTML template: {error}")
    if template.count(PAYLOAD_PLACEHOLDER) != 1:
        die("the HTML template must contain the payload placeholder once")
    return template


def render_html(
    manifest: Mapping[str, Any],
    findings: Sequence[Mapping[str, Any]],
    coverage: Mapping[str, Any],
    reasons: Sequence[str],
) -> str:
    payload = {
        "meta": {
            "target": describe_target(manifest),
            "revision": revision_summary(manifest),
            "mode": manifest.get("mode"),
            "effort": manifest.get("effort"),
            "model": manifest.get("model"),
            "guidance": manifest.get("guidance") or "",
            "completed_at": manifest.get("completed_at"),
            "verification": manifest["verification"]["status"],
            "completion": manifest["completion"]["status"],
            "counts": manifest.get("counts"),
        },
        "partialReasons": list(reasons),
        "findings": [dict(finding) for finding in findings],
        "coverage": dict(coverage),
    }
    return read_template().replace(PAYLOAD_PLACEHOLDER, embed_json(payload))


def jsonl_line(finding: Mapping[str, Any]) -> str:
    record = {
        key: finding[key]
        for key in (
            "id",
            "file",
            "line",
            "location",
            "category",
            "issue_type",
            "severity",
            "confidence",
            "verdict",
            "short_summary",
            "summary",
            "failure_scenario",
            "reports",
            "rule_ids",
            "anchors",
            "source",
        )
    }
    if finding.get("suggestion") is not None:
        record["suggestion"] = finding["suggestion"]
    if finding.get("duplicate_of_posted") is not None:
        record["duplicate_of_posted"] = finding["duplicate_of_posted"]
    return json.dumps(record, ensure_ascii=False, separators=(",", ":"))


# --- SARIF rendering ---------------------------------------------------------


SARIF_SCHEMA_URI = "https://json.schemastore.org/sarif-2.1.0.json"
SARIF_VERSION = "2.1.0"
SARIF_LEVELS = {"HIGH": "error", "MEDIUM": "warning", "LOW": "note"}
CATEGORY_DESCRIPTIONS = {
    "correctness": (
        "The change can produce wrong behavior: incorrect output, a crash, "
        "or corrupted state."
    ),
    "reuse": "The change duplicates behavior the codebase already provides.",
    "simplification": "The change is more complex than the problem requires.",
    "efficiency": (
        "The change does avoidable work: wasted time, memory, or I/O."
    ),
    "altitude": (
        "The change solves the problem at the wrong level of abstraction."
    ),
    "conventions": "The change breaks a stated project convention or rule.",
    "test-coverage": "The change lacks test coverage its behavior needs.",
}


def sarif_uri(path: str) -> str:
    return quote(path, safe="/")


def sarif_location(
    path: str, start_line: int, end_line: Optional[int] = None
) -> Dict[str, Any]:
    region: Dict[str, Any] = {"startLine": start_line}
    if end_line is not None and end_line != start_line:
        region["endLine"] = end_line
    return {
        "physicalLocation": {
            "artifactLocation": {
                "uri": sarif_uri(path),
                "uriBaseId": "%SRCROOT%",
            },
            "region": region,
        }
    }


def sarif_rules(
    findings: Sequence[Mapping[str, Any]],
    coverage: Mapping[str, Any],
) -> Tuple[List[Dict[str, Any]], Dict[str, int]]:
    """One reportingDescriptor per rule ID the results reference.

    A finding backed by compiled rule checks reports under its first check
    ID, with the check's guidance as the rule help; a finding without one
    reports under its category. Descriptors cover every cited check so the
    check IDs in result properties stay resolvable.
    """
    rules_block = coverage.get("rules")
    catalog: Mapping[str, Any] = {}
    if isinstance(rules_block, dict) and isinstance(
        rules_block.get("checkCatalog"), dict
    ):
        catalog = rules_block["checkCatalog"]
    descriptors: List[Dict[str, Any]] = []
    for category in CATEGORIES:
        if any(
            not finding["rule_ids"] and finding["category"] == category
            for finding in findings
        ):
            description = CATEGORY_DESCRIPTIONS[category]
            descriptors.append(
                {
                    "id": category,
                    "shortDescription": {"text": description},
                    "help": {"text": description},
                }
            )
    cited_checks = sorted(
        {
            check_id
            for finding in findings
            for check_id in finding["rule_ids"]
        }
    )
    for check_id in cited_checks:
        descriptor: Dict[str, Any] = {"id": check_id}
        entry = catalog.get(check_id)
        guidance = (
            str(entry.get("guidance") or "").strip()
            if isinstance(entry, dict)
            else ""
        )
        if guidance:
            descriptor["fullDescription"] = {"text": guidance}
            descriptor["help"] = {"text": guidance}
        descriptors.append(descriptor)
    indices = {
        descriptor["id"]: index
        for index, descriptor in enumerate(descriptors)
    }
    return descriptors, indices


def sarif_result(
    finding: Mapping[str, Any],
    rule_indices: Mapping[str, int],
) -> Dict[str, Any]:
    rule_ids = finding["rule_ids"]
    primary = rule_ids[0] if rule_ids else finding["category"]
    message = (
        f"{finding['summary']}\n\n"
        f"Failure scenario: {finding['failure_scenario']}"
    )
    if finding["verdict"] == "UNVERIFIED":
        message += "\n\n" + UNVERIFIED_FINDING_NOTE
    location = finding["location"]
    start_line = location["start_line"]
    end_line = location["end_line"]
    fingerprint_anchor = (
        location["existing_code"]
        or f"{start_line}:{end_line}"
    )
    fingerprint = hashlib.sha256(
        (
            f"{finding['file']}\0{finding['issue_type']}\0"
            f"{fingerprint_anchor}"
        ).encode("utf-8")
    ).hexdigest()
    result: Dict[str, Any] = {
        "ruleId": primary,
        "ruleIndex": rule_indices[primary],
        "level": SARIF_LEVELS[finding["severity"]],
        "message": {"text": message},
        "locations": [
            sarif_location(finding["file"], start_line, end_line)
        ],
        "partialFingerprints": {
            "codeReviewIdentity/v2": fingerprint
        },
        "properties": {
            key: finding[key]
            for key in (
                "id",
                "category",
                "issue_type",
                "severity",
                "confidence",
                "verdict",
                "reports",
                "reporters",
                "rule_ids",
                "anchors",
                "source",
            )
        },
    }
    if finding.get("duplicate_of_posted") is not None:
        result["properties"]["duplicate_of_posted"] = finding[
            "duplicate_of_posted"
        ]
    suggestion = finding.get("suggestion")
    if isinstance(suggestion, dict):
        result["fixes"] = [
            {
                "description": {"text": "Apply the verified suggested change"},
                "artifactChanges": [
                    {
                        "artifactLocation": {
                            "uri": sarif_uri(finding["file"]),
                            "uriBaseId": "%SRCROOT%",
                        },
                        "replacements": [
                            {
                                "deletedRegion": {
                                    "startLine": start_line,
                                    "endLine": end_line,
                                },
                                "insertedContent": {
                                    "text": suggestion["replacement_code"]
                                },
                            }
                        ],
                    }
                ],
            }
        ]
    anchors = finding.get("anchors") or []
    if anchors:
        result["relatedLocations"] = [
            {
                **sarif_location(anchor["file"], anchor["line"]),
                "message": {
                    "text": (
                        f"Also reported as {anchor['id']} "
                        f"({anchor['category']}) and folded in."
                    )
                },
            }
            for anchor in anchors
        ]
    return result


def render_sarif(
    manifest: Mapping[str, Any],
    findings: Sequence[Mapping[str, Any]],
    coverage: Mapping[str, Any],
    reasons: Sequence[str],
) -> str:
    rules, rule_indices = sarif_rules(findings, coverage)
    run = {
        "tool": {
            "driver": {
                "name": "code-review",
                "informationUri": (
                    "https://github.com/lithoscomputer/code-review"
                ),
                "rules": rules,
            }
        },
        "automationDetails": {"id": f"code-review/{manifest['mode']}"},
        "columnKind": "utf16CodeUnits",
        "originalUriBaseIds": {
            "%SRCROOT%": {
                "description": {"text": "The root of the reviewed repository."}
            }
        },
        "results": [
            sarif_result(finding, rule_indices) for finding in findings
        ],
        "properties": {
            "review_id": manifest.get("review_id"),
            "mode": manifest.get("mode"),
            "effort": manifest.get("effort"),
            "model": manifest.get("model"),
            "guidance": manifest.get("guidance") or "",
            "completed_at": manifest.get("completed_at"),
            "revision": manifest.get("revision"),
            "verification": manifest["verification"]["status"],
            "completion": manifest["completion"]["status"],
            "partial_reasons": list(reasons),
        },
    }
    document = {
        "$schema": SARIF_SCHEMA_URI,
        "version": SARIF_VERSION,
        "runs": [run],
    }
    return json.dumps(document, ensure_ascii=True, indent=2) + "\n"


# --- Entry point -------------------------------------------------------------


def atomic_write(path: Path, text: str) -> None:
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(text, encoding="utf-8")
    os.replace(temporary, path)


def render(
    evidence_dir: str,
    products_dir: str,
    metadata_dir: str,
) -> Tuple[List[Dict[str, Any]], Dict[str, Any]]:
    manifest = validate_manifest(read_json(evidence_dir, "review-manifest.json"))
    findings = validate_findings(read_json(evidence_dir, "findings.json"))
    ledger = validate_ledger(read_jsonl(evidence_dir, "candidate-ledger.jsonl"))
    votes = validate_votes(read_jsonl(evidence_dir, "votes.jsonl"))
    coverage = validate_coverage(read_json(evidence_dir, "coverage.json"))
    validate_relationships(manifest, findings, ledger, votes, coverage)
    reasons = partial_reasons(manifest, coverage)

    products = Path(products_dir)
    atomic_write(
        products / "CODE-REVIEW-RESULTS.md",
        render_markdown(manifest, findings, coverage, reasons),
    )
    atomic_write(
        products / "CODE-REVIEW-RESULTS.html",
        render_html(manifest, findings, coverage, reasons),
    )
    atomic_write(
        products / "CODE-REVIEW-RESULTS.jsonl",
        "".join(jsonl_line(finding) + "\n" for finding in findings),
    )
    atomic_write(
        products / "CODE-REVIEW-RESULTS.sarif",
        render_sarif(manifest, findings, coverage, reasons),
    )
    revision = {
        "schema_version": CANONICAL_SCHEMA_VERSION,
        "review_id": manifest.get("review_id"),
        "completed_at": manifest.get("completed_at"),
        "mode": manifest.get("mode"),
        "effort": manifest.get("effort"),
        "model": manifest.get("model"),
        "guidance": manifest.get("guidance"),
        "scope": manifest.get("scope"),
        "range": manifest.get("range"),
        "revision": manifest.get("revision"),
        "counts": manifest.get("counts"),
        "verification": manifest.get("verification"),
        "completion": manifest.get("completion"),
        "evidence_dir": evidence_dir,
    }
    atomic_write(
        Path(metadata_dir) / "revision.json",
        json.dumps(revision, ensure_ascii=False, indent=2, sort_keys=True)
        + "\n",
    )
    return findings, dict(manifest["verification"])
