import { useState } from "react";
import { useNavigate } from "react-router";
import {
  Disclosure,
  DisclosureButton,
  DisclosurePanel,
} from "@headlessui/react";
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
  MagnifyingGlassIcon,
  ChevronDownIcon,
} from "@heroicons/react/24/outline";
import {
  typeConfig,
  modeConfig,
  slugify,
} from "../data/verifications";
import type {
  VerificationType,
  VerificationMode,
  VerificationResult,
  VerificationCategory,
  CriterionPerformance,
} from "../data/verifications";
import { apiJson } from "../api";
import type { VerificationCriterion as ApiVerificationCriterion } from "@qltysh/fabro-api-client";

export async function loader({ request }: any) {
  const { data: apiCategories } = await apiJson<{ data: ApiVerificationCriterion[] }>("/verification/criteria", { request });
  const categories: VerificationCategory[] = apiCategories.map((cat) => ({
    name: cat.name,
    question: cat.question,
    status: "pass" as const,
    criteria: cat.controls.map((c) => ({
      name: c.name,
      description: c.description,
      type: (c.type ?? null) as VerificationType | null,
      status: "pass" as const,
    })),
  }));
  // Build a performance map from API data
  const criterionPerformance: Record<string, CriterionPerformance> = {};
  for (const cat of apiCategories) {
    for (const ctrl of cat.controls) {
      criterionPerformance[ctrl.name] = {
        f1: ctrl.f1 ?? null,
        passAt1: ctrl.pass_at_1 ?? null,
        mode: (ctrl.mode ?? "disabled") as VerificationMode,
        evaluations: (ctrl.evaluations ?? []) as VerificationResult[],
      };
    }
  }
  return { categories, criterionPerformance };
}

export const handle = { wide: true };

