export type SmoothnessRating = "effortless" | "smooth" | "bumpy" | "struggled" | "failed";

export type LearningCategory = "repo" | "code" | "workflow" | "tool";

export interface Learning {
  category: LearningCategory;
  text: string;
}

export type FrictionKind = "retry" | "timeout" | "wrong_approach" | "tool_failure" | "ambiguity";

export interface FrictionPoint {
  kind: FrictionKind;
  description: string;
  stage_id?: string;
}

export type OpenItemKind = "tech_debt" | "follow_up" | "investigation" | "test_gap";

export interface OpenItem {
  kind: OpenItemKind;
  description: string;
}

export interface StageRetro {
  stage_id: string;
  stage_label: string;
  status: string;
  duration_ms: number;
  retries: number;
  cost?: number;
  notes?: string;
  failure_reason?: string;
  files_touched: string[];
}

export interface AggregateStats {
  total_duration_ms: number;
  total_cost?: number;
  total_retries: number;
  files_touched: string[];
  stages_completed: number;
  stages_failed: number;
}

export interface Retro {
  run_id: string;
  pipeline_name: string;
  goal: string;
  timestamp: string;
  smoothness?: SmoothnessRating;
  stages: StageRetro[];
  stats: AggregateStats;
  intent?: string;
  outcome?: string;
  learnings?: Learning[];
  friction_points?: FrictionPoint[];
  open_items?: OpenItem[];
}

export const smoothnessConfig: Record<SmoothnessRating, { label: string; bg: string; text: string; dot: string }> = {
  effortless: { label: "Effortless", bg: "bg-emerald-500/15", text: "text-emerald-400", dot: "bg-emerald-400" },
  smooth: { label: "Smooth", bg: "bg-mint/15", text: "text-mint", dot: "bg-mint" },
  bumpy: { label: "Bumpy", bg: "bg-amber/15", text: "text-amber", dot: "bg-amber" },
  struggled: { label: "Struggled", bg: "bg-orange-500/15", text: "text-orange-400", dot: "bg-orange-400" },
  failed: { label: "Failed", bg: "bg-coral/15", text: "text-coral", dot: "bg-coral" },
};

export const learningCategoryConfig: Record<LearningCategory, { label: string; text: string }> = {
  repo: { label: "Repo", text: "text-teal-400" },
  code: { label: "Code", text: "text-sky-400" },
  workflow: { label: "Workflow", text: "text-violet-400" },
  tool: { label: "Tool", text: "text-amber" },
};

export const frictionKindConfig: Record<FrictionKind, { label: string; text: string }> = {
  retry: { label: "Retry", text: "text-amber" },
  timeout: { label: "Timeout", text: "text-coral" },
  wrong_approach: { label: "Wrong Approach", text: "text-orange-400" },
  tool_failure: { label: "Tool Failure", text: "text-coral" },
  ambiguity: { label: "Ambiguity", text: "text-violet-400" },
};

export const openItemKindConfig: Record<OpenItemKind, { label: string; text: string }> = {
  tech_debt: { label: "Tech Debt", text: "text-orange-400" },
  follow_up: { label: "Follow-up", text: "text-teal-400" },
  investigation: { label: "Investigation", text: "text-sky-400" },
  test_gap: { label: "Test Gap", text: "text-coral" },
};

