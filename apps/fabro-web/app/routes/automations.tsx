import { useState, type ComponentType } from "react";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";
import { useSWRConfig } from "swr";
import { PlusIcon } from "@heroicons/react/20/solid";
import { EllipsisVerticalIcon } from "@heroicons/react/20/solid";
import {
  ArrowPathIcon,
  ArrowsRightLeftIcon,
  ClockIcon,
  CodeBracketIcon,
  MagnifyingGlassIcon,
  PauseIcon,
  RocketLaunchIcon,
  ShieldCheckIcon,
  WrenchIcon,
} from "@heroicons/react/24/outline";
import { FilterButton } from "../components/runs-list/filter-button";
import type { Automation, AutomationListResponse } from "@qltysh/fabro-api-client";
import { Link, useNavigate } from "react-router";
import { ApiError, apiData, automationsApi } from "../lib/api-client";
import { findScheduleTrigger, hasEnabledApiTrigger } from "../lib/automation";
import { useAutomations } from "../lib/queries";
import { queryKeys } from "../lib/query-keys";
import { ConfirmDialog } from "../components/ui";
import { useToast } from "../components/toast";

export function meta({}: any) {
  return [{ title: "Automations — Fabro" }];
}

export const handle = { hideHeader: true };

function CreateAutomationButton() {
  return (
    <Link
      to="/automations/new"
      className="inline-flex shrink-0 items-center gap-1.5 rounded-md border border-mint/20 px-3 py-2 text-sm font-medium text-mint transition-colors hover:border-mint/50 hover:bg-mint/10 hover:text-fg"
    >
      <PlusIcon className="size-3.5" aria-hidden="true" />
      Create Automation
    </Link>
  );
}

interface AutomationRow {
  id: string;
  revision: string;
  name: string;
  workflow: string;
  repository: string;
  schedule?: string;
  apiEnabled: boolean;
  icon: ComponentType<{ className?: string }>;
  color: string;
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

const MENU_ITEM_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-fg-3 transition-colors data-focus:bg-overlay data-focus:text-fg data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

const MENU_ITEM_DANGER_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-coral transition-colors data-focus:bg-coral/10 data-focus:text-coral data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

function mapAutomations(result: AutomationListResponse | undefined): AutomationRow[] {
  const automations = result?.data ?? [];
  return automations.map((a) => ({
    id:         a.id,
    revision:   a.revision,
    name:       a.name,
    workflow:   a.target.workflow,
    repository: a.target.repository,
    schedule:   findScheduleTrigger(a)?.expression,
    apiEnabled: hasEnabledApiTrigger(a),
    icon:       slugIconMap[a.target.workflow] ?? CodeBracketIcon,
    color:      slugColorMap[a.target.workflow] ?? "var(--color-teal-500)",
  }));
}

function PlayIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
      <path fillRule="evenodd" d="M4.5 5.653c0-1.427 1.529-2.33 2.779-1.643l11.54 6.347c1.295.712 1.295 2.573 0 3.286L7.28 19.99c-1.25.687-2.779-.217-2.779-1.643V5.653Z" clipRule="evenodd" />
    </svg>
  );
}

function AutomationCard({
  automation,
  busy,
  running,
  onRun,
  onDelete,
}: {
  automation: AutomationRow;
  busy: boolean;
  running: boolean;
  onRun: () => void;
  onDelete: () => void;
}) {
  const Icon = automation.icon;
  const runDisabled = busy || running || !automation.apiEnabled;
  return (
    <div className="group flex items-center gap-4 rounded-md border border-line bg-panel/80 p-4 transition-all duration-200 hover:border-line-strong hover:bg-panel hover:shadow-lg hover:shadow-black/20">
      <Link to={`/automations/${automation.id}`} className="flex min-w-0 flex-1 items-center gap-4">
        <div
          className="flex size-9 shrink-0 items-center justify-center rounded-md border bg-panel-alt/60"
          style={{ borderColor: `color-mix(in srgb, ${automation.color} 20%, transparent)`, color: automation.color }}
        >
          <Icon className="size-4" />
        </div>

        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-fg-2 group-hover:text-fg">{automation.name}</span>
            <span className="font-mono text-xs text-fg-muted">{automation.workflow}</span>
            {automation.schedule && (
              <span className="inline-flex items-center gap-1 rounded-full bg-teal-500/10 border border-teal-500/20 px-2 py-0.5 text-[11px] font-medium text-teal-300">
                <ClockIcon className="size-3" />
                {automation.schedule}
              </span>
            )}
          </div>
          <p className="mt-1 text-xs text-fg-muted">{automation.repository}</p>
        </div>
      </Link>

      {automation.schedule ? (
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
          onClick={onRun}
          disabled={runDisabled}
          aria-label={running ? "Starting run…" : "Run automation"}
          title={
            running
              ? "Starting run..."
              : automation.apiEnabled
                ? "Run automation"
                : "Enable the API trigger to run it"
          }
          className="flex size-8 shrink-0 items-center justify-center rounded-full border border-mint/20 text-mint transition-colors hover:border-mint/50 hover:bg-mint/10 hover:text-fg disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:bg-transparent disabled:hover:text-mint"
        >
          {running ? (
            <ArrowPathIcon className="size-3.5 animate-spin [animation-duration:450ms]" aria-hidden="true" />
          ) : (
            <PlayIcon className="size-3.5" />
          )}
        </button>
      )}

      <RowMenu automation={automation} disabled={busy} onDelete={onDelete} />
    </div>
  );
}

