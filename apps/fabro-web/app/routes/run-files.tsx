import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactElement,
} from "react";
import {
  useMatches,
  useNavigation,
  useParams,
  useRevalidator,
} from "react-router";
import { MultiFileDiff, PatchDiff, Virtualizer } from "@pierre/diffs/react";
import { useTheme } from "../lib/theme";
import { apiJsonOrNull } from "../api";
import type {
  FileDiff as ApiFileDiff,
  PaginatedRunFileList,
} from "@qltysh/fabro-api-client";
import {
  DegradedBanner,
  pickPlaceholder,
} from "./run-files/placeholders";
import {
  deriveEmptyKind,
  EmptyState,
  InlineErrorBanner,
  LoadingSkeleton,
  RunFilesErrorBoundary,
  Toast,
} from "./run-files/states";
import { useFileKeyboardNav } from "./run-files/keyboard";
import { Toolbar, type DiffStyle } from "./run-files/toolbar";

export const handle = { wide: true };

/**
 * Loader return type. Both initial loads and revalidations flow through the
 * same discriminated union so a revalidation failure does NOT unmount to
 * the route ErrorBoundary — it stays in-band as `{ data: null, error }`
 * and the component keeps showing the last-good data with an inline banner.
 * This is the plan's "prior content stays mounted; inline banner with
 * Retry" behavior for mid-session refresh failures (§ Unit 11).
 */
export type RunFilesLoaderResult = {
  data: PaginatedRunFileList | null;
  error: { status: number; message: string } | null;
};

export async function loader({ request, params }: any): Promise<RunFilesLoaderResult> {
  try {
    const data = await apiJsonOrNull<PaginatedRunFileList>(
      `/runs/${params.id}/files`,
      { request },
    );
    return { data, error: null };
  } catch (e) {
    if (e instanceof Response) {
      return {
        data:  null,
        error: {
          status:  e.status,
          message: e.statusText || `HTTP ${e.status}`,
        },
      };
    }
    return {
      data:  null,
      error: { status: 0, message: String(e) },
    };
  }
}

// Events that should trigger a revalidation. CheckpointCompleted is the
// canonical signal; terminal events cover the final-state transitions too.
const REFRESH_EVENTS = new Set([
  "checkpoint.completed",
  "run.completed",
  "run.failed",
]);

const MD_BREAKPOINT_PX = 768;
const DIFF_STYLE_STORAGE_KEY = "fabro.run-files.diff-style";

export const ErrorBoundary = RunFilesErrorBoundary;

function useNarrowViewport(): boolean {
  const [narrow, setNarrow] = useState(false);
  useEffect(() => {
    if (typeof window === "undefined") return;
    const mql = window.matchMedia(`(max-width: ${MD_BREAKPOINT_PX - 1}px)`);
    const apply = () => setNarrow(mql.matches);
    apply();
    mql.addEventListener("change", apply);
    return () => mql.removeEventListener("change", apply);
  }, []);
  return narrow;
}

function useSseRevalidation(runId: string | undefined) {
  const revalidator = useRevalidator();
  useEffect(() => {
    if (!runId) return;
    const source = new EventSource(`/api/v1/runs/${runId}/attach?since_seq=1`);
    let debounce: ReturnType<typeof setTimeout> | undefined;
    source.onmessage = (msg) => {
      try {
        const payload = JSON.parse(msg.data);
        if (REFRESH_EVENTS.has(payload.event)) {
          clearTimeout(debounce);
          debounce = setTimeout(() => revalidator.revalidate(), 500);
        }
      } catch {
        // ignore malformed payloads
      }
    };
    return () => {
      clearTimeout(debounce);
      source.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runId]);
}

function useFreshness(
  meta: PaginatedRunFileList["meta"] | null,
  lastFetchedAt: number | null,
): string | null {
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), 10_000);
    return () => clearInterval(id);
  }, []);

  if (!meta) return null;
  const now = Date.now();
  const captured = meta.to_sha_committed_at
    ? `Captured ${formatRelative(meta.to_sha_committed_at, now)}`
    : null;
  const fetched = lastFetchedAt
    ? `Fetched ${formatRelative(new Date(lastFetchedAt).toISOString(), now)}`
    : null;
  if (meta.degraded && captured && fetched) {
    return `${captured} · ${fetched}`;
  }
  return captured ?? fetched ?? null;
}

