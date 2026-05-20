import type {
  SystemCpuResources,
  SystemDiskResources,
  SystemInfoResponse,
  SystemMemoryResources,
  SystemResourcesResponse,
} from "@qltysh/fabro-api-client";
import { formatAbsoluteTs, formatBytesAsMemory, formatDurationSecs } from "../lib/format";
import { useSystemInfo, useSystemResources } from "../lib/queries";
import {
  Badge,
  Mono,
  Muted,
  NumberValue,
  Panel,
  PanelSkeleton,
  Row,
  SettingsPageIntro,
} from "../components/settings-panel";

export function meta() {
  return [{ title: "Resources — Fabro" }];
}

const DESCRIPTION =
  "Server-visible CPU, memory, and storage filesystem usage for this Fabro process.";

export default function SettingsResources() {
  const infoQuery = useSystemInfo();
  const resourcesQuery = useSystemResources();
  const info = infoQuery.data;
  const resources = resourcesQuery.data;

  return (
    <div className="space-y-6">
      <SettingsPageIntro description={DESCRIPTION} />
      {info && resources ? (
        <>
          <OverviewPanel info={info} resources={resources} />
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

function OverviewPanel({
  info,
  resources,
}: {
  info: SystemInfoResponse;
  resources: SystemResourcesResponse;
}) {
  return (
    <Panel title="Overview">
      <Row title="OS" help="Operating system reported by the server binary.">
        <Badge>{info.os ?? "unknown"}</Badge>
      </Row>
      <Row title="Architecture" help="CPU architecture reported by the server binary.">
        <Badge>{info.arch ?? "unknown"}</Badge>
      </Row>
      <Row title="Uptime" help="Elapsed time since this server process started.">
        {info.uptime_secs != null ? (
          formatDurationSecs(info.uptime_secs)
        ) : (
          <Muted>Unknown</Muted>
        )}
      </Row>
      <Row title="Sandbox provider" help="Effective provider for launched runs.">
        <Badge>{info.sandbox_provider ?? "unknown"}</Badge>
      </Row>
      <Row title="Storage path" help="Configured Fabro storage directory.">
        <Mono>{info.storage_dir ?? resources.disk.storage_path}</Mono>
      </Row>
      <Row title="Last sampled" help="Most recent resource sample timestamp.">
        {formatAbsoluteTs(resources.sampled_at)}
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
      <Row title="Logical CPUs" help="Visible logical processor count.">
        {cpu.logical_cpus != null ? (
          <NumberValue value={cpu.logical_cpus} />
        ) : (
          <Muted>Unknown</Muted>
        )}
      </Row>
      <Row title="Sample window" help="Expected polling interval for CPU deltas.">
        {cpu.sample_window_ms != null ? (
          formatDurationSecs(Math.round(cpu.sample_window_ms / 1000))
        ) : (
          <Muted>Unknown</Muted>
        )}
      </Row>
      <Row title="Scope" help="Environment the server can observe.">
        <Badge>{labelScope(cpu.scope)}</Badge>
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
              ? `${formatBytesAsMemory(memory.used_bytes)} / ${formatBytesAsMemory(memory.total_bytes)}`
              : undefined
          }
        />
      </Row>
      <Row title="Available" help="Memory available within the reported scope.">
        {formatNullableBytes(memory.available_bytes)}
      </Row>
      <Row title="Scope" help="Host memory or current container limit.">
        <Badge>{memory.scope === "cgroup" ? "Container limit" : "Host"}</Badge>
      </Row>
      {memory.scope === "cgroup" && memory.host_total_bytes != null ? (
        <Row title="Host total" help="Host memory before cgroup scoping.">
          {formatBytesAsMemory(memory.host_total_bytes)}
        </Row>
      ) : null}
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
        <Row title="Fabro managed" help="Bytes currently tracked under Fabro storage.">
          {formatBytesAsMemory(disk.fabro_managed_bytes)}
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
              ? `${formatBytesAsMemory(disk.used_bytes)} / ${formatBytesAsMemory(disk.total_bytes)}`
              : undefined
          }
        />
      </Row>
      <Row title="Available" help="Free bytes on the storage filesystem.">
        {formatNullableBytes(disk.available_bytes)}
      </Row>
      <Row title="Mount point" help="Filesystem containing the storage path.">
        {disk.mount_point ? <Mono>{disk.mount_point}</Mono> : <Muted>Unknown</Muted>}
      </Row>
      <Row title="Filesystem" help="Filesystem name reported by the operating system.">
        {disk.filesystem ? <Badge>{disk.filesystem}</Badge> : <Muted>Unknown</Muted>}
      </Row>
      <Row title="Fabro managed" help="Bytes currently tracked under Fabro storage.">
        {formatBytesAsMemory(disk.fabro_managed_bytes)}
      </Row>
      <Row title="Reclaimable" help="Bytes Fabro can reclaim by pruning inactive data.">
        {formatBytesAsMemory(disk.fabro_reclaimable_bytes)}
      </Row>
    </Panel>
  );
}

function NotesPanel({ resources }: { resources: SystemResourcesResponse }) {
  const notes = resourceNotes(resources);
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

function resourceNotes(resources: SystemResourcesResponse): string[] {
  const notes = [...resources.notes];
  for (const resource of [resources.cpu, resources.memory, resources.disk]) {
    if (!resource.supported && resource.unavailable_reason) {
      notes.push(resource.unavailable_reason);
    }
  }
  return Array.from(new Set(notes));
}

function formatNullableBytes(value: number | null | undefined) {
  return value != null ? formatBytesAsMemory(value) : <Muted>Unknown</Muted>;
}

function formatPercent(value: number) {
  return `${Number.isInteger(value) ? value : value.toFixed(1)}%`;
}

function labelScope(scope: string) {
  return scope.replaceAll("_", " ");
}
