import { useEffect, useRef, type ComponentType } from "react";
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
import { Bars3BottomLeftIcon, DocumentTextIcon, MapIcon } from "@heroicons/react/24/outline";
import { formatDurationSecs } from "../lib/format";
import { ACTIVE_STAGE_STATES } from "../lib/stage-sidebar";
import { useTickingNow } from "../lib/time";

export interface Stage {
  id: string;
  name: string;
  status: StageState;
  duration: string;
  dotId?: string;
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
  activeLink?: "settings" | "graph" | "logs";
}

export function StageSidebar({ stages, runId, selectedStageId, activeLink }: StageSidebarProps) {
  // Track when we first observed each running stage (for ticking timer)
  const runningStartRef = useRef<Map<string, number>>(new Map());

  // Track start times for running stages
  useEffect(() => {
    const running = new Set<string>(
      stages.filter((s) => ACTIVE_STAGE_STATES.has(s.status)).map((s) => s.id),
    );
    for (const stageId of running) {
      if (!runningStartRef.current.has(stageId)) {
        runningStartRef.current.set(stageId, Date.now());
      }
    }
    for (const stageId of runningStartRef.current.keys()) {
      if (!running.has(stageId)) {
        runningStartRef.current.delete(stageId);
      }
    }
  }, [stages]);

  // Tick every second while any stage is running
  const hasActive = stages.some((s) => ACTIVE_STAGE_STATES.has(s.status));
  const now = useTickingNow(hasActive);

  function stageDuration(stage: Stage): string {
    if (ACTIVE_STAGE_STATES.has(stage.status)) {
      const start = runningStartRef.current.get(stage.id);
      if (start) return formatDurationSecs(Math.floor((now - start) / 1000));
      return "0s";
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
                    <span className="flex-1 truncate">{stage.name}</span>
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
          <li>
            <Link
              to={`/runs/${runId}/graph`}
              className={`${linkBase} ${
                activeLink === "graph"
                  ? "bg-overlay text-fg"
                  : "text-fg-3 hover:bg-overlay hover:text-fg"
              }`}
            >
              <MapIcon className="size-4 shrink-0 text-fg-muted" />
              Workflow Graph
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
        </ul>
      </div>
    </nav>
  );
}