const mockRetros: Retro[] = [
  {
    run_id: "run-1",
    pipeline_name: "implement",
    goal: "Add rate limiting to auth endpoints",
    timestamp: "2026-02-28T14:32:00Z",
    smoothness: "smooth",
    intent: "Implement token-bucket rate limiting on /auth/login and /auth/register to prevent brute-force attacks.",
    outcome: "Rate limiter deployed with configurable per-IP limits. Integration tests added. Redis-backed counter with sliding window.",
    stages: [
      {
        stage_id: "detect-drift",
        stage_label: "Detect Drift",
        status: "completed",
        duration_ms: 72_000,
        retries: 0,
        cost: 0.48,
        files_touched: ["src/middleware/rate-limit.ts"],
      },
      {
        stage_id: "propose-changes",
        stage_label: "Propose Changes",
        status: "completed",
        duration_ms: 154_000,
        retries: 0,
        cost: 1.12,
        files_touched: ["src/middleware/rate-limit.ts", "src/routes/auth.ts", "src/config.ts"],
      },
      {
        stage_id: "review-changes",
        stage_label: "Review Changes",
        status: "completed",
        duration_ms: 45_000,
        retries: 0,
        cost: 0.31,
        files_touched: [],
      },
      {
        stage_id: "apply-changes",
        stage_label: "Apply Changes",
        status: "completed",
        duration_ms: 118_000,
        retries: 0,
        cost: 0.87,
        files_touched: ["src/middleware/rate-limit.ts", "src/routes/auth.ts", "src/config.ts", "tests/rate-limit.test.ts"],
      },
    ],
    stats: {
      total_duration_ms: 389_000,
      total_cost: 2.78,
      total_retries: 0,
      files_touched: ["src/middleware/rate-limit.ts", "src/routes/auth.ts", "src/config.ts", "tests/rate-limit.test.ts"],
      stages_completed: 4,
      stages_failed: 0,
    },
    learnings: [
      { category: "repo", text: "Redis client is initialized lazily in src/infra/redis.ts -- reuse existing connection pool." },
      { category: "code", text: "Auth middleware chain order matters: rate-limit must run before JWT validation." },
    ],
    friction_points: [],
    open_items: [
      { kind: "follow_up", description: "Add rate-limit headers (X-RateLimit-Remaining) to response." },
    ],
  },
  {
    run_id: "run-2",
    pipeline_name: "implement",
    goal: "Migrate to React Router v7",
    timestamp: "2026-02-28T10:15:00Z",
    smoothness: "bumpy",
    intent: "Upgrade react-router from v6 to v7, updating all route definitions and loader/action patterns to the new API.",
    outcome: "Migration completed but required 3 retries in the apply stage due to breaking changes in nested route handling. All routes now use the v7 data API.",
    stages: [
      {
        stage_id: "detect-drift",
        stage_label: "Detect Drift",
        status: "completed",
        duration_ms: 95_000,
        retries: 0,
        cost: 0.62,
        files_touched: ["package.json"],
      },
      {
        stage_id: "propose-changes",
        stage_label: "Propose Changes",
        status: "completed",
        duration_ms: 312_000,
        retries: 1,
        cost: 2.45,
        notes: "First proposal missed nested outlet patterns. Retry produced correct migration.",
        files_touched: ["src/routes.ts", "src/app.tsx", "src/routes/dashboard.tsx", "src/routes/settings.tsx"],
      },
      {
        stage_id: "review-changes",
        stage_label: "Review Changes",
        status: "completed",
        duration_ms: 88_000,
        retries: 0,
        cost: 0.54,
        files_touched: [],
      },
      {
        stage_id: "apply-changes",
        stage_label: "Apply Changes",
        status: "completed",
        duration_ms: 480_000,
        retries: 3,
        cost: 3.21,
        notes: "Type errors in nested layouts required multiple correction passes.",
        files_touched: ["src/routes.ts", "src/app.tsx", "src/routes/dashboard.tsx", "src/routes/settings.tsx", "src/routes/profile.tsx", "tests/routes.test.tsx"],
      },
    ],
    stats: {
      total_duration_ms: 975_000,
      total_cost: 6.82,
      total_retries: 4,
      files_touched: ["package.json", "src/routes.ts", "src/app.tsx", "src/routes/dashboard.tsx", "src/routes/settings.tsx", "src/routes/profile.tsx", "tests/routes.test.tsx"],
      stages_completed: 4,
      stages_failed: 0,
    },
    learnings: [
      { category: "workflow", text: "Framework migration tasks benefit from running type-check after each stage, not just at the end." },
      { category: "code", text: "React Router v7 outlets require explicit type annotations for loader data in nested routes." },
      { category: "tool", text: "The codemod tool missed JSX spread patterns -- manual fixup was needed." },
    ],
    friction_points: [
      { kind: "retry", description: "Nested route outlet types were incorrect on first 3 attempts.", stage_id: "apply-changes" },
      { kind: "wrong_approach", description: "Initially tried to keep v6 compat layer, which created more issues than a clean migration.", stage_id: "propose-changes" },
    ],
    open_items: [
      { kind: "tech_debt", description: "Leftover v6 compat shims in src/utils/router-compat.ts should be deleted." },
      { kind: "test_gap", description: "No E2E coverage for the new nested layout error boundaries." },
    ],
  },
  {
    run_id: "run-6",
    pipeline_name: "implement",
    goal: "Add dark mode toggle",
    timestamp: "2026-02-27T16:45:00Z",
    smoothness: "effortless",
    intent: "Add a theme toggle component to the dashboard header with system/light/dark options, persisting preference to localStorage.",
    outcome: "Dark mode toggle shipped with smooth CSS transitions. All existing components already used CSS variables, so no style refactoring was needed.",
    stages: [
      {
        stage_id: "detect-drift",
        stage_label: "Detect Drift",
        status: "completed",
        duration_ms: 42_000,
        retries: 0,
        cost: 0.28,
        files_touched: [],
      },
      {
        stage_id: "propose-changes",
        stage_label: "Propose Changes",
        status: "completed",
        duration_ms: 98_000,
        retries: 0,
        cost: 0.71,
        files_touched: ["src/components/ThemeToggle.tsx", "src/hooks/useTheme.ts"],
      },
      {
        stage_id: "apply-changes",
        stage_label: "Apply Changes",
        status: "completed",
        duration_ms: 76_000,
        retries: 0,
        cost: 0.52,
        files_touched: ["src/components/ThemeToggle.tsx", "src/hooks/useTheme.ts", "src/layouts/Header.tsx"],
      },
    ],
    stats: {
      total_duration_ms: 216_000,
      total_cost: 1.51,
      total_retries: 0,
      files_touched: ["src/components/ThemeToggle.tsx", "src/hooks/useTheme.ts", "src/layouts/Header.tsx"],
      stages_completed: 3,
      stages_failed: 0,
    },
    learnings: [
      { category: "repo", text: "CSS variables are defined in src/styles/tokens.css and already support dark values." },
    ],
    friction_points: [],
    open_items: [],
  },
  {
    run_id: "run-3",
    pipeline_name: "fix_build",
    goal: "Fix config parsing for nested values",
    timestamp: "2026-02-27T09:20:00Z",
    smoothness: "struggled",
    intent: "Fix TOML config parser to handle deeply nested table arrays, which was causing silent data loss on certain pipeline configs.",
    outcome: "Root cause identified as incorrect recursion depth limit in the TOML walker. Fix applied but exposed a second bug in default value merging that required additional changes.",
    stages: [
      {
        stage_id: "investigate",
        stage_label: "Investigate",
        status: "completed",
        duration_ms: 340_000,
        retries: 2,
        cost: 1.85,
        notes: "First investigation looked at wrong parser path. Second attempt found the actual recursion limit.",
        files_touched: ["src/config/parser.ts", "src/config/defaults.ts"],
      },
      {
        stage_id: "propose-fix",
        stage_label: "Propose Fix",
        status: "completed",
        duration_ms: 210_000,
        retries: 1,
        cost: 1.42,
        files_touched: ["src/config/parser.ts", "src/config/defaults.ts", "src/config/merge.ts"],
      },
      {
        stage_id: "apply-fix",
        stage_label: "Apply Fix",
        status: "completed",
        duration_ms: 185_000,
        retries: 1,
        cost: 1.15,
        failure_reason: "Initial fix broke the default value merging path. Required a second pass.",
        files_touched: ["src/config/parser.ts", "src/config/defaults.ts", "src/config/merge.ts", "tests/config-parser.test.ts"],
      },
      {
        stage_id: "verify",
        stage_label: "Verify",
        status: "completed",
        duration_ms: 95_000,
        retries: 0,
        cost: 0.55,
        files_touched: [],
      },
    ],
    stats: {
      total_duration_ms: 830_000,
      total_cost: 4.97,
      total_retries: 4,
      files_touched: ["src/config/parser.ts", "src/config/defaults.ts", "src/config/merge.ts", "tests/config-parser.test.ts"],
      stages_completed: 4,
      stages_failed: 0,
    },
    learnings: [
      { category: "code", text: "TOML walker in parser.ts has a hardcoded depth limit of 8 -- needs to be configurable." },
      { category: "code", text: "Default merging in merge.ts uses shallow spread, which silently drops nested keys." },
      { category: "workflow", text: "Bug fix pipelines should include a regression test stage before verification." },
    ],
    friction_points: [
      { kind: "wrong_approach", description: "Initial investigation focused on the YAML compatibility layer instead of the TOML parser.", stage_id: "investigate" },
      { kind: "retry", description: "Fix introduced a regression in default value merging that required rework.", stage_id: "apply-fix" },
      { kind: "ambiguity", description: "Config schema docs were outdated, making it unclear which nesting depth was intended." },
    ],
    open_items: [
      { kind: "tech_debt", description: "Remove the hardcoded depth limit in src/config/parser.ts and make it configurable." },
      { kind: "investigation", description: "Audit other parsers for similar shallow-spread bugs in merging logic." },
      { kind: "test_gap", description: "No tests for configs nested deeper than 4 levels." },
    ],
  },
  {
    run_id: "run-8",
    pipeline_name: "implement",
    goal: "Implement webhook retry logic",
    timestamp: "2026-02-26T11:00:00Z",
    smoothness: "smooth",
    intent: "Add exponential backoff retry logic for failed webhook deliveries with configurable max attempts and dead-letter queue.",
    outcome: "Webhook retry system implemented with exponential backoff (base 2s, max 5 retries). Failed deliveries route to SQS dead-letter queue. Dashboard shows retry status.",
    stages: [
      {
        stage_id: "detect-drift",
        stage_label: "Detect Drift",
        status: "completed",
        duration_ms: 55_000,
        retries: 0,
        cost: 0.35,
        files_touched: [],
      },
      {
        stage_id: "propose-changes",
        stage_label: "Propose Changes",
        status: "completed",
        duration_ms: 178_000,
        retries: 0,
        cost: 1.28,
        files_touched: ["src/webhooks/retry.ts", "src/webhooks/dlq.ts", "src/webhooks/dispatcher.ts"],
      },
      {
        stage_id: "review-changes",
        stage_label: "Review Changes",
        status: "completed",
        duration_ms: 62_000,
        retries: 0,
        cost: 0.41,
        files_touched: [],
      },
      {
        stage_id: "apply-changes",
        stage_label: "Apply Changes",
        status: "completed",
        duration_ms: 145_000,
        retries: 1,
        cost: 1.05,
        notes: "Minor type fix needed on retry delay calculation.",
        files_touched: ["src/webhooks/retry.ts", "src/webhooks/dlq.ts", "src/webhooks/dispatcher.ts", "tests/webhook-retry.test.ts"],
      },
    ],
    stats: {
      total_duration_ms: 440_000,
      total_cost: 3.09,
      total_retries: 1,
      files_touched: ["src/webhooks/retry.ts", "src/webhooks/dlq.ts", "src/webhooks/dispatcher.ts", "tests/webhook-retry.test.ts"],
      stages_completed: 4,
      stages_failed: 0,
    },
    learnings: [
      { category: "repo", text: "SQS client wrapper is in src/infra/sqs.ts with pre-configured DLQ ARNs per environment." },
      { category: "code", text: "Webhook dispatcher already had a hook point for retry logic via the onFailure callback." },
    ],
    friction_points: [
      { kind: "retry", description: "Retry delay formula had an off-by-one in the exponent calculation.", stage_id: "apply-changes" },
    ],
    open_items: [
      { kind: "follow_up", description: "Add webhook retry metrics to the Grafana dashboard." },
      { kind: "follow_up", description: "Document the DLQ reprocessing procedure in the runbook." },
    ],
  },
];

export function allRetros(): Retro[] {
  return mockRetros;
}

export function findRetro(runId: string): Retro | undefined {
  return mockRetros.find((r) => r.run_id === runId);
}

function formatDuration(ms: number): string {
  const totalSeconds = Math.floor(ms / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (minutes >= 60) {
    const hours = Math.floor(minutes / 60);
    const remainMinutes = minutes % 60;
    return `${hours}h ${remainMinutes}m`;
  }
  return `${minutes}m ${seconds}s`;
}

export { formatDuration };
