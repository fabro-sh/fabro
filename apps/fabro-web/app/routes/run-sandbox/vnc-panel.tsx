import { useMemo } from "react";
import {
  ArrowPathIcon,
  ArrowTopRightOnSquareIcon,
} from "@heroicons/react/20/solid";

import { useSandboxVncPreview } from "../../lib/queries";
import { ApiError } from "../../lib/api-client";
import {
  EmptyState,
  ErrorState,
  LoadingState,
} from "../../components/state";
import { SECONDARY_BUTTON_CLASS, Tooltip } from "../../components/ui";

interface VncPanelProps {
  runId:    string;
  provider: string | null;
  leading?: React.ReactNode;
}

// Pre-flight gate so unsupported providers never POST. The server returns 501
// for non-Daytona providers, but checking first keeps the empty state stable
// even before details have loaded.
export function vncSupported(provider: string | null): boolean {
  return provider === "daytona";
}

interface VncErrorState {
  title:       string;
  description: string;
  recoverable: boolean;
}

export function describeVncError(error: unknown): VncErrorState {
  if (error instanceof ApiError) {
    if (error.status === 501) {
      return {
        title:       "VNC not available",
        description:
          error.message
          || "This sandbox provider does not expose a VNC desktop.",
        recoverable: false,
      };
    }
    if (error.status === 409) {
      return {
        title:       "VNC unavailable",
        description:
          error.message
          || "Could not start the sandbox VNC service. The sandbox may not be running, or Computer Use failed to start.",
        recoverable: true,
      };
    }
    if (error.status === 404) {
      return {
        title:       "Run not found",
        description: error.message || "This run no longer has a sandbox.",
        recoverable: false,
      };
    }
    return {
      title:       "VNC unavailable",
      description: error.message || "Could not load the VNC preview.",
      recoverable: true,
    };
  }
  if (error instanceof Error) {
    return {
      title:       "VNC unavailable",
      description: error.message,
      recoverable: true,
    };
  }
  return {
    title:       "VNC unavailable",
    description: "Could not load the VNC preview.",
    recoverable: true,
  };
}

export default function VncPanel({ runId, provider, leading }: VncPanelProps) {
  const supported = vncSupported(provider);
  const vncQuery = useSandboxVncPreview(runId, supported);
  const errorState = useMemo<VncErrorState | null>(
    () => (vncQuery.error ? describeVncError(vncQuery.error) : null),
    [vncQuery.error],
  );

  return (
    <section
      className="flex h-full min-h-0 flex-col"
      aria-labelledby={`run-vnc-${runId}`}
    >
      <h2 id={`run-vnc-${runId}`} className="sr-only">
        VNC desktop
      </h2>
      <div className="mb-2 flex shrink-0 flex-wrap items-center gap-3">
        {leading}
        <StatusPill
          provider={provider}
          loading={supported && vncQuery.isLoading}
          error={errorState}
        />
        <div className="ml-auto flex items-center gap-2">
          {vncQuery.data && (
            <Tooltip label="Open in new tab">
              <a
                href={vncQuery.data.url}
                target="_blank"
                rel="noreferrer"
                className={SECONDARY_BUTTON_CLASS}
                aria-label="Open VNC desktop in new tab"
              >
                <ArrowTopRightOnSquareIcon
                  className="size-4"
                  aria-hidden="true"
                />
              </a>
            </Tooltip>
          )}
          <Tooltip label="Reconnect">
            <button
              type="button"
              className="inline-flex size-9 items-center justify-center rounded-lg text-fg-2 outline-1 -outline-offset-1 outline-white/10 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-50"
              onClick={() => void vncQuery.mutate()}
              aria-label="Reconnect VNC"
              disabled={!supported || vncQuery.isValidating}
            >
              <ArrowPathIcon
                className={`size-4 ${vncQuery.isValidating ? "animate-spin" : ""}`}
                aria-hidden="true"
              />
            </button>
          </Tooltip>
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-hidden rounded-md border border-line bg-neutral-950">
        <VncBody
          provider={provider}
          loading={supported && vncQuery.isLoading}
          url={vncQuery.data?.url ?? null}
          error={errorState}
          onRetry={() => void vncQuery.mutate()}
        />
      </div>
    </section>
  );
}

function StatusPill({
  provider,
  loading,
  error,
}: {
  provider: string | null;
  loading: boolean;
  error:   VncErrorState | null;
}) {
  const { dot, label } = pillState({ provider, loading, error });
  return (
    <output
      aria-live="polite"
      className="inline-flex items-center gap-2 rounded-full bg-overlay py-1 pr-3 pl-2 text-xs font-medium text-fg-2 outline-1 -outline-offset-1 outline-white/10"
    >
      <span className={`size-1.5 rounded-full ${dot}`} aria-hidden="true" />
      <span>{label}</span>
      {provider && (
        <>
          <span className="text-fg-muted" aria-hidden="true">·</span>
          <span className="font-mono text-fg-3">{provider}</span>
        </>
      )}
    </output>
  );
}

function pillState({
  provider,
  loading,
  error,
}: {
  provider: string | null;
  loading: boolean;
  error:   VncErrorState | null;
}): { dot: string; label: string } {
  if (!vncSupported(provider)) {
    return { dot: "bg-fg-muted", label: "Unsupported" };
  }
  if (error) {
    return { dot: "bg-coral", label: "Error" };
  }
  if (loading) {
    return { dot: "bg-amber animate-pulse", label: "Connecting" };
  }
  return { dot: "bg-teal-500", label: "Connected" };
}

function VncBody({
  provider,
  loading,
  url,
  error,
  onRetry,
}: {
  provider: string | null;
  loading: boolean;
  url:     string | null;
  error:   VncErrorState | null;
  onRetry: () => void;
}) {
  if (!vncSupported(provider)) {
    return (
      <div className="flex h-full items-center justify-center bg-bg p-6">
        <EmptyState
          title="VNC desktop unavailable"
          description={
            provider
              ? `The ${provider} sandbox provider does not expose a VNC desktop. Use Daytona for remote desktop access.`
              : "Waiting for sandbox details. The VNC desktop is only available for Daytona-hosted sandboxes."
          }
        />
      </div>
    );
  }
  if (error) {
    return (
      <div className="flex h-full items-center justify-center bg-bg p-6">
        <ErrorState
          title={error.title}
          description={error.description}
          onRetry={error.recoverable ? onRetry : undefined}
        />
      </div>
    );
  }
  if (loading || !url) {
    return (
      <div className="flex h-full items-center justify-center bg-bg p-6">
        <LoadingState label="Connecting to sandbox desktop…" />
      </div>
    );
  }
  return (
    <iframe
      src={url}
      title="Sandbox VNC desktop"
      // Daytona's signed preview already pins the iframe to the noVNC service;
      // clipboard + fullscreen let the embedded session feel native.
      allow="clipboard-read; clipboard-write; fullscreen"
      sandbox="allow-forms allow-pointer-lock allow-scripts"
      className="size-full border-0"
    />
  );
}
