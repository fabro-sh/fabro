# Deterministic code-review report and canonical bundle

The completed `evidence/` directory is the canonical bundle and the source of
truth for one code review. `render_report.py` validates that bundle and derives
every presentation artifact from it. No model writes or rewrites the final
report.

## Canonical files

The canonical bundle is schema version 4.

- `review-manifest.json` identifies the review, target, revision, request,
  completion status, counts, and canonical file set. At the rule-mapped
  tiers (every tier above `low`) it also carries a `rules` block: the
  compiled rule layers, the rule configuration SHA-256, the built-in rule
  manifest SHA-256, the repository rule revision, and pack/check counts for
  both layers. An incremental PR re-review adds an `incremental` record:
  whether it was requested and enabled, the last reviewed head the delta
  started from (`from`, with the commit count), and any declared
  history-fetch degradation.
- `candidate-ledger.jsonl` contains every unique candidate after
  deduplication, plus every sweep candidate. Each record has one disposition:
  `reportable`, `refuted`, `verification-incomplete`, `deferred-by-cap`,
  `duplicate` (folded into the finding named by `duplicate_of`), or
  `duplicate-of-posted` (judged the same defect as a finding already
  posted on the reviewed PR; `duplicate_of_posted` names the covering
  comment), and carries the candidate's applicable `rule_ids` (empty
  outside the rule-mapped tiers).
- `findings.json` contains only the reportable subset. It is the authoritative
  finding list. Each reported finding carries its orthogonal `issue_type`, an
  engine-derived `location` with the exact original code and start/end lines,
  a highlighted `code` excerpt, and its `rule_ids`. A verified replacement is
  stored as optional `suggestion.replacement_code`. A finding the verifier
  judged already posted on the reviewed PR stays in the list (the bundle
  never drops a finding) with an optional `duplicate_of_posted` record
  naming the covering comment; the PR publisher skips it instead of
  re-posting.
- `coverage.json` records what the review dispatched, what returned, what was
  rejected for failing the finding contract, and what a cap dropped. At the
  rule-mapped tiers it also records the authoritative target-file list, the
  grouping mode and final groups with fallback and corrections, whether a
  small target collapsed the shape, per-kind job accounting, the compiled
  rule layers, the effective check IDs per file, a `checkCatalog` with the
  category and guidance text of every effective check, overridden built-in
  checks per file, the `.m` classification, and rule-audit cells that
  returned no usable output.
  `coverage.calibration` is a compact, aggregatable summary of how the
  run's candidates fared -- dispositions and verdicts overall and per
  reporter kind, reporter, rule check, and category, plus rejection reasons
  and cap drops -- also emitted into the workflow context so calibration
  across many runs can read it from the event log.
- `votes.jsonl` contains one record for each dispatched verification, with the
  exact claim shown to the verifier (including its location, proposed
  replacement, claimed `rule_ids`, and the file's effective checks at the
  rule-mapped tiers), plus its verdict and reasoning when it completed. A vote
  over a proposed replacement also carries `suggestion_valid`.

## Derived files

The renderer creates these presentation artifacts at the root of the
timestamped result directory from the five canonical files:

- `CODE-REVIEW-RESULTS.md` for people.
- `CODE-REVIEW-RESULTS.html` for people, from `templates/report.html`.
- `CODE-REVIEW-RESULTS.jsonl` for finding consumers and CI gates.
- `CODE-REVIEW-RESULTS.sarif` for SARIF consumers such as GitHub Code
  Scanning.

At the rule-mapped tiers, the Markdown and HTML coverage sections include a
rules-coverage summary derived from the canonical bundle: distinct checks
audited (with pack counts) across audited files and audit cells, reported
findings citing a check with a per-check violation breakdown, policy-filtered
findings, and duplicates folded.

It also writes `metadata/revision.json`, recording the reviewed revision, run
settings, finding counts, verification status, and canonical bundle location.
The result directory also contains `metadata/state.json` and
`metadata/review-meta.json`, which preserve the deterministic workflow state
and review setup.

## Effort tiers and the keep rule

`low` ports the local /code-review workflow's single-pass shape: one
hunk-only finder, no rules, no verification, at most 4 reported findings.

Every tier above `low` projects one rule-mapped structure:

