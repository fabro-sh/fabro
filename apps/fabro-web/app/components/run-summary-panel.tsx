import type { ReactNode } from "react";
import type {
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
import { principalDisplay } from "../lib/principal-display";
import { useRun, useRunArtifacts, useRunSandboxDetails } from "../lib/queries";
import {
  SANDBOX_LIFECYCLE_DISPLAY,
  sandboxIsReady,
  sandboxLifecycleKind,
} from "../lib/run-sandbox-lifecycle";
import { SANDBOX_STATE_DISPLAY } from "../lib/sandbox-state";
import { Tooltip } from "./ui";

const LABEL_CLASS =
  "text-[10px] font-medium uppercase tracking-[0.08em] text-fg-muted";
const VALUE_WRAPPER_CLASS = "mt-1.5";
const VALUE_CLASS = "text-sm text-fg";
const VALUE_MONO_CLASS = "text-sm text-fg font-mono tabular-nums";
const EMPTY_VALUE_CLASS = "text-sm text-fg-muted";

function EmptyValue() {
  return <span className={EMPTY_VALUE_CLASS}>Not available</span>;
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
          aria-hidden="true"
          className={`size-2 rounded-full ${display.dot}`}
        />
      </Tooltip>
      <span className={VALUE_CLASS}>{valueText}</span>
    </div>
  );
}

function SandboxLifecycleValue({
  kind,
}: {
  kind: keyof typeof SANDBOX_LIFECYCLE_DISPLAY;
}) {
  const display = SANDBOX_LIFECYCLE_DISPLAY[kind];
  return (
    <div className="flex items-center gap-2">
      <Tooltip label={display.description}>
        <span
          aria-hidden="true"
          className={`size-2 rounded-full ${display.dot}`}
        />
      </Tooltip>
      <span className={`${VALUE_CLASS} ${display.text}`}>{display.label}</span>
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
  const created = run?.created_by ? principalDisplay(run.created_by) : null;
  const diff = run?.diff ?? null;
  const cost = formatUsdMicros(run?.billing?.total_usd_micros);
  const sandboxKind = sandboxLifecycleKind(run?.sandbox);

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
          <EmptyValue />
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
          <EmptyValue />
          )}
        </Cell>

        <Cell label="Sandbox">
          {sandboxLoading ? (
            <Skeleton widthClass="w-24" />
          ) : sandboxState ? (
            <SandboxValue state={sandboxState} resources={sandboxResources} />
          ) : sandboxKind ? (
            <SandboxLifecycleValue kind={sandboxKind} />
          ) : (
          <EmptyValue />
          )}
        </Cell>

        <Cell label="Cost">
          {runLoading ? (
            <Skeleton widthClass="w-12" />
          ) : cost != null ? (
            <span className={VALUE_MONO_CLASS}>{cost}</span>
          ) : (
          <EmptyValue />
          )}
        </Cell>

        <Cell label="Artifacts">
          {artifactsLoading ? (
            <Skeleton widthClass="w-8" />
          ) : artifactsCount != null && artifactsCount > 0 ? (
            <span className={VALUE_MONO_CLASS}>{artifactsCount}</span>
          ) : (
          <EmptyValue />
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
  const sandboxQuery = useRunSandboxDetails(
    sandboxIsReady(runQuery.data?.sandbox) ? runId : undefined,
  );
  const artifactsQuery = useRunArtifacts(runId);
  const sandboxReady = sandboxIsReady(runQuery.data?.sandbox);

  return (
    <RunSummaryPanelView
      run={runQuery.data ?? null}
      runLoading={runQuery.isLoading && !runQuery.data}
      sandboxState={sandboxQuery.data?.state ?? null}
      sandboxResources={sandboxQuery.data?.resources ?? null}
      sandboxLoading={sandboxReady && sandboxQuery.isLoading && !sandboxQuery.data}
      artifactsCount={artifactsQuery.data?.data.length ?? null}
      artifactsLoading={artifactsQuery.isLoading && !artifactsQuery.data}
    />
  );
}
