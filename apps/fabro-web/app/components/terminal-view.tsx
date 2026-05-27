import {
  useCallback,
  useReducer,
  useRef,
  useState,
} from "react";
import {
  ArrowPathIcon,
  ArrowTopRightOnSquareIcon,
  ClipboardDocumentIcon,
} from "@heroicons/react/20/solid";

import { SECONDARY_BUTTON_CLASS, Tooltip } from "./ui";
import { ErrorState } from "./state";
import { useToast } from "./toast";
import { apiData, humanInTheLoopApi } from "../lib/api-client";
import { useRunState } from "../lib/queries";
import { sandboxInstance } from "../lib/run-sandbox-lifecycle";
import {
  buildFullScreenTerminalUrl,
  sandboxStatusDetail,
  terminalAccessCommandLabel,
} from "./terminal-view-helpers";
import {
  TERMINAL_BACKGROUND,
  useTerminalSession,
  type ConnectionStatus,
  type TerminalConnectionError,
} from "../hooks/use-terminal-session";

const ICON_BUTTON_CLASS =
  "inline-flex size-9 items-center justify-center rounded-lg text-fg-2 outline-1 -outline-offset-1 outline-white/10 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500";

function terminalAccessCommandCopiedMessage(provider: string | null): string {
  return provider === "docker" ? "Docker exec command copied." : "SSH command copied.";
}

function terminalAccessCommandErrorMessage(provider: string | null): string {
  return provider === "docker"
    ? "Could not copy Docker exec command."
    : "Could not copy SSH command.";
}

function statusDotClasses(status: ConnectionStatus): string {
  switch (status) {
    case "ready":
      return "bg-teal-500";
    case "error":
      return "bg-coral";
    case "closed":
      return "bg-fg-muted";
    case "connecting":
      return "bg-amber animate-pulse";
  }
}

function statusLabel(status: ConnectionStatus): string {
  switch (status) {
    case "ready":
      return "Connected";
    case "error":
      return "Error";
    case "closed":
      return "Closed";
    case "connecting":
      return "Connecting";
  }
}

function StatusPill({
  status,
  detail,
}: {
  status: ConnectionStatus;
  detail: string | null;
}) {
  return (
    <output
      aria-live="polite"
      className="inline-flex items-center gap-2 rounded-full bg-overlay py-1 pr-3 pl-2 text-xs font-medium text-fg-2 outline-1 -outline-offset-1 outline-white/10"
    >
      <span
        className={`size-1.5 rounded-full ${statusDotClasses(status)}`}
        aria-hidden="true"
      />
      <span>{statusLabel(status)}</span>
      {detail ? (
        <>
          <span className="text-fg-muted" aria-hidden="true">·</span>
          <span className="max-w-72 truncate font-mono text-fg-3" title={detail}>
            {detail}
          </span>
        </>
      ) : null}
    </output>
  );
}

export default function TerminalView({
  runId,
  leading,
  chromeless = false,
}: {
  runId: string;
  leading?: React.ReactNode;
  chromeless?: boolean;
}) {
  const { push } = useToast();
  const stateQuery = useRunState(runId);
  const sandbox = stateQuery.data?.sandbox ?? null;
  const provider = sandboxInstance(sandbox)?.provider ?? null;
  const sandboxDetail = sandboxStatusDetail(sandbox);
  const accessCommandLabel = terminalAccessCommandLabel(provider);
  const [connectionKey, reconnectTerminal] = useReducer((key: number) => key + 1, 0);
  const [status, setStatus] = useState<ConnectionStatus>("connecting");
  const [error, setError] = useState<TerminalConnectionError | null>(null);
  const terminalEl = useRef<HTMLDivElement | null>(null);
  const headingId = `run-terminal-${runId}`;
  useTerminalSession({
    connectionKey,
    runId,
    setError,
    setStatus,
    terminalEl,
  });

  const reconnect = useCallback(() => {
    setError(null);
    setStatus("connecting");
    reconnectTerminal();
  }, []);

  const copyAccessCommand = useCallback(async () => {
    if (!accessCommandLabel) return;
    try {
      const response = await apiData(() =>
        humanInTheLoopApi.createRunSshAccess(runId, { ttl_minutes: 60 }),
      );
      await navigator.clipboard.writeText(response.command);
      push({ message: terminalAccessCommandCopiedMessage(provider) });
    } catch (err) {
      push({
        tone: "error",
        message: err instanceof Error
          ? err.message
          : terminalAccessCommandErrorMessage(provider),
      });
    }
  }, [accessCommandLabel, runId, provider, push]);

  return (
    <section
      className="flex h-full min-h-0 flex-col"
      aria-labelledby={headingId}
      style={chromeless ? { backgroundColor: TERMINAL_BACKGROUND } : undefined}
    >
      <h2 id={headingId} className="sr-only">Terminal</h2>
      {!chromeless && (
        <div className="mb-2 flex shrink-0 flex-wrap items-center gap-3">
          {leading}
          <StatusPill status={status} detail={sandboxDetail} />
          <div className="ml-auto flex items-center gap-2">
            <Tooltip label="Open in new tab">
              <a
                href={buildFullScreenTerminalUrl(runId)}
                target="_blank"
                rel="noreferrer"
                className={ICON_BUTTON_CLASS}
                aria-label="Open terminal in new tab"
              >
                <ArrowTopRightOnSquareIcon
                  className="size-4"
                  aria-hidden="true"
                />
              </a>
            </Tooltip>
            <Tooltip label="Reconnect">
              <button
                type="button"
                className={ICON_BUTTON_CLASS}
                onClick={reconnect}
                aria-label="Reconnect terminal"
              >
                <ArrowPathIcon className="size-4" aria-hidden="true" />
              </button>
            </Tooltip>
            {accessCommandLabel && (
              <button
                type="button"
                className={SECONDARY_BUTTON_CLASS}
                onClick={() => void copyAccessCommand()}
                aria-label={`Copy ${accessCommandLabel} command`}
              >
                <ClipboardDocumentIcon className="size-4" aria-hidden="true" />
                {accessCommandLabel}
              </button>
            )}
          </div>
        </div>
      )}
      {error ? (
        <div className="flex min-h-0 flex-1 items-center justify-center" role="alert">
          <ErrorState
            title="Terminal unavailable"
            description={error.message}
            onRetry={error.recoverable ? reconnect : undefined}
          />
        </div>
      ) : chromeless ? (
        <div ref={terminalEl} className="h-full min-h-0 p-3" />
      ) : (
        <div
          className="min-h-0 flex-1 overflow-hidden rounded border border-line pb-3"
          style={{ backgroundColor: TERMINAL_BACKGROUND }}
        >
          <div ref={terminalEl} className="h-full min-h-0 p-3" />
        </div>
      )}
    </section>
  );
}
