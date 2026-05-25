import { useCallback, useMemo } from "react";
import { useSearchParams } from "react-router";
import { ArrowTopRightOnSquareIcon } from "@heroicons/react/20/solid";

import TerminalView from "../components/terminal-view";
import { TERMINAL_DOCK_CLEARANCE_CLASS } from "../components/terminal-view-helpers";
import { EmptyState, ErrorState } from "../components/state";
import {
  formatAbsoluteTs,
  formatBytesAsMemory,
  formatCpuCores,
} from "../lib/format";
import { useRunSandboxDetails } from "../lib/queries";
import { SANDBOX_STATE_DISPLAY } from "../lib/sandbox-state";
import type {
  SandboxDetails,
  SandboxNetwork,
  SandboxResources,
} from "@qltysh/fabro-api-client";
import FilesystemPanel from "./run-sandbox/filesystem-panel";
import ServicesPanel from "./run-sandbox/services-panel";
import VncPanel from "./run-sandbox/vnc-panel";

export const handle = { wide: true, fullHeight: true };

export type SandboxMode = "terminal" | "services" | "filesystem" | "vnc";

export function normalizeSandboxMode(value: string | null): SandboxMode {
  if (value === "services") return "services";
  if (value === "filesystem") return "filesystem";
  if (value === "vnc") return "vnc";
  return "terminal";
}

// VNC is Daytona-only. Hide the tab for other providers so the user never
// clicks into a guaranteed unsupported state. We only know the provider
// after sandbox details have loaded; until then, treat VNC as available.
function vncTabAvailable(provider: string | null | undefined): boolean {
  return provider === "daytona" || provider == null;
}

const EMPTY_VALUE = "—";

function nullable(value: string | null | undefined): string {
  return value && value.length > 0 ? value : EMPTY_VALUE;
}

function nullableTimestamp(value: string | null | undefined): string {
  return value ? formatAbsoluteTs(value) : EMPTY_VALUE;
}

function nullableMemory(bytes: number | null | undefined): string {
  return bytes != null ? formatBytesAsMemory(bytes) : EMPTY_VALUE;
}

function nullableCpu(cores: number | null | undefined): string {
  return cores != null ? formatCpuCores(cores) : EMPTY_VALUE;
}

type SandboxNetworkPolicy = SandboxNetwork["egress"];
type SandboxNetworkPolicyMode = SandboxNetworkPolicy["mode"];

const NETWORK_POLICY_DISPLAY: Record<SandboxNetworkPolicyMode, string> = {
  unknown:          "Unknown",
  open:             "Open",
  blocked:          "Blocked",
  cidr_allow_list:  "CIDR allow list",
  essentials_only:  "Essentials only",
};

function networkPolicySummary(policy: SandboxNetworkPolicy): string {
  return NETWORK_POLICY_DISPLAY[policy.mode] ?? policy.mode;
}

interface RowProps {
  label: string;
  value: React.ReactNode;
  valueClassName?: string;
}

function DetailRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4 px-4 py-2.5 text-sm">
      <span className="text-fg-3">{label}</span>
      {children}
    </div>
  );
}

function Row({ label, value, valueClassName }: RowProps) {
  return (
    <DetailRow label={label}>
      <span
        className={`text-right font-mono text-xs text-fg-2 ${
          valueClassName ?? ""
        } ${value === EMPTY_VALUE ? "text-fg-muted" : ""}`}
      >
        {value}
      </span>
    </DetailRow>
  );
}

function LinkRow({ label, href, text }: { label: string; href: string; text: string }) {
  return (
    <DetailRow label={label}>
      <a
        href={href}
        target="_blank"
        rel="noopener noreferrer"
        className="inline-flex min-w-0 items-center gap-1.5 text-right font-mono text-xs text-teal-500 transition-colors hover:text-teal-300 focus-visible:rounded-sm focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500"
      >
        <span className="truncate">{text}</span>
        <ArrowTopRightOnSquareIcon className="size-3.5 shrink-0" aria-hidden="true" />
      </a>
    </DetailRow>
  );
}

interface PanelProps {
  title: string;
  children: React.ReactNode;
}

function Panel({ title, children }: PanelProps) {
  return (
    <div className="overflow-hidden rounded-md border border-line">
      <h3 className="border-b border-line bg-panel/60 px-4 py-2.5 text-xs font-medium text-fg-3">
        {title}
      </h3>
      <div className="divide-y divide-line">{children}</div>
    </div>
  );
}

