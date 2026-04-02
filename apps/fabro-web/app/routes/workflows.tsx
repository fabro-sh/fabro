import { useState, type ComponentType } from "react";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";
import { ChevronDownIcon, PlusIcon } from "@heroicons/react/20/solid";
import { ChevronDownIcon as ChevronDownOutline } from "@heroicons/react/24/outline";
import {
  ArrowsRightLeftIcon,
  ClockIcon,
  CodeBracketIcon,
  MagnifyingGlassIcon,
  PauseIcon,
  RocketLaunchIcon,
  ShieldCheckIcon,
  WrenchIcon,
} from "@heroicons/react/24/outline";
import { Link } from "react-router";
import { apiJson } from "../api";
import { timeAgo, timeUntil } from "../lib/time";
import type { PaginatedWorkflowList } from "@qltysh/fabro-api-client";

export function meta({}: any) {
  return [{ title: "Workflows — Fabro" }];
}

export const handle = {
  headerExtra: (
    <div className="relative inline-flex rounded-md">
      <button
        type="button"
        className="inline-flex items-center gap-1.5 rounded-l-md border border-r-0 border-mint/20 px-3 py-1.5 text-sm font-medium text-mint transition-colors hover:border-mint/50 hover:bg-mint/10 hover:text-fg"
      >
        <PlusIcon className="size-3.5" aria-hidden="true" />
        Create Workflow
      </button>
      <Menu as="div" className="relative -ml-px flex">
        <MenuButton className="inline-flex items-center rounded-r-md border border-mint/20 px-1.5 text-mint transition-colors hover:border-mint/50 hover:bg-mint/10 hover:text-fg">
          <ChevronDownIcon className="size-3.5" aria-hidden="true" />
        </MenuButton>
        <MenuItems
          transition
          className="absolute right-0 top-full z-10 mt-2 w-48 origin-top-right rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:transform data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
        >
          <MenuItem>
            <button
              type="button"
              className="block w-full px-4 py-2 text-left text-sm text-fg-3 data-focus:bg-overlay data-focus:outline-hidden"
            >
              Import from file
            </button>
          </MenuItem>
          <MenuItem>
            <button
              type="button"
              className="block w-full px-4 py-2 text-left text-sm text-fg-3 data-focus:bg-overlay data-focus:outline-hidden"
            >
              Duplicate existing
            </button>
          </MenuItem>
        </MenuItems>
      </Menu>
    </div>
  ),
};

interface Workflow {
  name: string;
  slug: string;
  filename: string;
  lastRun: string;
  icon: ComponentType<{ className?: string }>;
  color: string;
  schedule?: string;
  nextRun?: string;
}

function getSlugIcon(slug: string): ComponentType<{ className?: string }> {
  return slugIconMap[slug] ?? CodeBracketIcon;
}

const slugIconMap: Record<string, ComponentType<{ className?: string }>> = {
  fix_build: WrenchIcon,
  implement: CodeBracketIcon,
  sync_drift: ArrowsRightLeftIcon,
  expand: RocketLaunchIcon,
  security_scan: ShieldCheckIcon,
  dep_audit: ClockIcon,
};

const slugColorMap: Record<string, string> = {
  fix_build: "var(--color-amber)",
  implement: "var(--color-teal-500)",
  sync_drift: "var(--color-mint)",
  expand: "var(--color-coral)",
  security_scan: "var(--color-teal-500)",
  dep_audit: "var(--color-amber)",
};

interface WorkflowData {
  name: string;
  slug: string;
  filename: string;
  lastRun: string;
  color: string;
  schedule?: string;
  nextRun?: string;
}

export async function loader({ request }: any) {
  const { data: apiWorkflows } = await apiJson<PaginatedWorkflowList>("/workflows", { request });
  const workflows: WorkflowData[] = apiWorkflows.map((w) => ({
    name: w.name,
    slug: w.slug,
    filename: w.filename,
    lastRun: w.last_run?.ran_at ? timeAgo(w.last_run.ran_at) : "never",
    color: slugColorMap[w.slug] ?? "var(--color-teal-500)",
    schedule: w.schedule?.expression,
    nextRun: w.schedule?.next_run ? timeUntil(w.schedule.next_run) : undefined,
  }));
  return { workflows };
}

function enrichWorkflows(data: WorkflowData[]): Workflow[] {
  return data.map((w) => ({
    ...w,
    icon: getSlugIcon(w.slug),
  }));
}

function PlayIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
      <path fillRule="evenodd" d="M4.5 5.653c0-1.427 1.529-2.33 2.779-1.643l11.54 6.347c1.295.712 1.295 2.573 0 3.286L7.28 19.99c-1.25.687-2.779-.217-2.779-1.643V5.653Z" clipRule="evenodd" />
    </svg>
  );
}

function EllipsisIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
      <path fillRule="evenodd" d="M10.5 12a1.5 1.5 0 1 1 3 0 1.5 1.5 0 0 1-3 0Zm6 0a1.5 1.5 0 1 1 3 0 1.5 1.5 0 0 1-3 0Zm-12 0a1.5 1.5 0 1 1 3 0 1.5 1.5 0 0 1-3 0Z" clipRule="evenodd" />
    </svg>
  );
}

function WorkflowCard({ workflow }: { workflow: Workflow }) {
  const Icon = workflow.icon;
  return (
    <div className="group flex items-center gap-4 rounded-md border border-line bg-panel/80 p-4 transition-all duration-200 hover:border-line-strong hover:bg-panel hover:shadow-lg hover:shadow-black/20">
      <Link to={`/workflows/${workflow.slug}`} className="flex min-w-0 flex-1 items-center gap-4">
        <div
          className="flex size-9 shrink-0 items-center justify-center rounded-md border bg-panel-alt/60"
          style={{ borderColor: `color-mix(in srgb, ${workflow.color} 20%, transparent)`, color: workflow.color }}
        >
          <Icon className="size-4" />
        </div>

        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-fg-2 group-hover:text-fg">{workflow.name}</span>
            <span className="font-mono text-xs text-fg-muted">{workflow.filename}</span>
            {workflow.schedule && (
              <span className="inline-flex items-center gap-1 rounded-full bg-teal-500/10 border border-teal-500/20 px-2 py-0.5 text-[11px] font-medium text-teal-300">
                <ClockIcon className="size-3" />
                {workflow.schedule}
              </span>
            )}
          </div>
          <p className="mt-1 text-xs text-fg-muted">
            {workflow.nextRun ?? `Last run ${workflow.lastRun}`}
          </p>
        </div>
      </Link>

      {workflow.schedule ? (
        <button
          type="button"
          title="Pause schedule"
          className="flex size-8 shrink-0 items-center justify-center rounded-full border border-amber/20 text-amber transition-colors hover:border-amber/50 hover:bg-amber/10 hover:text-fg"
        >
          <PauseIcon className="size-3.5" />
        </button>
      ) : (
        <button
          type="button"
          title="Run workflow"
          className="flex size-8 shrink-0 items-center justify-center rounded-full border border-mint/20 text-mint transition-colors hover:border-mint/50 hover:bg-mint/10 hover:text-fg"
        >
          <PlayIcon className="size-3.5" />
        </button>
      )}

      <button
        type="button"
        title="Actions"
        className="flex size-8 shrink-0 items-center justify-center rounded-md text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3"
      >
        <EllipsisIcon className="size-5" />
      </button>
    </div>
  );
}

type TriggerFilter = "all" | "scheduled" | "manual";

export default function Workflows({ loaderData }: any) {
  const workflows = enrichWorkflows(loaderData.workflows);
  const [query, setQuery] = useState("");
  const [triggerFilter, setTriggerFilter] = useState<TriggerFilter>("all");
  const filtered = workflows.filter(
    (w) =>
      (triggerFilter === "all" ||
        (triggerFilter === "scheduled" && w.schedule != null) ||
        (triggerFilter === "manual" && w.schedule == null)) &&
      (w.name.toLowerCase().includes(query.toLowerCase()) ||
        w.filename.toLowerCase().includes(query.toLowerCase())),
  );

  return (
    <div className="space-y-4">
      <div className="flex gap-3">
        <div className="relative flex-1">
          <MagnifyingGlassIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
          <input
            type="text"
            placeholder="Search workflows..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="w-full rounded-md border border-line bg-panel/80 py-2 pl-9 pr-3 text-sm text-fg-2 placeholder-fg-muted outline-none transition-colors focus:border-focus focus:ring-0"
          />
        </div>
        <div className="relative">
          <select
            value={triggerFilter}
            onChange={(e) => setTriggerFilter(e.target.value as TriggerFilter)}
            className="appearance-none rounded-md border border-line bg-panel/80 py-2 pl-3 pr-8 text-sm text-fg-2 outline-none transition-colors focus:border-focus focus:ring-0"
          >
            <option value="all">All triggers</option>
            <option value="scheduled">Scheduled</option>
            <option value="manual">Manual</option>
          </select>
          <ChevronDownOutline className="pointer-events-none absolute right-2 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
        </div>
      </div>
      <div className="space-y-3">
        {filtered.map((workflow) => (
          <WorkflowCard key={workflow.filename} workflow={workflow} />
        ))}
        {filtered.length === 0 && (
          <p className="py-8 text-center text-sm text-fg-muted">No workflows match "{query}"</p>
        )}
      </div>
    </div>
  );
}