- Every target file lands in exactly one file group of at most ten files.
  At `high` and above a grouping agent proposes semantic groups and a
  deterministic merge corrects it to exact coverage, falling back to
  lexical chunks when the agent fails; `medium` uses lexical chunks
  directly. The grouping mode and any fallback are recorded in coverage.
- One local-correctness finder job per final group, four whole-change angle
  jobs (behavior preservation, contracts and data flow, design economy,
  performance and lifetime), and one rule-audit job per non-empty cell of
  files sharing the same effective check set, packed across the whole
  target rather than within groups (at most ten files and twelve checks
  per cell; a larger check set splits into evenly sized cells over the
  same files). Discovery is capped at 64
  jobs; a target that cannot fit fails before dispatch rather than omitting
  files or checks. A small target at `medium` (at most 5 files and 300
  changed lines, or a scope of at most 5 files) collapses the shape to the
  local passes and rule audits only; coverage records the collapse.
- Rules come from the built-in library (`rules/builtin`, verified against a
  graph-pinned manifest) and from repository YAML (`.fabro/rules.yaml` and
  `.fabro/rules/**/*.yaml`), read from the review's base revision so a
  change cannot weaken the rules used to review itself. `medium` compiles
  only the repository rules plus the built-in repository-instructions pack;
  `high` and above compile the full built-in library. All matching packs
  compose; a repository pack with `mode: override` suppresses the built-ins
  for its matched files only.
- The tier dials: `medium` -- up to 6 candidates per job, standard-bias
  verification capped at 60, at most 8 reported findings; `high` -- the
  same caps with recall-biased verification and at most 10; `xhigh` and
  `max` -- up to 8 candidates per job, standard bias capped at 120, one
  coverage-aware gap-fill sweep whose fresh candidates are verified the
  same way, and at most 25. `xhigh` and `max` are identical and differ only
  in the model reasoning effort the graph's model stylesheet selects.
- A rule-audit finding must name one applicable compiled check ID
  (`builtin:<pack>/<check>` or `repo:<pack>/<check>`); the engine rejects a
  missing or inapplicable ID. Deduplication unions rule IDs and reporter
  job IDs when generic and rule-derived candidates describe the same
  defect.

The keep rule is the same at every verified tier: `CONFIRMED` and `PLAUSIBLE`
survive, `REFUTED` drops, and a candidate whose verifier returned no verdict
is `verification-incomplete` and is not reported. The bias changes only the
verifier's instructions, never the arithmetic. At `low`, verification is
skipped by design: findings carry `verdict: "UNVERIFIED"` and the reports say
so.

A proposed replacement is independent of the keep verdict. The verifier must
return `suggestion_valid: true`, the engine must be able to read the exact
original range from the unchanged reviewed tree, and the replacement must
differ from it. Low-effort findings never carry suggestions because they have
no independent verification.

## Deduplication and ranking

A candidate's identity is its normalized file, line, and category. Two angles
that flag the same line for different reasons stay separate findings; the same
defect reported twice under one category merges, keeping the highest severity
and confidence and counting the reports. Sweep candidates are deduplicated
against every candidate already seen -- kept or not -- so a refuted candidate
cannot reappear through the sweep.

The same defect can also be reported at different lines or under different
categories. Each verification claim therefore carries `siblings` -- the
other candidates in the same file, nearest first -- and a verifier that
judges its claim to describe the same defect as a sibling returns
`duplicate_of` with that sibling's id. After verification the engine folds
deterministically: the named sibling must have been shown to that verifier
and must itself have survived; the lower-ranked finding folds into the
higher-ranked one (a mutual claim resolves the same way); the primary gains
the secondary's anchor, reporters, rule IDs, and report count. Folded
candidates take the ledger disposition `duplicate` with `duplicate_of`, the
primary's `anchors` list them, and `manifest.counts.duplicates` counts them.
A duplicate claim naming a refuted, unshown, or lower-ranked sibling is
ignored and the finding stands on its own verdict.

Ranking is deterministic: `correctness` findings always outrank the cleanup
categories (`reuse`, `simplification`, `efficiency`, `altitude`,
`conventions`, `test-coverage`); within a class the order is severity, then
report count, then confidence, then file and line. The report cap cuts from
the bottom, and everything cut is in the ledger as `deferred-by-cap`.