function StatusStrip({ details }: { details: SandboxDetails }) {
  const display = SANDBOX_STATE_DISPLAY[details.state] ?? SANDBOX_STATE_DISPLAY.unknown;
  const provider = details.sandbox.provider;
  const showNative =
    details.native_state &&
    details.native_state.toLowerCase() !== details.state.toLowerCase();
  return (
    <div className="flex flex-wrap items-center gap-x-5 gap-y-2 rounded-md border border-line bg-panel/60 px-4 py-3 text-sm">
      <span className="font-mono text-xs text-fg-muted uppercase tracking-wide">
        {provider}
      </span>
      <span className="flex items-center gap-1.5">
        <span className={`size-2 rounded-full ${display.dot}`} />
        <span className={`font-medium ${display.text}`}>{display.label}</span>
      </span>
      {showNative && (
        <span className="font-mono text-xs text-fg-muted">
          ({details.native_state})
        </span>
      )}
    </div>
  );
}

function OverviewPanel({ details }: { details: SandboxDetails }) {
  const sandbox = details.sandbox;
  const runtime = sandbox.runtime;
  return (
    <Panel title="Overview">
      <Row label="ID" value={nullable(runtime?.id)} />
      <Row label="Working directory" value={nullable(runtime?.working_directory)} />
      <Row
        label="Region"
        value={details.region ? details.region : sandbox.provider === "docker" ? "local" : EMPTY_VALUE}
      />
      <Row label="Image" value={nullable(sandbox.image ?? sandbox.snapshot)} />
      {details.web_url && (
        <LinkRow
          label="Provider"
          href={details.web_url}
          text={
            sandbox.provider === "daytona"
              ? "Open in Daytona"
              : `Open in ${sandbox.provider}`
          }
        />
      )}
    </Panel>
  );
}

function ResourcesPanel({ resources }: { resources: SandboxResources }) {
  return (
    <Panel title="Resources">
      <Row label="CPU" value={nullableCpu(resources.cpu_cores)} />
      <Row label="Memory" value={nullableMemory(resources.memory_bytes)} />
      <Row label="Disk" value={nullableMemory(resources.disk_bytes)} />
    </Panel>
  );
}

function NetworkPanel({ network }: { network: SandboxNetwork }) {
  const cidrRows: Array<{ label: string; policy: SandboxNetworkPolicy }> = [
    { label: "Egress CIDRs", policy: network.egress },
    { label: "Ingress CIDRs", policy: network.ingress },
  ].filter(({ policy }) => policy.mode === "cidr_allow_list");

  return (
    <Panel title="Network">
      <Row label="Egress" value={networkPolicySummary(network.egress)} />
      <Row label="Ingress" value={networkPolicySummary(network.ingress)} />
      {cidrRows.map(({ label, policy }) => (
        <Row key={label} label={label} value={policy.cidrs.join(", ") || EMPTY_VALUE} />
      ))}
    </Panel>
  );
}

function LabelsPanel({ labels }: { labels: { [key: string]: string } | null | undefined }) {
  const entries = labels ? Object.entries(labels) : [];
  return (
    <Panel title="Labels">
      {entries.length === 0 ? (
        <div className="px-4 py-3 text-sm text-fg-muted">No labels</div>
      ) : (
        entries
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([key, value]) => <Row key={key} label={key} value={value} />)
      )}
    </Panel>
  );
}

function TimestampsPanel({ details }: { details: SandboxDetails }) {
  return (
    <Panel title="Timestamps">
      <Row label="Created" value={nullableTimestamp(details.timestamps.created_at)} />
      <Row
        label="Last activity"
        value={nullableTimestamp(details.timestamps.last_activity_at)}
      />
    </Panel>
  );
}

function DetailsColumn({ details }: { details: SandboxDetails | null }) {
  if (!details) {
    return (
      <EmptyState
        title="No sandbox"
        description="This run has no sandbox or its provider does not expose details."
      />
    );
  }
  return (
    <div className="space-y-4">
      <StatusStrip details={details} />
      <OverviewPanel details={details} />
      <ResourcesPanel resources={details.resources} />
      <NetworkPanel network={details.network} />
      <LabelsPanel labels={details.labels} />
      <TimestampsPanel details={details} />
    </div>
  );
}

