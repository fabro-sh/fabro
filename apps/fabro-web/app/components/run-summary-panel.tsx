import type { ReactNode } from "react";
import {
  BoltIcon,
  ChatBubbleLeftEllipsisIcon,
  Cog6ToothIcon,
  CpuChipIcon,
  QuestionMarkCircleIcon,
  ServerIcon,
} from "@heroicons/react/20/solid";
import type {
  Principal,
  Run,
  SandboxResources,
  SandboxState,
} from "@qltysh/fabro-api-client";
import { Link } from "react-router";

import {
  formatBytesAsMemory,
  formatCpuCores,
  formatUsdMicros,
} from "../lib/format";
import { useRun, useRunArtifacts, useRunSandboxDetails } from "../lib/queries";
import { SANDBOX_STATE_DISPLAY } from "../lib/sandbox-state";
import { Tooltip } from "./ui";

const LABEL_CLASS =
  "text-[10px] font-medium uppercase tracking-[0.08em] text-fg-muted";
const VALUE_WRAPPER_CLASS = "mt-1.5";
const VALUE_CLASS = "text-sm text-fg";
const VALUE_MONO_CLASS = "text-sm text-fg font-mono tabular-nums";
const EM_DASH_CLASS = "text-sm text-fg-muted font-mono";

function EmDash() {
  return <span className={EM_DASH_CLASS}>—</span>;
}

function Skeleton({ widthClass }: { widthClass: string }) {
  return (
    <div
      aria-hidden="true"
      className={`h-4 ${widthClass} animate-pulse rounded bg-overlay`}
    />
  );
}

function Cell({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div>
      <div className={LABEL_CLASS}>{label}</div>
      <div className={VALUE_WRAPPER_CLASS}>{children}</div>
    </div>
  );
}

interface CreatedByDisplay {
  glyph: ReactNode;
  label: string;
}

function principalGlyph(icon: ReactNode) {
  return (
    <span className="grid size-5 place-items-center rounded-full bg-teal-500/20 text-teal-500">
      {icon}
    </span>
  );
}

function createdByDisplay(actor: Principal): CreatedByDisplay {
  switch (actor.kind) {
    case "user": {
      let glyph: ReactNode;
      if (actor.avatar_url) {
        glyph = (
          <img
            alt=""
            src={actor.avatar_url}
            className="size-5 rounded-full outline -outline-offset-1 outline-line-strong"
          />
        );
      } else {
        const initial = actor.login.charAt(0).toUpperCase() || "?";
        glyph = (
          <span className="grid size-5 place-items-center rounded-full bg-teal-500/20 font-mono text-[10px] font-medium text-teal-500">
            {initial}
          </span>
        );
      }
      return { glyph, label: actor.login };
    }
    case "agent":
      return { glyph: principalGlyph(<CpuChipIcon className="size-3" />), label: "agent" };
    case "system":
      return { glyph: principalGlyph(<Cog6ToothIcon className="size-3" />), label: "system" };
    case "slack":
      return {
        glyph: principalGlyph(<ChatBubbleLeftEllipsisIcon className="size-3" />),
        label: "slack",
      };
    case "webhook":
      return { glyph: principalGlyph(<BoltIcon className="size-3" />), label: "webhook" };
    case "worker":
      return { glyph: principalGlyph(<ServerIcon className="size-3" />), label: "worker" };
    case "anonymous":
      return {
        glyph: principalGlyph(<QuestionMarkCircleIcon className="size-3" />),
        label: "anonymous",
      };
  }
}

export interface RunSummaryPanelViewProps {
  run:                Run | null;
  runLoading:         boolean;
  sandboxState:       SandboxState | null;
  sandboxResources:   SandboxResources | null;
  sandboxLoading:     boolean;
  artifactsCount:     number | null;
  artifactsLoading:   boolean;
}

