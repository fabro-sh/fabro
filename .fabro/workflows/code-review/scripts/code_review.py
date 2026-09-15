#!/usr/bin/env python3
"""Deterministic engine for the Fabro code-review workflow.

Review agents return validated JSON in their final messages. Fabro passes those
results to deterministic merge commands over standard input. This program owns
state transitions after Fabro's native agent retries, plus normalization, caps,
deduplication, verdict arithmetic, coverage records, and the canonical result
bundle.

Every tier above low projects one rule-mapped structure: a grouping pass
assigns every target file to exactly one local-correctness job, whole-change
angles cover cross-cutting concerns, path-matched YAML rules produce their
own audit jobs, and (at xhigh/max) a coverage-aware gap-fill sweep runs
last; fresh sweep candidates are verified the same way. The tiers differ
only in deterministic dials: grouping fidelity (semantic agent vs lexical
chunks), rule layers (full built-in library vs repository rules plus the
repository-instructions pack), caps, verification bias, the sweep, and the
model reasoning effort the graph's stylesheet selects. The low tier keeps
the original single-pass shape from the Claude Code local /code-review
workflow: one hunk-only finder, no verification, no rules. Deterministic
code decides what merges, what survives, and what the report can claim.

Python 3.9-compatible. Standard library only, except that rule compilation
(every tier above low) imports rule_loader, which requires the pinned
PyYAML dependency; the low tier never imports it.
"""

from __future__ import annotations

import argparse
import functools
import hashlib
import importlib.util
import json
import os
import re
import subprocess
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import (
    Any,
    Dict,
    Iterable,
    List,
    Mapping,
    Optional,
    Sequence,
    Tuple,
)

sys.path.insert(0, str(Path(__file__).resolve().parent))

from review_contract import (  # noqa: E402
    CATEGORIES,
    COMPILED_RULE_ID_RE,
    EFFORT_TIERS,
    ISSUE_TYPES,
    MAX_RULE_IDS_PER_FINDING,
    REVIEW_MODES,
)


WORKFLOW_ROOT = Path(".fabro/workflows/code-review")
CONTROL_DIR = WORKFLOW_ROOT / "runtime"
STATE_PATH = CONTROL_DIR / "state.json"
RENDERER_PATH = WORKFLOW_ROOT / "scripts/render_report.py"
PUBLISHER_PATH = WORKFLOW_ROOT / "scripts/publish_pr.py"
FINDINGS_SCHEMA_PATH = WORKFLOW_ROOT / "schemas/findings.schema.json"
VERDICT_SCHEMA_PATH = WORKFLOW_ROOT / "schemas/verdict.schema.json"

# Fabro resolves stdin_source before starting a command and enforces this same
# ceiling. Keep the driver's direct-input guard aligned with that transport.
MAX_STDIN_BYTES = 30 * 1024 * 1024
MAX_REVIEW_ID_STDIN_BYTES = 256
MAX_CHANGED_FILES_LISTED = 200

# Rule-mapped review shape (every tier above low).
GROUP_MAX_FILES = 10
GROUP_CHAR_BUDGET = 2000  # estimated per-job path payload, in characters
# A cell audits at most this many checks; a larger effective set splits into
# evenly sized cells over the same files. Each cell reports at most
# candidate_cap findings, so unbounded checks would dilute every check's
# share of the cell's attention as repository rules stack up.
MAX_CHECKS_PER_CELL = 12
DISCOVERY_JOB_CEILING = 64  # discovery jobs (local + angle + rule-audit)
# A small target at medium collapses to local passes and rule audits only
# (no whole-change angles), mirroring the security-review workflow's
# small-diff collapse. Verification still runs.
SMALL_DIFF_MAX_FILES = 5
SMALL_DIFF_MAX_LINES = 300
SMALL_SCOPE_MAX_FILES = 5

# Correctness bugs always outrank cleanup findings when a cap forces a cut.
CLEANUP_CATEGORIES = frozenset(CATEGORIES) - {"correctness"}
# Policy filters drop well-formed findings the review does not want; unlike a
# contract rejection they are recorded in coverage without making the run
# partial. Conventions findings must cite a rule check: calibration showed
# generic angles' unbacked style observations were the noisiest class, while
# every rule-cited conventions finding survived verification.
CONVENTIONS_FILTER_REASON = "conventions finding names no applicable rule check"
POLICY_FILTER_REASONS = frozenset({CONVENTIONS_FILTER_REASON})
VERDICTS = ("CONFIRMED", "PLAUSIBLE", "REFUTED")
KEPT_VERDICTS = frozenset({"CONFIRMED", "PLAUSIBLE"})
SEVERITY_RANK = {"HIGH": 3, "MEDIUM": 2, "LOW": 1}
CONFIDENCE_RANK = SEVERITY_RANK

SAFE_REV_RE = re.compile(r"^[A-Za-z0-9@][A-Za-z0-9._/@{}^~:+-]{0,399}$")
REVIEW_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$")
CANDIDATE_ID_RE = re.compile(r"^[FS][1-9][0-9]*$")
# A historical sibling: a finding already posted on the reviewed PR by an
# earlier run, addressed by its comment ID (P3 item 5). The grammar is
# disjoint from same-run candidate IDs so a verdict's duplicate_of target
# is never ambiguous.
HISTORICAL_ID_RE = re.compile(r"^H[1-9][0-9]*$")
# Other candidates in the same file a verifier is shown, nearest first, so it
# can mark its claim a duplicate of one that describes the same defect.
SIBLING_CAP = 6
# Historical siblings append under their own bound so they can never
# displace same-run siblings and starve the in-run fold.
HISTORICAL_SIBLING_CAP = 4

# Lines of context kept on each side of a finding's anchor line.
CODE_FRAME_CONTEXT = 4
CODE_FRAME_MAX_LINE_LENGTH = 400
CODE_FRAME_MAX_BYTES = 2 * 1024 * 1024
LOCATION_MAX_LINES = 50
SUGGESTION_CODE_MAX_LENGTH = 8000
CODE_FRAME_LANGUAGES = {
    "c": "C",
    "cc": "C++",
    "cpp": "C++",
    "cs": "C#",
    "css": "CSS",
    "ex": "Elixir",
    "exs": "Elixir",
    "go": "Go",
    "h": "C",
    "hpp": "C++",
    "html": "HTML",
    "java": "Java",
    "js": "JavaScript",
    "json": "JSON",
    "jsx": "JavaScript",
    "kt": "Kotlin",
    "lua": "Lua",
    "php": "PHP",
    "pl": "Perl",
    "py": "Python",
    "rb": "Ruby",
    "rs": "Rust",
    "scala": "Scala",
    "sh": "Shell",
    "sql": "SQL",
    "swift": "Swift",
    "toml": "TOML",
    "ts": "TypeScript",
    "tsx": "TypeScript",
    "yaml": "YAML",
    "yml": "YAML",
}

CANONICAL_SCHEMA_VERSION = 4
CANONICAL_FILES = (
    "review-manifest.json",
    "candidate-ledger.jsonl",
    "findings.json",
    "coverage.json",
    "votes.jsonl",
)
PHASE_OUTPUT_KEYS = {
    "finders": "output.finder",
    "verify": "output.verifier",
    "sweep_verify": "output.sweep_verifier",
}
PHASE_JOB_KEYS = {
    "finders": "finder_jobs",
    "verify": "verify_jobs",
    "sweep_verify": "sweep_verify_jobs",
}


# --- Review angles ------------------------------------------------------------

ANGLE_LOW_PASS = (
    "low-pass",
    "Single-pass diff scan",
    """Read the unified diff once. Skip test/fixture hunks (`test/`, `spec/`,
`__tests__/`, `*_test.*`, `*.test.*`, `fixtures/`, `testdata/`) -- test-file
changes are not reviewed at this level. Do not read whole files beyond the
hunks. Flag runtime-correctness bugs visible from the hunk alone:
inverted/wrong condition, off-by-one, null/undefined deref where adjacent
lines show the value can be absent, removed guard, falsy-zero check, missing
`await`, wrong-variable copy-paste, error swallowed in a catch that should
propagate. Also flag -- still from the hunk alone -- new code that duplicates
an existing helper visible in the diff context, and dead code the diff leaves
behind. Do NOT flag style, naming, perf, missing tests, or anything outside
the hunk.""",
)

# --- The rule-mapped shape (every tier above low) -----------------------------
#
# Generic angles describe how to investigate; path-matched rules describe what
# invariants apply. Language pitfalls and conventions live in the rule
# library, so the rule-mapped tiers run four whole-change angles plus one
# local-correctness pass per file group and one audit job per rule cell.

LOCAL_CORRECTNESS_INSTRUCTIONS = """Review the files listed in `files` one at
a time -- make an individual pass over every listed file; do not skim the set
as a whole. For each file, read every hunk of the diff that touches it, line
by line, then read the enclosing function of each hunk -- bugs in unchanged
lines of a touched function are in scope (the change re-exposes or fails to
fix them). For every line ask: what input, state, timing, or platform makes
this line wrong? Look for inverted/wrong conditions, off-by-one,
null/undefined deref, missing `await`, falsy-zero checks, wrong-variable
copy-paste, error swallowed in catch, unescaped regex metachars, and the
language's classic pitfalls (`==` coercion, closure-captured loop variables,
mutable default arguments, nil-map writes, float equality). When `mode` is
`files` there is no diff: read each listed file in full and treat every line
as under review."""

ANGLE_BEHAVIOR_PRESERVATION = (
    "behavior-preservation",
    "Behavior preservation",
    """For every line the diff DELETES or replaces, name the invariant or
behavior it enforced, then search the new code for where that invariant is
re-established. If you can't find it, that's a candidate: a removed guard, a
dropped error path, a narrowed validation, a lost compatibility shim, a
deleted test that was covering a real case. Also check that error paths and
validation the change touches still fire under the same conditions, and that
behavior contracts visible in tests survive the change.""",
)
ANGLE_CONTRACTS_DATA_FLOW = (
    "contracts-data-flow",
    "Contracts and data flow",
    """For each function or type the diff changes, find its callers (search
for the symbol) and check whether the change breaks any call site: a new
precondition, a changed return shape, a new exception, a timing/ordering
dependency. Also check callees: does a parallel change in the same change set
make a call unsafe? Trace cross-file contracts, data ownership, and ordering.
When the change adds or modifies a type that wraps another (cache, proxy,
decorator, adapter): check that every method routes to the wrapped instance
and not back through a registry/session/global -- e.g. a caching provider
holding a `delegate` field that resolves IDs via `session.get(...)` instead
of `delegate.get(...)` will re-enter the cache or recurse. Also check that
the wrapper forwards all the methods the callers actually use.""",
)
ANGLE_DESIGN_ECONOMY = (
    "design-economy",
    "Design economy",
    """This angle hunts for cleanup in the changed code, not bugs. Flag new
code that re-implements something the codebase already has -- search
shared/utility modules and files adjacent to the change, and name the
existing helper to call instead. Flag unnecessary complexity the diff adds:
redundant or derivable state, copy-paste with slight variation, deep nesting,
dead code left behind -- name the simpler form that does the same job. Check
that each change is implemented at the right depth, not as a fragile bandaid:
special cases layered on shared infrastructure are a sign the fix isn't deep
enough -- prefer generalizing the underlying mechanism over adding special
cases.""",
)
ANGLE_PERFORMANCE_LIFETIME = (
    "performance-lifetime",
    "Performance and lifetime",
    """This angle hunts for wasted work and lifetime problems in the changed
code, not logic bugs. Flag redundant computation or repeated I/O, independent
operations run sequentially, and blocking work added to startup or hot paths.
Check resource ownership: acquisitions without a release on every path, and
state retained longer than its use. Flag long-lived objects built from
closures or captured environments -- they keep the entire enclosing scope
alive for the object's lifetime (a memory leak when that scope holds large
values); prefer a class/struct that copies only the fields it needs. Name the
cheaper alternative.""",
)
WHOLE_CHANGE_ANGLES = (
    ANGLE_BEHAVIOR_PRESERVATION,
    ANGLE_CONTRACTS_DATA_FLOW,
    ANGLE_DESIGN_ECONOMY,
    ANGLE_PERFORMANCE_LIFETIME,
)

STANCE_PRECISION = (
    "precision: every finding you surface should be one a maintainer would "
    "act on."
)
STANCE_RECALL = (
    "recall: catch every real bug a careful reviewer would catch in one "
    "sitting. Catching real bugs matters more than avoiding false positives. "
    "Err on the side of surfacing."
)
STANCE_MAX_RECALL = (
    "recall: catch every real bug. A missed bug ships. Catching real bugs "
    "matters more than avoiding false positives. Err on the side of "
    "surfacing."
)
STANCE_LOW = (
    "precision, single pass: report only defects visible from the hunk alone."
)
STANCE_RULE_AUDIT = (
    "rule audit: report violations of the assigned checks that you can "
    "anchor to the changed code; each check's own guidance sets the "
    "precision bar."
)

SWEEP_FOCUS = (
    "moved/extracted code that dropped a guard or anchor; second-tier "
    "footguns (dataclass default evaluated once, `hash()` non-determinism, "
    "lock-scope shrink, predicate methods with side effects); setup/teardown "
    "asymmetry in tests; config defaults flipped."
)

# Every tier above low is a projection of the rule-mapped structure; the
# dials are grouping fidelity, rule layers, caps, bias, and the sweep. The
# xhigh and max cells are identical by design: the two tiers share one
# graph, job structure, caps, prompts, and verification policy, and differ
# only in the reasoning effort the Fabro model stylesheet selects.
_XHIGH_CELL = {
    "angles": (),
    "rule_mapped": True,
    "grouping": "semantic",
    "rule_layers": "full",
    "collapse": False,
    "stance": STANCE_MAX_RECALL,
    "per_angle_cap": 8,
    "verify": True,
    "bias": "standard",
    "sweep": True,
    "report_cap": 25,
    "verification_cap": 120,
}
EFFORT_CELLS = {
    "low": {
        "angles": (ANGLE_LOW_PASS,),
        "rule_mapped": False,
        "stance": STANCE_LOW,
        "per_angle_cap": 6,
        "verify": False,
        "bias": None,
        "sweep": False,
        "report_cap": 4,
        "verification_cap": 60,
    },
    "medium": {
        "angles": (),
        "rule_mapped": True,
        "grouping": "lexical",
        "rule_layers": "repo-instructions",
        "collapse": True,
        "stance": STANCE_PRECISION,
        "per_angle_cap": 6,
        "verify": True,
        "bias": "standard",
        "sweep": False,
        "report_cap": 8,
        "verification_cap": 60,
    },
    "high": {
        "angles": (),
        "rule_mapped": True,
        "grouping": "semantic",
        "rule_layers": "full",
        "collapse": False,
        "stance": STANCE_RECALL,
        "per_angle_cap": 6,
        "verify": True,
        "bias": "recall",
        "sweep": False,
        "report_cap": 10,
        "verification_cap": 60,
    },
    "xhigh": dict(_XHIGH_CELL),
    "max": dict(_XHIGH_CELL),
}
SWEEP_CANDIDATE_CAP = 8


class WorkflowDataError(RuntimeError):
    """A deterministic workflow-data failure."""


# --- Small shared helpers ----------------------------------------------------


def root() -> Path:
    return Path.cwd().resolve()


def clean_text(value: Any, cap: int = 4000) -> str:
    text = str("" if value is None else value)
    text = "".join(
        character
        if character in "\n\t" or ord(character) >= 0x20
        else " "
        for character in text
    )
    if len(text) > cap:
        return text[:cap] + f"...[+{len(text) - cap} chars]"
    return text


def one_line(value: Any, cap: int = 500) -> str:
    return (
        clean_text(value, cap)
        .replace("\r", " ")
        .replace("\n", " ")
        .replace("\t", " ")
    )


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(
        json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, path)


