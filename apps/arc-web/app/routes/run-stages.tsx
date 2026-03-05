import { useState } from "react";
import { Link, useParams } from "react-router";
import { ChevronRightIcon } from "@heroicons/react/20/solid";
import { CheckCircleIcon, ArrowPathIcon, PauseCircleIcon, XCircleIcon } from "@heroicons/react/24/solid";
import { DocumentTextIcon, MapIcon, CommandLineIcon, ChatBubbleLeftIcon, WrenchScrewdriverIcon } from "@heroicons/react/24/outline";
import { apiJson } from "../api-client";
import { formatDurationSecs } from "../lib/format";
import type { PaginatedRunStageList, StageTurn as ApiStageTurn, PaginatedStageTurnList } from "@qltysh/arc-api-client";
import type { Route } from "./+types/run-stages";

export const handle = { wide: true };

type StageStatus = "completed" | "running" | "pending" | "failed";

interface Stage {
  id: string;
  name: string;
  status: StageStatus;
  duration: string;
}

export async function loader({ request, params }: Route.LoaderArgs) {
  const { data: apiStages } = await apiJson<PaginatedRunStageList>(`/runs/${params.id}/stages`, { request });
  const stages: Stage[] = apiStages.map((s) => ({
    id: s.id,
    name: s.name,
    status: s.status as StageStatus,
    duration: s.duration_secs != null ? formatDurationSecs(s.duration_secs) : "--",
  }));

  // Fetch turns for the selected stage (first stage if none specified)
  const selectedStageId = params.stageId ?? stages[0]?.id;
  let turns: ApiStageTurn[] = [];
  if (selectedStageId) {
    const { data } = await apiJson<PaginatedStageTurnList>(`/runs/${params.id}/stages/${selectedStageId}/turns`, { request });
    turns = data;
  }

  return { stages, turns };
}

const statusConfig: Record<StageStatus, { icon: typeof CheckCircleIcon; color: string }> = {
  completed: { icon: CheckCircleIcon, color: "text-mint" },
  running: { icon: ArrowPathIcon, color: "text-teal-500" },
  pending: { icon: PauseCircleIcon, color: "text-fg-muted" },
  failed: { icon: XCircleIcon, color: "text-coral" },
};

interface ToolUse {
  toolName: string;
  args: string;
  result: string;
}

type TurnType =
  | { kind: "system"; content: string }
  | { kind: "assistant"; content: string }
  | { kind: "tool"; tools: ToolUse[] };

// selectedStage is resolved from the URL param in RunStages below

function ToolRow({ tool }: { tool: ToolUse }) {
  const [open, setOpen] = useState(false);

  return (
    <div className="border-b border-line last:border-b-0">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 px-2.5 py-1.5 text-left transition-colors hover:bg-overlay"
      >
        <ChevronRightIcon className={`size-3 shrink-0 text-fg-muted transition-transform duration-150 ${open ? "rotate-90" : ""}`} />
        <WrenchScrewdriverIcon className="size-3.5 shrink-0 text-fg-muted" />
        <span className="font-mono text-xs text-fg-3">{tool.toolName}</span>
        <span className="truncate font-mono text-xs text-fg-muted">{tool.args}</span>
      </button>
      {open && (
        <div className="space-y-px bg-overlay px-2.5 pb-2 pt-1">
          <div className="rounded bg-overlay px-2.5 py-2">
            <div className="mb-1 text-[10px] font-medium uppercase tracking-wider text-fg-muted">Args</div>
            <pre className="whitespace-pre-wrap font-mono text-xs leading-relaxed text-fg-3">{tool.args}</pre>
          </div>
          <div className="rounded bg-overlay px-2.5 py-2">
            <div className="mb-1 text-[10px] font-medium uppercase tracking-wider text-fg-muted">Result</div>
            <pre className="whitespace-pre-wrap font-mono text-xs leading-relaxed text-fg-3">{tool.result}</pre>
          </div>
        </div>
      )}
    </div>
  );
}

function ToolBlock({ tools }: { tools: ToolUse[] }) {
  return (
    <div className="rounded-md border border-line bg-overlay overflow-hidden">
      {tools.map((tool, i) => (
        <ToolRow key={i} tool={tool} />
      ))}
    </div>
  );
}