function formatRelative(iso: string | null, now: number): string {
  if (!iso) return "";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "";
  const diff = Math.max(0, Math.floor((now - then) / 1000));
  if (diff < 5) return "just now";
  if (diff < 60) return `${diff}s ago`;
  const m = Math.floor(diff / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

function loadStoredDiffStyle(): DiffStyle {
  if (typeof window === "undefined") return "split";
  try {
    const stored = window.localStorage.getItem(DIFF_STYLE_STORAGE_KEY);
    if (stored === "split" || stored === "unified") return stored;
  } catch {
    // localStorage not available (e.g., sandboxed iframe)
  }
  return "split";
}

function persistDiffStyle(style: DiffStyle) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(DIFF_STYLE_STORAGE_KEY, style);
  } catch {
    // non-fatal
  }
}

function fileRowId(name: string): string {
  return `run-file:${name}`;
}

function decodeDeepLinkFile(hash: string): string | null {
  if (!hash) return null;
  const withoutHash = hash.startsWith("#") ? hash.slice(1) : hash;
  const prefix = "file=";
  if (!withoutHash.startsWith(prefix)) return null;
  try {
    return decodeURIComponent(withoutHash.slice(prefix.length));
  } catch {
    return null;
  }
}

/**
 * Extract the lifecycle status from whichever ancestor match carries it.
 * The Run Detail loader (apps/fabro-web/app/routes/run-detail.tsx) returns
 * `{ run: { lifecycleStatus: string | null, ... } }` where
 * `lifecycleStatus` is the raw workflow status — "submitted", "running",
 * "succeeded", "failed", etc. `run.status` on the same object is the
 * ColumnStatus derived from checks and is NOT the right field to drive the
 * empty-state taxonomy from.
 */
function resolveRunStatus(matches: ReturnType<typeof useMatches>): string | undefined {
  for (const match of matches) {
    const data = match.data as any;
    if (!data) continue;
    if (typeof data?.run?.lifecycleStatus === "string") {
      return data.run.lifecycleStatus as string;
    }
    if (typeof data?.lifecycleStatus === "string") {
      return data.lifecycleStatus as string;
    }
  }
  return undefined;
}