## Locations, source excerpts, and suggestions

A finder supplies a bounded `start_line`/`end_line` range. `final-tally` reads
that range from the reviewed tree and records its exact text as
`location.existing_code`; the agent never supplies the canonical original
text. The adjacent `code` excerpt is read the same way and highlights the
complete range. Exact text and the excerpt are omitted when the file is
unreadable, binary, oversized, or the range is invalid. A proposed
`suggestion_code` becomes canonical only after the verifier approves it and
the exact original text is available.

## HTML rendering

`templates/report.html` carries the page and its own script. The renderer
substitutes one JSON payload into it and never builds markup from finding
text. The payload escapes `<`, `>`, `&`, and every non-ASCII codepoint, so no
finding text can close the script element, open an HTML comment, or end a
JavaScript statement. The template's script writes model-authored text with
`textContent` only.

## SARIF rendering

`CODE-REVIEW-RESULTS.sarif` is one SARIF 2.1.0 run derived from the same
validated bundle:

- A finding backed by compiled rule checks reports under its first check ID;
  the check's guidance from `coverage.rules.checkCatalog` becomes the rule's
  description and help. A finding without rule checks reports under its
  category, with a fixed description per category. The driver's rules list
  covers every check ID any result cites.
- Severity maps to level: `HIGH` is `error`, `MEDIUM` is `warning`, `LOW` is
  `note`.
- Each result's location is the finding's file and line range relative to
  `%SRCROOT%`; anchors become related locations. The finding's identity,
  category, issue type, severity, confidence, verdict, reports, reporters,
  rule IDs, anchors, and source are result properties. File, issue type, and
  exact original code form a stable hashed partial fingerprint, falling back
  to the line range when source text is unavailable.
- A verified suggestion becomes a SARIF `fix` that replaces the complete
  location range.
- An `UNVERIFIED` finding (the `low` tier) says so in its result message and
  carries the verdict in its properties.
- The run's automation ID is `code-review/<mode>`, and the run properties
  record the review ID, target, revision, request settings, verification and
  completion statuses, and any partial-review reasons.

## Required relationships

A `reportable` ledger record must match one entry in `findings.json`, and
every entry in `findings.json` must have a `reportable` ledger record.
Manifest counts must match the canonical records. At a verified tier, every
reported finding's verdict must be `CONFIRMED` or `PLAUSIBLE`; at `low`, every
reported finding's verdict must be `UNVERIFIED`.

Rule provenance must be consistent: the manifest and coverage either both
carry rule configuration or neither does, their configuration hashes must
agree, and every reported finding's `rule_ids` must be effective checks for
that finding's file per `coverage.rules.effectiveChecksByFile`. A bundle
with no rule configuration cannot report rule-derived findings.

A review is `partial` when a finder returned no usable result, verification
was incomplete, a reported finding was rejected for failing the finding
contract, a planned sweep returned nothing usable, or the verification cap
deferred candidates without adjudication. At `low` a report-cap cut also
makes the review partial. At the rule-mapped tiers, report-cap deferral is
a completed policy selection: it stays visible in coverage and the ledger
but does not by itself make the run partial. A failed rule-audit cell makes
the run partial, and its files and check IDs are recorded as uncovered.

`coverage.rejectedFindingReports` names every finding an agent reported that
failed the contract, with the reason and the angle that sent it. A dropped
finding never becomes a candidate, so without this record a review that
discarded everything it was given would be indistinguishable from one that
found nothing. The reasons are fixed strings naming the field at fault; they
never quote the model's own text.

`coverage.filteredFindingReports` names well-formed findings dropped by
review policy rather than by the contract -- today, a `conventions` finding
that names no applicable rule check, since that category belongs to rule
audits. Filters are recorded the same way as rejections but do not make the
review partial.

## Rendering safety

The renderer rejects unsafe repository paths, control characters, unknown
categories or verdicts, inconsistent ledger/finding relationships, and
inconsistent cross-file counts. It escapes model-authored text before placing
it in Markdown. Code excerpts use Markdown code blocks.

Findings are derived from source and history review. The workflow does not
attest whether agents executed commands.