function SystemBlock({ content }: { content: string }) {
  return (
    <div className="rounded-md border border-amber/10 bg-amber/5 overflow-hidden">
      <div className="flex items-center gap-2 px-3 py-2">
        <CommandLineIcon className="size-4 shrink-0 text-amber" />
        <span className="text-xs font-medium text-fg-3">System Prompt</span>
      </div>
      <div className="border-t border-line px-3 py-2.5">
        <pre className="whitespace-pre-wrap font-mono text-xs leading-relaxed text-fg-3">{content}</pre>
      </div>
    </div>
  );
}

function AssistantBlock({ content }: { content: string }) {
  return (
    <div className="rounded-md border border-teal-500/10 bg-teal-500/5 overflow-hidden">
      <div className="flex items-center gap-2 px-3 py-2">
        <ChatBubbleLeftIcon className="size-4 shrink-0 text-teal-500" />
        <span className="text-xs font-medium text-fg-3">Assistant</span>
      </div>
      <div className="border-t border-line px-3 py-2.5">
        <pre className="whitespace-pre-wrap font-mono text-xs leading-relaxed text-fg-3">{content}</pre>
      </div>
    </div>
  );
}

export default function RunStages({ loaderData }: Route.ComponentProps) {
  const { id, stageId } = useParams();
  const { stages, turns: apiTurns } = loaderData;

  const mappedTurns: TurnType[] = apiTurns.map((t) => {
    if (t.kind === "tool" && t.tools) {
      return {
        kind: "tool" as const,
        tools: t.tools.map((tu) => ({
          toolName: tu.tool_name,
          args: tu.args,
          result: tu.result,
        })),
      };
    }
    return { kind: t.kind as "system" | "assistant", content: t.content ?? "" };
  });

  const selectedStage = stages.find((s) => s.id === stageId) ?? stages[0];
  const selectedConfig = statusConfig[selectedStage.status];
  const SelectedIcon = selectedConfig.icon;

  return (
    <div className="flex gap-6">
      <nav className="w-56 shrink-0 space-y-6">
        <div>
          <h3 className="px-2 text-xs font-medium uppercase tracking-wider text-fg-muted">Stages</h3>
          <ul className="mt-2 space-y-0.5">
            {stages.map((stage) => {
              const config = statusConfig[stage.status];
              const Icon = config.icon;
              const isSelected = stage.id === selectedStage.id;
              return (
                <li key={stage.id}>
                  <Link
                    to={`/runs/${id}/stages/${stage.id}`}
                    className={`flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors ${
                      isSelected
                        ? "bg-overlay text-fg"
                        : "text-fg-3 hover:bg-overlay hover:text-fg"
                    }`}
                  >
                    <Icon className={`size-4 shrink-0 ${config.color} ${stage.status === "running" ? "animate-spin" : ""}`} />
                    <span className="flex-1 truncate">{stage.name}</span>
                    <span className="font-mono text-xs tabular-nums text-fg-muted">{stage.duration}</span>
                  </Link>
                </li>
              );
            })}
          </ul>
        </div>

        <div>
          <h3 className="px-2 text-xs font-medium uppercase tracking-wider text-fg-muted">Workflow</h3>
          <ul className="mt-2 space-y-0.5">
            <li>
              <Link
                to={`/runs/${id}/configuration`}
                className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm text-fg-3 transition-colors hover:bg-overlay hover:text-fg"
              >
                <DocumentTextIcon className="size-4 shrink-0 text-fg-muted" />
                Run Configuration
              </Link>
            </li>
            <li>
              <Link
                to={`/runs/${id}/graph`}
                className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm text-fg-3 transition-colors hover:bg-overlay hover:text-fg"
              >
                <MapIcon className="size-4 shrink-0 text-fg-muted" />
                Workflow Graph
              </Link>
            </li>
          </ul>
        </div>
      </nav>

      <div className="min-w-0 flex-1 space-y-3">
        <div className="flex items-center gap-2">
          <SelectedIcon className={`size-5 ${selectedConfig.color}`} />
          <h3 className="text-sm font-medium text-fg">{selectedStage.name}</h3>
          <span className="font-mono text-xs text-fg-muted">{selectedStage.duration}</span>
        </div>

        {mappedTurns.map((turn, i) => {
          switch (turn.kind) {
            case "system":
              return <SystemBlock key={i} content={turn.content} />;
            case "assistant":
              return <AssistantBlock key={i} content={turn.content} />;
            case "tool":
              return <ToolBlock key={i} tools={turn.tools} />;
          }
        })}
      </div>
    </div>
  );
}