export function meta({}: any) {
  return [{ title: "Verifications — Fabro" }];
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

type ViewMode = "grouped" | "ungrouped";

function CriterionRow({ slug, children }: { slug: string; children: React.ReactNode }) {
  const navigate = useNavigate();
  return (
    <tr
      className="border-b border-line last:border-b-0 cursor-pointer transition-colors hover:bg-overlay"
      onClick={() => navigate(`/verification/controls/${slug}`)}
    >
      {children}
    </tr>
  );
}

function CategoryCard({ category, perfMap }: { category: VerificationCategory; perfMap: Record<string, CriterionPerformance> }) {
  return (
    <Disclosure
      as="div"
      className="rounded-md border border-line overflow-hidden"
    >
      <DisclosureButton className="group flex w-full items-center gap-3 px-4 py-3.5 text-left transition-colors hover:bg-overlay">
        {(() => {
          const CatIcon = categoryIcons[category.name];
          return CatIcon ? <CatIcon className="size-5 shrink-0 text-fg-3" /> : null;
        })()}
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-3">
            <span className="shrink-0 text-sm font-semibold text-fg">
              {category.name}
            </span>
            <span className="truncate text-xs text-fg-muted">
              {category.question}
            </span>
          </div>
        </div>
        <span className="shrink-0 text-xs text-fg-muted">
          <span className="font-mono tabular-nums">{category.criteria.length}</span> controls
        </span>
        <ChevronRightIcon className="size-4 shrink-0 text-fg-muted transition-transform duration-200 group-data-open:rotate-90" />
      </DisclosureButton>

      <DisclosurePanel
        transition
        className="origin-top transition duration-200 ease-out data-closed:-translate-y-1 data-closed:opacity-0"
      >
        <div className="border-t border-line">
          <table className="w-full text-sm">
            <tbody>
              {category.criteria.map((criterion) => {
                const Icon = criterionIcons[criterion.name];
                const perf = perfMap[criterion.name];
                return (
                  <CriterionRow key={criterion.name} slug={slugify(criterion.name)}>
                    <td className="w-8 py-2.5 pl-5 pr-0">
                      {Icon && <Icon className="size-4 text-fg-3" />}
                    </td>
                    <td className="whitespace-nowrap py-2.5 pl-2 pr-3 font-medium text-fg-2">
                      {criterion.name}
                    </td>
                    <td className="py-2.5 px-3 text-fg-muted">
                      {criterion.description || (
                        <span className="italic">Not configured</span>
                      )}
                    </td>
                    <td className="whitespace-nowrap py-2.5 pl-3 pr-1 text-right">
                      <TypeBadge type={criterion.type} />
                    </td>
                    <td className="whitespace-nowrap py-2.5 px-1">
                      {perf && <ModeBadge mode={perf.mode} />}
                    </td>
                    <td className="whitespace-nowrap py-2.5 pl-1 pr-4">
                      {perf && <EvaluationDots evaluations={perf.evaluations} />}
                    </td>
                  </CriterionRow>
                );
              })}
            </tbody>
          </table>
        </div>
      </DisclosurePanel>
    </Disclosure>
  );
}

function GroupedView({ categories, perfMap }: { categories: readonly VerificationCategory[]; perfMap: Record<string, CriterionPerformance> }) {
  return (
    <div className="space-y-3">
      {categories.map((category) => (
        <CategoryCard key={category.name} category={category} perfMap={perfMap} />
      ))}
    </div>
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

function UngroupedView({ categories, perfMap }: { categories: readonly VerificationCategory[]; perfMap: Record<string, CriterionPerformance> }) {
  return (
    <div className="rounded-md border border-line overflow-hidden">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-line bg-panel/60 text-left text-xs text-fg-muted">
            <th className="w-8 py-2.5 pl-4 pr-0 font-medium" />
            <th className="py-2.5 pl-2 pr-3 font-medium">Verification</th>
            <th className="py-2.5 px-3 font-medium">Description</th>
            <th className="py-2.5 px-3 font-medium">Category</th>
            <th className="py-2.5 px-3 font-medium text-right">Type</th>
            <th className="py-2.5 px-3 font-medium text-right">Accuracy (F1)</th>
            <th className="py-2.5 px-3 font-medium text-right">pass@1</th>
            <th className="py-2.5 px-3 font-medium">Mode</th>
            <th className="py-2.5 pl-3 pr-4 font-medium">Evaluations</th>
          </tr>
        </thead>
        <tbody>
          {categories.flatMap((category) =>
            category.criteria.map((criterion) => {
              const Icon = criterionIcons[criterion.name];
              const perf = perfMap[criterion.name];
              return (
                <CriterionRow key={`${category.name}-${criterion.name}`} slug={slugify(criterion.name)}>
                  <td className="w-8 py-2.5 pl-4 pr-0">
                    {Icon && <Icon className="size-4 text-fg-3" />}
                  </td>
                  <td className="whitespace-nowrap py-2.5 pl-2 pr-3 font-medium text-fg-2">
                    {criterion.name}
                  </td>
                  <td className="py-2.5 px-3 text-fg-muted">
                    {criterion.description || (
                      <span className="italic">Not configured</span>
                    )}
                  </td>
                  <td className="whitespace-nowrap py-2.5 px-3 text-xs text-fg-muted">
                    {category.name}
                  </td>
                  <td className="whitespace-nowrap py-2.5 px-3 text-right">
                    <TypeBadge type={criterion.type} />
                  </td>
                  <td className="whitespace-nowrap py-2.5 px-3 text-right font-mono text-xs tabular-nums text-fg-2">
                    {perf?.f1 != null ? perf.f1.toFixed(2) : <span className="text-fg-muted">—</span>}
                  </td>
                  <td className="whitespace-nowrap py-2.5 px-3 text-right font-mono text-xs tabular-nums text-fg-2">
                    {perf?.passAt1 != null ? perf.passAt1.toFixed(2) : <span className="text-fg-muted">—</span>}
                  </td>
                  <td className="whitespace-nowrap py-2.5 px-3">
                    {perf && <ModeBadge mode={perf.mode} />}
                  </td>
                  <td className="whitespace-nowrap py-2.5 pl-3 pr-4">
                    {perf && <EvaluationDots evaluations={perf.evaluations} />}
                  </td>
                </CriterionRow>
              );
            }),
          )}
        </tbody>
      </table>
    </div>
  );
}

function filterCategories(
  categories: readonly VerificationCategory[],
  query: string,
  modeFilter: VerificationMode | "all",
  perfMap: Record<string, CriterionPerformance>,
): VerificationCategory[] {
  const lowerQuery = query.toLowerCase();
  return categories
    .map((category) => {
      const filtered = category.criteria.filter((c) => {
        const perf = perfMap[c.name];
        const matchesMode = modeFilter === "all" || perf?.mode === modeFilter;
        const matchesQuery =
          lowerQuery === "" ||
          c.name.toLowerCase().includes(lowerQuery) ||
          c.description.toLowerCase().includes(lowerQuery) ||
          category.name.toLowerCase().includes(lowerQuery);
        return matchesMode && matchesQuery;
      });
      return { ...category, criteria: filtered };
    })
    .filter((category) => category.criteria.length > 0);
}

export default function Verifications({ loaderData }: any) {
  const { categories: verificationCategories, criterionPerformance } = loaderData;
  const [view, setView] = useState<ViewMode>("grouped");
  const [query, setQuery] = useState("");
  const [modeFilter, setModeFilter] = useState<VerificationMode | "all">("all");

  const filtered = filterCategories(verificationCategories, query, modeFilter, criterionPerformance);

  return (
    <div className="space-y-4">
      {/* Toolbar */}
      <div className="flex gap-3">
        <div className="relative flex-1">
          <MagnifyingGlassIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
          <input
            type="text"
            placeholder="Search verifications…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="w-full rounded-md border border-line bg-panel/80 py-2 pl-9 pr-3 text-sm text-fg-2 placeholder-fg-muted outline-none transition-colors focus:border-focus focus:ring-0"
          />
        </div>
        <div className="relative">
          <select
            value={modeFilter}
            onChange={(e) => setModeFilter(e.target.value as VerificationMode | "all")}
            className="appearance-none rounded-md border border-line bg-panel/80 py-2 pl-3 pr-8 text-sm text-fg-2 outline-none transition-colors focus:border-focus focus:ring-0"
          >
            <option value="all">All modes</option>
            <option value="active">Active</option>
            <option value="evaluate">Evaluate</option>
            <option value="disabled">Disabled</option>
          </select>
          <ChevronDownIcon className="pointer-events-none absolute right-2 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
        </div>
        <div className="flex items-center gap-1 rounded-md border border-line bg-panel/80 p-0.5">
          <button
            type="button"
            onClick={() => setView("grouped")}
            className={`inline-flex items-center gap-1.5 rounded px-2.5 py-1 text-xs font-medium transition-colors ${view === "grouped" ? "bg-overlay text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="size-3.5" aria-hidden="true">
              <path strokeLinecap="round" strokeLinejoin="round" d="M2.25 7.125C2.25 6.504 2.754 6 3.375 6h6c.621 0 1.125.504 1.125 1.125v3.75c0 .621-.504 1.125-1.125 1.125h-6a1.125 1.125 0 0 1-1.125-1.125v-3.75ZM14.25 8.625c0-.621.504-1.125 1.125-1.125h5.25c.621 0 1.125.504 1.125 1.125v8.25c0 .621-.504 1.125-1.125 1.125h-5.25a1.125 1.125 0 0 1-1.125-1.125v-8.25ZM3.75 16.125c0-.621.504-1.125 1.125-1.125h5.25c.621 0 1.125.504 1.125 1.125v1.5c0 .621-.504 1.125-1.125 1.125h-5.25a1.125 1.125 0 0 1-1.125-1.125v-1.5Z" />
            </svg>
            Grouped
          </button>
          <button
            type="button"
            onClick={() => setView("ungrouped")}
            className={`inline-flex items-center gap-1.5 rounded px-2.5 py-1 text-xs font-medium transition-colors ${view === "ungrouped" ? "bg-overlay text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="size-3.5" aria-hidden="true">
              <path strokeLinecap="round" strokeLinejoin="round" d="M3.75 12h16.5m-16.5 3.75h16.5M3.75 19.5h16.5M5.625 4.5h12.75a1.875 1.875 0 0 1 0 3.75H5.625a1.875 1.875 0 0 1 0-3.75Z" />
            </svg>
            List
          </button>
        </div>
      </div>

      {view === "grouped" ? <GroupedView categories={filtered} perfMap={criterionPerformance} /> : <UngroupedView categories={filtered} perfMap={criterionPerformance} />}
    </div>
  );
}
