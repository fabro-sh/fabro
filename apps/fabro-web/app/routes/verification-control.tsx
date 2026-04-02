import { Link, useParams } from "react-router";
import {
  ChevronRightIcon,
  LightBulbIcon,
  ClipboardDocumentListIcon,
  BookOpenIcon,
  FunnelIcon,
  Bars3BottomLeftIcon,
  WrenchIcon,
  PaintBrushIcon,
  CheckBadgeIcon,
  BugAntIcon,
  BoltIcon,
  BeakerIcon,
  StarIcon,
  ComputerDesktopIcon,
  CubeTransparentIcon,
  ArrowsRightLeftIcon,
  DocumentDuplicateIcon,
  SparklesIcon,
  ArchiveBoxXMarkIcon,
  ShieldExclamationIcon,
  ServerStackIcon,
  ExclamationTriangleIcon,
  LockClosedIcon,
  PuzzlePieceIcon,
  ArrowUturnLeftIcon,
  EyeIcon,
  CurrencyDollarIcon,
  ClipboardDocumentCheckIcon,
  CpuChipIcon,
  FingerPrintIcon,
  HandRaisedIcon,
  ScaleIcon,
  MapPinIcon,
  DocumentTextIcon,
  ShieldCheckIcon,
  WrenchScrewdriverIcon,
  KeyIcon,
  RocketLaunchIcon,
  BuildingLibraryIcon,
  CheckCircleIcon,
  XCircleIcon,
  MinusCircleIcon,
} from "@heroicons/react/20/solid";
import {
  slugify,
  typeConfig,
  modeConfig,
  statusConfig,
} from "../data/verifications";
import type {
  VerificationType,
  VerificationMode,
  VerificationResult,
} from "../data/verifications";
import { apiJson } from "../api";
import { timeAgo } from "../lib/time";
import type { VerificationDetailResponse } from "@qltysh/fabro-api-client";

export const handle = { hideHeader: true };

export async function loader({ request, params }: any) {
  const data = await apiJson<VerificationDetailResponse>(`/verification/controls/${params.id}`, { request });
  return { data };
}

export function meta({ data }: any) {
  const name = data?.data?.control?.name ?? "Verification";
  return [{ title: `${name} — Verification — Fabro` }];
}

type IconComponent = React.ComponentType<{ className?: string }>;

const criterionIcons: Record<string, IconComponent> = {
  "Motivation": LightBulbIcon,
  "Specifications": ClipboardDocumentListIcon,
  "Documentation": BookOpenIcon,
  "Minimization": FunnelIcon,
  "Formatting": Bars3BottomLeftIcon,
  "Linting": WrenchIcon,
  "Style": PaintBrushIcon,
  "Completeness": CheckBadgeIcon,
  "Defects": BugAntIcon,
  "Performance": BoltIcon,
  "Test Coverage": BeakerIcon,
  "Test Quality": StarIcon,
  "E2E Coverage": ComputerDesktopIcon,
  "Architecture": CubeTransparentIcon,
  "Interfaces": ArrowsRightLeftIcon,
  "Duplication": DocumentDuplicateIcon,
  "Simplicity": SparklesIcon,
  "Dead Code": ArchiveBoxXMarkIcon,
  "Vulnerabilities": ShieldExclamationIcon,
  "IaC Scanning": ServerStackIcon,
  "Dependency Alerts": ExclamationTriangleIcon,
  "Security Controls": LockClosedIcon,
  "Compatibility": PuzzlePieceIcon,
  "Rollout / Rollback": ArrowUturnLeftIcon,
  "Observability": EyeIcon,
  "Cost": CurrencyDollarIcon,
  "Change Control": ClipboardDocumentCheckIcon,
  "AI Governance": CpuChipIcon,
  "Privacy": FingerPrintIcon,
  "Accessibility": HandRaisedIcon,
  "Licensing": ScaleIcon,
};

const categoryIcons: Record<string, IconComponent> = {
  "Traceability": MapPinIcon,
  "Readability": DocumentTextIcon,
  "Reliability": ShieldCheckIcon,
  "Code Coverage": BeakerIcon,
  "Maintainability": WrenchScrewdriverIcon,
  "Security": KeyIcon,
  "Deployability": RocketLaunchIcon,
  "Compliance": BuildingLibraryIcon,
};

function TypeBadge({ type }: { type: VerificationType | null }) {
  if (type === null) return null;
  const config = typeConfig[type];
  return (
    <span
      className={`rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider ${config.color} ${config.bg}`}
    >
      {config.label}
    </span>
  );
}

function ModeBadge({ mode }: { mode: VerificationMode }) {
  const config = modeConfig[mode];
  return (
    <span
      className={`rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider ${config.color} ${config.bg}`}
    >
      {config.label}
    </span>
  );
}