export default function RunFiles({ loaderData }: any) {
  const theme = useTheme();
  const params = useParams();
  const navigation = useNavigation();
  const revalidator = useRevalidator();
  const matches = useMatches();
  const result = loaderData as RunFilesLoaderResult | null;
  const narrow = useNarrowViewport();
  const runStatus = resolveRunStatus(matches);

  // Preserve the last successful payload so a failed revalidation can keep
  // rendering the previous files while surfacing an inline banner. On the
  // very first failure (no prior good data), we render the error state
  // equivalent of the ErrorBoundary inline.
  const lastGoodDataRef = useRef<PaginatedRunFileList | null>(null);
  if (result?.data) {
    lastGoodDataRef.current = result.data;
  }
  const data: PaginatedRunFileList | null =
    result?.data ?? lastGoodDataRef.current;

  const lastFetchedAtRef = useRef<number | null>(null);
  const lastToShaRef = useRef<string | null>(null);
  const previousDataLengthRef = useRef<number | null>(null);
  const [emptyToast, setEmptyToast] = useState<string | null>(null);
  const [deepLinkToast, setDeepLinkToast] = useState<string | null>(null);

  useEffect(() => {
    if (!result?.data) return;
    lastFetchedAtRef.current = Date.now();
    const currentToSha = (result.data.meta?.to_sha ?? null) as string | null;
    const prevLen = previousDataLengthRef.current;
    if (prevLen !== null && prevLen > 0 && result.data.data.length === 0) {
      // Revalidation-now-empty toast: the user was looking at files, the
      // latest fetch shows none.
      setEmptyToast("No changes in this run.");
      const id = setTimeout(() => setEmptyToast(null), 3500);
      return () => clearTimeout(id);
    }
    previousDataLengthRef.current = result.data.data.length;
    lastToShaRef.current = currentToSha;
    return undefined;
  }, [result?.data]);

  useSseRevalidation(params.id);

  const isInitialLoading = navigation.state === "loading" && !loaderData;
  const isRevalidating = revalidator.state === "loading";

  // Revalidation error is whatever the most recent loader call returned;
  // the inline banner renders when we still have prior data to show. When
  // there's no prior data AND this is the initial load, we render a
  // full-panel error state instead (the Toolbar would have nothing to act
  // on with no data).
  const revalidationError =
    result?.error && lastGoodDataRef.current
      ? `Couldn't refresh (${result.error.status}).`
      : null;
  const initialError = result?.error && !lastGoodDataRef.current ? result.error : null;

  const freshness = useFreshness(data?.meta ?? null, lastFetchedAtRef.current);

  // Persisted desktop preference + md-breakpoint forced unified.
  const [persistedStyle, setPersistedStyle] = useState<DiffStyle>(
    loadStoredDiffStyle,
  );
  const diffStyle: DiffStyle = narrow ? "unified" : persistedStyle;
  const diffStyleForced = narrow;
  const handleDiffStyleChange = useCallback(
    (style: DiffStyle) => {
      if (diffStyleForced) return;
      setPersistedStyle(style);
      persistDiffStyle(style);
    },
    [diffStyleForced],
  );

  const pierreTheme =
    theme.theme === "dark" ? "pierre-dark" : "pierre-light";

  const refreshButtonRef = useRef<HTMLButtonElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);

  // Return focus to the Refresh button after a revalidation completes so
  // keyboard-first users stay oriented.
  const refreshingPrev = useRef(false);
  useEffect(() => {
    if (refreshingPrev.current && !isRevalidating) {
      refreshButtonRef.current?.focus({ preventScroll: true });
    }
    refreshingPrev.current = isRevalidating;
  }, [isRevalidating]);

  const fileCount = data?.data.length ?? 0;
  useFileKeyboardNav(containerRef, fileCount);

  // Deep-link handling: scroll + focus the matching row. Expansion is
  // handled by passing `expandUnchanged: true` to the targeted MultiFileDiff
  // via per-file options (see `renderFiles` below) — @pierre/diffs 1.1.x
  // exposes no imperative expand API, so click-based "expand" is not
  // available.
  const [hashFile, setHashFile] = useState<string | null>(() => {
    if (typeof window === "undefined") return null;
    return decodeDeepLinkFile(window.location.hash);
  });
  useEffect(() => {
    if (typeof window === "undefined") return;
    const onHashChange = () =>
      setHashFile(decodeDeepLinkFile(window.location.hash));
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  useEffect(() => {
    if (!hashFile || !data) return;
    if (data.meta.degraded && data.meta.patch) {
      setDeepLinkToast(
        "File-level navigation isn't available in the patch-only view.",
      );
      const id = setTimeout(() => setDeepLinkToast(null), 5000);
      return () => clearTimeout(id);
    }
    const exists = data.data.some(
      (f) => f.new_file.name === hashFile || f.old_file.name === hashFile,
    );
    if (!exists) {
      setDeepLinkToast(`File ${hashFile} is not in this run.`);
      const id = setTimeout(() => setDeepLinkToast(null), 5000);
      return () => clearTimeout(id);
    }
    const el = document.getElementById(fileRowId(hashFile));
    if (el) {
      el.scrollIntoView({ block: "start", behavior: "smooth" });
      el.focus({ preventScroll: true });
    }
  }, [hashFile, data]);

  const renderFiles = useCallback(
    (files: ApiFileDiff[]): ReactElement[] =>
      files.map((file, idx) => {
        const display = file.new_file.name || file.old_file.name;
        const placeholder = pickPlaceholder(file);
        if (placeholder) {
          return (
            <div
              key={`${display}-${idx}`}
              id={fileRowId(display)}
              tabIndex={-1}
              data-run-file-row="true"
              role="region"
              aria-label={`${file.change_kind ?? "changed"}: ${display}`}
              className="focus:outline-2 focus:outline-focus focus:outline-offset-2 rounded-md"
            >
              {placeholder}
            </div>
          );
        }
        // When the deep-link targets this file, pass expandUnchanged:true so
        // the full surrounding context renders without per-hunk clicking.
        const isDeepLinkTarget =
          !!hashFile &&
          (file.new_file.name === hashFile || file.old_file.name === hashFile);
        return (
          <div
            key={`${file.new_file.name}-${idx}`}
            id={fileRowId(display)}
            tabIndex={-1}
            data-run-file-row="true"
            role="region"
            aria-label={`${file.change_kind ?? "modified"}: ${file.new_file.name}`}
            className="focus:outline-2 focus:outline-focus focus:outline-offset-2 rounded-md"
          >
            <MultiFileDiff
              oldFile={file.old_file}
              newFile={file.new_file}
              options={{
                diffStyle,
                theme: pierreTheme,
                expandUnchanged: isDeepLinkTarget ? true : undefined,
              }}
            />
          </div>
        );
      }),
    [diffStyle, pierreTheme, hashFile],
  );

  if (isInitialLoading) {
    return <LoadingSkeleton />;
  }

  // Initial load failed and we have no prior data to fall back on —
  // render the status-specific error state inline (mirrors what the
  // ErrorBoundary would show, but without unmounting the route).
  if (initialError) {
    if (initialError.status === 401 || initialError.status === 403) {
      return (
        <EmptyState kind="unknown" />
      );
    }
    return (
      <InlineErrorBanner
        message={
          initialError.status >= 500
            ? `Something went wrong (${initialError.status}).`
            : `Couldn't load files (${initialError.status}).`
        }
        onRetry={() => revalidator.revalidate()}
      />
    );
  }

  if (!data) {
    return (
      <EmptyState
        kind={deriveEmptyKind({
          runStatus,
          totalChanged: 0,
          degraded: false,
        })}
      />
    );
  }

  const { data: files, meta } = data;

  // Refresh is disabled when the server reports the same `to_sha` it
  // reported on the previous fetch — no new checkpoint yet.
  const refreshDisabled =
    !!meta.to_sha &&
    lastToShaRef.current !== null &&
    lastToShaRef.current === meta.to_sha;

  const toolbar = (
    <Toolbar
      onRefresh={() => revalidator.revalidate()}
      refreshing={isRevalidating}
      refreshDisabled={refreshDisabled}
      freshness={freshness}
      refreshButtonRef={refreshButtonRef}
      diffStyle={diffStyle}
      onDiffStyleChange={handleDiffStyleChange}
      diffStyleForced={diffStyleForced}
    />
  );

  // Degraded: render the unified patch string directly.
  if (meta.degraded && meta.patch) {
    return (
      <div ref={containerRef} className="flex flex-col gap-4">
        {toolbar}
        {revalidationError ? (
          <InlineErrorBanner
            message={revalidationError}
            onRetry={() => revalidator.revalidate()}
          />
        ) : null}
        <DegradedBanner reason={meta.degraded_reason} />
        <PatchDiff
          patch={meta.patch}
          options={{
            diffStyle,
            theme: pierreTheme,
          }}
        />
        {emptyToast && <Toast>{emptyToast}</Toast>}
        {deepLinkToast && <Toast>{deepLinkToast}</Toast>}
      </div>
    );
  }

  if (files.length === 0) {
    return (
      <div ref={containerRef} className="flex flex-col gap-4">
        {toolbar}
        <EmptyState
          kind={deriveEmptyKind({
            runStatus,
            totalChanged: meta.total_changed,
            degraded: meta.degraded ?? false,
          })}
        />
        {emptyToast && <Toast>{emptyToast}</Toast>}
        {deepLinkToast && <Toast>{deepLinkToast}</Toast>}
      </div>
    );
  }

  // Large result sets get @pierre/diffs Virtualizer for lazy mounting so
  // 200-file runs don't synchronously mount every diff.
  const body =
    files.length > 20 ? (
      <Virtualizer>{renderFiles(files)}</Virtualizer>
    ) : (
      <>{renderFiles(files)}</>
    );

  return (
    <div ref={containerRef} className="flex flex-col gap-4">
      {toolbar}
      {revalidationError ? (
        <InlineErrorBanner
          message={revalidationError}
          onRetry={() => revalidator.revalidate()}
        />
      ) : null}
      {body}
      {emptyToast && <Toast>{emptyToast}</Toast>}
      {deepLinkToast && <Toast>{deepLinkToast}</Toast>}
    </div>
  );
}
