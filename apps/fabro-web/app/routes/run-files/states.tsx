import { isRouteErrorResponse, useRouteError } from "react-router";
import { extractRequestId } from "../../lib/api-client";

/**
 * R4 empty-state taxonomy. See plan § Unit 11:
 *   - `starting` (R4a): run still spinning up, no base_sha yet
 *   - `no_changes` (R4b): run completed but touched no files
 *   - `failed_before_checkpoint` (R4c1): failed run without captured diff
 *   - `diff_lost` (R4c2): succeeded run whose diff is no longer recoverable
 *   - `unknown`: fallback — loader returned null (404/501/other)
 */
export type EmptyKind =
  | "starting"
  | "no_changes"
  | "failed_before_checkpoint"
  | "diff_lost"
  | "unknown";

export function EmptyState({ kind }: { kind: EmptyKind }) {
  return (
    <div
      role="status"
      className="rounded-md border border-dashed border-line bg-panel/40 px-6 py-10 text-center text-sm text-fg-muted"
    >
      {emptyStateCopy(kind)}
    </div>
  );
}

export function emptyStateCopy(kind: EmptyKind): string {
  switch (kind) {
    case "starting":
      return "Run is still starting. Files will appear once it begins.";
    case "no_changes":
      return "This run didn't change any files.";
    case "failed_before_checkpoint":
      return "This run failed before capturing any changes.";
    case "diff_lost":
      return "The diff for this run is no longer available. If you expect files here, please report it.";
    case "unknown":
    default:
      return "The diff for this run is not available right now.";
  }
}

/// Derive the empty-state variant from the full loader context. `runStatus`
/// comes from the parent run loader (`run.lifecycleStatus`); its absence
/// collapses to the "unknown" catchall so the empty state never displays
/// misleading copy.
///
/// The full RunStatus enum (per fabro-types/src/status.rs) is:
///   submitted, queued, starting, running, blocked, paused, removing,
///   succeeded, failed, dead, archived
/// partially_succeeded is a stage outcome, not a run status.
export function deriveEmptyKind(args: {
  runStatus: string | undefined;
  totalChanged: number;
  degraded: boolean;
}): EmptyKind {
  const { runStatus, totalChanged, degraded } = args;
  if (!runStatus) {
    return "unknown";
  }
  const s = runStatus.toLowerCase();

  // Pre-work states: run has no base_sha / hasn't started producing a diff.
  if (s === "submitted" || s === "queued" || s === "starting") {
    return "starting";
  }

  // Actively-in-progress states: the run is running but just hasn't
  // changed any files yet. Avoid alarmist "diff lost" copy here — the
  // user may refresh and see files appear.
  if (s === "running" || s === "blocked" || s === "paused") {
    return "no_changes";
  }

  // Terminal-failure states: Failed and Dead both mean the run stopped
  // without a clean conclusion. If the degraded-fallback branch also
  // couldn't surface a patch, we never captured a diff at all.
  if (s === "failed" || s === "dead") {
    return degraded ? "unknown" : "failed_before_checkpoint";
  }

  // Terminal-success, teardown, and archive states: the run ran to
  // completion (possibly long ago). Distinguish "had no changes" (R4b)
  // from "diff captured then lost" (R4c2) via total_changed.
  if (s === "succeeded" || s === "removing" || s === "archived") {
    if (degraded) {
      // We have a patch; the component renders PatchDiff instead of an
      // empty state. Shouldn't reach here in practice.
      return "unknown";
    }
    return totalChanged > 0 ? "diff_lost" : "no_changes";
  }

  // Unknown future status — fail conservative.
  return "unknown";
}

export function FileTreeSidebarSkeleton() {
  return (
    <div
      aria-hidden="true"
      className="sticky top-4 h-[calc(100vh-6rem)] w-72 shrink-0 self-start rounded-md border border-line bg-panel/40 motion-safe:animate-pulse"
    />
  );
}

export function LoadingSkeleton({
  reserveSidebar = false,
}: {
  reserveSidebar?: boolean;
} = {}) {
  const diffSkeleton = (
    <div className="flex min-w-0 flex-1 flex-col gap-3">
      <div className="h-32 rounded-md bg-panel/60 motion-safe:animate-pulse" />
      <div className="h-32 rounded-md bg-panel/60 motion-safe:animate-pulse" />
    </div>
  );

  return (
    <div className="flex flex-col gap-3" aria-label="Loading files">
      <div className="h-8 rounded-md bg-panel/60 motion-safe:animate-pulse" />
      {reserveSidebar ? (
        <div className="flex gap-4">
          <FileTreeSidebarSkeleton />
          {diffSkeleton}
        </div>
      ) : (
        diffSkeleton
      )}
    </div>
  );
}

export function InlineErrorBanner({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 rounded-md border border-rose-500/30 bg-rose-950/20 px-4 py-3 text-sm text-rose-100">
      <span>{message}</span>
      <button
        type="button"
        onClick={onRetry}
        className="min-h-[32px] rounded-md border border-rose-500/40 bg-rose-950/40 px-3 py-1 text-xs font-medium text-rose-50 transition-colors hover:bg-rose-950/60"
      >
        Retry
      </button>
    </div>
  );
}

/**
 * Shared helper for rendering the documented status-code taxonomy. Consumed
 * by both the inline `initialError` branch in run-files.tsx and the
 * `RunFilesErrorBoundary` below — keeps the copy in one place so updates
 * don't drift between the two surfaces.
 */
export function renderStatusError(args: {
  status:    number;
  requestId: string | null;
  onRetry:   () => void;
}): React.ReactElement {
  const { status, requestId, onRetry } = args;
  if (status === 401 || status === 403) {
    return (
      <div
        role="status"
        className="rounded-md border border-dashed border-line bg-panel/40 px-6 py-10 text-center text-sm text-fg-muted"
      >
        You don't have access to this run's files.
      </div>
    );
  }
  if (status === 429 || status === 503) {
    return (
      <InlineErrorBanner
        message="The diff service is temporarily unavailable."
        onRetry={onRetry}
      />
    );
  }
  if (status >= 500) {
    const suffix = requestId ? ` Request ID: ${requestId}.` : "";
    return (
      <div
        role="status"
        className="rounded-md border border-dashed border-line bg-panel/40 px-6 py-10 text-center text-sm text-fg-muted"
      >
        Something went wrong.{suffix} Please contact support if this persists.
      </div>
    );
  }
  return (
    <InlineErrorBanner
      message={`Couldn't load files (${status}).`}
      onRetry={onRetry}
    />
  );
}

/**
 * Route-level ErrorBoundary for render-time React errors. The Files loader
 * no longer throws (it returns errors in-band via RunFilesLoaderResult), so
 * this only fires for React render crashes.
 */
export function RunFilesErrorBoundary() {
  const error = useRouteError();
  if (isRouteErrorResponse(error)) {
    return renderStatusError({
      status:    error.status,
      requestId: extractRequestId(error.data),
      onRetry:   () => window.location.reload(),
    });
  }
  return (
    <div
      role="status"
      className="rounded-md border border-dashed border-line bg-panel/40 px-6 py-10 text-center text-sm text-fg-muted"
    >
      Something went wrong loading this run's files.
    </div>
  );
}
