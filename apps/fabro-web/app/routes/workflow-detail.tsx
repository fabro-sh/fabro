import { ChevronRightIcon } from "@heroicons/react/20/solid";
import { Link, Outlet, useLocation, useParams } from "react-router";
import { apiJson } from "../api";
import type { RunSettings, WorkflowDetailResponse as ApiWorkflowDetail } from "../lib/workflow-api";

export interface WorkflowEntry {
  name: string;
  slug: string;
  description: string;
  filename: string;
  settings: RunSettings;
  graph: string;
}

// Keep this exported for backward compatibility with other routes that import it.
// It will be populated by the loader, but the static version is kept as fallback.
export const workflowData: Record<string, WorkflowEntry> = {
  fix_build: {
    name: "Fix Build",
    slug: "fix_build",
    filename: "fix_build.fabro",
    description: "Automatically diagnoses and fixes CI build failures by analyzing error logs, identifying root causes, and applying targeted code changes.",
    settings: {
      version: 1,
      goal: "Diagnose and fix CI build failures",
      graph: "fix_build.fabro",
      llm: { model: "claude-sonnet" },
      vars: { repo_url: "https://github.com/org/service", branch: "main" },
      sandbox: {
        provider: "daytona",
        daytona: {
          auto_stop_interval: 60,
          labels: { project: "fix-build" },
          snapshot: { name: "fix-build-dev", cpu: 4, memory: 8, disk: 10 },
        },
      },
    },
    graph: `digraph fix_build {
    graph [
        goal="Diagnose and fix CI build failures",
        label="Fix Build"
    ]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    diagnose [label="Diagnose Failure", prompt="@prompts/fix_build/diagnose.md", reasoning_effort="high"]
    fix      [label="Apply Fix",        prompt="@prompts/fix_build/fix.md"]
    validate [label="Run Build",        prompt="@prompts/fix_build/validate.md", goal_gate=true]
    gate     [shape=diamond,            label="Build passing?"]

    start -> diagnose -> fix -> validate -> gate
    gate -> exit     [label="Yes", condition="outcome=success"]
    gate -> diagnose [label="No",  condition="outcome!=success", max_visits=3]
}
`,
  },
  implement: {
    name: "Implement Feature",
    slug: "implement",
    filename: "implement.fabro",
    description: "Generates production-ready code from a technical blueprint, including tests, documentation, and a pull request ready for review.",
    settings: {
      version: 1,
      goal: "Implement feature from technical blueprint",
      graph: "implement.fabro",
      llm: { model: "claude-sonnet" },
      vars: { spec_path: "specs/feature.md", test_framework: "vitest" },
      setup: { commands: ["bun install", "bun run typecheck"], timeout_ms: 120000 },
      sandbox: {
        provider: "daytona",
        daytona: {
          auto_stop_interval: 120,
          labels: { project: "implement", team: "engineering" },
          snapshot: { name: "implement-dev", cpu: 4, memory: 8, disk: 20 },
        },
      },
    },
    graph: `digraph implement {
    graph [
        goal="",
        label="Implement"
    ]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    strategy [shape=hexagon, label="Choose decomposition strategy:"]

    subgraph cluster_impl {
        label="Implementation Loop"
        node [fidelity="full", thread_id="impl"]

        plan      [label="Plan Implementation", prompt="@prompts/implement/plan.md", reasoning_effort="high"]
        implement [label="Implement",            prompt="@prompts/implement/implement.md"]
        review    [label="Review",               prompt="@prompts/implement/review.md"]
        validate  [label="Validate",             prompt="@prompts/implement/validate.md", goal_gate=true]
        fix       [label="Fix Failures",         prompt="@prompts/implement/fix.md", max_visits=3]
    }

    start -> strategy
    strategy -> plan [label="[L] Layer-by-layer"]
    strategy -> plan [label="[F] Feature slice"]
    strategy -> plan [label="[P] Embarrassingly parallel"]
    strategy -> plan [label="[S] Sequential / linear"]
    plan -> implement -> review -> validate
    validate -> exit [condition="outcome=success"]
    validate -> fix  [condition="outcome!=success", label="Fix"]
    fix -> validate
}
`,
  },
  sync_drift: {
    name: "Sync Drift",
    slug: "sync_drift",
    filename: "sync_drift.fabro",
    description: "Detects configuration and code drift between environments, then generates reconciliation patches to bring everything back in sync.",
    settings: {
      version: 1,
      goal: "Detect and reconcile configuration drift across environments",
      graph: "sync_drift.fabro",
      llm: { model: "claude-sonnet" },
      vars: { source_env: "production", target_env: "staging", drift_threshold: "warn" },
      sandbox: {
        provider: "daytona",
        daytona: {
          auto_stop_interval: 120,
          labels: { project: "sync-drift", team: "platform" },
          snapshot: { name: "sync-drift-dev", cpu: 2, memory: 4, disk: 10 },
        },
      },
    },
    graph: `digraph sync {
    graph [
        goal="Detect and resolve drift between product docs, architecture docs, and code",
        label="Sync"
    ]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    detect  [label="Detect Drift",     prompt="@prompts/sync/detect.md", reasoning_effort="high"]
    propose [label="Propose Changes",  prompt="@prompts/sync/propose.md"]
    review  [shape=hexagon,            label="Review Changes"]
    apply   [label="Apply Changes",    prompt="@prompts/sync/apply.md"]

    start -> detect
    detect -> exit    [condition="context.drift_found=false", label="No drift"]
    detect -> propose [condition="context.drift_found=true", label="Drift found"]
    propose -> review
    review -> apply    [label="[A] Accept"]
    review -> propose  [label="[R] Revise"]
    apply -> exit
}
`,
  },
  expand: {
    name: "Expand Product",
    slug: "expand",
    filename: "expand.fabro",
    description: "Evolves the product by analyzing usage patterns and specifications to propose and implement incremental improvements.",
    settings: {
      version: 1,
      goal: "Propose and implement incremental product improvements",
      graph: "expand.fabro",
      llm: { model: "claude-sonnet" },
      vars: { analytics_window: "30d", min_confidence: "0.8" },
      sandbox: {
        provider: "daytona",
        daytona: {
          auto_stop_interval: 180,
          labels: { project: "expand", team: "product" },
          snapshot: { name: "expand-dev", cpu: 2, memory: 4, disk: 10 },
        },
      },
    },
    graph: `digraph expand {
    graph [
        goal="",
        label="Expand"
    ]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    propose [label="Propose Changes",  prompt="@prompts/expand/propose.md", reasoning_effort="high"]
    approve [shape=hexagon,            label="Approve Changes"]
    execute [label="Execute Changes",  prompt="@prompts/expand/execute.md"]

    start -> propose -> approve
    approve -> execute [label="[A] Accept"]
    approve -> propose [label="[R] Revise"]
    execute -> exit
}
`,
  },
};

