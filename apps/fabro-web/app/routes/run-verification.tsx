import {
  Disclosure,
  DisclosureButton,
  DisclosurePanel,
} from "@headlessui/react";
import {
  CheckCircleIcon,
  XCircleIcon,
  MinusCircleIcon,
  ChevronRightIcon,
} from "@heroicons/react/20/solid";
import {
  statusConfig,
  typeConfig,
  getCriteriaSummary,
} from "../data/verifications";
import type {
  VerificationResult,
  VerificationType,
  VerificationCategory,
} from "../data/verifications";
import { apiJson } from "../api";
import type { PaginatedRunVerificationList } from "@qltysh/fabro-api-client";

export async function loader({ request, params }: any) {
  const { data: apiCategories } = await apiJson<PaginatedRunVerificationList>(`/runs/${params.id}/verification`, { request });
  const categories: VerificationCategory[] = apiCategories.map((cat) => ({
    name: cat.name,
    question: cat.question,
    status: cat.status as VerificationResult,
    criteria: cat.controls.map((c) => ({
      name: c.name,
      description: c.description,
      type: (c.type ?? null) as VerificationType | null,
      status: c.status as VerificationResult,
    })),
  }));
  return { categories };
}

function StatusIcon({
  status,
  className = "size-5",
}: {
  status: VerificationResult;
  className?: string;
}) {
  switch (status) {
    case "pass":
      return <CheckCircleIcon className={`${className} text-mint`} />;
    case "fail":
      return <XCircleIcon className={`${className} text-coral`} />;
    case "na":
      return <MinusCircleIcon className={`${className} text-fg-muted`} />;
  }
}

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

function CategoryCard({ category }: { category: VerificationCategory }) {
  const criteriaStats = getCriteriaSummary(category.criteria);
  const applicable = criteriaStats.total - criteriaStats.na;
  const config = statusConfig[category.status];

  return (
    <Disclosure
      as="div"
      defaultOpen={category.status === "fail"}
      className={`rounded-md border border-line overflow-hidden border-l-2 ${config.border}`}
    >
      <DisclosureButton className="group flex w-full items-center gap-3 px-4 py-3.5 text-left transition-colors hover:bg-overlay">
        <StatusIcon status={category.status} />
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
        <span className="shrink-0 font-mono text-xs tabular-nums text-fg-muted">
          {criteriaStats.passing}/{applicable}
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
              {category.criteria.map((criterion) => (
                <tr
                  key={criterion.name}
                  className="border-b border-line last:border-b-0 cursor-pointer transition-colors hover:bg-overlay"
                >
                  <td className="w-8 py-2.5 pl-5 pr-0">
                    <span
                      className={`inline-block size-2 rounded-full ${statusConfig[criterion.status].dot}`}
                    />
                  </td>
                  <td className="whitespace-nowrap py-2.5 pl-2 pr-3 font-medium text-fg-2">
                    {criterion.name}
                  </td>
                  <td className="py-2.5 px-3 text-fg-muted">
                    {criterion.description || (
                      <span className="italic">Not configured</span>
                    )}
                  </td>
                  <td className="whitespace-nowrap py-2.5 pl-3 pr-4 text-right">
                    <TypeBadge type={criterion.type} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </DisclosurePanel>
    </Disclosure>
  );
}

export default function RunVerifications({ loaderData }: any) {
  const { categories } = loaderData;
  return (
    <div className="space-y-3">
      {categories.map((category) => (
        <CategoryCard key={category.name} category={category} />
      ))}
    </div>
  );
}