function StatCard({ label, value, warn }: { label: string; value: string; warn?: boolean }) {
  return (
    <div className="rounded-md border border-line bg-panel/60 px-4 py-3">
      <p className="text-xs font-medium uppercase tracking-wider text-fg-muted">{label}</p>
      <p className={`mt-1 font-mono text-lg font-semibold tabular-nums ${warn ? "text-amber" : "text-fg"}`}>
        {value}
      </p>
    </div>
  );
}

function EvaluationBar({ evaluations }: { evaluations: readonly VerificationResult[] }) {
  if (evaluations.length === 0) {
    return <p className="text-sm italic text-fg-muted">No evaluations yet</p>;
  }

  const passCount = evaluations.filter((e) => e === "pass").length;
  const failCount = evaluations.filter((e) => e === "fail").length;
  const skipCount = evaluations.filter((e) => e === "skip").length;

  return (
    <div>
      <div className="flex items-center gap-1">
        {evaluations.map((result, i) => (
          <div
            key={`eval-${i}`}
            className={`h-6 flex-1 rounded-sm ${
              result === "pass"
                ? "bg-mint/70"
                : result === "fail"
                  ? "bg-coral/70"
                  : "bg-navy-600/50"
            }`}
          />
        ))}
      </div>
      <div className="mt-2 flex gap-4 text-xs text-fg-muted">
        <span className="flex items-center gap-1">
          <span className="inline-block size-2 rounded-sm bg-mint/70" />
          {passCount} pass
        </span>
        <span className="flex items-center gap-1">
          <span className="inline-block size-2 rounded-sm bg-coral/70" />
          {failCount} fail
        </span>
        {skipCount > 0 && (
          <span className="flex items-center gap-1">
            <span className="inline-block size-2 rounded-sm bg-navy-600/50" />
            {skipCount} skip
          </span>
        )}
      </div>
    </div>
  );
}

function ResultIcon({ result }: { result: VerificationResult }) {
  const config = statusConfig[result];
  if (result === "pass") return <CheckCircleIcon className={`size-4 ${config.color}`} />;
  if (result === "fail") return <XCircleIcon className={`size-4 ${config.color}`} />;
  return <MinusCircleIcon className={`size-4 ${config.color}`} />;
}

