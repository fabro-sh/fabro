import { parseWorkflow } from "./dot-parse";
import { randomId } from "./draft";
import { effectiveNodeAttributes, getAttr } from "./effective";
import { promptPartsFromAttribute } from "./prompts";
import type { PromptPart } from "./types";

/**
 * The sample a person gets from "Load example": a complete workflow bundle, not
 * just a graph. Three rules keep it honest:
 *
 * - A check passes only through an exit code. Each child ends in a command that
 *   reads the report and fails unless it says `Status: PASS`, so a confident
 *   paragraph from a model can't get a change delivered.
 * - No node names a model, so it runs on whatever the server has ready.
 * - Children read the ticket as `@ticket.md` beside them. Fabro's admission reads
 *   bundled files by exact path, so `@../ticket.md` would be refused.
 */

/** A DOT string. Newlines stay literal so scripts and prompts stay readable in the code view. */
function q(value: string): string {
  return `"${value.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
}

const TICKET_PATH = "shared/ticket.md";
const NEVER_COMMITTED = "plan.md, remediation.md, PR.md, jira-update.md and the *-report.md files";

const ANALYZE_PROMPT = `Ticket:

{{ goal }}

Read the ticket and its acceptance criteria. Inspect the existing frontend architecture, components, styling conventions, dependencies, and tests. Identify affected components and document a detailed implementation plan in plan.md.

The first line of plan.md must be exactly "Base commit: <sha>", using the output of "git rev-parse HEAD". The checks compare against that commit, and refuse to pass without it.

Identify required test cases for every acceptance criterion. Capture before screenshots of affected UI states when possible. Do not modify application code.`;

const IMPLEMENT_PROMPT = `Ticket:

{{ goal }}

Read the ticket and plan.md. Implement the requested frontend changes following existing project conventions. Ensure every acceptance criterion is addressed. Add or update relevant unit, integration, and browser tests. Follow existing styling and accessibility conventions. Avoid unrelated changes.`;

const BUG_FIX_PROMPT = `Ticket:

{{ goal }}

Read the ticket, plan.md, and whichever of testing-report.md, security-report.md and code-review-report.md exist. The first line of each is its status. A report that says Status: PASS has nothing for you.

If a report says Status: BLOCKED or Status: INCOMPLETE, or its failure is a missing tool, a missing credential or a network problem rather than a defect, the check could not do its job and changing code will not help. Change nothing, and finish with the failed JSON below. That stops the run so a person can fix the environment.

Otherwise, identify the root cause of each [blocking] finding. Implement targeted fixes without unrelated changes. Add regression tests for resolved defects. Document the problems, root causes, and fixes in remediation.md. Do not ignore or bypass failing acceptance criteria. Never skip, weaken or delete a test or check to make it pass.

End your response with one JSON object and nothing after it: {"outcome":"succeeded"} when the fixes are in, or {"outcome":"failed","failure_reason":"<one line saying why>"} when you changed nothing or cannot fix it.`;

const PR_PROMPT = `Ticket:

{{ goal }}

Testing, security and code review have all passed, or this step would not be running.

Prepare a pull request describing the frontend changes, implementation details, acceptance criteria validation, test results, security findings, and code review results. Include before-and-after screenshots when available. Leave ${NEVER_COMMITTED} out of the pull request.

Create the pull request using an authorized GitHub integration. Never merge automatically. If GitHub integration is unavailable, write PR.md and explicitly report that no pull request was created.`;

const JIRA_PROMPT = `Ticket:

{{ goal }}

Update the associated Jira ticket to In Review using an authorized Jira integration. Include the pull request URL (or say that PR.md was written instead), implementation summary, acceptance criteria results, testing results, security review, and code review outcome. If Jira integration is unavailable, or the ticket names no Jira key, prepare jira-update.md and explicitly report that Jira was not changed.`;

/** The corrected workflow: same stages, nodes and remediation loop as the one it came from. */
export const EXAMPLE_SOURCE: string = `digraph FrontendComponentChanges {
  // Frontend component changes: planning, development, QA, review, delivery.
  // Groups organize the canvas; edges determine execution order.
  //
  // Models: no node sets one, so each runs on the server's default. To choose,
  // select a node and set Model in the inspector, or set Default model on the graph.
  //
  // Failures: on_failure="exit" means an unhandled failure ends the run as failed.
  // Under "route", a failed plan would flow on into implementation and a failed pull
  // request on to exit, and the run would report success. The three checks opt back
  // into "route" so that their failures can take the edge to bug_fix.
  graph [
    goal="@${TICKET_PATH}",
    default_max_retries=0,
    max_node_visits=4,
    on_failure="exit"
  ]
  rankdir=TB
  start [shape=Mdiamond, label="Start"]
  exit [shape=Msquare, label="Exit"]
  subgraph cluster_planning {
    label="Stage 1: Planning"
    analyze [
      shape=box, label="1. Analyze Existing UI",
      prompt=${q(ANALYZE_PROMPT)}
    ]
  }
  subgraph cluster_development {
    label="Stage 2: Development"
    implement [
      shape=box, label="2. Implement Component Changes",
      prompt=${q(IMPLEMENT_PROMPT)}
    ]
    // A report saying a check couldn't run can't be fixed in code. This node reports
    // that as failed, and on_failure="exit" then stops the run instead of looping.
    bug_fix [
      shape=box, label="4. Fix Bugs / Security Findings", output_schema="routing",
      prompt=${q(BUG_FIX_PROMPT)}
    ]
  }
  subgraph cluster_quality_assurance {
    label="Stage 3: Quality Assurance"
    testing [shape=house, label="3. Testing Sub-workflow", on_failure="route", stack.child_workflow="shared/testing.fabro"]
  }
  subgraph cluster_security_review {
    label="Stage 4: Security & Code Review"
    security [shape=house, label="5. Security Sub-workflow", on_failure="route", stack.child_workflow="shared/security.fabro"]
    review [shape=house, label="6. Code Review Sub-workflow", on_failure="route", stack.child_workflow="shared/code-review.fabro"]
  }
  subgraph cluster_delivery {
    label="Stage 5: Delivery"
    pr [
      shape=box, label="7. Create Pull Request",
      prompt=${q(PR_PROMPT)}
    ]
    jira [
      shape=box, label="8. Update Jira Ticket",
      prompt=${q(JIRA_PROMPT)}
    ]
  }
  start -> analyze
  analyze -> implement
  implement -> testing
  testing -> security [condition="outcome=succeeded"]
  security -> review [condition="outcome=succeeded"]
  review -> pr [condition="outcome=succeeded"]
  pr -> jira
  jira -> exit
  // Validation failures return to development and then repeat validation.
  // Fabro checks conditions first, so a check that succeeded takes its edge above. These
  // edges have no condition on purpose: Fabro refuses a node whose every edge is
  // conditional, and an unconditional edge also catches any result other than success,
  // so the only way to delivery is through outcome=succeeded. max_node_visits caps the
  // loop: a fifth visit fails the run rather than delivering.
  testing -> bug_fix [label="Blocking findings"]
  security -> bug_fix [label="Blocking findings"]
  review -> bug_fix [label="Blocking findings"]
  bug_fix -> testing
}
`;

/** Writes the report `bug_fix` reads, then exits 0 only if the agent step still has to run. */
function reportHelpers(report: string): string {
  return String.raw`set -u
set -o pipefail
export CI=1
report=${report}
log=$(mktemp)
rm -f "$report"

finish() {
  {
    printf 'Status: %s\n\n%s\n\n## Command output (last 200 lines)\n\n~~~text\n' "$1" "$2"
    tail -n 200 "$log"
    printf '~~~\n'
  } > "$report"
  rm -f "$log"
  printf '%s: %s\n' "$1" "$2"
  [ "$1" = INCOMPLETE ]
  exit $?
}

run_step() {
  printf '\n$ %s\n' "$1" | tee -a "$log"
  bash -c "$1" </dev/null 2>&1 | tee -a "$log"
}

need() {
  command -v "$1" >/dev/null 2>&1 || finish BLOCKED "$1 is not installed in the run environment, so this check cannot run."
}`;
}

const BASE_COMMIT = String.raw`base=$(sed -n 's/^Base commit:[[:space:]]*//p' plan.md 2>/dev/null | head -n 1)
git cat-file -e "$base^{commit}" 2>/dev/null ||
  finish BLOCKED 'plan.md has no usable "Base commit: <sha>" line, or this is not a Git checkout, so the change cannot be identified.'
git add --intent-to-add . 2>/dev/null`;

const TEST_SCRIPT = String.raw`${reportHelpers("testing-report.md")}

# Project tests. Put your own commands in .fabro/checks/test.sh to skip detection.
commands=
add() { commands="$commands$1
"; }
install=
if [ -f .fabro/checks/test.sh ]; then
  add 'bash .fabro/checks/test.sh'
elif [ -f package.json ]; then
  if [ -f pnpm-lock.yaml ]; then manager=pnpm; install='pnpm install --frozen-lockfile'
  elif [ -f yarn.lock ] && [ -f .yarnrc.yml ]; then manager=yarn; install='yarn install --immutable'
  elif [ -f yarn.lock ]; then manager=yarn; install='yarn install --frozen-lockfile'
  elif [ -f bun.lock ] || [ -f bun.lockb ]; then manager=bun; install='bun install --frozen-lockfile'
  elif [ -f package-lock.json ]; then manager=npm; install='npm ci'
  else manager=npm; install='npm install'
  fi
  need "$manager"
  scripts=$(sed -n '/"scripts"[[:space:]]*:/,/}/p' package.json)
  printf '%s' "$scripts" | grep -q '"test"[[:space:]]*:' ||
    finish BLOCKED 'package.json has no test script. Add one, or put your commands in .fabro/checks/test.sh.'
  for name in lint typecheck test test:e2e; do
    if printf '%s' "$scripts" | grep -q "\"$name\"[[:space:]]*:"; then add "$manager run $name"; fi
  done
elif [ -f Cargo.toml ]; then need cargo; add 'cargo test'
elif [ -f go.mod ]; then need go; add 'go test ./...'
elif [ -f pyproject.toml ] || [ -f pytest.ini ] || [ -f setup.py ]; then need python3; add 'python3 -m pytest'
elif [ -f Makefile ] && grep -q '^test:' Makefile; then need make; add 'make test'
fi
[ -n "$commands" ] ||
  finish BLOCKED 'No test command was found. Add .fabro/checks/test.sh to this project.'

if [ -n "$install" ] && [ ! -d node_modules ]; then
  run_step "$install" || finish BLOCKED "Installing dependencies failed: $install"
fi

failed=
while IFS= read -r command <&3; do
  [ -n "$command" ] || continue
  run_step "$command" || failed="$failed
- [blocking] $command failed"
done 3<<< "$commands"

[ -z "$failed" ] || finish FAIL "These project checks failed:$failed"
finish INCOMPLETE 'All project checks passed. The acceptance check has not finished yet.'`;

const SECURITY_SCRIPT = String.raw`${reportHelpers("security-report.md")}

${BASE_COMMIT}

findings=

# Dependency audit. Put your own commands in .fabro/checks/security.sh to skip detection.
audit=
if [ -f .fabro/checks/security.sh ]; then audit='bash .fabro/checks/security.sh'
elif [ -f pnpm-lock.yaml ]; then audit='pnpm audit --audit-level high'
elif [ -f yarn.lock ] && [ -f .yarnrc.yml ]; then audit='yarn npm audit --all --recursive --severity high'
elif [ -f yarn.lock ]; then audit='yarn audit --level high --groups dependencies'
elif [ -f bun.lock ] || [ -f bun.lockb ]; then audit='bun audit --audit-level=high'
elif [ -f package-lock.json ]; then audit='npm audit --audit-level=high'
elif [ -f Cargo.lock ]; then audit='cargo audit'
elif [ -f go.sum ]; then audit='govulncheck ./...'
elif [ -f requirements.txt ]; then audit='pip-audit -r requirements.txt'
elif [ -f pyproject.toml ]; then audit='pip-audit'
fi
if [ -n "$audit" ]; then
  need "$(printf '%s' "$audit" | cut -d' ' -f1)"
  run_step "$audit" || findings="$findings
- [blocking] Dependency audit failed: $audit"
else
  printf 'No dependency lockfile found, so no dependency audit ran.\n' | tee -a "$log"
fi

# Secret scan of what changed. Without gitleaks, a basic pattern check names the files
# and never prints the value.
if command -v gitleaks >/dev/null 2>&1; then
  run_step "gitleaks detect --source . --no-banner --redact --log-opts=$base..HEAD" || findings="$findings
- [blocking] gitleaks found a secret in the commits since $base"
else
  printf 'gitleaks is not installed, so only a basic secret pattern check ran.\n' | tee -a "$log"
  pattern='-----BEGIN [A-Z ]*PRIVATE KEY-----|AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{36,}|xox[baprs]-[A-Za-z0-9-]{10,}|sk-[A-Za-z0-9_-]{20,}'
  while IFS= read -r file; do
    [ -n "$file" ] || continue
    if git diff --unified=0 "$base" -- "$file" | grep -E '^\+' | grep -Eq -e "$pattern"; then
      findings="$findings
- [blocking] A likely secret was added in $file"
    fi
  done <<< "$(git diff --name-only "$base")"
fi

[ -z "$findings" ] || finish FAIL "The automated security scans found problems:$findings"
finish INCOMPLETE 'The automated scans found nothing blocking. The security review has not finished yet.'`;

const REVIEW_SCRIPT = String.raw`${reportHelpers("code-review-report.md")}

${BASE_COMMIT}

run_step "git diff --stat $base"
if git diff --quiet "$base" -- . ':(exclude)plan.md' ':(exclude)remediation.md' ':(exclude)PR.md' ':(exclude)jira-update.md' ':(exclude)*-report.md'; then
  finish FAIL "- [blocking] Nothing changed since $base, so there is no change to review or deliver."
fi
finish INCOMPLETE 'The change is ready. The code review has not finished yet.'`;

/** The check that makes a report binding: only a clean PASS exits 0. */
function gateScript(report: string): string {
  return String.raw`report=${report}
first=$(head -n 1 "$report" 2>/dev/null)
if [ "$first" = 'Status: PASS' ] && ! grep -qF '[blocking]' "$report"; then
  printf '%s says PASS with no blocking findings.\n' "$report"
  exit 0
fi
printf '%s does not show a clean pass (first line: %s), so delivery stays blocked.\n' "$report" "$first"
exit 1`;
}

function reportContract(report: string): string {
  return `Overwrite ${report}, keeping the command output already in it. Its first line must be exactly "Status: PASS", "Status: FAIL" or "Status: BLOCKED". Put each finding on its own line starting "- [blocking]" (must be fixed before delivery) or "- [advisory]". Use BLOCKED only if you could not do the check at all. Do not change application code or tests.

End your response with one JSON object and nothing after it: {"outcome":"succeeded"} only when the report says Status: PASS, otherwise {"outcome":"failed","failure_reason":"<one line>"}.`;
}

const ACCEPTANCE_PROMPT = `Ticket:

{{ goal }}

Check this change against the ticket. testing-report.md holds the result of the project's tests, and plan.md lists the test cases that were planned.

For every acceptance criterion, find evidence that it holds: a test that exercises it, or run the app or its browser tests if the project supports that. A criterion with no evidence is a [blocking] finding, not a pass. List each criterion with its evidence.

${reportContract("testing-report.md")}`;

const SECURITY_PROMPT = `Ticket:

{{ goal }}

Review the change for security problems. The first line of plan.md names its base commit; compare with "git diff <base>". security-report.md holds the automated scan results, including anything those scans could not cover.

Look for what a frontend change can introduce: script injection (raw HTML, unescaped input, unsafe URLs), secrets or credentials in client code, sensitive data in logs or browser storage, authorization enforced only in the client, weakened content security policy, and new or upgraded dependencies you can't justify. Report only problems you can point at in this change.

${reportContract("security-report.md")}`;

const CODE_REVIEW_PROMPT = `Ticket:

{{ goal }}

Review the change as a careful teammate would. The first line of plan.md names its base commit; compare with "git diff <base>". Leave plan.md, remediation.md and the report files out of the review.

Check that it does what the ticket asks and nothing more, that it follows this project's conventions, that it handles failure and empty states, that it is accessible, and that its tests would fail if the change were wrong. Bugs, regressions and missing acceptance criteria are [blocking]; preferences are [advisory].

${reportContract("code-review-report.md")}`;

function checkWorkflow(options: {
  name: string;
  comment: string;
  scan: { id: string; label: string; timeout: string; script: string };
  agent: { id: string; label: string; prompt: string };
  gate: { id: string; label: string; report: string };
}): string {
  const { name, comment, scan, agent, gate } = options;
  return `digraph ${name} {
  // ${comment}
  //
  // A failure anywhere ends this workflow as failed: on_failure="exit" skips the edge
  // onward, and the run never reaches exit. The parent's node then fails too, and takes
  // its edge to bug_fix. A pass needs all three steps, and the last is an exit code.
  graph [goal="@ticket.md", default_max_retries=0, on_failure="exit"]
  start [shape=Mdiamond, label="Start"]
  exit [shape=Msquare, label="Exit"]
  ${scan.id} [
    shape=parallelogram, label=${q(scan.label)}, timeout=${q(scan.timeout)},
    script=${q(scan.script)}
  ]
  ${agent.id} [
    shape=box, label=${q(agent.label)}, output_schema="routing",
    prompt=${q(agent.prompt)}
  ]
  ${gate.id} [
    shape=parallelogram, label=${q(gate.label)},
    script=${q(gateScript(gate.report))}
  ]
  start -> ${scan.id}
  ${scan.id} -> ${agent.id}
  ${agent.id} -> ${gate.id}
  ${gate.id} -> exit
}
`;
}

const TESTING_WORKFLOW = checkWorkflow({
  name: "SharedTesting",
  comment: "Runs the project's real tests, then holds the change against the ticket's acceptance criteria.",
  scan: { id: "project_tests", label: "Run project tests", timeout: "1800s", script: TEST_SCRIPT },
  agent: { id: "acceptance_check", label: "Check acceptance criteria", prompt: ACCEPTANCE_PROMPT },
  gate: { id: "testing_gate", label: "Testing gate", report: "testing-report.md" },
});

const SECURITY_WORKFLOW = checkWorkflow({
  name: "SharedSecurity",
  comment: "Audits dependencies, scans the change for secrets, then reviews it for security problems.",
  scan: { id: "automated_scans", label: "Run security scans", timeout: "900s", script: SECURITY_SCRIPT },
  agent: { id: "security_review", label: "Review security", prompt: SECURITY_PROMPT },
  gate: { id: "security_gate", label: "Security gate", report: "security-report.md" },
});

const CODE_REVIEW_WORKFLOW = checkWorkflow({
  name: "SharedCodeReview",
  comment: "Confirms there is a change to review, then reviews it against the ticket and the project's conventions.",
  scan: { id: "collect_change", label: "Collect the change", timeout: "300s", script: REVIEW_SCRIPT },
  agent: { id: "code_review", label: "Review the code", prompt: CODE_REVIEW_PROMPT },
  gate: { id: "review_gate", label: "Code review gate", report: "code-review-report.md" },
});

const TICKET = `# Implement the frontend component change below

Implement this change, validate all acceptance criteria, complete security and code review, and prepare a pull request.

<!-- This ticket is an example. Replace everything below with your own before running. -->

## Ticket

Jira key: none yet. Write the key here, such as WEB-123, to have the Jira step update it.

## Problem

The Save button on the account settings form gives no feedback. People click it several times, and each click sends another request.

## Change

While a save is in progress, disable the button and show a spinner beside its label. When the save finishes, re-enable it. If the save fails, show the error next to the button and keep what the person typed.

## Acceptance criteria

1. Clicking Save disables the button and shows a spinner until the request finishes.
2. Clicking again during a save sends no second request.
3. A failed save shows the server's error message and keeps the form's contents.
4. Screen readers announce the saving state, and the button stays reachable by keyboard.
5. The existing form tests still pass, and new tests cover criteria 1 to 3.

## Out of scope

Other forms, and any change to the settings API.
`;

const README = `# Frontend component changes

A sample workflow you can load, edit and run on your own project.

ticket → plan → implement → tests → security → code review → pull request → Jira. A failed check goes to bug_fix and back to tests, at most 4 visits per node.

## Before you run it

1. **Write your ticket** in \`shared/ticket.md\`. The workflow and all three checks read it as their goal, so edit the file rather than passing \`--goal\`, which only reaches the parent. It is rendered as a template, so wrap any literal \`{{\` or \`{%\` in \`{% raw %}\`.
2. **Put the folder in your project** at \`.fabro/workflows/frontend-component-changes/\`. The project must be a Git checkout with a \`.fabro/project.toml\`, so run \`fabro repo init\` once if it has none. Docker and Daytona environments clone its GitHub origin, so push first; a local environment uses the folder as it is.
3. **Have a provider ready.** No node sets a model, so each uses the server's default: the default model on the highest-priority ready provider. If none is ready, run creation says so.
4. **Check the test commands.** See below.
5. **Validate and start it** from the project:

\`\`\`bash
fabro validate .fabro/workflows/frontend-component-changes/workflow.fabro
fabro preflight .fabro/workflows/frontend-component-changes/workflow.toml
fabro run frontend-component-changes
\`\`\`

\`fabro validate\` checks the workflow's structure offline. \`fabro preflight\` also checks your server's models and providers. Neither runs anything, so passing them does not promise the run succeeds.

## Choosing models

In the builder, select a node and set **Model** in the inspector, or set **Default model** in the graph settings. \`fabro model list\` shows what your server knows. Good places to spend differently: a strong reasoning model for planning (\`analyze\`), a strong coding model for \`implement\` and \`bug_fix\`, and a fast one for \`pr\` and \`jira\`. A reviewer on another provider brings fresher eyes. Nodes that report a status use \`output_schema\`, which needs the default API backend, not \`backend="acp"\`.

## What the checks run

| Check | Commands |
|---|---|
| Tests | \`.fabro/checks/test.sh\` if it exists. Otherwise detected: \`lint\`, \`typecheck\`, \`test\` and \`test:e2e\` from \`package.json\` with your package manager, or \`cargo test\`, \`go test ./...\`, \`python3 -m pytest\`, \`make test\` |
| Security | \`.fabro/checks/security.sh\` if it exists. Otherwise the audit for your lockfile (such as \`npm audit --audit-level=high\`), and \`gitleaks\`, or a basic pattern check when gitleaks isn't installed |
| Code review | An agent, after confirming there is a change to review |

An agent then judges each check against the ticket and writes the report. All of those commands have to be available where the run executes, and audits need network access. A check that can't run says \`Status: BLOCKED\`, and the run stops rather than pretending or looping.

## How a failure blocks delivery

Each check ends in a command that reads its report and exits non-zero unless the first line is \`Status: PASS\` and no line says \`[blocking]\`. That fails the child workflow, which fails its node in this workflow, which takes the edge to \`bug_fix\`. Only \`outcome=succeeded\` leads toward the pull request. A fifth visit to any node in the loop fails the run.

## GitHub and Jira

Neither is set up here, so by default \`pr\` writes \`PR.md\` and \`jira\` writes \`jira-update.md\`, each saying nothing was created or changed. Nothing is ever merged automatically.

- **GitHub:** the builder writes \`workflow.toml\` on export, so add this to the exported file. It needs the GitHub App installed on the repository.

  \`\`\`toml
  [run.integrations.github.permissions]
  contents = "write"
  pull_requests = "write"
  \`\`\`

- **Jira:** give agents a Jira MCP server under \`[run.agent.mcps]\`, and put the key in the ticket.

Reports and notes (${NEVER_COMMITTED}) are written to the project's root while the run works. They are not meant to be merged.
`;

/** Every file the sample needs besides the workflow itself. */
export const EXAMPLE_ASSETS: Readonly<Record<string, string>> = Object.freeze({
  "README.md": README,
  [TICKET_PATH]: TICKET,
  "shared/testing.fabro": TESTING_WORKFLOW,
  "shared/security.fabro": SECURITY_WORKFLOW,
  "shared/code-review.fabro": CODE_REVIEW_WORKFLOW,
});

/** Prompt blocks matching the sample's prompts, with fresh ids, so its source isn't rewritten on open. */
export function examplePromptParts(): Record<string, PromptPart[]> {
  const parts: Record<string, PromptPart[]> = {};
  const { graph } = parseWorkflow(EXAMPLE_SOURCE);
  if (!graph) return parts;
  for (const node of graph.nodes) {
    const prompt = getAttr(effectiveNodeAttributes(graph, node), "prompt");
    if (prompt === undefined) continue;
    parts[node.id] = promptPartsFromAttribute(node.id, prompt).map((part) => ({ ...part, id: randomId() }));
  }
  return parts;
}
