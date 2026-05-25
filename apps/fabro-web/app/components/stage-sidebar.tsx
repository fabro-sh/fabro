import { type ComponentType, type ReactNode, useCallback, useState } from "react";
import { Link } from "react-router";
import type { StageState } from "@qltysh/fabro-api-client";
import {
  ArrowPathIcon,
  CheckCircleIcon,
  ExclamationCircleIcon,
  NoSymbolIcon,
  PauseCircleIcon,
  XCircleIcon,
} from "@heroicons/react/24/solid";
import {
  Bars3BottomLeftIcon,
  BoltIcon,
  ChevronDoubleLeftIcon,
  ChevronDoubleRightIcon,
  DocumentTextIcon,
  MapIcon,
  PaperClipIcon,
} from "@heroicons/react/24/outline";
import { formatDurationSecs } from "../lib/format";
import { ACTIVE_STAGE_STATES, formatStageLabel, type Stage } from "../lib/stage-sidebar";
import { elapsedSecsSince, useTickingNow } from "../lib/time";
import { HoverCard } from "./ui";
import { StagePopover } from "./stage-popover";

export type { Stage };

const statusConfig: Record<StageState, { icon: ComponentType<{ className?: string }>; color: string }> = {
  pending: { icon: PauseCircleIcon, color: "text-fg-muted" },
  running: { icon: ArrowPathIcon, color: "text-teal-500" },
  retrying: { icon: ArrowPathIcon, color: "text-amber" },
  succeeded: { icon: CheckCircleIcon, color: "text-mint" },
  partially_succeeded: { icon: ExclamationCircleIcon, color: "text-amber" },
  failed: { icon: XCircleIcon, color: "text-coral" },
  skipped: { icon: PauseCircleIcon, color: "text-fg-muted" },
  cancelled: { icon: NoSymbolIcon, color: "text-fg-muted" },
};

const COLLAPSED_STORAGE_KEY = "fabro:stage-sidebar-collapsed";

/** Read the persisted collapsed preference; defaults to expanded. */
function loadStoredCollapsed(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(COLLAPSED_STORAGE_KEY) === "1";
  } catch {
    // localStorage not available (e.g. sandboxed iframe)
    return false;
  }
}

function persistCollapsed(collapsed: boolean) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(COLLAPSED_STORAGE_KEY, collapsed ? "1" : "0");
  } catch {
    // non-fatal
  }
}

interface SidebarRowProps {
  to: string;
  icon: ComponentType<{ className?: string }>;
  iconClass: string;
  label: string;
  /** Right-aligned text (e.g. stage duration); hidden when collapsed. */
  trailing?: string;
  active: boolean;
  collapsed: boolean;
  /** Spin the icon to signal an in-flight stage. */
  spin?: boolean;
  /** Rich popover shown on hover/focus; supersedes the collapsed-mode title. */
  popover?: ReactNode;
}

/** A single sidebar link. The icon stays visible when collapsed; the label
 * becomes screen-reader-only and a `title` tooltip stands in for sighted users. */
function SidebarRow({ to, icon: Icon, iconClass, label, trailing, active, collapsed, spin, popover }: SidebarRowProps) {
  const link = (
    <Link
      to={to}
      title={popover || !collapsed ? undefined : label}
      className={`flex items-center rounded-md py-1.5 text-sm transition-colors ${
        collapsed ? "mx-1 justify-center" : "gap-2 px-2"
      } ${active ? "bg-overlay text-fg" : "text-fg-3 hover:bg-overlay hover:text-fg"}`}
    >
      <Icon className={`size-4 shrink-0 ${iconClass} ${spin ? "animate-spin" : ""}`} />
      <span className={collapsed ? "sr-only" : "flex-1 truncate"}>{label}</span>
      {trailing != null && !collapsed && (
        <span className="shrink-0 font-mono text-xs tabular-nums text-fg-muted">{trailing}</span>
      )}
    </Link>
  );

  return (
    <li>
      {popover ? (
        <HoverCard openDelay={200} className="block" content={popover}>
          {link}
        </HoverCard>
      ) : (
        link
      )}
    </li>
  );
}

/** Section heading. When a `toggle` is given it sits inline with the heading
 * (or stands alone, centered, when collapsed) rather than on its own row. */