function SandboxValue({
  state,
  resources,
}: {
  state: SandboxState;
  resources: SandboxResources | null;
}) {
  const display = SANDBOX_STATE_DISPLAY[state] ?? SANDBOX_STATE_DISPLAY.unknown;
  const cpu = resources?.cpu_cores;
  const memory = resources?.memory_bytes;
  const valueText =
    cpu != null && memory != null
      ? `${formatCpuCores(cpu)} CPU · ${formatBytesAsMemory(memory)}`
      : display.label;

  return (
    <div className="flex items-center gap-2">
      <Tooltip label={display.description}>
        <span
          role="img"
          aria-label={`Sandbox ${display.label}`}
          className={`size-2 rounded-full ${display.dot}`}
        />
      </Tooltip>
      <span className={VALUE_CLASS}>{valueText}</span>
    </div>
  );
}

export function RunSummaryPanelView({
  run,
  runLoading,
  sandboxState,
  sandboxResources,
  sandboxLoading,
  artifactsCount,
  artifactsLoading,
}: RunSummaryPanelViewProps) {
  const created = run?.created_by ? createdByDisplay(run.created_by) : null;
  const diff = run?.diff ?? null;
  const cost = formatUsdMicros(run?.billing?.total_usd_micros);

  return (
    <div className="rounded-md border border-line bg-panel/60 px-6 py-4">
      <div className="flex flex-wrap items-baseline gap-x-14 gap-y-3">
        <Cell label="Created by">
          {runLoading ? (
            <Skeleton widthClass="w-20" />
          ) : created ? (
            <div className="flex items-center gap-2">
              {created.glyph}
              <span className={VALUE_CLASS}>{created.label}</span>
            </div>
          ) : (
            <EmDash />
          )}
        </Cell>

        <Cell label="Changes">
          {runLoading ? (
            <Skeleton widthClass="w-32" />
          ) : diff ? (
            <div className="flex items-baseline gap-2 text-sm">
              <span className="font-mono tabular-nums">
                <span className="text-mint">+{diff.additions.toLocaleString()}</span>{" "}
                <span className="text-coral">−{diff.deletions.toLocaleString()}</span>
              </span>
              <span className="text-fg-3">
                in {diff.files_changed.toLocaleString()} {diff.files_changed === 1 ? "file" : "files"}
              </span>
            </div>
          ) : (
            <EmDash />
          )}
        </Cell>

        <Cell label="Sandbox">
          {sandboxLoading ? (
            <Skeleton widthClass="w-24" />
          ) : sandboxState ? (
            <SandboxValue state={sandboxState} resources={sandboxResources} />
          ) : (
            <EmDash />
          )}
        </Cell>

        <Cell label="Cost">
          {runLoading ? (
            <Skeleton widthClass="w-12" />
          ) : cost != null ? (
            <span className={VALUE_MONO_CLASS}>{cost}</span>
          ) : (
            <EmDash />
          )}
        </Cell>

        <Cell label="Artifacts">
          {artifactsLoading ? (
            <Skeleton widthClass="w-8" />
          ) : artifactsCount != null && artifactsCount > 0 ? (
            <span className={VALUE_MONO_CLASS}>{artifactsCount}</span>
          ) : (
            <EmDash />
          )}
        </Cell>

        {run?.retried_from && (
          <Cell label="Retried from">
            <Link
              to={`/runs/${encodeURIComponent(run.retried_from)}`}
              className="font-mono text-sm text-teal-500 hover:text-teal-300"
            >
              {run.retried_from.slice(0, 8)}
            </Link>
          </Cell>
        )}
      </div>
    </div>
  );
}

export function RunSummaryPanel({ runId }: { runId: string }) {
  const runQuery = useRun(runId);
  const sandboxQuery = useRunSandboxDetails(runId);
  const artifactsQuery = useRunArtifacts(runId);

  return (
    <RunSummaryPanelView
      run={runQuery.data ?? null}
      runLoading={runQuery.isLoading && !runQuery.data}
      sandboxState={sandboxQuery.data?.state ?? null}
      sandboxResources={sandboxQuery.data?.resources ?? null}
      sandboxLoading={sandboxQuery.isLoading && !sandboxQuery.data}
      artifactsCount={artifactsQuery.data?.data.length ?? null}
      artifactsLoading={artifactsQuery.isLoading && !artifactsQuery.data}
    />
  );
}