export default function VerificationDetail({ loaderData }: any) {
  const { data } = loaderData;
  const { control: controlInfo, performance: apiPerf, control_detail: apiDetail, recent_results: apiRecentResults, siblings: apiSiblings } = data;

  const criterion = {
    name: controlInfo.name,
    description: controlInfo.description,
    type: (controlInfo.type ?? null) as VerificationType | null,
  };
  const categoryName = controlInfo.criterion.name;
  const performance = {
    f1: apiPerf.f1 ?? null,
    passAt1: apiPerf.pass_at_1 ?? null,
    mode: apiPerf.mode as VerificationMode,
    evaluations: apiPerf.evaluations as VerificationResult[],
  };
  const detail = apiDetail ? {
    description: apiDetail.rationale,
    checks: apiDetail.checks,
    passExample: apiDetail.pass_example,
    failExample: apiDetail.fail_example,
  } : null;
  const recentResults = apiRecentResults.map((r) => ({
    runId: r.run.id,
    runTitle: r.run.title,
    workflow: r.workflow.slug,
    result: r.result as VerificationResult,
    timestamp: r.timestamp,
  }));
  const siblings = apiSiblings.map((s) => ({
    name: s.name,
    slug: s.slug,
    type: (s.type ?? null) as VerificationType | null,
    mode: (s.mode ?? "disabled") as VerificationMode,
  }));

  const Icon = criterionIcons[criterion.name];
  const CatIcon = categoryIcons[categoryName];

  const passRate = performance.evaluations.length > 0
    ? (performance.evaluations.filter((e) => e === "pass").length / performance.evaluations.length * 100).toFixed(0)
    : null;

  return (
    <div className="space-y-6">
      {/* Breadcrumb */}
      <nav className="flex items-center gap-1 text-sm text-fg-muted">
        <Link to="/verification/criteria" className="text-fg-3 hover:text-fg">Verification</Link>
        <ChevronRightIcon className="size-3" />
        <span className="text-fg-3">{categoryName}</span>
        <ChevronRightIcon className="size-3" />
        <span>{criterion.name}</span>
      </nav>

      {/* Header */}
      <div className="flex items-start gap-3">
        {Icon && <Icon className="mt-0.5 size-6 text-fg-3" />}
        <div className="min-w-0 flex-1">
          <h2 className="text-xl font-semibold text-fg">{criterion.name}</h2>
          <p className="mt-1 text-sm text-fg-muted">{criterion.description}</p>
          <div className="mt-2 flex items-center gap-2">
            <TypeBadge type={criterion.type} />
            <ModeBadge mode={performance.mode} />
          </div>
        </div>
      </div>

      {/* Description block */}
      {detail && (
        <div className="rounded-md border border-line bg-panel/60 px-5 py-4">
          <p className="text-sm leading-relaxed text-fg-2">{detail.description}</p>
        </div>
      )}

      {/* Stat cards */}
      <div className="grid grid-cols-4 gap-3">
        <StatCard label="Accuracy (F1)" value={performance.f1 != null ? performance.f1.toFixed(2) : "—"} />
        <StatCard label="pass@1" value={performance.passAt1 != null ? performance.passAt1.toFixed(2) : "—"} />
        <StatCard label="Pass Rate" value={passRate != null ? `${passRate}%` : "—"} />
        <StatCard label="Total Evals" value={String(performance.evaluations.length)} />
      </div>

      {/* Evaluation history */}
      <div>
        <h3 className="mb-3 text-sm font-semibold text-fg">Evaluation History</h3>
        <EvaluationBar evaluations={performance.evaluations} />
      </div>

      {/* Recent runs table */}
      <div>
        <h3 className="mb-3 text-sm font-semibold text-fg">Recent Runs</h3>
        <div className="rounded-md border border-line overflow-hidden">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-line bg-panel/60 text-left text-xs text-fg-muted">
                <th className="py-2.5 pl-4 pr-3 font-medium">Run</th>
                <th className="py-2.5 px-3 font-medium w-8">Result</th>
                <th className="py-2.5 px-3 font-medium">Workflow</th>
                <th className="py-2.5 pl-3 pr-4 font-medium text-right">Time</th>
              </tr>
            </thead>
            <tbody>
              {recentResults.map((run) => (
                <tr key={run.runId} className="border-b border-line last:border-b-0 cursor-pointer transition-colors hover:bg-overlay">
                  <td className="py-2.5 pl-4 pr-3">
                    <Link to={`/runs/${run.runId}`} className="font-medium text-fg-2 hover:text-fg">
                      {run.runTitle}
                    </Link>
                  </td>
                  <td className="py-2.5 px-3">
                    <ResultIcon result={run.result} />
                  </td>
                  <td className="py-2.5 px-3 text-fg-muted">{run.workflow}</td>
                  <td className="py-2.5 pl-3 pr-4 text-right text-fg-muted">{timeAgo(run.timestamp)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

      {/* What this checks / Examples */}
      {detail && (
        <div className="grid grid-cols-2 gap-4">
          <div className="rounded-md border border-line bg-panel/60 px-5 py-4">
            <h3 className="mb-3 text-sm font-semibold text-fg">What This Checks</h3>
            <ul className="space-y-2 text-sm text-fg-2">
              {detail.checks.map((check) => (
                <li key={check} className="flex items-start gap-2">
                  <span className="mt-1.5 size-1.5 shrink-0 rounded-full bg-fg-muted" />
                  {check}
                </li>
              ))}
            </ul>
          </div>
          <div className="space-y-4">
            <div className="rounded-md border border-line bg-panel/60 px-5 py-4">
              <h3 className="mb-2 text-sm font-semibold text-mint">Pass Example</h3>
              <p className="text-sm text-fg-2">{detail.passExample}</p>
            </div>
            <div className="rounded-md border border-line bg-panel/60 px-5 py-4">
              <h3 className="mb-2 text-sm font-semibold text-coral">Fail Example</h3>
              <p className="text-sm text-fg-2">{detail.failExample}</p>
            </div>
          </div>
        </div>
      )}

      {/* Sibling controls */}
      {siblings.length > 0 && (
        <div>
          <h3 className="mb-3 text-sm font-semibold text-fg">
            {CatIcon && <CatIcon className="mr-1.5 inline size-4 text-fg-3" />}
            Other {categoryName} Controls
          </h3>
          <div className="rounded-md border border-line overflow-hidden">
            <table className="w-full text-sm">
              <tbody>
                {siblings.map((sibling) => {
                  const SibIcon = criterionIcons[sibling.name];
                  return (
                    <tr key={sibling.name} className="border-b border-line last:border-b-0 cursor-pointer transition-colors hover:bg-overlay">
                      <td className="w-8 py-2.5 pl-4 pr-0">
                        {SibIcon && <SibIcon className="size-4 text-fg-3" />}
                      </td>
                      <td className="py-2.5 pl-2 pr-3">
                        <Link
                          to={`/verification/controls/${sibling.slug}`}
                          className="font-medium text-fg-2 hover:text-fg"
                        >
                          {sibling.name}
                        </Link>
                      </td>
                      <td className="py-2.5 px-3 text-fg-muted" />
                      <td className="whitespace-nowrap py-2.5 px-3 text-right">
                        <TypeBadge type={sibling.type} />
                      </td>
                      <td className="whitespace-nowrap py-2.5 pl-3 pr-4">
                        <ModeBadge mode={sibling.mode} />
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}
