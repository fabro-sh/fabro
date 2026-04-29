import { useState, useEffect, useRef } from "react";
import { Link } from "react-router";
import {
  ArrowPathIcon,
  CheckCircleIcon,
  NoSymbolIcon,
  PauseCircleIcon,
  XCircleIcon,
} from "@heroicons/react/24/solid";
import { Bars3BottomLeftIcon, DocumentTextIcon, MapIcon } from "@heroicons/react/24/outline";
import { formatDurationSecs } from "../lib/format";

export type StageStatus = "completed" | "running" | "pending" | "failed" | "cancelled";

export interface Stage {
  id: string;
  name: string;
  status: StageStatus;
  duration: string;
  dotId?: string;
}

export const statusConfig: Record<StageStatus, { icon: typeof CheckCircleIcon; color: string }> = {
  completed: { icon: CheckCircleIcon, color: "text-mint" },
  running: { icon: ArrowPathIcon, color: "text-teal-500" },
  pending: { icon: PauseCircleIcon, color: "text-fg-muted" },
  failed: { icon: XCircleIcon, color: "text-coral" },
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
  const [, setTick] = useState(0);

  // Track start times for running stages
  useEffect(() => {
    const running = new Set<string>(
      stages.filter((s) => s.status === "running").map((s) => s.id),
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
  useEffect(() => {
    if (!stages.some((s) => s.status === "running")) return;
    const interval = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(interval);
  }, [stages]);

  function stageDuration(stage: Stage): string {
    if (stage.status === "running") {
      const start = runningStartRef.current.get(stage.id);
      if (start) return formatDurationSecs(Math.floor((Date.now() - start) / 1000));
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
                    <Icon className={`size-4 shrink-0 ${config.color} ${stage.status === "running" ? "animate-spin" : ""}`} />
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
