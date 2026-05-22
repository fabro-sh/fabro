import { type ComponentType } from "react";
import { Link } from "react-router";
import type { StageHandler, StageState } from "@qltysh/fabro-api-client";
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
  DocumentTextIcon,
  MapIcon,
  PaperClipIcon,
} from "@heroicons/react/24/outline";
import { formatDurationSecs } from "../lib/format";
import { ACTIVE_STAGE_STATES, formatStageLabel } from "../lib/stage-sidebar";
import { elapsedSecsSince, useTickingNow } from "../lib/time";

export interface Stage {
  id: string;
  name: string;
  handler: StageHandler;
  status: StageState;
  duration: string;
  nodeId: string;
  visit: number;
  startedAt: string | null;
}

export const statusConfig: Record<StageState, { icon: ComponentType<{ className?: string }>; color: string }> = {
  pending: { icon: PauseCircleIcon, color: "text-fg-muted" },
  running: { icon: ArrowPathIcon, color: "text-teal-500" },
  retrying: { icon: ArrowPathIcon, color: "text-amber" },
  succeeded: { icon: CheckCircleIcon, color: "text-mint" },
  partially_succeeded: { icon: ExclamationCircleIcon, color: "text-amber" },
  failed: { icon: XCircleIcon, color: "text-coral" },
  skipped: { icon: PauseCircleIcon, color: "text-fg-muted" },
  cancelled: { icon: NoSymbolIcon, color: "text-fg-muted" },
};

interface StageSidebarProps {
  stages: Stage[];
  runId: string;
  selectedStageId?: string;
  activeLink?: "settings" | "source" | "logs" | "artifacts" | "events";
}

export function StageSidebar({ stages, runId, selectedStageId, activeLink }: StageSidebarProps) {
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

  const linkBase = "flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors";

  return (
    <nav className="w-56 shrink-0 space-y-6">
      {stages.length > 0 && (
        <div>
          <h3 className="px-2 text-xs font-medium uppercase tracking-wider text-fg-muted">Stages</h3>
          <ul className="mt-2 space-y-0.5">
            {stages.map((stage) => {
              const config = statusConfig[stage.status];
              const Icon = config.icon;
              const isSelected = selectedStageId === stage.id;
              return (
                <li key={stage.id}>
                  <Link
                    to={`/runs/${runId}/stages/${stage.id}`}
                    className={`${linkBase} ${
                      isSelected
                        ? "bg-overlay text-fg"
                        : "text-fg-3 hover:bg-overlay hover:text-fg"
                    }`}
                  >
                    <Icon className={`size-4 shrink-0 ${config.color} ${ACTIVE_STAGE_STATES.has(stage.status) ? "animate-spin" : ""}`} />
                    <span className="flex-1 truncate">{formatStageLabel(stage)}</span>
                    <span className="font-mono text-xs tabular-nums text-fg-muted">{stageDuration(stage)}</span>
                  </Link>
                </li>
              );
            })}
          </ul>
        </div>
      )}

      <div>
        <h3 className="px-2 text-xs font-medium uppercase tracking-wider text-fg-muted">Workflow</h3>
        <ul className="mt-2 space-y-0.5">
          <li>
            <Link
              to={`/runs/${runId}/source`}
              className={`${linkBase} ${
                activeLink === "source"
                  ? "bg-overlay text-fg"
                  : "text-fg-3 hover:bg-overlay hover:text-fg"
              }`}
            >
              <MapIcon className="size-4 shrink-0 text-fg-muted" />
              Graph Source
            </Link>
          </li>
          <li>
            <Link
              to={`/runs/${runId}/logs`}
              className={`${linkBase} ${
                activeLink === "logs"
                  ? "bg-overlay text-fg"
                  : "text-fg-3 hover:bg-overlay hover:text-fg"
              }`}
            >
              <Bars3BottomLeftIcon className="size-4 shrink-0 text-fg-muted" />
              Run Logs
            </Link>
          </li>
          <li>
            <Link
              to={`/runs/${runId}/events`}
              className={`${linkBase} ${
                activeLink === "events"
                  ? "bg-overlay text-fg"
                  : "text-fg-3 hover:bg-overlay hover:text-fg"
              }`}
            >
              <BoltIcon className="size-4 shrink-0 text-fg-muted" />
              Run Events
            </Link>
          </li>
          <li>
            <Link
              to={`/runs/${runId}/artifacts`}
              className={`${linkBase} ${
                activeLink === "artifacts"
                  ? "bg-overlay text-fg"
                  : "text-fg-3 hover:bg-overlay hover:text-fg"
              }`}
            >
              <PaperClipIcon className="size-4 shrink-0 text-fg-muted" />
              Artifacts
            </Link>
          </li>
          <li>
            <Link
              to={`/runs/${runId}/settings`}
              className={`${linkBase} ${
                activeLink === "settings"
                  ? "bg-overlay text-fg"
                  : "text-fg-3 hover:bg-overlay hover:text-fg"
              }`}
            >
              <DocumentTextIcon className="size-4 shrink-0 text-fg-muted" />
              Run Settings
            </Link>
          </li>
        </ul>
      </div>
    </nav>
  );
}
