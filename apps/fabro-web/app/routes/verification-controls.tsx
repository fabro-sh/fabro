import { useNavigate } from "react-router";
import {
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
} from "@heroicons/react/20/solid";
import {
  typeConfig,
  modeConfig,
} from "../data/verifications";
import type {
  VerificationType,
  VerificationMode,
} from "../data/verifications";
import { apiJson } from "../api";
import type { VerificationControlListItem } from "@qltysh/fabro-api-client";

export async function loader({ request }: any) {
  const { data: controls } = await apiJson<{ data: VerificationControlListItem[] }>("/verification/controls", { request });
  return { controls };
}

export const handle = { wide: true };

export function meta({}: any) {
  return [{ title: "Controls — Verification — Fabro" }];
}

type IconComponent = React.ComponentType<{ className?: string }>;

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

export default function VerificationControls({ loaderData }: any) {
  const { controls } = loaderData;
  const navigate = useNavigate();

  return (
    <div className="space-y-4">
      <div className="rounded-md border border-line overflow-hidden">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-line bg-panel/60 text-left text-xs text-fg-muted">
              <th className="w-8 py-2.5 pl-4 pr-0 font-medium" />
              <th className="py-2.5 pl-2 pr-3 font-medium">Control</th>
              <th className="py-2.5 px-3 font-medium">Description</th>
              <th className="py-2.5 px-3 font-medium">Criterion</th>
              <th className="py-2.5 px-3 font-medium text-right">Type</th>
              <th className="py-2.5 px-3 font-medium text-right">Accuracy (F1)</th>
              <th className="py-2.5 px-3 font-medium text-right">pass@1</th>
              <th className="py-2.5 pl-3 pr-4 font-medium">Mode</th>
            </tr>
          </thead>
          <tbody>
            {controls.map((control) => {
              const Icon = controlIcons[control.name];
              const mode = (control.mode ?? "disabled") as VerificationMode;
              return (
                <tr
                  key={control.slug}
                  className="border-b border-line last:border-b-0 cursor-pointer transition-colors hover:bg-overlay"
                  onClick={() => navigate(`/verification/controls/${control.slug}`)}
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
                  <td className="whitespace-nowrap py-2.5 px-3 text-xs text-fg-muted">
                    {control.criterion.name}
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
                  <td className="whitespace-nowrap py-2.5 pl-3 pr-4">
                    <ModeBadge mode={mode} />
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