function SectionHeading({
  title,
  collapsed,
  toggle,
}: {
  title: string;
  collapsed: boolean;
  toggle?: ReactNode;
}) {
  if (collapsed) {
    return (
      <>
        <h3 className="sr-only">{title}</h3>
        {toggle && <div className="flex h-7 items-center justify-center">{toggle}</div>}
      </>
    );
  }
  return (
    <div className="flex h-7 items-center justify-between">
      <h3 className="px-2 text-xs font-medium uppercase tracking-wider text-fg-muted">{title}</h3>
      {toggle}
    </div>
  );
}

interface StageSidebarProps {
  stages: Stage[];
  runId: string;
  selectedStageId?: string;
  activeLink?: "settings" | "source" | "logs" | "artifacts" | "events";
}

const WORKFLOW_LINKS: ReadonlyArray<{
  key: NonNullable<StageSidebarProps["activeLink"]>;
  path: string;
  icon: ComponentType<{ className?: string }>;
  label: string;
}> = [
  { key: "source", path: "source", icon: MapIcon, label: "Graph Source" },
  { key: "logs", path: "logs", icon: Bars3BottomLeftIcon, label: "Run Logs" },
  { key: "events", path: "events", icon: BoltIcon, label: "Run Events" },
  { key: "artifacts", path: "artifacts", icon: PaperClipIcon, label: "Artifacts" },
  { key: "settings", path: "settings", icon: DocumentTextIcon, label: "Run Settings" },
];

export function StageSidebar({ stages, runId, selectedStageId, activeLink }: StageSidebarProps) {
  // Persisted so the choice carries across the Overview and Stages tabs.
  const [collapsed, setCollapsed] = useState(loadStoredCollapsed);
  const toggleCollapsed = useCallback(() => {
    setCollapsed((prev) => {
      const next = !prev;
      persistCollapsed(next);
      return next;
    });
  }, []);

  // Tick every second while any stage is running so the elapsed clock keeps up.
  const hasActive = stages.some((s) => ACTIVE_STAGE_STATES.has(s.status));
  const now = useTickingNow(hasActive);

  function stageDuration(stage: Stage): string {
    if (ACTIVE_STAGE_STATES.has(stage.status)) {
      const secs = elapsedSecsSince(stage.startedAt, now);
      if (secs !== null) return formatDurationSecs(secs);
    }
    return stage.duration;
  }

  const toggleButton = (
    <button
      type="button"
      onClick={toggleCollapsed}
      aria-expanded={!collapsed}
      aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
      title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
      className="-mr-1 inline-flex size-7 shrink-0 items-center justify-center rounded-md text-fg-3 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
    >
      {collapsed ? (
        <ChevronDoubleRightIcon className="size-4" />
      ) : (
        <ChevronDoubleLeftIcon className="size-4" />
      )}
    </button>
  );

  return (
    <nav
      className={`${collapsed ? "w-12" : "w-56"} shrink-0 transition-[width] duration-300 ease-[cubic-bezier(0.16,1,0.3,1)]`}
    >
      <div className="space-y-6">
        {stages.length > 0 && (
          <div>
            <SectionHeading title="Stages" collapsed={collapsed} toggle={toggleButton} />
            <ul className="mt-2 space-y-0.5 overflow-hidden">
              {stages.map((stage) => {
                const config = statusConfig[stage.status];
                return (
                  <SidebarRow
                    key={stage.id}
                    to={`/runs/${runId}/stages/${stage.id}`}
                    icon={config.icon}
                    iconClass={config.color}
                    label={formatStageLabel(stage)}
                    trailing={stageDuration(stage)}
                    active={selectedStageId === stage.id}
                    collapsed={collapsed}
                    spin={ACTIVE_STAGE_STATES.has(stage.status)}
                    popover={<StagePopover runId={runId} stage={stage} duration={stageDuration(stage)} />}
                  />
                );
              })}
            </ul>
          </div>
        )}

        <div>
          <SectionHeading
            title="Workflow"
            collapsed={collapsed}
            toggle={stages.length === 0 ? toggleButton : undefined}
          />
          {collapsed && stages.length > 0 && (
            <div aria-hidden="true" className="mx-3 border-t border-line" />
          )}
          <ul className="mt-2 space-y-0.5 overflow-hidden">
            {WORKFLOW_LINKS.map((link) => (
              <SidebarRow
                key={link.key}
                to={`/runs/${runId}/${link.path}`}
                icon={link.icon}
                iconClass="text-fg-muted"
                label={link.label}
                active={activeLink === link.key}
                collapsed={collapsed}
              />
            ))}
          </ul>
        </div>
      </div>
    </nav>
  );
}