def write_jsonl(path: Path, values: Iterable[Mapping[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("w", encoding="utf-8", newline="\n") as handle:
        for value in values:
            handle.write(
                json.dumps(
                    value,
                    ensure_ascii=False,
                    separators=(",", ":"),
                )
                + "\n"
            )
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def read_json(path: Path, required: bool = True) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        if required:
            raise WorkflowDataError(f"required file is missing: {path}")
        return None
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        if required:
            raise WorkflowDataError(
                f"could not read JSON from {path}: {error}"
            ) from error
        return None


def load_state() -> Dict[str, Any]:
    value = read_json(STATE_PATH)
    if not isinstance(value, dict):
        raise WorkflowDataError(f"{STATE_PATH} must contain a JSON object")
    return value


def point_state_locator_at(path: Path) -> None:
    """Make the fixed runtime path locate the published canonical state."""
    CONTROL_DIR.mkdir(parents=True, exist_ok=True)
    temporary = STATE_PATH.with_name(STATE_PATH.name + ".link.tmp")
    try:
        temporary.unlink(missing_ok=True)
        target = os.path.relpath(path, start=STATE_PATH.parent)
        temporary.symlink_to(target)
        os.replace(temporary, STATE_PATH)
    finally:
        temporary.unlink(missing_ok=True)


def save_state(state: Mapping[str, Any]) -> None:
    copy = dict(state)
    state_path = copy.get("state_path")
    if isinstance(state_path, str) and state_path:
        canonical_path = Path(state_path)
        write_json(canonical_path, copy)
        point_state_locator_at(canonical_path)
    else:
        write_json(STATE_PATH, copy)


def emit(**updates: Any) -> None:
    print(
        json.dumps(
            {"context_updates": updates},
            ensure_ascii=False,
            separators=(",", ":"),
        )
    )


def git(
    *arguments: str,
    check: bool = False,
    input_bytes: Optional[bytes] = None,
) -> subprocess.CompletedProcess:
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
        result = subprocess.run(
            ["git", "-C", str(root()), *arguments],
            input=input_bytes,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=environment,
        )
    except OSError as error:
        raise WorkflowDataError(f"could not run Git: {error}") from error
    if check and result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise WorkflowDataError(
            f"git {' '.join(arguments)} failed"
            + (f": {one_line(detail, 2000)}" if detail else "")
        )
    return result


def git_text(*arguments: str, check: bool = False) -> Optional[str]:
    result = git(*arguments, check=check)
    if result.returncode != 0:
        return None
    return result.stdout.decode("utf-8", "replace").rstrip("\r\n")


def inside_git_worktree() -> bool:
    return git_text("rev-parse", "--is-inside-work-tree") == "true"


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def review_id_from_args(args: argparse.Namespace) -> str:
    """Resolve the run-scoped review ID, using Fabro's run ID when supplied."""
    explicit = getattr(args, "review_id", "")
    from_stdin = bool(getattr(args, "review_id_stdin", False))
    if from_stdin:
        raw = sys.stdin.buffer.read(MAX_REVIEW_ID_STDIN_BYTES + 1)
        if len(raw) > MAX_REVIEW_ID_STDIN_BYTES:
            raise WorkflowDataError(
                f"review ID input exceeds {MAX_REVIEW_ID_STDIN_BYTES} bytes"
            )
        try:
            explicit = raw.decode("utf-8").strip()
        except UnicodeError as error:
            raise WorkflowDataError(
                "review ID input is not valid UTF-8"
            ) from error
        if not explicit:
            raise WorkflowDataError("Fabro did not supply a review ID")
    review_id = str(explicit or f"local_{uuid.uuid4().hex}").strip()
    if not REVIEW_ID_RE.fullmatch(review_id):
        raise WorkflowDataError("review ID has an invalid format")
    return review_id


# --- Git target resolution ---------------------------------------------------


def validate_revision(value: str, field: str) -> str:
    text = value.strip()
    if not SAFE_REV_RE.fullmatch(text):
        raise WorkflowDataError(
            f"{field} must be one conservative Git revision token, got {value!r}"
        )
    return text


def resolve_commit(value: str, field: str) -> str:
    revision = validate_revision(value, field)
    resolved = git_text(
        "rev-parse",
        "--verify",
        "--quiet",
        revision + "^{commit}",
    )
    if not resolved:
        raise WorkflowDataError(
            f"{field} {value!r} does not resolve to a commit in this checkout; "
            "the workflow does not fetch missing refs"
        )
    return resolved


def parse_two_sided_range(raw: str) -> Tuple[str, str, str]:
    text = raw.strip()
    separator = "..." if "..." in text else ".."
    if separator not in text:
        raise WorkflowDataError(
            "range must be explicit and two-sided, such as base..HEAD"
        )
    left, right = text.split(separator, 1)
    if not left or not right or ".." in left or ".." in right:
        raise WorkflowDataError(
            "range must contain exactly two Git revision tokens"
        )
    return (
        validate_revision(left, "range start"),
        separator,
        validate_revision(right, "range end"),
    )


def default_base_ref() -> str:
    candidates: List[str] = []
    upstream = git_text(
        "rev-parse",
        "--abbrev-ref",
        "--symbolic-full-name",
        "@{upstream}",
    )
    if upstream and SAFE_REV_RE.fullmatch(upstream):
        candidates.append(upstream)
    candidates.extend(
        ["origin/HEAD", "origin/main", "origin/master", "main", "master"]
    )
    seen = set()
    for candidate in candidates:
        if candidate in seen:
            continue
        seen.add(candidate)
        if git_text(
            "rev-parse",
            "--verify",
            "--quiet",
            candidate + "^{commit}",
        ):
            return candidate
    raise WorkflowDataError(
        "changes mode could not resolve a base ref. Supply --base or an "
        "explicit two-sided --range; the workflow does not fetch"
    )


def empty_tree_hash() -> str:
    result = git(
        "hash-object", "-t", "tree", "--stdin", check=True, input_bytes=b""
    )
    value = result.stdout.decode("ascii", "replace").strip()
    if not value:
        raise WorkflowDataError("Git did not return the empty-tree object id")
    return value


def parse_scope(raw: str) -> List[str]:
    entries = [entry.strip().replace("\\", "/") for entry in raw.split(",")]
    entries = [entry for entry in entries if entry]
    if entries and all(entry in (".", "./") for entry in entries):
        return []
    normalized: List[str] = []
    for entry in entries:
        candidate = normalize_repo_path(entry)
        if candidate is None:
            raise WorkflowDataError(f"scope path is unsafe: {entry!r}")
        if candidate != "." and candidate not in normalized:
            normalized.append(candidate)
    return normalized


def normalize_repo_path(value: Any) -> Optional[str]:
    text = str("" if value is None else value).strip().replace("\\", "/")
    repository = root().as_posix().rstrip("/")
    if text == repository:
        return "."
    if text.startswith(repository + "/"):
        text = text[len(repository) + 1 :]
    while text.startswith("./"):
        text = text[2:]
    text = re.sub(r"/+$", "", text)
    if not text:
        return "."
    path = PurePosixPath(text)
    if path.is_absolute() or ".." in path.parts:
        return None
    return path.as_posix()


def decode_z_paths(raw: bytes) -> List[str]:
    return [
        item.decode("utf-8", "surrogateescape").replace("\\", "/")
        for item in raw.split(b"\0")
        if item
    ]


def tracked_files(scopes: Sequence[str] = ()) -> List[str]:
    if not inside_git_worktree():
        return []
    arguments = ["ls-files", "-z"]
    if scopes:
        arguments.extend(["--", *scopes])
    result = git(*arguments, check=True)
    return sorted(decode_z_paths(result.stdout))


def is_generated_path(path: str) -> bool:
    normalized = path.replace("\\", "/")
    while normalized.startswith("./"):
        normalized = normalized[2:]
    top = normalized.split("/", 1)[0]
    if top.startswith("CODE-REVIEW-"):
        return True
    generated = (
        ".fabro/blobs",
        ".fabro/workflows/code-review/runtime",
    )
    return any(
        normalized == prefix or normalized.startswith(prefix + "/")
        for prefix in generated
    )


def repo_files() -> List[str]:
    listing = git("ls-files", "--cached", "--others", "--exclude-standard", "-z")
    if listing.returncode == 0:
        return sorted(
            path
            for path in decode_z_paths(listing.stdout)
            if not is_generated_path(path)
        )

    paths: List[str] = []
    skipped_directories = {
        ".git",
        ".cache",
        ".venv",
        "dist",
        "node_modules",
        "target",
    }
    for current, directories, files in os.walk(root()):
        directories[:] = [
            name
            for name in directories
            if name not in skipped_directories
            and not name.startswith("CODE-REVIEW-")
        ]
        for name in files:
            relative = (Path(current) / name).relative_to(root()).as_posix()
            if not is_generated_path(relative):
                paths.append(relative)
    return sorted(paths)


def diff_stats(
    revision_range: str,
    scopes: Sequence[str],
) -> Tuple[List[str], Optional[int]]:
    suffix = ["--", *scopes] if scopes else ["--"]
    names = git(
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--name-only",
        "-z",
        revision_range,
        *suffix,
        check=True,
    )
    files = decode_z_paths(names.stdout)
    numstat = git(
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--numstat",
        revision_range,
        *suffix,
        check=True,
    )
    total = 0
    for raw_line in numstat.stdout.decode("utf-8", "replace").splitlines():
        columns = raw_line.split("\t", 2)
        if (
            len(columns) < 3
            or not columns[0].isdigit()
            or not columns[1].isdigit()
        ):
            return files, None
        total += int(columns[0]) + int(columns[1])
    return files, total


def diff_file_records(
    revision_range: str,
    scopes: Sequence[str],
) -> Dict[str, Dict[str, Any]]:
    """Per-file status and churn for the range, keyed by new-side path."""
    suffix = ["--", *scopes] if scopes else ["--"]
    records: Dict[str, Dict[str, Any]] = {}
    status_listing = git(
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--name-status",
        "-z",
        revision_range,
        *suffix,
        check=True,
    )
    items = [
        item.decode("utf-8", "surrogateescape").replace("\\", "/")
        for item in status_listing.stdout.split(b"\0")
    ]
    index = 0
    while index < len(items):
        status = items[index]
        if not status:
            index += 1
            continue
        letter = status[0]
        if letter in {"R", "C"} and index + 2 < len(items):
            old_path, new_path = items[index + 1], items[index + 2]
            records[new_path] = {"status": letter, "old_path": old_path}
            index += 3
        elif index + 1 < len(items):
            records[items[index + 1]] = {"status": letter}
            index += 2
        else:
            break
    numstat = git(
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--numstat",
        "-z",
        revision_range,
        *suffix,
        check=True,
    )
    entries = [
        item.decode("utf-8", "surrogateescape").replace("\\", "/")
        for item in numstat.stdout.split(b"\0")
    ]
    index = 0
    while index < len(entries):
        entry = entries[index]
        if not entry:
            index += 1
            continue
        columns = entry.split("\t")
        if len(columns) < 3:
            index += 1
            continue
        added, deleted, path = columns[0], columns[1], columns[2]
        if not path:
            # -z rename form: "added\tdeleted\t\0old\0new\0"
            if index + 2 >= len(entries):
                break
            path = entries[index + 2]
            index += 3
        else:
            index += 1
        record = records.setdefault(path, {"status": "M"})
        if added.isdigit() and deleted.isdigit():
            record["added"] = int(added)
            record["deleted"] = int(deleted)
    return records


def diff_record_summary(
    records: Mapping[str, Mapping[str, Any]],
) -> Tuple[List[str], Optional[int]]:
    """Return sorted paths and total text churn from one diff scan."""
    churn = [
        record.get("added", 0) + record.get("deleted", 0)
        for record in records.values()
        if isinstance(record.get("added"), int)
        and isinstance(record.get("deleted"), int)
    ]
    total = sum(churn) if len(churn) == len(records) else None
    return sorted(records), total


def read_file_at_revision(revision: str, path: str) -> Optional[bytes]:
    result = git("show", f"{revision}:{path}")
    if result.returncode != 0:
        return None
    return result.stdout


def workspace_digest() -> str:
    digest = hashlib.sha256()
    for relative in repo_files():
        digest.update(relative.encode("utf-8", "surrogateescape"))
        digest.update(b"\0")
        path = root() / relative
        try:
            stat_result = path.lstat()
        except FileNotFoundError:
            digest.update(b"MISSING\0")
            continue
        digest.update(str(stat_result.st_mode).encode("ascii"))
        digest.update(b"\0")
        if path.is_symlink():
            digest.update(os.readlink(path).encode("utf-8", "surrogateescape"))
        elif path.is_file():
            with path.open("rb") as handle:
                while True:
                    chunk = handle.read(1024 * 1024)
                    if not chunk:
                        break
                    digest.update(chunk)
        digest.update(b"\0")
    return digest.hexdigest()


def assert_workspace_unchanged(state: Mapping[str, Any]) -> None:
    """Refuse to publish results derived from a tampered source tree.

    Tamper evidence behind the read-only tool guard: an agent that finds a way
    to write could shape what the verifiers and the report see. Checked at
    final-tally after the last agent and again at publish-pr immediately before
    anything leaves for GitHub.
    """
    expected = state.get("workspace_digest")
    actual = workspace_digest()
    if not isinstance(expected, str) or actual != expected:
        raise WorkflowDataError(
            "the reviewed source tree changed during the review; refusing "
            "to publish results derived from it"
        )


def worktree_dirty() -> Optional[bool]:
    status = git("status", "--porcelain=v1", "-z", "--untracked-files=all")
    if status.returncode != 0:
        return None
    entries = status.stdout.split(b"\0")
    index = 0
    while index < len(entries):
        raw_entry = entries[index]
        index += 1
        if not raw_entry:
            continue
        entry = raw_entry.decode("utf-8", "surrogateescape")
        if len(entry) < 4:
            return True
        status_code = entry[:2]
        paths = [entry[3:]]
        if status_code[0] in {"R", "C"} or status_code[1] in {"R", "C"}:
            if index >= len(entries) or not entries[index]:
                return True
            paths.append(
                entries[index].decode("utf-8", "surrogateescape")
            )
            index += 1
        if any(not is_generated_path(path) for path in paths):
            return True
    return False


def revision_record(
    mode: str,
    target_commit: Optional[str],
    base: Optional[str],
    merge_base: Optional[str],
    parent: Optional[str],
    revision_range: Optional[str],
) -> Dict[str, Any]:
    if not inside_git_worktree():
        return {"versioned": False}
    head = git_text("rev-parse", "HEAD")
    branch = git_text("symbolic-ref", "--short", "-q", "HEAD")
    if mode == "commit":
        return {
            "versioned": True,
            "commit": target_commit,
            "parent": parent,
            "branch": branch,
            "dirty": worktree_dirty(),
            "range": revision_range,
        }
    revision: Dict[str, Any] = {
        "versioned": True,
        "commit": target_commit or head,
        "branch": branch,
        "dirty": worktree_dirty(),
    }
    if mode == "changes":
        revision.update(
            {
                "base": base,
                "merge_base": merge_base,
                "range": revision_range,
            }
        )
    return revision


def unique_report_dir() -> Tuple[Path, str]:
    stem = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
    name = f"CODE-REVIEW-{stem}"
    candidate = root() / name
    suffix = 1
    while candidate.exists():
        name = f"CODE-REVIEW-{stem}-{suffix}"
        candidate = root() / name
        suffix += 1
    candidate.mkdir(parents=False)
    return candidate, name


def common_target(state: Mapping[str, Any]) -> Dict[str, Any]:
    changed = list(state.get("changed_files") or [])
    return {
        "mode": state.get("mode"),
        "scope": state.get("scope") or [],
        "range": state.get("range"),
        "changedFileCount": state.get("diff_files"),
        "changedLineCount": state.get("diff_lines"),
        "changedFiles": changed[:MAX_CHANGED_FILES_LISTED],
        "changedFilesTruncated": max(0, len(changed) - MAX_CHANGED_FILES_LISTED),
        "reviewRoot": str(root()),
        "gitWrapper": (
            "python3 -I .fabro/workflows/code-review/scripts/git_readonly.py"
        ),
    }


# --- Contract drift checks ---------------------------------------------------


def verify_schema_sources() -> None:
    """Refuse to start when the static schemas disagree with this engine.

    Fabro reads the schema files directly, and this engine restates their
    closed enums. Neither is generated from the other, so drift is caught here
    rather than in a finished report.
    """
    findings_schema = read_json(root() / FINDINGS_SCHEMA_PATH)
    try:
        schema_categories = findings_schema["properties"]["findings"]["items"][
            "properties"
        ]["category"]["enum"]
    except (KeyError, TypeError) as error:
        raise WorkflowDataError(
            f"{FINDINGS_SCHEMA_PATH} has no category enum"
        ) from error
    if list(schema_categories) != list(CATEGORIES):
        raise WorkflowDataError(
            f"{FINDINGS_SCHEMA_PATH} category enum does not match this "
            "engine's category list"
        )
    try:
        schema_issue_types = findings_schema["properties"]["findings"]["items"][
            "properties"
        ]["issue_type"]["enum"]
    except (KeyError, TypeError) as error:
        raise WorkflowDataError(
            f"{FINDINGS_SCHEMA_PATH} has no issue_type enum"
        ) from error
    if list(schema_issue_types) != list(ISSUE_TYPES):
        raise WorkflowDataError(
            f"{FINDINGS_SCHEMA_PATH} issue_type enum does not match this "
            "engine's issue type list"
        )
    verdict_schema = read_json(root() / VERDICT_SCHEMA_PATH)
    try:
        schema_verdicts = verdict_schema["properties"]["verdict"]["enum"]
    except (KeyError, TypeError) as error:
        raise WorkflowDataError(
            f"{VERDICT_SCHEMA_PATH} has no verdict enum"
        ) from error
    if list(schema_verdicts) != list(VERDICTS):
        raise WorkflowDataError(
            f"{VERDICT_SCHEMA_PATH} verdict enum does not match this "
            "engine's verdict list"
        )


# --- Rule compilation (rule-mapped tiers) -------------------------------------

def import_rule_loader() -> Any:
    try:
        import rule_loader
    except ImportError as error:
        raise WorkflowDataError(
            "the rule-mapped tiers need the rule loader and its pinned PyYAML "
            f"dependency (see README, Developing): {error}"
        ) from error
    return rule_loader


def import_publisher() -> Any:
    """The publisher module (stdlib-only), for its shared P3 contracts."""
    import publish_pr

    return publish_pr


# --- Incremental re-review (P3) ----------------------------------------------


HISTORY_FILE_NAME = "pr-history.json"


def run_publisher(
    publisher_path: Path,
    arguments: List[str],
    environment: Optional[Dict[str, str]] = None,
) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(publisher_path), *arguments],
        cwd=root(),
        env=environment,
        capture_output=True,
    )


def prepare_incremental(args: argparse.Namespace, mode: str) -> Dict[str, Any]:
    """Fetch per-PR history and resolve the incremental posture (items 1-2).

    Runs before range resolution: the delta needs the last reviewed head.
    The fetch itself is `publish_pr.py history` -- pinned code holding the
    token, the P3 sibling of apply. A fetch failure degrades open (full
    review, no suppression, declared downstream; decided Q2), but a
    snapshot that disagrees with the local HEAD fails the run: it was
    taken around a racing push and every decision built on it would be
    wrong from the outset.
    """
    publisher = import_publisher()
    state: Dict[str, Any] = {
        "requested": True,
        "enabled": False,
        "from": None,
        "commits": None,
        "history_unavailable": None,
        "history_comments": [],
    }
    if mode != "changes":
        raise WorkflowDataError(
            "incremental review requires mode=changes (a PR review)"
        )
    if not inside_git_worktree():
        raise WorkflowDataError("incremental review requires a Git worktree")
    repo = args.pr_repo.strip()
    pr = args.pr_number.strip()
    if not repo or not pr:
        raise WorkflowDataError(
            "incremental is enabled but pr_repo/pr_number do not name the "
            "pull request whose history to read"
        )
    try:
        # The parsed value rides in the state record so the publish node
        # reuses this validated decision instead of re-reading the input.
        state["overlap_threshold"] = publisher.parse_overlap_threshold(
            args.incremental_overlap_threshold
        )
    except publisher.PublishError as error:
        raise WorkflowDataError(str(error)) from error

    publisher_path = resolve_workflow_script(PUBLISHER_PATH, "PR publisher")
    history_path = CONTROL_DIR / HISTORY_FILE_NAME
    fetched = run_publisher(
        publisher_path,
        [
            "history",
            "--repo", repo,
            "--pr", pr,
            "--api-base", args.api_base,
            "--bot-login", args.bot_login,
            "--output", str(history_path),
        ],
    )
    if fetched.returncode != 0:
        reason = one_line(
            fetched.stderr.decode("utf-8", "replace").strip()
            or "the history fetch failed",
            500,
        )
        state["history_unavailable"] = reason
        print(f"Incremental history unavailable, degrading open: {reason}")
        return state
    stdout_text = fetched.stdout.decode("utf-8", "replace").strip()
    if stdout_text:
        print(stdout_text)

    try:
        history = publisher.validate_history_document(
            read_json(history_path)
        )
    except publisher.PublishError as error:
        raise WorkflowDataError(f"the fetched history is invalid: {error}")
    live_head = history["live_head"]
    local_head = resolve_commit("HEAD", "HEAD")
    if live_head != local_head:
        raise WorkflowDataError(
            f"the PR head moved during preparation: the history snapshot "
            f"describes {live_head[:12]} but the checkout is at "
            f"{local_head[:12]}; re-run the review"
        )
    state["enabled"] = True
    state["history_comments"] = list(history.get("comments") or [])

    last_head = (history.get("summary") or {}).get("head")
    if isinstance(last_head, str):
        ancestry = git("merge-base", "--is-ancestor", last_head, "HEAD")
        if ancestry.returncode == 0:
            # Delta review (item 2): two-dot, literal base. A same-head
            # re-run resolves to an empty range and exits quietly (Q3).
            state["from"] = last_head
            commits = git_text("rev-list", "--count", f"{last_head}..HEAD")
            state["commits"] = (
                int(commits) if commits and commits.isdigit() else None
            )
        else:
            # A force-push or rebase: cold start for the range, but the
            # history still suppresses where mapping succeeded (item 1).
            print(
                f"Last reviewed head {last_head[:12]} is not an ancestor "
                "of HEAD (force-push?); reviewing the full range"
            )
    return state


def repo_rule_revision(state: Mapping[str, Any]) -> Optional[str]:
    """The revision repository rules are read from.

    Rules come from the base side of the review -- the merge base (or the
    left endpoint of an explicit two-dot range) in changes mode, the reviewed
    commit's parent in commit mode -- so a change cannot weaken the rules
    used to review itself. Files mode reads the reviewed HEAD revision, or
    the working filesystem outside a Git worktree (returned as None).
    """
    mode = str(state.get("mode"))
    if mode == "changes":
        merge_base = state.get("merge_base")
        if not isinstance(merge_base, str) or not merge_base:
            raise WorkflowDataError("changes mode has no resolved merge base")
        return merge_base
    if mode == "commit":
        parent = state.get("parent")
        return parent if isinstance(parent, str) and parent else None
    if inside_git_worktree():
        return git_text("rev-parse", "HEAD", check=True)
    return None


def read_repo_rule_files(
    loader: Any,
    revision: Optional[str],
) -> List[Tuple[str, bytes]]:
    if revision is None:
        candidates: List[str] = []
        entry = root() / loader.REPO_ENTRYPOINT
        if entry.is_file():
            candidates.append(loader.REPO_ENTRYPOINT)
        rules_dir = root() / loader.REPO_RULES_PREFIX
        if rules_dir.is_dir():
            candidates.extend(
                path.relative_to(root()).as_posix()
                for path in rules_dir.rglob("*.yaml")
                if path.is_file()
            )
        ordered = loader.discover_repo_rule_paths(candidates)
        return [(path, (root() / path).read_bytes()) for path in ordered]
    listing = git(
        "ls-tree", "-r", "--name-only", "-z", revision, "--", ".fabro",
        check=True,
    )
    ordered = loader.discover_repo_rule_paths(decode_z_paths(listing.stdout))
    files: List[Tuple[str, bytes]] = []
    for path in ordered:
        content = read_file_at_revision(revision, path)
        if content is None:
            raise WorkflowDataError(
                f"could not read repository rule file {path} at the rule "
                "revision"
            )
        files.append((path, content))
    return files


def sniff_m_files(
    loader: Any,
    state: Mapping[str, Any],
    file_records: Mapping[str, Mapping[str, Any]],
) -> Dict[str, Dict[str, str]]:
    """Classify every ".m" target file as MATLAB or Objective-C.

    Added, modified, and renamed files are read at the reviewed target
    revision; deleted files at the base revision; never an unrelated
    checkout. Missing or inconclusive bytes keep the deterministic MATLAB
    default.
    """
    mode = str(state.get("mode"))
    target_revision = state.get("commit")
    base_revision = (
        state.get("merge_base") if mode == "changes" else state.get("parent")
    )
    results: Dict[str, Dict[str, str]] = {}
    for path in state.get("changed_files") or []:
        if not path.lower().endswith(".m"):
            continue
        record = file_records.get(path) or {}
        deleted = record.get("status") == "D"
        content: Optional[bytes] = None
        if mode in {"changes", "commit"}:
            revision = base_revision if deleted else target_revision
            if isinstance(revision, str) and revision:
                content = read_file_at_revision(revision, path)
        elif isinstance(target_revision, str) and target_revision:
            content = read_file_at_revision(target_revision, path)
        else:
            try:
                target_path = root() / path
                if target_path.is_file() and not target_path.is_symlink():
                    content = target_path.read_bytes()
            except OSError:
                content = None
        language, source = loader.sniff_m_language(content)
        results[path] = {"language": language, "source": source}
    return results


def compile_rule_state(
    state: Dict[str, Any],
    file_records: Mapping[str, Mapping[str, Any]],
) -> None:
    """Resolve the effective rule checks for every target file.

    Runs for every rule-mapped tier. An invalid built-in or base-revision
    repository rule file is a deterministic workflow failure: the run must
    not proceed while claiming rule coverage that was not applied.

    The tier's rule layers select what compiles: "full" uses the whole
    built-in library; "repo-instructions" keeps only the
    repository-instructions built-in pack alongside the repository rules.
    Manifest integrity is verified either way.
    """
    layers = str(state.get("rule_layers") or "full")
    loader = import_rule_loader()
    workflow_root = root() / WORKFLOW_ROOT
    manifest_path = workflow_root / loader.BUILTIN_MANIFEST
    manifest = read_json(manifest_path)
    builtin_manifest_sha = hashlib.sha256(
        manifest_path.read_bytes()
    ).hexdigest()
    try:
        builtin_files = loader.load_builtin_files(workflow_root, manifest)
        builtin_packs = loader.load_rule_layer(builtin_files, "builtin")
        revision = repo_rule_revision(state)
        repo_files = read_repo_rule_files(loader, revision)
        repo_packs = loader.load_rule_layer(repo_files, "repo")
    except loader.RuleLoaderError as error:
        raise WorkflowDataError(f"rule configuration is invalid: {error}")
    if layers == "repo-instructions":
        builtin_packs = [
            pack
            for pack in builtin_packs
            if pack["pack_id"] == loader.INSTRUCTIONS_PACK_ID
        ]

    # The ".m" sniff only selects between built-in language packs, which the
    # filtered layers do not compile.
    sniff = (
        sniff_m_files(loader, state, file_records)
        if layers == "full"
        else {}
    )
    catalog: Dict[str, Dict[str, Any]] = {}
    effective: Dict[str, List[str]] = {}
    overridden: Dict[str, List[str]] = {}
    for path in state.get("changed_files") or []:
        m_language = sniff.get(path, {}).get("language")
        resolved = loader.effective_checks_for_path(
            path, builtin_packs, repo_packs, m_language
        )
        ids: List[str] = []
        for check in resolved["checks"]:
            catalog[check["id"]] = check
            ids.append(check["id"])
        effective[path] = ids
        if resolved["overridden"]:
            overridden[path] = list(resolved["overridden"])

    state["rules"] = {
        "enabled": True,
        "layers": layers,
        "config_sha256": loader.rule_config_sha256(
            builtin_packs, repo_packs
        ),
        "builtin_manifest_sha256": builtin_manifest_sha,
        "repo_rule_revision": revision,
        "repo_rule_files": [path for path, _content in repo_files],
        "counts": {
            "builtin_packs": len(builtin_packs),
            "repo_packs": len(repo_packs),
            "builtin_checks": sum(
                len(pack["checks"]) for pack in builtin_packs
            ),
            "repo_checks": sum(len(pack["checks"]) for pack in repo_packs),
        },
        "catalog": catalog,
        "effective": effective,
        "overridden": overridden,
        "sniff": sniff,
    }


# --- Grouping and rule-mapped job planning -------------------------------------


def path_cost(path: str) -> int:
    return len(path) + 16


def chunk_paths(paths: Sequence[str]) -> List[List[str]]:
    """Split an ordered path list at the group size and size-estimate caps."""
    chunks: List[List[str]] = []
    current: List[str] = []
    cost = 0
    for path in paths:
        item_cost = path_cost(path)
        if current and (
            len(current) >= GROUP_MAX_FILES
            or cost + item_cost > GROUP_CHAR_BUDGET
        ):
            chunks.append(current)
            current = []
            cost = 0
        current.append(path)
        cost += item_cost
    if current:
        chunks.append(current)
    return chunks


def finalize_groups(
    state: Mapping[str, Any],
    raw_groups: Optional[Sequence[Sequence[str]]],
) -> Tuple[List[List[str]], Optional[str], List[str]]:
    """Turn the grouping agent's proposal into exact target-file coverage.

    The semantic choice can be model-authored; file coverage cannot be.
    Returns (groups, fallback, corrections); corrections are fixed engine
    strings that never quote model text.
    """
    target = list(state.get("changed_files") or [])
    target_set = set(target)
    corrections: List[str] = []
    fallback: Optional[str] = None
    if raw_groups:
        seen = set()
        unknown = duplicates = 0
        groups: List[List[str]] = []
        for raw in raw_groups:
            cleaned: List[str] = []
            for path in raw:
                if path not in target_set:
                    unknown += 1
                    continue
                if path in seen:
                    duplicates += 1
                    continue
                seen.add(path)
                cleaned.append(path)
            if cleaned:
                groups.append(sorted(cleaned))
        if unknown:
            corrections.append(f"ignored {unknown} unknown path(s)")
        if duplicates:
            corrections.append(
                f"kept the first assignment for {duplicates} duplicate "
                "path(s)"
            )
        omitted = [path for path in target if path not in seen]
        if omitted:
            corrections.append(
                f"added {len(omitted)} omitted file(s) in lexical chunks"
            )
            groups.extend(chunk_paths(sorted(omitted)))
        split_groups: List[List[str]] = []
        oversized = 0
        for group in groups:
            chunks = chunk_paths(group)
            if len(chunks) > 1:
                oversized += 1
            split_groups.extend(chunks)
        if oversized:
            corrections.append(f"split {oversized} oversized group(s)")
        groups = split_groups
        if not groups:
            fallback = "lexical"
            corrections.append(
                "the grouping result assigned no target files; fell back to "
                "lexical chunks"
            )
            groups = chunk_paths(sorted(target))
    else:
        fallback = "lexical"
        groups = chunk_paths(sorted(target))

    flat = [path for group in groups for path in group]
    if sorted(flat) != sorted(target) or len(flat) != len(set(flat)):
        raise WorkflowDataError(
            "file grouping lost exact target coverage; refusing to plan"
        )
    groups.sort(key=lambda group: group[0])
    return groups, fallback, corrections


def build_rule_audit_cells(
    state: Mapping[str, Any],
    groups: Sequence[Sequence[str]],
) -> List[Dict[str, Any]]:
    """Pack files sharing one effective check set into audit cells.

    Packing is across the whole target, not within semantic groups: a rule
    audit checks each file against the same guidance regardless of its
    neighbors, so grouping only multiplied cells (calibration measured
    rule cells as 43% of finder agents for 11% of candidates). Cells are
    deterministic -- lexical files per check set, the ten-file and size
    caps applied -- and ordered by check set, then first file.
    """
    rules_state = state.get("rules") or {}
    effective: Mapping[str, Sequence[str]] = rules_state.get("effective") or {}
    by_check_set: Dict[Tuple[str, ...], List[str]] = {}
    for path in sorted(path for group in groups for path in group):
        check_ids = tuple(effective.get(path) or ())
        if check_ids:
            by_check_set.setdefault(check_ids, []).append(path)
    cells: List[Dict[str, Any]] = []
    for check_ids in sorted(by_check_set):
        slice_count = -(-len(check_ids) // MAX_CHECKS_PER_CELL)
        slice_size = -(-len(check_ids) // slice_count)
        for start in range(0, len(check_ids), slice_size):
            check_slice = check_ids[start : start + slice_size]
            for chunk in chunk_paths(by_check_set[check_ids]):
                cells.append({"files": chunk, "check_ids": list(check_slice)})
    return cells


def build_discovery_jobs(
    state: Mapping[str, Any],
    groups: Sequence[Sequence[str]],
) -> List[Dict[str, Any]]:
    cell = EFFORT_CELLS[str(state["effort"])]
    target = common_target(state)
    candidate_cap = int(cell["per_angle_cap"])
    rules_state = state.get("rules") or {}
    catalog: Mapping[str, Mapping[str, Any]] = rules_state.get("catalog") or {}
    overridden: Mapping[str, Sequence[str]] = (
        rules_state.get("overridden") or {}
    )
    jobs: List[Dict[str, Any]] = []
    for index, group in enumerate(groups, 1):
        job_id = f"finder:local:{index:02d}"
        jobs.append(
            {
                "name": job_id,
                "job_id": job_id,
                "kind": "local-correctness",
                "files": list(group),
                "instructions": LOCAL_CORRECTNESS_INSTRUCTIONS,
                "stance": cell["stance"],
                "candidate_cap": candidate_cap,
                "target": target,
            }
        )
    # A collapsed small target keeps its local passes and rule audits (exact
    # coverage) and skips the whole-change fan-out.
    whole_change_angles = (
        () if state.get("collapsed") else WHOLE_CHANGE_ANGLES
    )
    for key, title, instructions in whole_change_angles:
        jobs.append(
            {
                "name": f"finder:angle:{key}",
                "job_id": f"finder:angle:{key}",
                "kind": "angle",
                "angle": {
                    "key": key,
                    "title": title,
                    "instructions": instructions,
                },
                "stance": cell["stance"],
                "candidate_cap": candidate_cap,
                "target": target,
            }
        )
    for index, audit_cell in enumerate(
        build_rule_audit_cells(state, groups), 1
    ):
        job_id = f"finder:rule:{index:02d}"
        checks = [
            dict(catalog[check_id])
            for check_id in audit_cell["check_ids"]
            if check_id in catalog
        ]
        resolution = {
            path: list(overridden[path])
            for path in audit_cell["files"]
            if path in overridden
        }
        jobs.append(
            {
                "name": job_id,
                "job_id": job_id,
                "kind": "rule-audit",
                "files": audit_cell["files"],
                "checks": checks,
                "overridden_builtin_checks": resolution,
                "stance": STANCE_RULE_AUDIT,
                "candidate_cap": candidate_cap,
                "target": target,
            }
        )
    return jobs


# --- Prepare -----------------------------------------------------------------


def build_finder_jobs(state: Mapping[str, Any]) -> List[Dict[str, Any]]:
    cell = EFFORT_CELLS[str(state["effort"])]
    target = common_target(state)
    jobs: List[Dict[str, Any]] = []
    for key, title, instructions in cell["angles"]:
        jobs.append(
            {
                "name": f"finder:{key}",
                "job_id": f"finder:{key}",
                "kind": "angle",
                "angle": {
                    "key": key,
                    "title": title,
                    "instructions": instructions,
                },
                "stance": cell["stance"],
                "candidate_cap": cell["per_angle_cap"],
                "target": target,
            }
        )
    return jobs


def prepare(args: argparse.Namespace) -> None:
    verify_schema_sources()
    CONTROL_DIR.mkdir(parents=True, exist_ok=True)
    started_at = (
        datetime.now(timezone.utc).replace(microsecond=0).isoformat()
    )
    review_id = review_id_from_args(args)
    mode = args.mode.strip().lower()
    if mode not in REVIEW_MODES:
        raise WorkflowDataError(
            f"mode must be one of {', '.join(REVIEW_MODES)}, got {args.mode!r}"
        )
    effort = args.effort if args.effort in EFFORT_TIERS else "medium"
    if args.effort and args.effort not in EFFORT_TIERS:
        print(
            f'unknown effort "{one_line(args.effort, 60)}" -- using medium '
            f"(tiers: {', '.join(EFFORT_TIERS)})"
        )
    scope = parse_scope(args.scope)
    base_input = args.base.strip()
    commit_input = args.commit.strip()
    range_input = args.range.strip()
    publisher = import_publisher()
    try:
        incremental_requested = publisher.parse_incremental_flag(
            args.incremental
        )
    except publisher.PublishError as error:
        raise WorkflowDataError(str(error)) from error
    incremental_state: Dict[str, Any] = {"requested": False}
    if incremental_requested:
        if range_input or commit_input:
            raise WorkflowDataError(
                "incremental review resolves its own range; leave commit "
                "and range empty (base is allowed as the cold-start "
                "fallback)"
            )
        incremental_state = prepare_incremental(args, mode)
        if incremental_state.get("from"):
            # The delta wins; base is only the cold-start fallback. A
            # sandbox clone's default base resolves to the PR branch's
            # own upstream (an empty range), so PR runs pass the base
            # branch explicitly for the first, untagged review.
            range_input = f"{incremental_state['from']}..HEAD"
            base_input = ""
    model = one_line(args.model, 120)
    # Guidance reaches the finder and sweep prompts through their MiniJinja
    # templates; the engine only records it so the report says what steering
    # was applied.
    guidance = clean_text(args.guidance, 2000).strip()
    cell = EFFORT_CELLS[effort]

    revision_range: Optional[str] = None
    base: Optional[str] = None
    merge_base: Optional[str] = None
    target_commit: Optional[str] = None
    parent: Optional[str] = None
    changed_files: List[str] = []
    diff_lines: Optional[int] = None
    scope_file_count: Optional[int] = None
    file_records: Optional[Dict[str, Dict[str, Any]]] = None

    if mode == "changes":
        if not inside_git_worktree():
            raise WorkflowDataError("changes mode requires a Git worktree")
        if commit_input:
            raise WorkflowDataError(
                "changes mode does not accept commit; use mode=commit"
            )
        if range_input and base_input:
            raise WorkflowDataError(
                "changes mode accepts either base or an explicit range, not both"
            )
        if range_input:
            left, separator, right = parse_two_sided_range(range_input)
            left_commit = resolve_commit(left, "range start")
            target_commit = resolve_commit(right, "range end")
            revision_range = f"{left}{separator}{right}"
            base = left
            if separator == "...":
                merge_base = git_text("merge-base", left_commit, target_commit)
                if not merge_base:
                    raise WorkflowDataError(
                        "the explicit range endpoints have no merge base"
                    )
            else:
                merge_base = left_commit
        else:
            base = validate_revision(
                base_input or default_base_ref(),
                "base",
            )
            base_commit = resolve_commit(base, "base")
            target_commit = resolve_commit("HEAD", "HEAD")
            merge_base = git_text("merge-base", base_commit, target_commit)
            if not merge_base:
                raise WorkflowDataError(
                    f"base {base!r} and HEAD have no merge base"
                )
            revision_range = f"{merge_base}..HEAD"
        if cell["rule_mapped"]:
            file_records = diff_file_records(revision_range, scope)
            changed_files, diff_lines = diff_record_summary(file_records)
        else:
            changed_files, diff_lines = diff_stats(revision_range, scope)
    elif mode == "commit":
        if not inside_git_worktree():
            raise WorkflowDataError("commit mode requires a Git worktree")
        if not commit_input:
            raise WorkflowDataError("commit mode requires a commit input")
        if base_input or range_input:
            raise WorkflowDataError(
                "commit mode accepts commit only; base and range are not used"
            )
        target_commit = resolve_commit(commit_input, "commit")
        parent = git_text(
            "rev-parse",
            "--verify",
            "--quiet",
            target_commit + "^",
        )
        revision_range = f"{parent or empty_tree_hash()}..{target_commit}"
        if cell["rule_mapped"]:
            file_records = diff_file_records(revision_range, scope)
            changed_files, diff_lines = diff_record_summary(file_records)
        else:
            changed_files, diff_lines = diff_stats(revision_range, scope)
    else:
        if base_input or commit_input or range_input:
            raise WorkflowDataError(
                "files mode does not accept base, commit, or range inputs"
            )
        if not scope:
            raise WorkflowDataError(
                "files mode requires a scope naming the files to review"
            )
        if inside_git_worktree():
            changed_files = tracked_files(scope)
        else:
            changed_files = [
                path
                for path in repo_files()
                if any(
                    path == item
                    or path.startswith(item.rstrip("/") + "/")
                    for item in scope
                )
            ]
        scope_file_count = len(changed_files)

    diff_file_count = (
        len(changed_files) if mode in {"changes", "commit"} else None
    )
    empty_diff = mode in {"changes", "commit"} and diff_file_count == 0
    empty_scope = mode == "files" and scope_file_count == 0
    empty_target = empty_diff or empty_scope

    state: Dict[str, Any] = {
        "version": 1,
        "root": str(root()),
        "started_at": started_at,
        "review_id": review_id,
        "products_dir": None,
        "products_rel": None,
        "evidence_dir": None,
        "evidence_rel": None,
        "metadata_dir": None,
        "metadata_rel": None,
        "state_path": None,
        "mode": mode,
        "effort": effort,
        "model": model,
        "guidance": guidance,
        "scope": scope,
        "range": revision_range,
        "base": base,
        "merge_base": merge_base,
        "commit": target_commit,
        "parent": parent,
        "changed_files": changed_files,
        "diff_files": diff_file_count,
        "diff_lines": diff_lines,
        "scope_files": scope_file_count,
        "empty_diff": empty_diff,
        "empty_scope": empty_scope,
        "incremental": {
            key: value
            for key, value in incremental_state.items()
            if key != "history_comments"
        },
        "history_comments": list(
            incremental_state.get("history_comments") or []
        ),
        "use_verify": bool(cell["verify"]),
        "verify_bias": cell["bias"],
        "use_sweep": bool(cell["sweep"]),
        "per_angle_cap": int(cell["per_angle_cap"]),
        "report_cap": int(cell["report_cap"]),
        "verification_cap": int(cell["verification_cap"]),
        "rule_mapped": bool(cell["rule_mapped"]),
        "rule_layers": cell.get("rule_layers"),
        "collapsed": None,
        "phase_results": {},
        "phase_jobs": {},
    }

    if empty_target:
        save_state(state)
        reason = (
            "the committed range has no changed files"
            if empty_diff
            else "the scope resolves to no files"
        )
        print(f"Nothing to review: {reason}")
        emit(
            empty_target=True,
            empty_reason=reason,
            mode=mode,
            effort=effort,
        )
        return

    products_dir, products_rel = unique_report_dir()
    evidence_dir = products_dir / "evidence"
    metadata_dir = products_dir / "metadata"
    evidence_dir.mkdir()
    metadata_dir.mkdir()
    (products_dir / ".gitignore").write_text("*\n", encoding="utf-8")
    if incremental_state.get("enabled"):
        # Ship the history snapshot as a run artifact: every skip stays
        # replayable from the products directory.
        history_source = CONTROL_DIR / HISTORY_FILE_NAME
        (products_dir / HISTORY_FILE_NAME).write_bytes(
            history_source.read_bytes()
        )
        state["incremental"]["history_rel"] = (
            f"{products_rel}/{HISTORY_FILE_NAME}"
        )
    state["products_dir"] = products_dir.as_posix()
    state["products_rel"] = products_rel
    state["evidence_dir"] = evidence_dir.as_posix()
    state["evidence_rel"] = f"{products_rel}/evidence"
    state["metadata_dir"] = metadata_dir.as_posix()
    state["metadata_rel"] = f"{products_rel}/metadata"
    state["state_path"] = (metadata_dir / "state.json").as_posix()

    revision = revision_record(
        mode,
        target_commit,
        base,
        merge_base,
        parent,
        revision_range,
    )
    state["revision"] = revision
    review_meta = {
        "schema_version": CANONICAL_SCHEMA_VERSION,
        "started_at": started_at,
        "review_id": review_id,
        "review_root": str(root()),
        "metadata_dir": metadata_dir.as_posix(),
        "agent": "fabro:code-review",
        "mode": mode,
        "scope": scope,
        "effort": effort,
        "model": model,
        "guidance": guidance,
        "revision": revision,
        "revision_source": "self-reported",
        "range": revision_range,
    }
    write_json(metadata_dir / "review-meta.json", review_meta)
    state["workspace_digest"] = workspace_digest()

    if mode == "files":
        described = (
            f"{scope_file_count} file(s) in scope"
        )
    else:
        described = (
            f"{diff_file_count} changed file(s)"
            + (f", {diff_lines} line(s)" if diff_lines is not None else "")
        )

    if cell["rule_mapped"]:
        # Compile the rule configuration now (an invalid rule file must fail
        # here, before any review agent runs), then hand the target list to
        # the grouping pass. Finder jobs are planned by plan-finders after
        # grouping.
        if mode in {"changes", "commit"}:
            if file_records is None:
                raise WorkflowDataError(
                    "rule-mapped diff metadata was not prepared"
                )
        else:
            file_records = {path: {"status": "full"} for path in changed_files}
        compile_rule_state(state, file_records)
        if cell.get("collapse"):
            small_diff = (
                mode in {"changes", "commit"}
                and diff_file_count is not None
                and 0 < diff_file_count <= SMALL_DIFF_MAX_FILES
                and diff_lines is not None
                and diff_lines <= SMALL_DIFF_MAX_LINES
            )
            small_scope = (
                mode == "files"
                and scope_file_count is not None
                and 0 < scope_file_count <= SMALL_SCOPE_MAX_FILES
            )
            state["collapsed"] = (
                "small-diff"
                if small_diff
                else ("small-scope" if small_scope else None)
            )
        grouping_mode = str(cell.get("grouping") or "lexical")
        use_grouping = grouping_mode == "semantic" and len(changed_files) > 1
        state["use_grouping"] = use_grouping
        state["use_planner"] = True
        state["grouping"] = {
            "mode": grouping_mode,
            "planned": use_grouping,
            "agent_returned": False,
            "fallback": None,
            "corrections": [],
            "groups": [],
        }
        assignment = {
            "files": [
                {
                    "path": path,
                    "status": str(
                        (file_records.get(path) or {}).get("status") or "M"
                    ),
                    "added": (file_records.get(path) or {}).get("added"),
                    "deleted": (file_records.get(path) or {}).get("deleted"),
                }
                for path in sorted(changed_files)
            ],
            "max_files_per_group": GROUP_MAX_FILES,
            "mode": mode,
        }
        state["grouping_assignment"] = assignment
        state["finder_jobs"] = []
        set_phase_jobs(state, "finders", [])
        save_state(state)
        rule_counts = state["rules"]["counts"]
        print(
            f"Prepared {effort} {mode} code review: {described}; "
            f"{rule_counts['builtin_packs']} built-in and "
            f"{rule_counts['repo_packs']} repository rule pack(s) compiled; "
            "grouping "
            + ("dispatched" if use_grouping else grouping_mode)
            + (
                f"; collapsed ({state['collapsed']})"
                if state.get("collapsed")
                else ""
            )
        )
        emit(
            empty_target=False,
            mode=mode,
            effort=effort,
            products_dir=products_rel,
            use_grouping=use_grouping,
            use_planner=True,
            grouping_assignment=assignment,
        )
        return

    state["use_grouping"] = False
    state["use_planner"] = False
    finder_jobs = build_finder_jobs(state)
    state["finder_jobs"] = finder_jobs
    set_phase_jobs(state, "finders", finder_jobs)
    save_state(state)

    print(
        f"Prepared {effort} {mode} code review: {described}; "
        f"{len(finder_jobs)} finder angle(s), verify="
        + ("on" if state["use_verify"] else "off")
        + ", sweep="
        + ("on" if state["use_sweep"] else "off")
    )
    emit(
        empty_target=False,
        mode=mode,
        effort=effort,
        products_dir=products_rel,
        use_grouping=False,
        use_planner=False,
        **phase_jobs_context(state, "finders", finder_jobs),
    )


# --- Phase job bookkeeping ---------------------------------------------------


def set_phase_jobs(
    state: Dict[str, Any],
    phase: str,
    jobs: Sequence[Mapping[str, Any]],
) -> None:
    values = [dict(job) for job in jobs]
    state.setdefault("phase_jobs", {})[phase] = values
    state.setdefault("phase_results", {}).setdefault(phase, {})


def phase_jobs_context(
    state: Mapping[str, Any],
    phase: str,
    jobs: Sequence[Mapping[str, Any]],
) -> Dict[str, Any]:
    del state
    key = PHASE_JOB_KEYS[phase]
    # Fabro's parallel handler requires the context value itself to be an
    # array. Fabro offloads large values and hydrates them before `for_each`.
    return {key: [dict(job) for job in jobs]}


# --- Finding and verdict normalization ---------------------------------------


def bounded_rule_ids(values: Iterable[Any]) -> List[str]:
    """Return the renderer-safe, deterministic union of compiled rule IDs."""
    return sorted({value for value in values if isinstance(value, str)})[
        :MAX_RULE_IDS_PER_FINDING
    ]


def finding_or_rejection(
    value: Any,
    rule_context: Optional[Mapping[str, Any]] = None,
) -> Tuple[Optional[Dict[str, Any]], Optional[str]]:
    """Normalize one reported finding, or say which part of the contract failed.

    A rejected finding is dropped from the review, so the reason travels with
    the rejection into coverage. Reasons are fixed strings: they name the
    field, and never quote the model's own text back into a report.

    ``rule_context`` (rule-mapped tiers) carries ``require`` -- whether this
    result came from a rule-audit job, which must name a violated check -- and
    ``effective``, the engine's file-to-check-ID map. The engine is
    authoritative about applicability: a named check that does not apply to
    the finding's file is rejected. This also enforces the anchor rule: a
    finding must anchor in a changed file its check applies to.
    """
    if not isinstance(value, dict):
        return None, "the finding is not a JSON object"
    path = normalize_repo_path(value.get("file"))
    legacy_line = value.get("line")
    start_line = value.get("start_line", legacy_line)
    end_line = value.get("end_line", legacy_line)
    summary = one_line(value.get("summary"), 600).strip()
    short_summary = one_line(value.get("short_summary"), 200).strip()[:60]
    failure_scenario = clean_text(value.get("failure_scenario"), 4000).strip()
    category = one_line(value.get("category"), 40).strip().lower()
    issue_type = one_line(value.get("issue_type"), 40).strip().lower()
    severity = one_line(value.get("severity"), 20).upper()
    confidence = one_line(value.get("confidence"), 20).upper()
    raw_suggestion = value.get("suggestion_code", "")
    for failed, reason in (
        (path is None or path == ".", "file does not name a repository file"),
        (
            isinstance(start_line, bool)
            or not isinstance(start_line, int)
            or start_line < 1,
            "start_line is not a positive integer",
        ),
        (
            isinstance(end_line, bool)
            or not isinstance(end_line, int)
            or end_line < 1,
            "end_line is not a positive integer",
        ),
        (
            isinstance(start_line, int)
            and isinstance(end_line, int)
            and start_line > end_line,
            "start_line is after end_line",
        ),
        (
            isinstance(start_line, int)
            and isinstance(end_line, int)
            and end_line - start_line + 1 > LOCATION_MAX_LINES,
            f"location spans more than {LOCATION_MAX_LINES} lines",
        ),
        (not summary, "summary is empty"),
        (not failure_scenario, "failure_scenario is empty"),
        (category not in CATEGORIES, "category is not in the closed list"),
        (issue_type not in ISSUE_TYPES, "issue_type is not in the closed list"),
        (severity not in SEVERITY_RANK, "severity is not HIGH, MEDIUM, or LOW"),
        (
            confidence not in CONFIDENCE_RANK,
            "confidence is not HIGH, MEDIUM, or LOW",
        ),
        (
            not isinstance(raw_suggestion, str),
            "suggestion_code is not a string",
        ),
        (
            isinstance(raw_suggestion, str)
            and len(raw_suggestion) > SUGGESTION_CODE_MAX_LENGTH,
            f"suggestion_code exceeds {SUGGESTION_CODE_MAX_LENGTH} characters",
        ),
        (
            isinstance(raw_suggestion, str)
            and any(
                character not in "\n\t" and ord(character) < 0x20
                for character in raw_suggestion
            ),
            "suggestion_code contains control characters",
        ),
    ):
        if failed:
            return None, reason
    if not short_summary:
        short_summary = summary[:60]

    # Agents report one "rule_id"; normalized findings carry "rule_ids".
    # Accepting both lets stored findings re-normalize without loss.
    raw_rule_ids: List[Any] = []
    if isinstance(value.get("rule_ids"), list):
        raw_rule_ids = list(value["rule_ids"])
    elif value.get("rule_id") not in (None, ""):
        raw_rule_ids = [value.get("rule_id")]
    rule_ids: List[str] = []
    if rule_context is not None:
        if not raw_rule_ids and rule_context.get("require"):
            return None, "a rule-audit finding names no rule check"
        effective = rule_context.get("effective") or {}
        for raw_rule_id in raw_rule_ids:
            if not isinstance(raw_rule_id, str) or not (
                COMPILED_RULE_ID_RE.fullmatch(raw_rule_id)
            ):
                return None, "rule_id is not a compiled check ID"
            if raw_rule_id not in (effective.get(path) or ()):
                return None, "the named rule check does not apply to the file"
        rule_ids = bounded_rule_ids(raw_rule_ids)
        if len(set(raw_rule_ids)) > MAX_RULE_IDS_PER_FINDING:
            return None, (
                "rule_ids names more than "
                f"{MAX_RULE_IDS_PER_FINDING} distinct checks"
            )
    if category == "conventions" and not rule_ids:
        return None, CONVENTIONS_FILTER_REASON

    return {
        "file": path,
        # ``line`` remains the stable end-line alias used by ranking,
        # deduplication, and older consumers. The canonical finding also
        # carries the complete range.
        "line": end_line,
        "start_line": start_line,
        "end_line": end_line,
        "summary": summary,
        "short_summary": short_summary,
        "failure_scenario": failure_scenario,
        "category": category,
        "issue_type": issue_type,
        "severity": severity,
        "confidence": confidence,
        "suggestion_code": raw_suggestion if raw_suggestion.strip() else "",
        "rule_ids": rule_ids,
    }, None


def findings_and_rejections(
    value: Any,
    rule_context: Optional[Mapping[str, Any]] = None,
) -> Tuple[Optional[Dict[str, Any]], List[str], List[str]]:
    """Split a finder result into findings, contract rejections, and filters."""
    if not isinstance(value, dict) or not isinstance(value.get("findings"), list):
        return None, [], []
    findings: List[Dict[str, Any]] = []
    rejections: List[str] = []
    filtered: List[str] = []
    for position, raw in enumerate(value["findings"], 1):
        finding, reason = finding_or_rejection(raw, rule_context)
        if finding is not None:
            findings.append(finding)
        elif reason in POLICY_FILTER_REASONS:
            filtered.append(f"finding {position}: {reason}")
        else:
            rejections.append(f"finding {position}: {reason}")
    return {"findings": findings}, rejections, filtered


def normalize_findings_result(
    value: Any,
    rule_context: Optional[Mapping[str, Any]] = None,
) -> Optional[Dict[str, Any]]:
    result, _rejections, _filtered = findings_and_rejections(value, rule_context)
    return result


def state_rule_context(
    state: Mapping[str, Any],
    require: bool,
) -> Optional[Dict[str, Any]]:
    rules_state = state.get("rules")
    if not isinstance(rules_state, dict) or not rules_state.get("enabled"):
        return None
    return {
        "require": require,
        "effective": rules_state.get("effective") or {},
    }


def normalize_verdict(value: Any) -> Optional[Dict[str, Any]]:
    if not isinstance(value, dict):
        return None
    verdict = value.get("verdict")
    if verdict not in VERDICTS:
        return None
    if not isinstance(value.get("reasoning"), str):
        return None
    result = {
        "verdict": verdict,
        "reasoning": clean_text(value.get("reasoning"), 4000),
    }
    duplicate_of = value.get("duplicate_of")
    if isinstance(duplicate_of, str):
        token = duplicate_of.strip()
        # Same-run candidates (F/S) and historical siblings (H, P3 item
        # 5) are both addressable; anything else is silently dropped.
        if CANDIDATE_ID_RE.fullmatch(token) or HISTORICAL_ID_RE.fullmatch(
            token
        ):
            result["duplicate_of"] = token
    suggestion_valid = value.get("suggestion_valid")
    if isinstance(suggestion_valid, bool):
        result["suggestion_valid"] = suggestion_valid
    return result


# --- Parallel merges ---------------------------------------------------------


def read_merge_input() -> Any:
    raw = sys.stdin.buffer.read(MAX_STDIN_BYTES + 1)
    if len(raw) > MAX_STDIN_BYTES:
        raise WorkflowDataError(
            f"merge input exceeds the {MAX_STDIN_BYTES}-byte limit"
        )
    try:
        return json.loads(raw.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as error:
        raise WorkflowDataError(
            f"merge stdin is not valid JSON: {error}"
        ) from error


def merge_phase(
    state: Dict[str, Any],
    phase: str,
    raw_results: Any,
) -> Dict[str, Any]:
    if phase not in PHASE_OUTPUT_KEYS:
        raise WorkflowDataError(f"unknown parallel merge phase: {phase}")
    if not isinstance(raw_results, list):
        raise WorkflowDataError("parallel merge input must be a JSON array")
    jobs = (
        state.get("phase_jobs", {}).get(phase)
        if isinstance(state.get("phase_jobs"), dict)
        else None
    )
    if not isinstance(jobs, list):
        raise WorkflowDataError(f"{phase} merge jobs are missing from state")
    result_map = state.setdefault("phase_results", {}).setdefault(phase, {})
    if not isinstance(result_map, dict):
        raise WorkflowDataError(f"{phase} result accumulator is invalid")
    for position, branch in enumerate(raw_results):
        if position >= len(jobs) or not isinstance(branch, dict):
            continue
        branch_index = branch.get("index")
        if (
            branch_index is not None
            and (
                isinstance(branch_index, bool)
                or not isinstance(branch_index, int)
                or branch_index != position
            )
        ):
            continue
        updates = branch.get("context_updates")
        if not isinstance(updates, dict):
            continue
        value = updates.get(PHASE_OUTPUT_KEYS[phase])
        job = jobs[position]
        if not isinstance(job, dict):
            continue
        rejections: List[str] = []
        filtered: List[str] = []
        if phase == "finders":
            # Rejections are recorded here, where the agent's raw output is
            # first seen. Later steps re-normalize an already-clean result and
            # would find nothing to report.
            normalized, rejections, filtered = findings_and_rejections(
                value,
                state_rule_context(
                    state, require=job.get("kind") == "rule-audit"
                ),
            )
        else:
            normalized = normalize_verdict(value)
        if normalized is None:
            continue
        job_id = job.get("job_id")
        if isinstance(job_id, str) and job_id:
            if job_id not in result_map:
                result_map[job_id] = normalized
                for key, reasons in (
                    ("rejected_findings", rejections),
                    ("filtered_findings", filtered),
                ):
                    if reasons:
                        state.setdefault(key, {})[job_id] = [
                            f"{one_line(job.get('name'), 200)}: {reason}"
                            for reason in reasons
                        ]
    return {f"{phase}_results_merged": len(result_map)}


def merge_grouping(state: Dict[str, Any], raw: Any) -> Dict[str, Any]:
    """Record the grouping agent's proposal after minimal normalization.

    Unusable output is not an error: plan-finders falls back to
    deterministic lexical chunks and coverage records the degradation.
    """
    groups_raw: Optional[List[List[str]]] = None
    if isinstance(raw, dict) and isinstance(raw.get("groups"), list):
        collected: List[List[str]] = []
        for entry in raw["groups"]:
            files = entry.get("files") if isinstance(entry, dict) else None
            if not isinstance(files, list):
                continue
            paths: List[str] = []
            for item in files:
                normalized = normalize_repo_path(item)
                if normalized is not None and normalized != ".":
                    paths.append(normalized)
            if paths:
                collected.append(paths)
        if collected:
            groups_raw = collected
    grouping = state.setdefault("grouping", {})
    if isinstance(grouping, dict):
        grouping["agent_returned"] = groups_raw is not None
    state["grouping_raw"] = groups_raw
    return {"grouping_merged": groups_raw is not None}


def plan_finders() -> None:
    """Finalize file groups and build the rule-mapped discovery jobs."""
    state = load_state()
    if not state.get("rule_mapped"):
        raise WorkflowDataError(
            "plan-finders only runs for the rule-mapped tiers"
        )
    grouping = state.get("grouping")
    if not isinstance(grouping, dict):
        grouping = {}
    raw = state.get("grouping_raw")
    groups, fallback, corrections = finalize_groups(state, raw)
    if not grouping.get("planned"):
        # A single-file target never dispatched the grouping agent; its
        # lexical group is the plan, not a degradation.
        fallback = None
        corrections = []
    jobs = build_discovery_jobs(state, groups)
    if len(jobs) > DISCOVERY_JOB_CEILING and fallback is None:
        # Grouping degradation alone never trips the ceiling: lexical
        # chunking is the densest exact-coverage packing.
        lexical = chunk_paths(sorted(state.get("changed_files") or []))
        lexical_jobs = build_discovery_jobs(state, lexical)
        if len(lexical_jobs) <= DISCOVERY_JOB_CEILING:
            groups, jobs = lexical, lexical_jobs
            fallback = "lexical"
            corrections.append("regrouped lexically to fit the job ceiling")
    if len(jobs) > DISCOVERY_JOB_CEILING:
        raise WorkflowDataError(
            f"exact coverage needs {len(jobs)} discovery jobs, over the "
            f"{DISCOVERY_JOB_CEILING}-job discovery ceiling. Narrow the review "
            "scope and run again; files and rule checks are never silently "
            "omitted"
        )
    grouping.update(
        {"fallback": fallback, "corrections": corrections, "groups": groups}
    )
    state["grouping"] = grouping
    state["finder_jobs"] = jobs
    set_phase_jobs(state, "finders", jobs)
    save_state(state)
    by_kind: Dict[str, int] = {}
    for job in jobs:
        by_kind[str(job.get("kind"))] = by_kind.get(str(job.get("kind")), 0) + 1
    print(
        f"Planned {len(jobs)} discovery job(s): "
        f"{by_kind.get('local-correctness', 0)} local-correctness, "
        f"{by_kind.get('angle', 0)} whole-change angle(s), "
        f"{by_kind.get('rule-audit', 0)} rule-audit cell(s)"
        + (f"; grouping fallback: {fallback}" if fallback else "")
    )
    emit(**phase_jobs_context(state, "finders", jobs))


def merge_sweep(state: Dict[str, Any], raw: Any) -> Dict[str, Any]:
    """Record the single sweeper's result and plan verification of what's new.

    Sweep candidates are deduplicated against every candidate already seen --
    kept or not -- so a candidate the panel already refuted cannot reappear
    through the sweep.
    """
    normalized, rejections, filtered = findings_and_rejections(
        raw, state_rule_context(state, require=False)
    )
    if normalized is None:
        state["sweep_returned"] = False
        state["sweep_candidates"] = []
        state["sweep_verify_jobs"] = []
        state["run_sweep_verify"] = False
        set_phase_jobs(state, "sweep_verify", [])
        return {"run_sweep_verify": False}
    for key, reasons in (
        ("rejected_findings", rejections),
        ("filtered_findings", filtered),
    ):
        if reasons:
            state.setdefault(key, {})["sweep"] = [
                f"sweep: {reason}" for reason in reasons
            ]
    seen = {
        candidate_key(candidate)
        for candidate in state.get("candidates") or []
    }
    fresh: List[Dict[str, Any]] = []
    for finding in normalized["findings"]:
        key = candidate_key(finding)
        if key in seen:
            continue
        seen.add(key)
        copy = dict(finding)
        copy["reports"] = 1
        copy["source"] = "sweep"
        fresh.append(copy)
    fresh.sort(key=rank_key)
    fresh = fresh[:SWEEP_CANDIDATE_CAP]
    for index, candidate in enumerate(fresh, 1):
        candidate["id"] = f"S{index}"

    use_verify = bool(state.get("use_verify"))
    # A sweep candidate's siblings include the kept finder findings, so a
    # re-found defect can be folded into the finding already on the list.
    kept_finder = [
        record["candidate"]
        for record in state.get("reviewed") or []
        if isinstance(record, dict) and record.get("kept")
    ]
    jobs = (
        build_verify_jobs(state, fresh, "sweep_verify", pool=fresh + kept_finder)
        if use_verify
        else []
    )
    state["sweep_returned"] = True
    state["sweep_candidates"] = fresh
    state["sweep_verify_jobs"] = jobs
    state["run_sweep_verify"] = bool(jobs)
    set_phase_jobs(state, "sweep_verify", jobs)
    updates: Dict[str, Any] = {"run_sweep_verify": bool(jobs)}
    if jobs:
        updates.update(phase_jobs_context(state, "sweep_verify", jobs))
    return updates


def merge(phase: str) -> None:
    state = load_state()
    raw = read_merge_input()
    if phase == "grouping":
        updates = merge_grouping(state, raw)
        print(
            "Merged grouping: "
            + (
                f"{len(state.get('grouping_raw') or [])} proposed group(s)"
                if state.get("grouping_raw")
                else "no usable grouping; plan-finders will fall back to "
                "lexical chunks"
            )
        )
    elif phase == "sweep":
        updates = merge_sweep(state, raw)
        print(
            f"Merged sweep: {len(state.get('sweep_candidates') or [])} fresh "
            f"candidate(s), {len(state.get('sweep_verify_jobs') or [])} "
            "verification job(s)"
        )
    else:
        updates = merge_phase(state, phase, raw)
        print(
            f"Merged {phase}: "
            f"{updates[f'{phase}_results_merged']} result(s) recorded"
        )
    if phase in PHASE_OUTPUT_KEYS:
        # The merge has copied every usable branch output into canonical
        # state. Do not let the next fan-out inherit this fan-in payload.
        # Fabro includes inherited context changes in each branch result, so
        # retaining an earlier parallel.results array multiplies it by the
        # next branch count and can exceed the checkpoint request limit.
        updates["parallel.results"] = []
    save_state(state)
    emit(**updates)


# --- Candidate planning and verification -------------------------------------


def candidate_key(finding: Mapping[str, Any]) -> str:
    """The deduplication identity: normalized file, line, and category."""
    return "\0".join(
        [
            str(finding.get("file")),
            str(finding.get("line")),
            str(finding.get("category")),
        ]
    )


def category_class(finding: Mapping[str, Any]) -> int:
    return 0 if finding.get("category") == "correctness" else 1


def rank_key(finding: Mapping[str, Any]) -> Tuple[Any, ...]:
    return (
        category_class(finding),
        -SEVERITY_RANK.get(str(finding.get("severity")), 0),
        -int(finding.get("reports") or 1),
        -CONFIDENCE_RANK.get(str(finding.get("confidence")), 0),
        str(finding.get("file")),
        int(finding.get("line") or 0),
        str(finding.get("category")),
    )


def sibling_claims(
    candidate: Mapping[str, Any],
    pool: Sequence[Mapping[str, Any]],
) -> List[Dict[str, Any]]:
    """Other candidates in the same file, nearest by line first."""
    line = int(candidate.get("line") or 0)
    same_file = [
        other
        for other in pool
        if other.get("file") == candidate.get("file")
        and other.get("id") != candidate.get("id")
    ]
    same_file.sort(
        key=lambda other: (
            abs(int(other.get("line") or 0) - line),
            str(other.get("id")),
        )
    )
    return [
        {
            "id": other.get("id"),
            "line": other.get("line"),
            "category": other.get("category"),
            "short_summary": other.get("short_summary"),
        }
        for other in same_file[:SIBLING_CAP]
    ]


def historical_sibling_claims(
    candidate: Mapping[str, Any],
    state: Optional[Mapping[str, Any]],
) -> List[Dict[str, Any]]:
    """Previously posted findings on this PR, as extra verifier siblings.

    Same-file only, nearest by live-mapped line first, under their own
    bound (HISTORICAL_SIBLING_CAP) so they never displace same-run
    siblings. A comment without a mapped span or a parseable meta tag
    (a pre-P3 comment) is skipped.
    """
    comments = (state or {}).get("history_comments") or []
    line = int(candidate.get("line") or 0)
    entries: List[Dict[str, Any]] = []
    for comment in comments:
        if not isinstance(comment, dict):
            continue
        span = comment.get("mapped_span")
        meta = comment.get("meta")
        if not isinstance(span, dict) or not isinstance(meta, dict):
            continue
        if span.get("path") != candidate.get("file"):
            continue
        entries.append(
            {
                "id": f"H{comment.get('id')}",
                "line": span.get("end_line"),
                "category": meta.get("category"),
                "short_summary": one_line(meta.get("short_summary"), 200),
            }
        )
    entries.sort(
        key=lambda entry: (
            abs(int(entry.get("line") or 0) - line),
            str(entry.get("id")),
        )
    )
    return entries[:HISTORICAL_SIBLING_CAP]


def verification_claim(
    candidate: Mapping[str, Any],
    state: Optional[Mapping[str, Any]] = None,
    pool: Optional[Sequence[Mapping[str, Any]]] = None,
) -> Dict[str, Any]:
    """The subset of a candidate a verifier is shown.

    The reporter's claim only. The reporter's confidence is withheld -- it
    could anchor a verifier that must judge the claim on the code. At the
    rule-mapped tiers the claim also carries the claimed rule IDs and every
    effective check for the candidate's file; the engine stays authoritative
    about applicability, and the verifier judges only violation. ``pool``
    supplies the same-file siblings the verifier may name as duplicates.
    """
    start_line = int(candidate.get("start_line") or candidate.get("line") or 0)
    end_line = int(candidate.get("end_line") or candidate.get("line") or 0)
    location = resolved_location(
        str(candidate.get("file") or ""), start_line, end_line, state
    )
    claim: Dict[str, Any] = {
        "file": candidate.get("file"),
        "line": candidate.get("line"),
        "location": location,
        "category": candidate.get("category"),
        "issue_type": candidate.get("issue_type"),
        "severityAsReported": candidate.get("severity"),
        "summary": candidate.get("summary"),
        "failure_scenario": candidate.get("failure_scenario"),
        "reports": int(candidate.get("reports") or 1),
        "siblings": sibling_claims(candidate, pool or [])
        + historical_sibling_claims(candidate, state),
    }
    suggestion_code = str(candidate.get("suggestion_code") or "")
    if suggestion_code and location["existing_code"]:
        claim["suggestion"] = {"replacement_code": suggestion_code}
    rules_state = (state or {}).get("rules")
    if isinstance(rules_state, dict) and rules_state.get("enabled"):
        catalog = rules_state.get("catalog") or {}
        effective_ids = (rules_state.get("effective") or {}).get(
            str(candidate.get("file")), []
        )
        claim["rule_ids"] = list(candidate.get("rule_ids") or [])
        claim["effective_checks"] = [
            {
                "id": check["id"],
                "category": check["category"],
                "guidance": check["guidance"],
                "source": check["source"],
                "pack": check["pack"],
                "pattern": check["pattern"],
            }
            for check in (
                catalog.get(check_id) for check_id in effective_ids
            )
            if isinstance(check, dict)
        ]
    return claim


def build_verify_jobs(
    state: Mapping[str, Any],
    candidates: Sequence[Mapping[str, Any]],
    phase: str,
    pool: Optional[Sequence[Mapping[str, Any]]] = None,
) -> List[Dict[str, Any]]:
    bias = str(state.get("verify_bias") or "standard")
    target = common_target(state)
    prefix = "verify" if phase == "verify" else "sweep-verify"
    siblings_pool = list(pool if pool is not None else candidates)
    jobs: List[Dict[str, Any]] = []
    for candidate in candidates:
        jobs.append(
            {
                "name": f"{prefix}:{candidate['id']}",
                "job_id": f"{prefix}:{candidate['id']}",
                "candidate_id": candidate["id"],
                "claim": verification_claim(candidate, state, siblings_pool),
                "bias": bias,
                "target": target,
            }
        )
    return jobs


def stored_claim(
    state: Mapping[str, Any],
    phase: str,
    candidate: Mapping[str, Any],
) -> Dict[str, Any]:
    """The exact claim a verifier was shown, from the dispatched job."""
    for job in (state.get("phase_jobs") or {}).get(phase) or []:
        if isinstance(job, dict) and job.get("candidate_id") == candidate.get(
            "id"
        ) and isinstance(job.get("claim"), dict):
            return dict(job["claim"])
    return verification_claim(candidate, state)


def plan_verify() -> None:
    state = load_state()
    finder_jobs = list(state.get("finder_jobs") or [])
    finder_results = (
        state.get("phase_results", {}).get("finders", {})
        if isinstance(state.get("phase_results"), dict)
        else {}
    )
    rule_context = state_rule_context(state, require=False)
    raw_candidates: List[Dict[str, Any]] = []
    returned = 0
    invalid_results: List[str] = []
    kind_stats: Dict[str, Dict[str, Any]] = {}
    job_outcomes: Dict[str, str] = {}
    for job in finder_jobs:
        if not isinstance(job, dict):
            continue
        kind = str(job.get("kind") or "angle")
        stats = kind_stats.setdefault(
            kind, {"dispatched": 0, "returned": 0, "invalid": []}
        )
        stats["dispatched"] += 1
        job_id = str(job.get("job_id") or "")
        raw = (
            finder_results.get(job.get("job_id"))
            if isinstance(finder_results, dict)
            else None
        )
        normalized = normalize_findings_result(raw, rule_context)
        if normalized is None:
            invalid_results.append(one_line(job.get("name"), 200))
            stats["invalid"].append(one_line(job.get("name"), 200))
            job_outcomes[job_id] = "failed"
            continue
        returned += 1
        stats["returned"] += 1
        job_outcomes[job_id] = "returned"
        # A whole-change angle reports under its angle key; local and
        # rule-audit jobs report under their stable job IDs.
        if kind == "angle" and isinstance(job.get("angle"), dict):
            reporter = str(job["angle"].get("key") or "finder")
        else:
            reporter = job_id or "finder"
        job_cap = int(job.get("candidate_cap") or state.get("per_angle_cap") or 6)
        job_findings = sorted(normalized["findings"], key=rank_key)
        dropped_by_job_cap = max(0, len(job_findings) - job_cap)
        if dropped_by_job_cap:
            state.setdefault("job_cap_drops", {})[reporter] = (
                dropped_by_job_cap
            )
        for finding in job_findings[:job_cap]:
            copy = dict(finding)
            copy["angle"] = one_line(reporter, 60)
            raw_candidates.append(copy)

    # Two angles flagging the same line for different reasons stay separate
    # findings; the same defect reported twice under one category merges,
    # keeping the union of reporter job IDs and applicable rule IDs.
    by_key: Dict[str, Dict[str, Any]] = {}
    for report in sorted(raw_candidates, key=rank_key):
        key = candidate_key(report)
        existing = by_key.get(key)
        if existing is None:
            merged = dict(report)
            merged["reports"] = 1
            merged["reporters"] = [report["angle"]]
            merged["rule_ids"] = bounded_rule_ids(report.get("rule_ids") or [])
            by_key[key] = merged
            continue
        existing["reports"] += 1
        if report["angle"] not in existing["reporters"]:
            existing["reporters"].append(report["angle"])
        existing["rule_ids"] = bounded_rule_ids(
            list(existing.get("rule_ids") or [])
            + list(report.get("rule_ids") or [])
        )
        # A fix is publishable only when reporting passes agree on its exact
        # range and replacement. A pass that offers no fix does not veto an
        # otherwise consistent proposal.
        existing_suggestion = str(existing.get("suggestion_code") or "")
        report_suggestion = str(report.get("suggestion_code") or "")
        if not existing_suggestion and report_suggestion:
            existing["start_line"] = report["start_line"]
            existing["end_line"] = report["end_line"]
            existing["line"] = report["end_line"]
            existing["suggestion_code"] = report_suggestion
        elif existing_suggestion and report_suggestion and (
            existing_suggestion != report_suggestion
            or existing.get("start_line") != report.get("start_line")
            or existing.get("end_line") != report.get("end_line")
        ):
            existing["suggestion_code"] = ""
        if report.get("issue_type") == "security":
            existing["issue_type"] = "security"
        if (
            SEVERITY_RANK[report["severity"]]
            > SEVERITY_RANK[existing["severity"]]
        ):
            existing["severity"] = report["severity"]
        if (
            CONFIDENCE_RANK[report["confidence"]]
            > CONFIDENCE_RANK[existing["confidence"]]
        ):
            existing["confidence"] = report["confidence"]

    deduplicated = sorted(by_key.values(), key=rank_key)
    for index, candidate in enumerate(deduplicated, 1):
        candidate["id"] = f"F{index}"
        candidate["source"] = "finder"

    use_verify = bool(state.get("use_verify"))
    verification_cap = int(state.get("verification_cap") or 60)
    for_verification = deduplicated[:verification_cap]
    deferred_by_cap = max(0, len(deduplicated) - len(for_verification))
    state["finder_kind_stats"] = kind_stats
    state["finder_job_outcomes"] = job_outcomes
    verify_jobs = (
        build_verify_jobs(state, for_verification, "verify")
        if use_verify
        else []
    )

    state["raw_candidate_count"] = len(raw_candidates)
    state["candidates"] = deduplicated
    state["verification_candidates"] = for_verification if use_verify else []
    state["verification_deferred_by_cap"] = (
        deferred_by_cap if use_verify else 0
    )
    state["verify_jobs"] = verify_jobs
    state["run_verify"] = bool(verify_jobs)
    state["finders_dispatched"] = len(finder_jobs)
    state["finders_returned"] = returned
    state["invalid_finder_results"] = invalid_results
    set_phase_jobs(state, "verify", verify_jobs)
    save_state(state)
    updates: Dict[str, Any] = {"run_verify": bool(verify_jobs)}
    if verify_jobs:
        updates.update(phase_jobs_context(state, "verify", verify_jobs))
    if invalid_results:
        print(
            f"finders: {len(invalid_results)} of {len(finder_jobs)} finder "
            "agent(s) did not return a usable result"
        )
    if use_verify and deferred_by_cap:
        print(
            f"verification cap: {deferred_by_cap} lower-ranked candidate(s) "
            "will not be verified or reported -- the ledger records them as "
            "deferred"
        )
    print(
        f"Candidates: {len(raw_candidates)} raw -> "
        f"{len(deduplicated)} deduplicated; "
        f"{len(verify_jobs)} verification job(s)"
    )
    emit(**updates)


def candidate_verdict(
    state: Mapping[str, Any],
    phase: str,
    candidate: Mapping[str, Any],
) -> Optional[Dict[str, str]]:
    phase_results = state.get("phase_results")
    if not isinstance(phase_results, dict):
        return None
    results = phase_results.get(phase)
    if not isinstance(results, dict):
        return None
    prefix = "verify" if phase == "verify" else "sweep-verify"
    return normalize_verdict(results.get(f"{prefix}:{candidate.get('id')}"))


def apply_verdicts(
    state: Mapping[str, Any],
    phase: str,
    candidates: Sequence[Mapping[str, Any]],
) -> List[Dict[str, Any]]:
    """Attach each candidate's verdict and decide whether it is kept.

    The keep rule is the same at every verified tier: CONFIRMED and PLAUSIBLE
    survive, REFUTED drops, and a candidate whose verifier returned nothing is
    verification-incomplete and is not reported.
    """
    reviewed: List[Dict[str, Any]] = []
    for candidate in candidates:
        verdict = candidate_verdict(state, phase, candidate)
        record = {
            "candidate": dict(candidate),
            "verdict": verdict,
            "kept": verdict is not None
            and verdict["verdict"] in KEPT_VERDICTS,
        }
        reviewed.append(record)
    return reviewed


def sweep_coverage_summary(state: Mapping[str, Any]) -> Dict[str, Any]:
    """A compact map of what discovery covered, for the gap-fill sweep."""
    outcomes = state.get("finder_job_outcomes") or {}
    jobs = state.get("finder_jobs") or []
    grouping = state.get("grouping") if isinstance(state.get("grouping"), dict) else {}
    angle_returned: List[str] = []
    angle_failed: List[str] = []
    cells_returned: List[str] = []
    cells_failed: List[str] = []
    uncovered_files: set = set()
    uncovered_checks: set = set()
    for job in jobs:
        if not isinstance(job, dict):
            continue
        kind = str(job.get("kind") or "angle")
        job_id = str(job.get("job_id") or "")
        returned = outcomes.get(job_id) == "returned"
        if kind == "rule-audit":
            (cells_returned if returned else cells_failed).append(job_id)
            if not returned:
                uncovered_files.update(job.get("files") or [])
                uncovered_checks.update(
                    check.get("id")
                    for check in job.get("checks") or []
                    if isinstance(check, dict)
                )
        else:
            (angle_returned if returned else angle_failed).append(job_id)
            if not returned and kind == "local-correctness":
                uncovered_files.update(job.get("files") or [])
    return {
        "fileGroups": list(grouping.get("groups") or []),
        "angleJobs": {"returned": angle_returned, "failed": angle_failed},
        "ruleAuditCells": {
            "returned": cells_returned,
            "failed": cells_failed,
        },
        "uncoveredFiles": sorted(uncovered_files),
        "uncoveredCheckIds": sorted(
            check_id for check_id in uncovered_checks if check_id
        ),
    }


def tally() -> None:
    state = load_state()
    use_verify = bool(state.get("use_verify"))
    candidates = list(state.get("candidates") or [])
    if use_verify:
        for_verification = list(state.get("verification_candidates") or [])
        reviewed = apply_verdicts(state, "verify", for_verification)
    else:
        # Low effort skips verification by design; every candidate carries
        # through unverified, and the report says so.
        reviewed = [
            {"candidate": dict(candidate), "verdict": None, "kept": True}
            for candidate in candidates
        ]
    state["reviewed"] = reviewed
    kept = [record for record in reviewed if record["kept"]]

    run_sweep = bool(state.get("use_sweep"))
    state["run_sweep"] = run_sweep
    updates: Dict[str, Any] = {"run_sweep": run_sweep}
    if run_sweep:
        verified_summary = [
            {
                "id": record["candidate"].get("id"),
                "file": record["candidate"].get("file"),
                "line": record["candidate"].get("line"),
                "category": record["candidate"].get("category"),
                "short_summary": record["candidate"].get("short_summary"),
            }
            for record in kept
        ]
        sweep_assignment = {
            "verified": verified_summary,
            "candidate_cap": SWEEP_CANDIDATE_CAP,
            "focus": SWEEP_FOCUS,
            "stance": str(state.get("verify_bias") or "standard"),
            "target": common_target(state),
        }
        if state.get("rule_mapped"):
            sweep_assignment["coverage"] = sweep_coverage_summary(state)
        state["sweep_assignment"] = sweep_assignment
        updates["sweep_assignment"] = sweep_assignment
    save_state(state)
    verdict_count = sum(
        1 for record in reviewed if record.get("verdict") is not None
    )
    if use_verify:
        print(
            f"Verification returned {verdict_count} verdict(s) for "
            f"{len(reviewed)} candidate(s); {len(kept)} kept"
        )
    else:
        print(
            f"Low effort: verification skipped; {len(kept)} candidate(s) "
            "carry through unverified"
        )
    emit(**updates)


# --- Final assembly ----------------------------------------------------------


def code_frame_language(file_path: str) -> str:
    suffix = PurePosixPath(file_path).suffix.lstrip(".").lower()
    return CODE_FRAME_LANGUAGES.get(suffix, "Source")


def safe_code_text(value: str) -> str:
    text = "".join(
        character
        if character == "\t" or ord(character) >= 0x20
        else " "
        for character in value
    )
    if len(text) > CODE_FRAME_MAX_LINE_LENGTH:
        return text[:CODE_FRAME_MAX_LINE_LENGTH] + "..."
    return text


def reviewed_revision(state: Optional[Mapping[str, Any]]) -> Optional[str]:
    if not state or state.get("mode") not in {"changes", "commit"}:
        return None
    revision = state.get("commit")
    return revision if isinstance(revision, str) and revision else None


@functools.lru_cache(maxsize=16)
def reviewed_source_lines(
    file_path: str, revision: Optional[str] = None
) -> Optional[List[str]]:
    """Read one UTF-8 source file from the exact reviewed revision."""
    if revision is not None:
        tree_entry = git("ls-tree", "-z", revision, "--", file_path)
        if tree_entry.returncode != 0 or not tree_entry.stdout:
            return None
        mode = tree_entry.stdout.split(None, 1)[0]
        if mode == b"120000":
            return None
        raw = read_file_at_revision(revision, file_path)
        if raw is None or len(raw) > CODE_FRAME_MAX_BYTES:
            return None
    else:
        target = root() / file_path
        try:
            if target.is_symlink() or not target.is_file():
                return None
            if target.stat().st_size > CODE_FRAME_MAX_BYTES:
                return None
            raw = target.read_bytes()
        except OSError:
            return None
    if b"\0" in raw:
        return None
    try:
        return raw.decode("utf-8").splitlines()
    except UnicodeError:
        return None


def resolved_location(
    file_path: str,
    start_line: int,
    end_line: int,
    state: Optional[Mapping[str, Any]] = None,
) -> Dict[str, Any]:
    """Build an engine-derived exact anchor for a finding."""
    existing_code = ""
    source_lines = reviewed_source_lines(file_path, reviewed_revision(state))
    if (
        source_lines is not None
        and 1 <= start_line <= end_line <= len(source_lines)
    ):
        candidate = "\n".join(source_lines[start_line - 1:end_line])
        if (
            len(candidate) <= SUGGESTION_CODE_MAX_LENGTH
            and not any(
                character not in "\n\t" and ord(character) < 0x20
                for character in candidate
            )
        ):
            existing_code = candidate
    return {
        "start_line": start_line,
        "end_line": end_line,
        "existing_code": existing_code,
    }


def code_frame(
    file_path: str,
    start_line: int,
    end_line: Optional[int] = None,
    state: Optional[Mapping[str, Any]] = None,
) -> Dict[str, Any]:
    """Read the lines around a finding's anchor range from the reviewed tree.

    The excerpt shown in the report is read here, so its line numbers are the
    tree's own and no agent transcribes them. An unreadable, binary,
    oversized, or out-of-range target yields an empty excerpt.
    """
    end_line = start_line if end_line is None else end_line
    language = code_frame_language(file_path)
    empty: Dict[str, Any] = {
        "language": language,
        "label": f"{file_path}:{start_line}-{end_line}",
        "lines": [],
    }
    source_lines = reviewed_source_lines(file_path, reviewed_revision(state))
    if (
        source_lines is None
        or start_line < 1
        or end_line < start_line
        or end_line > len(source_lines)
    ):
        return empty
    start = max(1, start_line - CODE_FRAME_CONTEXT)
    end = min(len(source_lines), end_line + CODE_FRAME_CONTEXT)
    lines: List[Dict[str, Any]] = []
    for number in range(start, end + 1):
        entry: Dict[str, Any] = {
            "number": number,
            "text": safe_code_text(source_lines[number - 1]),
        }
        if start_line <= number <= end_line:
            entry["highlight"] = True
        lines.append(entry)
    return {
        "language": language,
        "label": f"{file_path}:{start}-{end}",
        "lines": lines,
    }


def reportable_finding(
    record: Mapping[str, Any],
    display_id: str,
    state: Optional[Mapping[str, Any]] = None,
) -> Dict[str, Any]:
    candidate = record["candidate"]
    verdict = record.get("verdict")
    start_line = int(candidate.get("start_line") or candidate["line"])
    end_line = int(candidate.get("end_line") or candidate["line"])
    location = resolved_location(
        candidate["file"], start_line, end_line, state
    )
    finding = {
        "id": display_id,
        "file": candidate["file"],
        "line": end_line,
        "location": location,
        "summary": candidate["summary"],
        "short_summary": candidate["short_summary"],
        "failure_scenario": candidate["failure_scenario"],
        "category": candidate["category"],
        "issue_type": candidate["issue_type"],
        "severity": candidate["severity"],
        "confidence": candidate["confidence"],
        "reports": int(candidate.get("reports") or 1),
        "reporters": list(candidate.get("reporters") or [])
        or [str(candidate.get("angle") or candidate.get("source") or "")],
        "rule_ids": list(candidate.get("rule_ids") or []),
        "anchors": list(candidate.get("anchors") or []),
        "source": candidate.get("source", "finder"),
        "verdict": verdict["verdict"] if verdict else "UNVERIFIED",
        "verdict_reasoning": verdict["reasoning"] if verdict else "",
        "code": code_frame(
            candidate["file"], start_line, end_line, state
        ),
    }
    if isinstance(candidate.get("duplicate_of_posted"), dict):
        finding["duplicate_of_posted"] = dict(candidate["duplicate_of_posted"])
    suggestion_code = str(candidate.get("suggestion_code") or "")
    if (
        suggestion_code
        and location["existing_code"]
        and verdict is not None
        and verdict.get("suggestion_valid") is True
        and suggestion_code != location["existing_code"]
    ):
        finding["suggestion"] = {"replacement_code": suggestion_code}
    return finding


def finding_reports(state: Mapping[str, Any], key: str) -> List[str]:
    """Flatten per-job rejection or filter reports in job-ID order."""
    by_job = state.get(key)
    if not isinstance(by_job, dict):
        return []
    reports: List[str] = []
    for job_id in sorted(by_job):
        entries = by_job[job_id]
        if isinstance(entries, list):
            reports.extend(str(entry) for entry in entries)
    return reports


def vote_records(
    state: Mapping[str, Any],
    reviewed: Sequence[Mapping[str, Any]],
    phase: str,
) -> List[Dict[str, Any]]:
    records: List[Dict[str, Any]] = []
    for record in reviewed:
        candidate = record["candidate"]
        verdict = record.get("verdict")
        entry: Dict[str, Any] = {
            "phase": phase,
            "candidate_id": candidate.get("id"),
            "claim": stored_claim(
                state, "verify" if phase == "verify" else "sweep_verify", candidate
            ),
            "bias": str(state.get("verify_bias") or "standard"),
            "completed": verdict is not None,
        }
        if verdict is not None:
            entry["verdict"] = verdict["verdict"]
            entry["reasoning"] = verdict["reasoning"]
            if verdict.get("duplicate_of"):
                entry["duplicate_of"] = verdict["duplicate_of"]
            if "suggestion_valid" in verdict:
                entry["suggestion_valid"] = verdict["suggestion_valid"]
        records.append(entry)
    return records


def reporter_kind(reporter: str) -> str:
    if reporter.startswith("finder:local:"):
        return "local-correctness"
    if reporter.startswith("finder:rule:"):
        return "rule-audit"
    if reporter == "sweep":
        return "sweep"
    return "angle"


def calibration_summary(
    state: Mapping[str, Any],
    candidates: Sequence[Mapping[str, Any]],
    ledger: Sequence[Mapping[str, Any]],
    votes: Sequence[Mapping[str, Any]],
    coverage: Mapping[str, Any],
) -> Dict[str, Any]:
    """A compact, aggregatable account of how the run's candidates fared.

    Emitted into the workflow context so calibration across many runs can
    read it from the event log without downloading bundles: dispositions
    and verdicts overall, then per reporter kind, per reporter, per rule
    check, and per category, plus rejection reasons and cap drops. Reasons
    and IDs are engine strings; no model text is included.
    """
    disposition_by_id = {
        str(entry.get("id")): str(entry.get("disposition"))
        for entry in ledger
    }

    def tally(bucket: Dict[str, int], disposition: str) -> None:
        bucket["candidates"] += 1
        if disposition in {"reportable", "deferred-by-cap", "duplicate"}:
            bucket["kept"] += 1
        elif disposition == "refuted":
            bucket["refuted"] += 1
        elif disposition == "verification-incomplete":
            bucket["incomplete"] += 1

    def fresh_bucket() -> Dict[str, int]:
        return {"candidates": 0, "kept": 0, "refuted": 0, "incomplete": 0}

    by_kind: Dict[str, Dict[str, int]] = {}
    by_reporter: Dict[str, Dict[str, int]] = {}
    by_rule: Dict[str, Dict[str, int]] = {}
    by_category: Dict[str, Dict[str, int]] = {}
    for candidate in candidates:
        disposition = disposition_by_id.get(str(candidate.get("id")), "")
        reporters = list(candidate.get("reporters") or []) or [
            str(candidate.get("source") or "finder")
        ]
        for reporter in reporters:
            tally(by_reporter.setdefault(reporter, fresh_bucket()), disposition)
            tally(
                by_kind.setdefault(reporter_kind(reporter), fresh_bucket()),
                disposition,
            )
        for rule_id in candidate.get("rule_ids") or []:
            tally(by_rule.setdefault(str(rule_id), fresh_bucket()), disposition)
        tally(
            by_category.setdefault(
                str(candidate.get("category")), fresh_bucket()
            ),
            disposition,
        )

    dispositions: Dict[str, int] = {}
    for entry in ledger:
        key = str(entry.get("disposition"))
        dispositions[key] = dispositions.get(key, 0) + 1
    verdicts: Dict[str, int] = {}
    for vote in votes:
        if vote.get("completed"):
            key = str(vote.get("verdict"))
            verdicts[key] = verdicts.get(key, 0) + 1
    rejections: Dict[str, int] = {}
    for report in coverage.get("rejectedFindingReports") or []:
        reason = str(report).rsplit(": ", 1)[-1]
        rejections[reason] = rejections.get(reason, 0) + 1
    filtered: Dict[str, int] = {}
    for report in coverage.get("filteredFindingReports") or []:
        reason = str(report).rsplit(": ", 1)[-1]
        filtered[reason] = filtered.get(reason, 0) + 1

    grouping = coverage.get("grouping") or {}
    rules = coverage.get("rules") or {}
    finders = coverage.get("finders") or {}
    caps = coverage.get("caps") or {}
    return {
        "effort": state.get("effort"),
        "mode": state.get("mode"),
        "model": state.get("model"),
        "targetFiles": len(state.get("changed_files") or []),
        "changedLines": state.get("diff_lines"),
        "collapsed": coverage.get("collapsed"),
        "grouping": {
            "mode": grouping.get("mode"),
            "fallback": grouping.get("fallback"),
            "groups": len(grouping.get("groups") or []),
        },
        "ruleLayers": rules.get("layers"),
        "jobs": {
            "dispatched": finders.get("dispatched", 0),
            "returned": finders.get("returned", 0),
            "byKind": {
                kind: {
                    "dispatched": stats.get("dispatched", 0),
                    "returned": stats.get("returned", 0),
                }
                for kind, stats in (finders.get("byKind") or {}).items()
            },
        },
        "candidates": {
            "raw": int(state.get("raw_candidate_count") or 0),
            "deduplicated": len(state.get("candidates") or []),
            "sweep": len(state.get("sweep_candidates") or []),
        },
        "dispositions": dispositions,
        "verdicts": verdicts,
        "byKind": by_kind,
        "byReporter": by_reporter,
        "byRule": by_rule,
        "byCategory": by_category,
        "rejections": rejections,
        "filtered": filtered,
        "caps": {
            "jobDrops": sum(
                int(value) for value in (caps.get("perJobDrops") or {}).values()
            ),
            "verificationDeferred": caps.get("verificationDeferred", 0),
            "reportDeferred": caps.get("reportDeferred", 0),
        },
    }


def fold_duplicates(
    state: Mapping[str, Any],
    kept_records: Sequence[Dict[str, Any]],
) -> Tuple[List[Dict[str, Any]], Dict[str, str]]:
    """Fold verified duplicates into the finding they duplicate.

    A verifier may name a sibling as ``duplicate_of``. The fold is
    deterministic: the named sibling must have been shown to that verifier
    and must itself have survived; the lower-ranked finding folds into the
    higher-ranked one (a mutual claim resolves the same way), chains follow
    to their surviving root, and the primary gains the secondary's anchor,
    reporters, rule IDs, and report count. Returns the surviving primaries,
    re-ranked, and the secondary-to-primary map.
    """
    allowed: Dict[str, set] = {}
    for phase in ("verify", "sweep_verify"):
        for job in (state.get("phase_jobs") or {}).get(phase) or []:
            if not isinstance(job, dict):
                continue
            siblings = (job.get("claim") or {}).get("siblings") or []
            allowed[str(job.get("candidate_id"))] = {
                str(sibling.get("id"))
                for sibling in siblings
                if isinstance(sibling, dict)
            }
    ordered = sorted(kept_records, key=lambda record: rank_key(record["candidate"]))
    by_id = {str(record["candidate"].get("id")): record for record in ordered}
    rank_index = {
        str(record["candidate"].get("id")): index
        for index, record in enumerate(ordered)
    }
    folded: Dict[str, str] = {}

    def root(candidate_id: str) -> str:
        seen = set()
        while candidate_id in folded and candidate_id not in seen:
            seen.add(candidate_id)
            candidate_id = folded[candidate_id]
        return candidate_id

    history_by_hid = {
        f"H{comment.get('id')}": comment
        for comment in state.get("history_comments") or []
        if isinstance(comment, dict)
    }

    for record in ordered:
        candidate_id = str(record["candidate"].get("id"))
        target = (record.get("verdict") or {}).get("duplicate_of")
        if not target:
            continue
        if HISTORICAL_ID_RE.fullmatch(str(target)):
            # A historical target (P3 item 5): the posted finding is not
            # in kept_records, so the shown-sibling check stays but the
            # survivor check does not apply. The candidate is not folded
            # away -- it is marked, kept in the bundle, and the publisher
            # places it as skipped (reason duplicate-of-posted).
            comment = history_by_hid.get(str(target))
            if (
                target in allowed.get(candidate_id, set())
                and comment is not None
            ):
                record["candidate"]["duplicate_of_posted"] = {
                    "comment_id": int(comment["id"]),
                    "html_url": str(comment.get("html_url") or ""),
                }
            continue
        if (
            target == candidate_id
            or target not in allowed.get(candidate_id, set())
            or target not in by_id
            or rank_index[target] > rank_index[candidate_id]
        ):
            continue
        primary_id = root(target)
        if primary_id != candidate_id:
            folded[candidate_id] = primary_id

    for secondary_id, primary_id in folded.items():
        primary = by_id[primary_id]["candidate"]
        secondary = by_id[secondary_id]["candidate"]
        primary["reports"] = int(primary.get("reports") or 1) + int(
            secondary.get("reports") or 1
        )
        reporters = list(primary.get("reporters") or [])
        for reporter in secondary.get("reporters") or [
            str(secondary.get("source") or "")
        ]:
            if reporter and reporter not in reporters:
                reporters.append(reporter)
        primary["reporters"] = reporters
        primary["rule_ids"] = bounded_rule_ids(
            list(primary.get("rule_ids") or [])
            + list(secondary.get("rule_ids") or [])
        )
        anchors = primary.setdefault("anchors", [])
        if len(anchors) < MAX_RULE_IDS_PER_FINDING:
            anchors.append(
                {
                    "id": secondary_id,
                    "file": secondary.get("file"),
                    "line": secondary.get("line"),
                    "category": secondary.get("category"),
                }
            )
    primaries = [
        record
        for record in ordered
        if str(record["candidate"].get("id")) not in folded
    ]
    for record in primaries:
        anchors = record["candidate"].get("anchors")
        if anchors:
            anchors.sort(key=lambda anchor: (str(anchor["file"]), int(anchor["line"])))
    primaries.sort(key=lambda record: rank_key(record["candidate"]))
    return primaries, folded


def final_tally() -> None:
    state = load_state()
    assert_workspace_unchanged(state)
    use_verify = bool(state.get("use_verify"))
    reviewed = list(state.get("reviewed") or [])
    sweep_candidates = list(state.get("sweep_candidates") or [])
    sweep_reviewed = (
        apply_verdicts(state, "sweep_verify", sweep_candidates)
        if state.get("run_sweep_verify")
        else [
            {"candidate": dict(candidate), "verdict": None, "kept": not use_verify}
            for candidate in sweep_candidates
        ]
    )
    state["sweep_reviewed"] = sweep_reviewed

    kept_records = [record for record in reviewed if record["kept"]]
    kept_records.extend(
        record for record in sweep_reviewed if record["kept"]
    )
    kept_records.sort(key=lambda record: rank_key(record["candidate"]))
    kept_records, folded = fold_duplicates(state, kept_records)
    report_cap = int(state.get("report_cap") or 8)
    reported_records = kept_records[:report_cap]
    deferred_by_report_cap = max(0, len(kept_records) - len(reported_records))

    findings = [
        reportable_finding(record, f"R{index}", state)
        for index, record in enumerate(reported_records, 1)
    ]

    reported_keys = {
        candidate_key(record["candidate"]) for record in reported_records
    }
    ledger: List[Dict[str, Any]] = []
    verification_incomplete = 0

    def ledger_entry(
        record: Mapping[str, Any],
        disposition: str,
    ) -> Dict[str, Any]:
        candidate = record["candidate"]
        verdict = record.get("verdict")
        entry = {
            "id": candidate.get("id"),
            "file": candidate.get("file"),
            "line": candidate.get("line"),
            "start_line": candidate.get("start_line"),
            "end_line": candidate.get("end_line"),
            "category": candidate.get("category"),
            "issue_type": candidate.get("issue_type"),
            "severity": candidate.get("severity"),
            "confidence": candidate.get("confidence"),
            "reports": int(candidate.get("reports") or 1),
            "rule_ids": list(candidate.get("rule_ids") or []),
            "source": candidate.get("source", "finder"),
            "short_summary": candidate.get("short_summary"),
            "summary": candidate.get("summary"),
            "failure_scenario": candidate.get("failure_scenario"),
            "disposition": disposition,
        }
        if candidate.get("suggestion_code"):
            entry["suggestion_code"] = candidate["suggestion_code"]
        if verdict is not None:
            entry["verdict"] = verdict["verdict"]
        if disposition == "duplicate":
            entry["duplicate_of"] = folded.get(str(candidate.get("id")))
        if isinstance(candidate.get("duplicate_of_posted"), dict):
            entry["duplicate_of_posted"] = dict(
                candidate["duplicate_of_posted"]
            )
        if candidate.get("anchors"):
            entry["anchors"] = list(candidate["anchors"])
        return entry

    verified_ids = {
        record["candidate"].get("id") for record in reviewed
    }

    def disposition_for(record: Mapping[str, Any]) -> str:
        candidate = record["candidate"]
        if str(candidate.get("id")) in folded:
            return "duplicate"
        if candidate_key(candidate) in reported_keys and record["kept"]:
            if isinstance(candidate.get("duplicate_of_posted"), dict):
                return "duplicate-of-posted"
            return "reportable"
        if record["kept"]:
            return "deferred-by-cap"
        if record.get("verdict") is None and use_verify:
            return "verification-incomplete"
        return "refuted"

    for candidate in state.get("candidates") or []:
        record = next(
            (
                entry
                for entry in reviewed
                if entry["candidate"].get("id") == candidate.get("id")
            ),
            None,
        )
        if record is None:
            if use_verify and candidate.get("id") not in verified_ids:
                ledger.append(
                    ledger_entry(
                        {"candidate": candidate, "verdict": None},
                        "deferred-by-cap",
                    )
                )
            continue
        disposition = disposition_for(record)
        if disposition == "verification-incomplete":
            verification_incomplete += 1
        ledger.append(ledger_entry(record, disposition))
    for record in sweep_reviewed:
        disposition = disposition_for(record)
        if disposition == "verification-incomplete":
            verification_incomplete += 1
        ledger.append(ledger_entry(record, disposition))

    votes = (
        vote_records(state, reviewed, "verify")
        + vote_records(state, sweep_reviewed, "sweep-verify")
        if use_verify
        else []
    )
    completed_votes = sum(1 for vote in votes if vote.get("completed"))

    if not use_verify:
        verification_status = "skipped-low-effort"
    elif verification_incomplete:
        verification_status = "partial"
    else:
        verification_status = "complete"

    finders_dispatched = int(state.get("finders_dispatched") or 0)
    finders_returned = int(state.get("finders_returned") or 0)
    rule_mapped = bool(state.get("rule_mapped"))
    coverage = {
        "finders": {
            "dispatched": finders_dispatched,
            "returned": finders_returned,
            "invalid": list(state.get("invalid_finder_results") or []),
        },
        "sweep": {
            "planned": bool(state.get("run_sweep")),
            "returned": bool(state.get("sweep_returned")),
        },
        "verification": {
            "status": verification_status,
            "bias": state.get("verify_bias"),
            "votesDispatched": len(votes),
            "votesCompleted": completed_votes,
            "incomplete": verification_incomplete,
        },
        "caps": {
            "perJobDrops": dict(state.get("job_cap_drops") or {}),
            "verificationDeferred": int(
                state.get("verification_deferred_by_cap") or 0
            ),
            "reportDeferred": deferred_by_report_cap,
        },
        "rejectedFindingReports": finding_reports(state, "rejected_findings"),
        "filteredFindingReports": finding_reports(state, "filtered_findings"),
    }
    if rule_mapped:
        coverage["finders"]["byKind"] = dict(
            state.get("finder_kind_stats") or {}
        )
        grouping = (
            state.get("grouping")
            if isinstance(state.get("grouping"), dict)
            else {}
        )
        rules_state = (
            state.get("rules") if isinstance(state.get("rules"), dict) else {}
        )
        discovery = sweep_coverage_summary(state)
        coverage["targetFiles"] = list(state.get("changed_files") or [])
        coverage["collapsed"] = state.get("collapsed")
        coverage["grouping"] = {
            "mode": grouping.get("mode"),
            "planned": bool(grouping.get("planned")),
            "agentReturned": bool(grouping.get("agent_returned")),
            "fallback": grouping.get("fallback"),
            "corrections": list(grouping.get("corrections") or []),
            "groups": list(grouping.get("groups") or []),
        }
        coverage["rules"] = {
            "layers": rules_state.get("layers"),
            "configSha256": rules_state.get("config_sha256"),
            "repoRuleRevision": rules_state.get("repo_rule_revision"),
            "repoRuleFiles": list(rules_state.get("repo_rule_files") or []),
            "counts": dict(rules_state.get("counts") or {}),
            "effectiveChecksByFile": dict(rules_state.get("effective") or {}),
            # The compiled text of every effective check, so the renderer
            # can attach guidance to rule-derived findings (SARIF rule help).
            "checkCatalog": {
                check_id: {
                    "category": check.get("category"),
                    "guidance": check.get("guidance"),
                }
                for check_id, check in sorted(
                    (rules_state.get("catalog") or {}).items()
                )
            },
            "overriddenBuiltinChecksByFile": dict(
                rules_state.get("overridden") or {}
            ),
            "mFileClassification": dict(rules_state.get("sniff") or {}),
            "failedAuditCells": discovery["ruleAuditCells"]["failed"],
            "uncoveredFiles": discovery["uncoveredFiles"],
            "uncoveredCheckIds": discovery["uncoveredCheckIds"],
        }
    completion_partial = (
        finders_returned < finders_dispatched
        or verification_status == "partial"
        or bool(coverage["rejectedFindingReports"])
        or bool(state.get("run_sweep")) and not state.get("sweep_returned")
        # Report-cap deferral is a completed policy selection at the
        # rule-mapped tiers: recorded in coverage and the ledger, but not a
        # completion failure. The low tier keeps its original meaning.
        or (not rule_mapped and deferred_by_report_cap > 0)
        or int(state.get("verification_deferred_by_cap") or 0) > 0
    )
    completed_at = (
        datetime.now(timezone.utc).replace(microsecond=0).isoformat()
    )
    manifest = {
        "schema_version": CANONICAL_SCHEMA_VERSION,
        "review_id": state.get("review_id"),
        "started_at": state.get("started_at"),
        "completed_at": completed_at,
        "mode": state.get("mode"),
        "effort": state.get("effort"),
        "model": state.get("model"),
        "guidance": state.get("guidance") or "",
        "scope": state.get("scope") or [],
        "range": state.get("range"),
        "revision": state.get("revision"),
        "counts": {
            "raw": int(state.get("raw_candidate_count") or 0),
            "deduplicated": len(state.get("candidates") or []),
            "sweep": len(sweep_candidates),
            "kept": len(kept_records),
            "duplicates": len(folded),
            "reported": len(findings),
        },
        "completion": {
            "status": "partial" if completion_partial else "complete",
        },
        "verification": {"status": verification_status},
        "canonical_files": list(CANONICAL_FILES),
    }
    incremental_state = (
        state.get("incremental")
        if isinstance(state.get("incremental"), dict)
        else {}
    )
    if incremental_state.get("requested"):
        manifest["incremental"] = {
            "requested": True,
            "enabled": bool(incremental_state.get("enabled")),
            "from": incremental_state.get("from"),
            "commits": incremental_state.get("commits"),
            "history_unavailable": incremental_state.get(
                "history_unavailable"
            ),
        }
    if rule_mapped:
        rules_state = (
            state.get("rules") if isinstance(state.get("rules"), dict) else {}
        )
        manifest["rules"] = {
            "layers": rules_state.get("layers"),
            "configSha256": rules_state.get("config_sha256"),
            "builtinManifestSha256": rules_state.get(
                "builtin_manifest_sha256"
            ),
            "repoRuleRevision": rules_state.get("repo_rule_revision"),
            "counts": dict(rules_state.get("counts") or {}),
        }

    coverage["calibration"] = calibration_summary(
        state,
        list(state.get("candidates") or []) + sweep_candidates,
        ledger,
        votes,
        coverage,
    )

    state["completed_at"] = completed_at
    state["review_manifest"] = manifest
    state["final_findings"] = findings
    state["final_coverage"] = coverage
    save_state(state)

    evidence_dir = Path(str(state["evidence_dir"]))
    write_json(evidence_dir / "review-manifest.json", manifest)
    write_jsonl(evidence_dir / "candidate-ledger.jsonl", ledger)
    write_json(evidence_dir / "findings.json", findings)
    write_json(evidence_dir / "coverage.json", coverage)
    write_jsonl(evidence_dir / "votes.jsonl", votes)

    print(
        f"Final result: {len(findings)} finding(s) reported of "
        f"{len(kept_records)} kept ({deferred_by_report_cap} beyond the "
        f"report cap); verification {verification_status}, completion "
        f"{manifest['completion']['status']}"
    )
    emit(
        reported_count=len(findings),
        verification_status=verification_status,
        products_dir=state["products_rel"],
        evidence_dir=state["evidence_rel"],
        metadata_dir=state["metadata_rel"],
        canonical_bundle_written=True,
        calibration=coverage["calibration"],
    )


# --- Rendering and expectations ----------------------------------------------


def resolve_workflow_script(rel_path: Path, description: str) -> Path:
    path = (root() / rel_path).resolve()
    if not path.is_file():
        raise WorkflowDataError(f"the {description} is missing: {rel_path}")
    return path


def load_renderer() -> Any:
    path = resolve_workflow_script(RENDERER_PATH, "deterministic renderer")
    spec = importlib.util.spec_from_file_location(
        "code_review_render_report",
        path,
    )
    if spec is None or spec.loader is None:
        raise WorkflowDataError("could not load the report renderer")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def render_report() -> None:
    state = load_state()
    products_rel = str(state["products_rel"])
    evidence_rel = str(state["evidence_rel"])
    metadata_rel = str(state["metadata_rel"])
    renderer = load_renderer()
    try:
        findings, verification = renderer.render(
            evidence_rel,
            products_rel,
            metadata_rel,
        )
    except Exception as error:
        raise WorkflowDataError(
            f"the report renderer refused the report: {error}"
        ) from error
    state["revision_path"] = f"{metadata_rel}/revision.json"
    state["verification_status"] = verification.get("status")
    state["finding_count"] = len(findings)
    save_state(state)
    print(
        f"Wrote {products_rel}/CODE-REVIEW-RESULTS.md, "
        "CODE-REVIEW-RESULTS.html, CODE-REVIEW-RESULTS.jsonl, and "
        "metadata/revision.json; canonical evidence retained with the reports"
    )
    emit(
        report_path=f"{products_rel}/CODE-REVIEW-RESULTS.md",
        revision_path=state["revision_path"],
        verification_status=verification.get("status"),
        finding_count=len(findings),
    )


def publish_pr_command(args: argparse.Namespace) -> None:
    """Post the completed review to its GitHub PR (the P1 publisher).

    The graph runs this node unconditionally after render-report; the
    post_pr input decides whether anything happens. The plan step is pure
    and runs with credentials scrubbed (R18); only apply sees
    GITHUB_TOKEN. The plan and outcome files land in the products
    directory as replayable evidence, peers of the canonical bundle.
    """
    requested = args.post_pr.strip().lower() in ("true", "1", "yes", "on")
    if not requested:
        print("PR publishing not requested (post_pr is off)")
        emit(publish_pr={"requested": False})
        return
    state = load_state()
    if not isinstance(state.get("review_manifest"), dict):
        raise WorkflowDataError(
            "publish-pr requires a completed review bundle; it runs after "
            "final-tally and render-report"
        )
    assert_workspace_unchanged(state)
    repo = args.pr_repo.strip()
    pr_text = args.pr_number.strip()
    if not repo or not pr_text:
        raise WorkflowDataError(
            "post_pr is enabled but pr_repo/pr_number do not name the "
            "target pull request"
        )
    if not os.environ.get("GITHUB_TOKEN"):
        raise WorkflowDataError(
            "publish-pr needs GITHUB_TOKEN in the environment; the "
            "workflow's [run.integrations.github.permissions] makes Fabro "
            "inject one when its GitHub integration is configured"
        )
    publisher = resolve_workflow_script(PUBLISHER_PATH, "PR publisher")
    products_rel = str(state["products_rel"])
    evidence_rel = str(state["evidence_rel"])
    plan_rel = f"{products_rel}/pr-publish-plan.json"
    outcome_rel = f"{products_rel}/pr-publish-outcome.json"

    def publisher_error(
        prefix: str, failed: subprocess.CompletedProcess
    ) -> WorkflowDataError:
        detail = failed.stderr.decode("utf-8", "replace").strip()
        return WorkflowDataError(prefix + one_line(detail, 2000))

    # The plan step needs no credentials and runs with none (R18). Its
    # environment is rebuilt from a benign allowlist, so a credential
    # injected under any name -- not just the ones Fabro uses today --
    # never reaches the plan.
    plan_environment = {
        key: value
        for key, value in os.environ.items()
        if key in ("PATH", "HOME", "TZ", "USER", "LOGNAME", "SHELL")
        or key.startswith(("LANG", "LC_", "PYTHON", "TMP", "TEMP"))
    }
    plan_arguments = [
        "plan",
        "--evidence-dir", evidence_rel,
        "--repo", repo,
        "--pr", pr_text,
        "--route-severity-below", args.route_severity_below,
        "--route-categories", args.route_categories,
        "--run-url", one_line(args.run_url, 2000),
        "--output", plan_rel,
    ]
    incremental_state = (
        state.get("incremental")
        if isinstance(state.get("incremental"), dict)
        else {}
    )
    if incremental_state.get("requested"):
        # Prepare parsed and recorded the threshold; the node reuses that
        # decision rather than re-reading the raw workflow input.
        plan_arguments.extend(
            [
                "--incremental", "true",
                "--incremental-overlap-threshold",
                str(incremental_state.get("overlap_threshold") or ""),
            ]
        )
        history_rel = incremental_state.get("history_rel")
        if incremental_state.get("enabled") and isinstance(history_rel, str):
            plan_arguments.extend(["--history", history_rel])
    result = run_publisher(publisher, plan_arguments, plan_environment)
    if result.returncode != 0:
        raise publisher_error("the publication plan failed: ", result)
    print(result.stdout.decode("utf-8", "replace").strip())

    result = run_publisher(
        publisher,
        [
            "apply",
            "--plan", plan_rel,
            "--repo", repo,
            "--pr", pr_text,
            "--api-base", args.api_base,
            "--bot-login", args.bot_login,
            "--outcome", outcome_rel,
        ],
    )
    value = read_json(root() / outcome_rel, required=False)
    outcome: Dict[str, Any] = value if isinstance(value, dict) else {}
    updates: Dict[str, Any] = {"requested": True}
    if outcome:
        updates["counts"] = outcome.get("counts")
        updates["summary_url"] = outcome.get("summary_url") or ""
        updates["outcome_path"] = outcome_rel
    emit(publish_pr=updates)
    stdout_text = result.stdout.decode("utf-8", "replace").strip()
    if stdout_text:
        print(stdout_text)
    if result.returncode != 0:
        raise publisher_error("posting to the PR failed: ", result)


def lint_rules() -> None:
    """Validate the rule configuration from the working tree, for authors.

    A review reads repository rules from its base revision, so an invalid
    rule file otherwise surfaces only after it lands and a rule-mapped run
    starts. This command runs the same loader against the working
    filesystem so a rule change can be checked before it is committed. It
    reads no workflow state and writes nothing.
    """
    loader = import_rule_loader()
    workflow_root = root() / WORKFLOW_ROOT
    manifest = read_json(workflow_root / loader.BUILTIN_MANIFEST)
    try:
        builtin_files = loader.load_builtin_files(workflow_root, manifest)
        builtin_packs = loader.load_rule_layer(builtin_files, "builtin")
        repo_files = read_repo_rule_files(loader, None)
        repo_packs = loader.load_rule_layer(repo_files, "repo")
    except loader.RuleLoaderError as error:
        raise WorkflowDataError(f"rule configuration is invalid: {error}")

    for path, _content in repo_files:
        pack_summaries = [
            f"{pack['pack_id']} ({len(pack['checks'])} check(s)"
            + (", override" if pack["mode"] == "override" else "")
            + ")"
            for pack in repo_packs
            if pack["source_path"] == path
        ]
        print(f"{path}: " + (", ".join(pack_summaries) or "no rules"))
    if not repo_files:
        print(
            "no repository rule files found (.fabro/rules.yaml, "
            ".fabro/rules/**/*.yaml)"
        )
    print(
        f"rule configuration OK: {len(builtin_packs)} built-in pack(s) "
        f"({sum(len(pack['checks']) for pack in builtin_packs)} check(s)), "
        f"{len(repo_packs)} repository pack(s) "
        f"({sum(len(pack['checks']) for pack in repo_packs)} check(s)); "
        f"config sha256 "
        f"{loader.rule_config_sha256(builtin_packs, repo_packs)[:12]}"
    )
    print(
        "note: reviews read repository rules from their base revision, so "
        "a change takes effect after it lands"
    )


def verify_expectations(
    expected_min_text: str,
    expected_file: str,
    expected_min_rule_text: str = "",
) -> None:
    expected_min_text = expected_min_text.strip()
    expected_file = expected_file.strip()
    expected_min_rule_text = expected_min_rule_text.strip()
    if not expected_min_text and not expected_file and not (
        expected_min_rule_text
    ):
        print("No report expectations configured")
        emit(report_expectations_checked=False)
        return
    for text, label in (
        (expected_min_text, "finding"),
        (expected_min_rule_text, "rule-derived finding"),
    ):
        if text and not re.fullmatch(r"0|[1-9][0-9]*", text):
            raise WorkflowDataError(
                f"expected minimum {label} count must be a non-negative "
                "integer"
            )
    expected_min = int(expected_min_text) if expected_min_text else 0
    expected_min_rule = (
        int(expected_min_rule_text) if expected_min_rule_text else 0
    )
    normalized_expected_file = (
        normalize_repo_path(expected_file) if expected_file else None
    )
    if expected_file and normalized_expected_file in (None, "."):
        raise WorkflowDataError("expected file is not a safe repository path")

    state = load_state()
    evidence_dir = state.get("evidence_dir")
    if not isinstance(evidence_dir, str) or not evidence_dir:
        raise WorkflowDataError("state has no evidence directory")
    findings = read_json(Path(evidence_dir) / "findings.json")
    if not isinstance(findings, list):
        raise WorkflowDataError("findings.json must contain a JSON array")
    if len(findings) < expected_min:
        raise WorkflowDataError(
            f"expected at least {expected_min} reported finding(s), found "
            f"{len(findings)}"
        )
    rule_findings = sum(
        1
        for finding in findings
        if isinstance(finding, dict) and finding.get("rule_ids")
    )
    if rule_findings < expected_min_rule:
        raise WorkflowDataError(
            f"expected at least {expected_min_rule} rule-derived "
            f"finding(s), found {rule_findings}"
        )
    if normalized_expected_file:
        files = {
            finding.get("file")
            for finding in findings
            if isinstance(finding, dict)
        }
        if normalized_expected_file not in files:
            raise WorkflowDataError(
                f"expected a finding in {normalized_expected_file!r}, found "
                f"findings in {sorted(str(name) for name in files)!r}"
            )

    print(
        "Verified report expectations: "
        f">={expected_min} finding(s)"
        + (
            f", >={expected_min_rule} rule-derived"
            if expected_min_rule_text
            else ""
        )
        + (
            f" including {normalized_expected_file}"
            if normalized_expected_file
            else ""
        )
    )
    emit(
        report_expectations_checked=True,
        expected_min_findings=expected_min,
        expected_min_rule_findings=expected_min_rule,
        expected_file=normalized_expected_file or "",
    )


# --- Entry point -------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    prepare_parser = subparsers.add_parser("prepare")
    prepare_parser.add_argument("--mode", default="changes")
    prepare_parser.add_argument("--effort", default="medium")
    prepare_parser.add_argument("--scope", default="")
    prepare_parser.add_argument("--base", default="")
    prepare_parser.add_argument("--commit", default="")
    prepare_parser.add_argument("--range", default="")
    prepare_parser.add_argument("--model", default="")
    prepare_parser.add_argument("--guidance", default="")
    prepare_parser.add_argument("--incremental", default="")
    prepare_parser.add_argument("--incremental-overlap-threshold", default="")
    prepare_parser.add_argument("--pr-repo", default="")
    prepare_parser.add_argument("--pr-number", default="")
    prepare_parser.add_argument("--bot-login", default="")
    prepare_parser.add_argument("--api-base", default="https://api.github.com")
    prepare_parser.add_argument("--review-id-stdin", action="store_true")

    merge_parser = subparsers.add_parser("merge")
    merge_parser.add_argument(
        "phase",
        choices=("grouping", "sweep", *PHASE_OUTPUT_KEYS.keys()),
    )

    publish_parser = subparsers.add_parser("publish-pr")
    publish_parser.add_argument("--post-pr", default="")
    publish_parser.add_argument("--pr-repo", default="")
    publish_parser.add_argument("--pr-number", default="")
    publish_parser.add_argument("--route-severity-below", default="")
    publish_parser.add_argument("--route-categories", default="")
    publish_parser.add_argument("--run-url", default="")
    publish_parser.add_argument("--bot-login", default="")
    publish_parser.add_argument("--api-base", default="https://api.github.com")

    expectations_parser = subparsers.add_parser("verify-expectations")
    expectations_parser.add_argument("--expected-min-findings", default="")
    expectations_parser.add_argument("--expected-file", default="")
    expectations_parser.add_argument(
        "--expected-min-rule-findings", default=""
    )

    for name in (
        "plan-finders",
        "plan-verify",
        "tally",
        "final-tally",
        "render-report",
        "lint-rules",
    ):
        subparsers.add_parser(name)
    return parser


def main(argv: Sequence[str]) -> int:
    args = build_parser().parse_args(argv)
    commands = {
        "prepare": lambda: prepare(args),
        "merge": lambda: merge(args.phase),
        "plan-finders": plan_finders,
        "plan-verify": plan_verify,
        "tally": tally,
        "final-tally": final_tally,
        "render-report": render_report,
        "publish-pr": lambda: publish_pr_command(args),
        "lint-rules": lint_rules,
        "verify-expectations": lambda: verify_expectations(
            args.expected_min_findings,
            args.expected_file,
            args.expected_min_rule_findings,
        ),
    }
    try:
        commands[args.command]()
    except WorkflowDataError as error:
        print(f"code_review.py: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