const tabs = [
  { name: "Definition", path: "" },
  { name: "Diagram", path: "/diagram" },
  { name: "Runs", path: "/runs" },
];

export const handle = { hideHeader: true };

export async function loader({ request, params }: any) {
  const apiWorkflow = await apiJson<ApiWorkflowDetail>(`/workflows/${params.name}`, { request });
  const workflow: WorkflowEntry = {
    name: apiWorkflow.name,
    slug: apiWorkflow.slug,
    description: apiWorkflow.description,
    filename: apiWorkflow.filename,
    settings: apiWorkflow.settings,
    graph: apiWorkflow.graph,
  };
  return { workflow };
}

export function meta({ data }: any) {
  const title = data?.workflow?.name ?? "Workflow";
  return [{ title: `${title} — Fabro` }];
}

export default function WorkflowDetail({ loaderData }: any) {
  const { name } = useParams();
  const { pathname } = useLocation();
  const workflow = loaderData.workflow;
  const basePath = `/workflows/${name}`;

  return (
    <div>
      <nav className="mb-4 flex items-center gap-1 text-sm text-fg-muted">
        <Link to="/workflows" className="text-fg-3 hover:text-fg">Workflows</Link>
        <ChevronRightIcon className="size-3" />
        <span>{workflow.name}</span>
      </nav>

      <div className="mb-6 flex items-center gap-4">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-3">
            <h2 className="text-xl font-semibold text-fg">{workflow.name}</h2>
            <span className="font-mono text-xs text-fg-muted">{workflow.filename}</span>
          </div>
          <p className="mt-2 max-w-prose text-sm leading-relaxed text-fg-3">{workflow.description}</p>
        </div>
        <button
          type="button"
          title="Run workflow"
          className="flex shrink-0 items-center gap-1.5 rounded-md border border-mint/20 px-3 py-1.5 text-sm font-medium text-mint transition-colors hover:border-mint/50 hover:bg-mint/10 hover:text-fg"
        >
          <svg viewBox="0 0 24 24" fill="currentColor" className="size-3.5" aria-hidden="true">
            <path fillRule="evenodd" d="M4.5 5.653c0-1.427 1.529-2.33 2.779-1.643l11.54 6.347c1.295.712 1.295 2.573 0 3.286L7.28 19.99c-1.25.687-2.779-.217-2.779-1.643V5.653Z" clipRule="evenodd" />
          </svg>
          Run
        </button>
      </div>

      <div className="border-b border-line">
        <nav className="-mb-px flex gap-6">
          {tabs.map((tab) => {
            const tabPath = `${basePath}${tab.path}`;
            const isActive = pathname === tabPath;
            return (
              <Link
                key={tab.name}
                to={tabPath}
                className={`border-b-2 pb-3 text-sm font-medium transition-colors ${
                  isActive
                    ? "border-teal-500 text-fg"
                    : "border-transparent text-fg-muted hover:border-line-strong hover:text-fg-3"
                }`}
              >
                {tab.name}
              </Link>
            );
          })}
        </nav>
      </div>

      <div className="mt-6">
        <Outlet />
      </div>
    </div>
  );
}
