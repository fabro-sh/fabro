import { useState } from "react";
import { ArrowPathIcon } from "@heroicons/react/20/solid";
import type {
  SandboxService,
  SandboxServiceListResponse,
} from "@qltysh/fabro-api-client";

import { useSandboxServices } from "../../lib/queries";
import { usePreviewRun } from "../../lib/mutations";
import { ApiError } from "../../lib/api-client";
import {
  EmptyState,
  ErrorState,
  LoadingState,
} from "../../components/state";
import { Tooltip } from "../../components/ui";

export interface ServicesPanelProps {
  runId:    string;
  leading?: React.ReactNode;
}

const REFRESH_BUTTON_CLASS =
  "inline-flex size-9 items-center justify-center rounded-lg text-fg-2 outline-1 -outline-offset-1 outline-white/10 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-50";

const PREVIEW_BUTTON_CLASS =
  "inline-flex items-center justify-center rounded-md px-3 py-1 text-xs font-medium text-fg-2 outline-1 -outline-offset-1 outline-white/10 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-60";

export default function ServicesPanel({ runId, leading }: ServicesPanelProps) {
  const servicesQuery = useSandboxServices(runId);
  const previewMutation = usePreviewRun(runId);
  return (
    <ServicesPanelView
      runId={runId}
      leading={leading}
      servicesQuery={servicesQuery}
      previewMutation={previewMutation}
    />
  );
}

export interface ServicesQueryShape {
  data?:        SandboxServiceListResponse | undefined;
  error?:       unknown;
  isLoading:    boolean;
  isValidating: boolean;
  mutate:       () => unknown;
}

export interface PreviewMutationShape {
  trigger: (arg: {
    port:            number;
    expires_in_secs: number;
    signed?:         boolean;
  }) => Promise<{ intent: "preview"; url: string }>;
}

export interface ServicesPanelViewProps {
  runId:           string;
  leading?:        React.ReactNode;
  servicesQuery:   ServicesQueryShape;
  previewMutation: PreviewMutationShape;
}

// Presentational split so tests inject the SWR shapes directly. bun's module
// mock state is global and the last loaded test file wins, so mocking the
// query/mutation modules from a single panel test isn't reliable.
export function ServicesPanelView({
  runId,
  leading,
  servicesQuery,
  previewMutation,
}: ServicesPanelViewProps) {
  // Per-row pending: previewMutation has no per-port flag, so track which port
  // is in flight to disable only that row's button.
  const [pendingPort, setPendingPort] = useState<number | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);

  const services = servicesQuery.data?.data ?? [];
  const discoverySource = servicesQuery.data?.meta.source;
  const queryErrorMessage = describeQueryError(servicesQuery.error);
  const showLoading = servicesQuery.isLoading && !servicesQuery.data;
  const showError = queryErrorMessage !== null && !servicesQuery.data;

  const handlePreview = async (port: number) => {
    setPendingPort(port);
    setPreviewError(null);
    try {
      const result = await previewMutation.trigger({
        port,
        expires_in_secs: 3600,
        signed: true,
      });
      window.open(result.url, "_blank", "noopener,noreferrer");
    } catch (error) {
      setPreviewError(
        error instanceof ApiError
          ? error.message
          : "Could not generate preview URL.",
      );
    } finally {
      setPendingPort(null);
    }
  };

  return (
    <section
      className="flex h-full min-h-0 flex-col"
      aria-labelledby={`run-services-${runId}`}
    >
      <h2 id={`run-services-${runId}`} className="sr-only">
        Sandbox services
      </h2>
      <div className="mb-2 flex shrink-0 flex-wrap items-center gap-3">
        {leading}
        <div className="ml-auto flex items-center gap-2">
          <Tooltip label="Refresh">
            <button
              type="button"
              className={REFRESH_BUTTON_CLASS}
              onClick={() => void servicesQuery.mutate()}
              aria-label="Refresh services"
              disabled={servicesQuery.isValidating}
            >
              <ArrowPathIcon
                className={`size-4 ${servicesQuery.isValidating ? "animate-spin" : ""}`}
                aria-hidden="true"
              />
            </button>
          </Tooltip>
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-auto">
        {previewError ? (
          <div className="mb-3">
            <ErrorState
              title="Preview unavailable"
              description={previewError}
            />
          </div>
        ) : null}
        {showLoading ? (
          <LoadingState label="Loading services…" />
        ) : showError ? (
          <ErrorState
            description={queryErrorMessage ?? undefined}
            onRetry={() => void servicesQuery.mutate()}
          />
        ) : services.length === 0 ? (
          <EmptyState title="No services" />
        ) : (
          <>
            {discoverySource === "procfs" ? <ProcfsDiscoveryTip /> : null}
            <ServicesTable
              services={services}
              pendingPort={pendingPort}
              onPreview={handlePreview}
            />
          </>
        )}
      </div>
    </section>
  );
}

function describeQueryError(error: unknown): string | null {
  if (!error) return null;
  if (error instanceof ApiError) return error.message;
  if (error instanceof Error) return error.message;
  return "Could not load services.";
}

function ProcfsDiscoveryTip() {
  return (
    <div className="mb-3 rounded-md border border-line bg-panel/60 px-3 py-2 text-xs leading-5 text-fg-3">
      <span className="font-medium text-fg-2">Tip:</span>{" "}
      Install <code className="font-mono text-fg-2">ss</code> in the sandbox
      for improved services listing:{" "}
      <code className="font-mono text-fg-2">apt-get install iproute2</code>
    </div>
  );
}

function ServicesTable({
  services,
  pendingPort,
  onPreview,
}: {
  services:    SandboxService[];
  pendingPort: number | null;
  onPreview:   (port: number) => void;
}) {
  return (
    <div className="overflow-hidden rounded-md border border-line">
      <table className="w-full text-sm">
        <thead className="border-b border-line bg-panel/60">
          <tr>
            <th className="px-4 py-3 text-left text-xs font-medium text-fg-3">Port</th>
            <th className="px-4 py-3 text-left text-xs font-medium text-fg-3">Bindings</th>
            <th className="px-4 py-3 text-left text-xs font-medium text-fg-3">Process</th>
            <th className="px-4 py-3 text-right text-xs font-medium text-fg-3">
              <span className="sr-only">Actions</span>
            </th>

          </tr>
        </thead>
        <tbody className="divide-y divide-line">
          {services.map((service) => (
            <ServiceRow
              key={service.port}
              service={service}
              pending={pendingPort === service.port}
              onPreview={onPreview}
            />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ServiceRow({
  service,
  pending,
  onPreview,
}: {
  service:   SandboxService;
  pending:   boolean;
  onPreview: (port: number) => void;
}) {
  return (
    <tr>
      <td className="px-4 py-3 font-mono text-xs text-fg-2">{service.port}</td>
      <td className="px-4 py-3 font-mono text-xs text-fg-3">
        {service.addresses.length > 0 ? (
          service.addresses.join(", ")
        ) : (
          <span className="text-fg-muted">-</span>
        )}
      </td>
      <td className="px-4 py-3 font-mono text-xs text-fg-3">
        {service.processes.length > 0 ? (
          service.processes.join(", ")
        ) : (
          <span className="text-fg-muted">-</span>
        )}
      </td>
      <td className="px-4 py-3 text-right">
        {service.preview_supported ? (
          <button
            type="button"
            className={PREVIEW_BUTTON_CLASS}
            onClick={() => onPreview(service.port)}
            disabled={pending}
          >
            {pending ? "Opening…" : "Preview"}
          </button>
        ) : null}
      </td>
    </tr>
  );
}
