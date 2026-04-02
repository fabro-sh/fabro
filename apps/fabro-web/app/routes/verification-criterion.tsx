import { Link, useNavigate } from "react-router";
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
} from "@heroicons/react/20/solid";
import {
  typeConfig,
  modeConfig,
  slugify,
} from "../data/verifications";
import type {
  VerificationType,
  VerificationMode,
  VerificationResult,
} from "../data/verifications";
import { apiJson } from "../api";
import type { VerificationCriterionDetail } from "@qltysh/fabro-api-client";

export const handle = { hideHeader: true };

export async function loader({ request, params }: any) {
  const data = await apiJson<VerificationCriterionDetail>(`/verification/criteria/${params.id}`, { request });
  return { data };
}

export function meta({ data }: any) {
  const name = data?.data?.name ?? "Criterion";
  return [{ title: `${name} — Verification — Fabro` }];
}

type IconComponent = React.ComponentType<{ className?: string }>;

function TrafficLightIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 20 20" fill="currentColor" className={className}>
      <path
        fillRule="evenodd"
        d="M7 3a3 3 0 0 1 6 0v14a3 3 0 0 1-6 0V3Zm3 1a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3Zm0 5a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3Zm0 5a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3Z"
        clipRule="evenodd"
      />
    </svg>
  );
}

const controlIcons: Record<string, IconComponent> = {
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
  "Code Coverage": TrafficLightIcon,
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

function EvaluationDots({ evaluations }: { evaluations: readonly VerificationResult[] }) {
  if (evaluations.length === 0) {
    return <span className="text-xs italic text-fg-muted">—</span>;
  }
  return (
    <div className="flex items-center gap-0.5">
      {evaluations.map((result, i) => (
        <span
          key={`eval-${i}`}
          className={`inline-block size-2.5 rounded-sm ${
            result === "pass"
              ? "bg-mint/70"
              : result === "fail"
                ? "bg-coral/70"
                : "bg-navy-600/50"
          }`}
        />
      ))}
    </div>
  );
}

export default function VerificationCriterion({ loaderData }: any) {
  const { data } = loaderData;
  const navigate = useNavigate();

  const CatIcon = categoryIcons[data.name];

  return (
    <div className="space-y-6">
      {/* Breadcrumb */}
      <nav className="flex items-center gap-1 text-sm text-fg-muted">
        <Link to="/verification/criteria" className="text-fg-3 hover:text-fg">Verification</Link>
        <ChevronRightIcon className="size-3" />
        <span>{data.name}</span>
      </nav>

      {/* Header */}
      <div className="flex items-start gap-3">
        {CatIcon && <CatIcon className="mt-0.5 size-6 text-fg-3" />}
        <div className="min-w-0 flex-1">
          <h2 className="text-xl font-semibold text-fg">{data.name}</h2>
          <p className="mt-1 text-sm text-fg-muted">{data.question}</p>
        </div>
      </div>

      {/* Controls table */}
      <div className="rounded-md border border-line overflow-hidden">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-line bg-panel/60 text-left text-xs text-fg-muted">
              <th className="w-8 py-2.5 pl-4 pr-0 font-medium" />
              <th className="py-2.5 pl-2 pr-3 font-medium">Control</th>
              <th className="py-2.5 px-3 font-medium">Description</th>
              <th className="py-2.5 px-3 font-medium text-right">Type</th>
              <th className="py-2.5 px-3 font-medium text-right">Accuracy (F1)</th>
              <th className="py-2.5 px-3 font-medium text-right">pass@1</th>
              <th className="py-2.5 px-3 font-medium">Mode</th>
              <th className="py-2.5 pl-3 pr-4 font-medium">Evaluations</th>
            </tr>
          </thead>
          <tbody>
            {data.controls.map((control) => {
              const Icon = controlIcons[control.name];
              const mode = (control.mode ?? "disabled") as VerificationMode;
              return (
                <tr
                  key={control.slug}
                  className="border-b border-line last:border-b-0 cursor-pointer transition-colors hover:bg-overlay"
                  onClick={() => navigate(`/verification/controls/${slugify(control.name)}`)}
                >
                  <td className="w-8 py-2.5 pl-4 pr-0">
                    {Icon && <Icon className="size-4 text-fg-3" />}
                  </td>
                  <td className="whitespace-nowrap py-2.5 pl-2 pr-3 font-medium text-fg-2">
                    {control.name}
                  </td>
                  <td className="py-2.5 px-3 text-fg-muted">
                    {control.description || (
                      <span className="italic">Not configured</span>
                    )}
                  </td>
                  <td className="whitespace-nowrap py-2.5 px-3 text-right">
                    <TypeBadge type={(control.type ?? null) as VerificationType | null} />
                  </td>
                  <td className="whitespace-nowrap py-2.5 px-3 text-right font-mono text-xs tabular-nums text-fg-2">
                    {control.f1 != null ? control.f1.toFixed(2) : <span className="text-fg-muted">—</span>}
                  </td>
                  <td className="whitespace-nowrap py-2.5 px-3 text-right font-mono text-xs tabular-nums text-fg-2">
                    {control.pass_at_1 != null ? control.pass_at_1.toFixed(2) : <span className="text-fg-muted">—</span>}
                  </td>
                  <td className="whitespace-nowrap py-2.5 px-3">
                    <ModeBadge mode={mode} />
                  </td>
                  <td className="whitespace-nowrap py-2.5 pl-3 pr-4">
                    <EvaluationDots evaluations={(control.evaluations ?? []) as VerificationResult[]} />
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
