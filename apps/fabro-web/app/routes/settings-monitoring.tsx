import type {
  SystemCpuResources,
  SystemDiskResources,
  SystemMemoryResources,
  SystemResourcesResponse,
} from "@qltysh/fabro-api-client";
import { formatBytesAsMemory, formatDurationMs } from "../lib/format";
import { useServerSettings, useSystemInfo, useSystemResources } from "../lib/queries";
import {
  Badge,
  Mono,
  Muted,
  Panel,
  PanelSkeleton,
  Row,
  SettingsPageIntro,
} from "../components/settings-panel";

export function meta() {
  return [{ title: "Monitoring — Fabro" }];
}

const DESCRIPTION =
  "Server-visible run concurrency, CPU, memory, and storage filesystem usage for this Fabro process.";

export default function SettingsMonitoring() {
  const resourcesQuery = useSystemResources();
  const resources = resourcesQuery.data;

  return (
    <div className="space-y-6">
      <SettingsPageIntro description={DESCRIPTION} />
      <RunsPanel />
      {resources ? (
        <>
          <CpuPanel cpu={resources.cpu} />
          <MemoryPanel memory={resources.memory} />
          <DiskPanel disk={resources.disk} />
          <NotesPanel resources={resources} />
        </>
      ) : (
        <>
          <PanelSkeleton />
          <PanelSkeleton />
          <PanelSkeleton />
        </>
      )}
    </div>
  );
}

function RunsPanel() {
  const infoQuery = useSystemInfo(5_000);
  const settingsQuery = useServerSettings();
  const info = infoQuery.data;
  const settings = settingsQuery.data;

  if (!info || !settings) {
    return <PanelSkeleton />;
  }

  const slotsUsed = info.runs?.scheduler_slots_used ?? 0;
  const max = settings.server.scheduler.max_concurrent_runs;
  const percent = max > 0 ? (slotsUsed / max) * 100 : null;

  return (
    <Panel title="Runs">
      <Row title="Concurrency used" help="Runs currently occupying scheduler slots.">
        <UsageMeter percent={percent} label={`${slotsUsed} / ${max} slots used`} />
      </Row>
    </Panel>
  );
}

function CpuPanel({ cpu }: { cpu: SystemCpuResources }) {
  if (!cpu.supported) {
    return (
      <Panel title="CPU">
        <UnsupportedRows reason={cpu.unavailable_reason} />
      </Panel>
    );
  }

  return (
    <Panel title="CPU">
      <Row title="Usage" help="Delta-based usage for the visible CPU set.">
        {cpu.usage_percent == null ? (
          <Muted>Collecting sample</Muted>
        ) : (
          <UsageMeter percent={cpu.usage_percent} />
        )}
      </Row>
      <Row title="Sample window" help="Elapsed time since the previous CPU sample.">
        {cpu.sample_window_ms != null ? (
          formatDurationMs(cpu.sample_window_ms, 0)
        ) : (
          <Muted>Unknown</Muted>
        )}
      </Row>
    </Panel>
  );
}

function MemoryPanel({ memory }: { memory: SystemMemoryResources }) {
  if (!memory.supported) {
    return (
      <Panel title="Memory">
        <UnsupportedRows reason={memory.unavailable_reason} />
      </Panel>
    );
  }

  return (
    <Panel title="Memory">
      <Row title="Usage" help="Memory used within the reported scope.">
        <UsageMeter
          percent={memory.used_percent}
          label={
            memory.used_bytes != null && memory.total_bytes != null
              ? `${formatBytesAsMemory(memory.used_bytes, 0)} / ${formatBytesAsMemory(memory.total_bytes, 0)}`
              : undefined
          }
        />
      </Row>
    </Panel>
  );
}

function DiskPanel({ disk }: { disk: SystemDiskResources }) {
  if (!disk.supported) {
    return (
      <Panel title="Disk">
        <UnsupportedRows reason={disk.unavailable_reason} />
        <Row title="Storage path" help="Configured Fabro storage directory.">
          <Mono>{disk.storage_path}</Mono>
        </Row>
      </Panel>
    );
  }

  return (
    <Panel title="Disk">
      <Row title="Usage" help="Storage filesystem capacity used.">
        <UsageMeter
          percent={disk.used_percent}
          label={
            disk.used_bytes != null && disk.total_bytes != null
              ? `${formatBytesAsMemory(disk.used_bytes, 0)} / ${formatBytesAsMemory(disk.total_bytes, 0)}`
              : undefined
          }
        />
      </Row>
    </Panel>
  );
}

function NotesPanel({ resources }: { resources: SystemResourcesResponse }) {
  const notes = Array.from(new Set(resources.notes));
  if (notes.length === 0) return null;

  return (
    <Panel title="Notes">
      {notes.map((note) => (
        <Row key={note} title="Note">
          <span className="text-fg-2">{note}</span>
        </Row>
      ))}
    </Panel>
  );
}

function UnsupportedRows({ reason }: { reason: string | null }) {
  return (
    <>
      <Row title="Status">
        <Badge>Unsupported</Badge>
      </Row>
      <Row title="Reason">
        <span className="text-fg-2">{reason ?? "Metric unavailable"}</span>
      </Row>
    </>
  );
}

function UsageMeter({
  percent,
  label,
}: {
  percent: number | null | undefined;
  label?: string;
}) {
  const safePercent = percent == null ? null : Math.min(100, Math.max(0, percent));
  const value = safePercent == null ? "Not available" : formatPercent(safePercent);
  return (
    <div className="min-w-0 space-y-1.5">
      <div className="flex items-baseline justify-between gap-3">
        <span className="truncate text-sm text-fg">{label ?? value}</span>
        {label ? (
          <span className="font-mono text-xs tabular-nums text-fg-muted">{value}</span>
        ) : null}
      </div>
      <div
        role="meter"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={safePercent ?? undefined}
        className="h-2 overflow-hidden rounded-sm bg-overlay-strong"
      >
        <div
          className="h-full rounded-sm bg-teal-500 transition-[width]"
          style={{ width: `${safePercent ?? 0}%` }}
        />
      </div>
    </div>
  );
}

function formatPercent(value: number) {
  return `${Math.round(value)}%`;
}