function RowMenu({
  automation,
  disabled,
  onDelete,
}: {
  automation: AutomationRow;
  disabled: boolean;
  onDelete: () => void;
}) {
  return (
    <Menu as="div" className="relative inline-block">
      <MenuButton
        type="button"
        disabled={disabled}
        aria-label={`Actions for ${automation.name}`}
        title="Actions"
        className="flex size-8 shrink-0 items-center justify-center rounded-md text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3 disabled:cursor-not-allowed disabled:opacity-60"
      >
        <EllipsisVerticalIcon className="size-5" aria-hidden="true" />
      </MenuButton>
      <MenuItems
        transition
        anchor={{ to: "bottom end", gap: 4 }}
        className="z-30 w-36 origin-top-right rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
      >
        <MenuItem>
          <Link
            to={`/automations/${encodeURIComponent(automation.id)}/edit`}
            className={MENU_ITEM_CLASS}
          >
            Edit
          </Link>
        </MenuItem>
        <hr className="my-1 h-px border-0 bg-line" />
        <MenuItem>
          <button
            type="button"
            onClick={onDelete}
            disabled={disabled}
            className={MENU_ITEM_DANGER_CLASS}
          >
            Delete
          </button>
        </MenuItem>
      </MenuItems>
    </Menu>
  );
}

type TriggerFilter = "all" | "scheduled" | "manual";

const TRIGGER_FILTER_OPTIONS: { value: TriggerFilter; label: string }[] = [
  { value: "all",       label: "All triggers" },
  { value: "scheduled", label: "Scheduled" },
  { value: "manual",    label: "Manual" },
];

export default function Automations() {
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const navigate = useNavigate();
  const automationsQuery = useAutomations();
  const automations = mapAutomations(automationsQuery.data);
  const [query, setQuery] = useState("");
  const [triggerFilter, setTriggerFilter] = useState<TriggerFilter>("all");
  const [pendingDelete, setPendingDelete] = useState<AutomationRow | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [runningId, setRunningId] = useState<string | null>(null);

  async function runAutomation(automation: AutomationRow) {
    if (runningId) return;
    setRunningId(automation.id);
    try {
      const run = await apiData(() => automationsApi.createAutomationRun(automation.id));
      toast.push({ message: `Started run for “${automation.name}”.` });
      navigate(`/runs/${run.id}`);
    } catch (cause) {
      toast.push({
        tone: "error",
        message:
          cause instanceof ApiError && cause.message
            ? cause.message
            : "Couldn't start a run. Please try again.",
      });
      setRunningId(null);
    }
  }

  const lowerQuery = query.toLowerCase();
  const filtered = automations.filter(
    (a) =>
      (triggerFilter === "all" ||
        (triggerFilter === "scheduled" && a.schedule != null) ||
        (triggerFilter === "manual" && a.schedule == null)) &&
      (a.name.toLowerCase().includes(lowerQuery) ||
        a.workflow.toLowerCase().includes(lowerQuery) ||
        a.repository.toLowerCase().includes(lowerQuery)),
  );

  async function confirmDelete() {
    if (!pendingDelete) return;
    const { id, revision, name } = pendingDelete;
    setDeleting(true);
    try {
      await apiData(() => automationsApi.deleteAutomation(id, revision));
      await mutate(queryKeys.automations.list());
      toast.push({ message: `Automation “${name}” deleted.` });
      setPendingDelete(null);
    } catch (cause) {
      toast.push({
        tone: "error",
        message:
          cause instanceof ApiError && cause.message
            ? cause.message
            : "Couldn't delete the automation. Please try again.",
      });
    } finally {
      setDeleting(false);
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative w-64">
          <MagnifyingGlassIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
          <input
            type="text"
            aria-label="Search automations"
            placeholder="Search automations…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="w-full rounded-md border border-line bg-panel/80 py-2 pl-9 pr-3 text-sm text-fg-2 placeholder-fg-muted outline-none transition-colors focus:border-focus focus:ring-0"
          />
        </div>
        <FilterButton<TriggerFilter>
          label="Trigger"
          value={triggerFilter}
          allValue="all"
          options={TRIGGER_FILTER_OPTIONS}
          onChange={(next) => setTriggerFilter(next)}
        />
        <div className="ml-auto">
          <CreateAutomationButton />
        </div>
      </div>
      <div className="space-y-3">
        {filtered.map((automation) => (
          <AutomationCard
            key={automation.id}
            automation={automation}
            busy={deleting || (runningId !== null && runningId !== automation.id)}
            running={runningId === automation.id}
            onRun={() => runAutomation(automation)}
            onDelete={() => setPendingDelete(automation)}
          />
        ))}
        {filtered.length === 0 && (
          <p className="py-8 text-center text-sm text-fg-muted">No automations match "{query}"</p>
        )}
      </div>
      <ConfirmDialog
        open={pendingDelete !== null}
        title="Delete automation"
        description={
          <>
            Delete{" "}
            <span className="font-mono text-fg-2">{pendingDelete?.name}</span>? This
            removes the automation and stops any scheduled triggers. Existing runs are not affected.
          </>
        }
        confirmLabel="Delete"
        pendingLabel="Deleting…"
        pending={deleting}
        onConfirm={confirmDelete}
        onCancel={() => {
          if (!deleting) setPendingDelete(null);
        }}
      />
    </div>
  );
}