export default function RunSandbox({ params }: { params: { id: string } }) {
  const sandboxQuery = useRunSandboxDetails(params.id);
  const provider = sandboxQuery.data?.sandbox.provider ?? null;
  const [searchParams, setSearchParams] = useSearchParams();
  const requestedMode = useMemo(
    () => normalizeSandboxMode(searchParams.get("mode")),
    [searchParams],
  );
  // If the URL points at VNC but the loaded provider doesn't support it,
  // fall back to terminal rather than rendering a guaranteed-empty pane.
  const mode: SandboxMode =
    requestedMode === "vnc" && !vncTabAvailable(provider) ? "terminal" : requestedMode;

  const setMode = useCallback((next: SandboxMode) => {
    setSearchParams(
      (current) => {
        const params = new URLSearchParams(current);
        if (next === "terminal") {
          params.delete("mode");
        } else {
          params.set("mode", next);
        }
        return params;
      },
      { replace: true },
    );
  }, [setSearchParams]);

  const modeToggle = useMemo(
    () => (
      <ModeToggle
        mode={mode}
        onChange={setMode}
        vncAvailable={vncTabAvailable(provider)}
      />
    ),
    [mode, provider, setMode],
  );

  // The outer flex spans from the tab bar's bottom border down to the
  // steer bar — `-mt-3` cancels the outlet wrapper's top padding so the
  // column divider runs the full height, and we omit `pb-[clearance]`
  // here. Each column adds its own `pt-3` and dock clearance instead.
  return (
    <div className="-mt-3 flex min-h-0 flex-1">
      <aside
        className={`w-80 shrink-0 min-h-0 overflow-y-auto pt-3 pr-6 ${TERMINAL_DOCK_CLEARANCE_CLASS}`}
      >
        {sandboxQuery.error ? (
          <ErrorState
            title="Sandbox unavailable"
            description={
              sandboxQuery.error instanceof Error
                ? sandboxQuery.error.message
                : "Could not load sandbox details."
            }
          />
        ) : sandboxQuery.isLoading && !sandboxQuery.data ? null : (
          <DetailsColumn details={sandboxQuery.data ?? null} />
        )}
      </aside>
      <div className="flex min-w-0 min-h-0 flex-1 flex-col border-l border-line">
        <div
          className={`flex min-h-0 flex-1 flex-col pt-3 pl-6 ${TERMINAL_DOCK_CLEARANCE_CLASS}`}
        >
          {(() => {
            if (mode === "terminal") {
              return <TerminalView runId={params.id} leading={modeToggle} />;
            }
            if (mode === "services") {
              return <ServicesPanel runId={params.id} leading={modeToggle} />;
            }
            if (mode === "filesystem") {
              const rootDirectory = sandboxQuery.data?.sandbox.runtime?.working_directory ?? null;
              return (
                <FilesystemPanel
                  key={rootDirectory ?? "default-root"}
                  runId={params.id}
                  rootDirectory={rootDirectory}
                  leading={modeToggle}
                />
              );
            }
            return (
              <VncPanel runId={params.id} provider={provider} leading={modeToggle} />
            );
          })()}
        </div>
      </div>
    </div>
  );
}

interface ModeToggleProps {
  mode: SandboxMode;
  onChange: (mode: SandboxMode) => void;
  vncAvailable: boolean;
}

function ModeToggle({ mode, onChange, vncAvailable }: ModeToggleProps) {
  return (
    <div
      role="tablist"
      aria-label="Sandbox view"
      className="flex shrink-0 items-center gap-1 rounded-md border border-line bg-panel/60 p-1 self-start"
    >
      <ModeToggleButton
        label="Terminal"
        active={mode === "terminal"}
        onClick={() => onChange("terminal")}
      />
      <ModeToggleButton
        label="Services"
        active={mode === "services"}
        onClick={() => onChange("services")}
      />
      <ModeToggleButton
        label="Filesystem"
        active={mode === "filesystem"}
        onClick={() => onChange("filesystem")}
      />
      {vncAvailable && (
        <ModeToggleButton
          label="VNC"
          active={mode === "vnc"}
          onClick={() => onChange("vnc")}
        />
      )}
    </div>
  );
}

function ModeToggleButton({
  label,
  active,
  onClick,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active}
      onClick={onClick}
      className={`rounded px-3 py-1 text-xs font-medium transition-colors focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500 ${
        active
          ? "bg-overlay text-fg"
          : "text-fg-3 hover:bg-overlay/50 hover:text-fg-2"
      }`}
    >
      {label}
    </button>
  );
}
