import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactElement,
  type ReactNode,
} from "react";
import { useParams } from "react-router";
import * as PierreDiffs from "@pierre/diffs/react";
import { useToast } from "../components/toast";
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
  renderStatusError,
  RunFilesErrorBoundary,
} from "./run-files/states";
import { useFileKeyboardNav } from "./run-files/keyboard";
import { Toolbar, type DiffStyle } from "./run-files/toolbar";
import { ApiError, extractRequestId } from "../lib/api-client";
import { useRun, useRunFiles } from "../lib/queries";

export { extractRequestId };

const { MultiFileDiff, PatchDiff } = PierreDiffs;
const FileTreeSidebar = lazy(() =>
  import("./run-files/file-tree-sidebar").then((module) => ({
    default: module.FileTreeSidebar,
  })),
);
const maybeVirtualizer = (PierreDiffs as Record<string, unknown>).Virtualizer;
const Virtualizer = typeof maybeVirtualizer === "function"
  ? maybeVirtualizer as ({ children }: { children: ReactNode }) => ReactElement
  : function VirtualizerFallback({ children }: { children: ReactNode }) {
      return <>{children}</>;
    };

export const handle = { wide: true };

const MD_BREAKPOINT_PX = 768;
const DIFF_STYLE_STORAGE_KEY = "fabro.run-files.diff-style";

export const ErrorBoundary = RunFilesErrorBoundary;

function useNarrowViewport(): boolean {
  const [narrow, setNarrow] = useState(() => {
    if (typeof window === "undefined") return false;
    return window.matchMedia(`(max-width: ${MD_BREAKPOINT_PX - 1}px)`).matches;
  });
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

function useFreshness(
  meta: PaginatedRunFileList["meta"] | null,
  lastFetchedAt: number | null,
): string | null {
  // Only tick when there is actually a freshness label to keep fresh — no
  // point re-rendering every 10s when `meta == null` and the toolbar
  // would show nothing.
  const hasLabel =
    !!meta && (!!meta.to_sha_committed_at || lastFetchedAt !== null);
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!hasLabel) return undefined;
    const id = setInterval(() => setTick((t) => t + 1), 10_000);
    return () => clearInterval(id);
  }, [hasLabel]);

  if (!meta) return null;
  const now = Date.now();
  const captured = meta.to_sha_committed_at
    ? `Captured ${formatRelative(meta.to_sha_committed_at, now)}`
    : null;
  const fetched = lastFetchedAt
    ? `Fetched ${formatRelative(new Date(lastFetchedAt).toISOString(), now)}`
    : null;
  // GitHub-style short SHA. The OpenAPI pattern guarantees at least 7 hex
  // chars, but degraded responses with no captured commit may have null
  // here — fall through to just the timestamp(s) in that case.
  const shortSha = meta.to_sha ? meta.to_sha.slice(0, 7) : null;
  const timeLabel = meta.degraded && captured && fetched
    ? `${captured} · ${fetched}`
    : captured ?? fetched;
  if (timeLabel && shortSha) return `${timeLabel} · ${shortSha}`;
  return timeLabel ?? shortSha;
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

export function emptyTransitionToastMessage(
  previousFileCount: number | null,
  nextFileCount: number,
): string | null {
  return previousFileCount !== null && previousFileCount > 0 && nextFileCount === 0
    ? "No changes in this run."
    : null;
}

function resolveDeepLinkToast(
  hashFile: string | null,
  data: PaginatedRunFileList | null,
): { key: string; message: string } | null {
  if (!hashFile || !data) return null;
  if (data.meta.degraded && data.meta.patch) {
    return {
      key: `patch-only:${hashFile}`,
      message: "File-level navigation isn't available in the patch-only view.",
    };
  }

  const exists = data.data.some(
    (file) => file.new_file.name === hashFile || file.old_file.name === hashFile,
  );
  if (!exists) {
    return {
      key: `missing:${hashFile}`,
      message: `File ${hashFile} is not in this run.`,
    };
  }

  return null;
}

export function deepLinkToastMessage(
  hashFile: string | null,
  data: PaginatedRunFileList | null,
): string | null {
  return resolveDeepLinkToast(hashFile, data)?.message ?? null;
}

export default function RunFiles() {
  const params = useParams();
  const filesQuery = useRunFiles(params.id);
  const runQuery = useRun(params.id);
  const { push } = useToast();
  const narrow = useNarrowViewport();
  const runStatus = runQuery.data?.status?.kind;

  // Preserve the last successful payload so a failed revalidation can keep
  // rendering the previous files while surfacing an inline banner.
  const lastGoodDataRef = useRef<PaginatedRunFileList | null>(null);
  const lastFetchedAtRef = useRef<number | null>(null);

  useEffect(() => {
    if (!filesQuery.data) return;
    const message = emptyTransitionToastMessage(
      lastGoodDataRef.current?.data.length ?? null,
      filesQuery.data.data.length,
    );
    if (message) {
      push({ message });
    }
    lastGoodDataRef.current = filesQuery.data;
    lastFetchedAtRef.current = Date.now();
  }, [push, filesQuery.data]);

  const data: PaginatedRunFileList | null =
    filesQuery.data ?? lastGoodDataRef.current;

  const isInitialLoading = filesQuery.isLoading && !data;
  const isRevalidating = filesQuery.isValidating;

  // Revalidation error is whatever the most recent loader call returned;
  // the inline banner renders when we still have prior data to show. When
  // there's no prior data AND this is the initial load, we render a
  // full-panel error state instead (the Toolbar would have nothing to act
  // on with no data).
  const apiError = filesQuery.error instanceof ApiError ? filesQuery.error : null;
  const revalidationError =
    apiError && lastGoodDataRef.current
      ? `Couldn't refresh (${apiError.status}).`
      : null;
  const initialError = apiError && !lastGoodDataRef.current ? apiError : null;

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

  const refreshButtonRef = useRef<HTMLButtonElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const lastDeepLinkToastRef = useRef<string | null>(null);

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
    const toast = resolveDeepLinkToast(hashFile, data);
    if (toast) {
      if (lastDeepLinkToastRef.current !== toast.key) {
        push({ message: toast.message, autoDismissMs: 5000 });
        lastDeepLinkToastRef.current = toast.key;
      }
      return;
    }
    lastDeepLinkToastRef.current = null;
    if (!hashFile || !data) return;
    const el = document.getElementById(fileRowId(hashFile));
    if (el) {
      el.scrollIntoView({ block: "start", behavior: "smooth" });
      el.focus({ preventScroll: true });
    }
  }, [data, hashFile, push]);

  const handleFileSelect = useCallback((path: string) => {
    if (typeof window === "undefined") return;
    const next = `#file=${encodeURIComponent(path)}`;
    if (window.location.hash === next) return;
    window.location.hash = next;
  }, []);

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
                theme: "pierre-dark",
                expandUnchanged: isDeepLinkTarget ? true : undefined,
              }}
            />
          </div>
        );
      }),
    [diffStyle, hashFile],
  );

  if (isInitialLoading) {
    return <LoadingSkeleton />;
  }

  // Initial load failed with no prior data to fall back on. The route
  // stays mounted; `RunFilesErrorBoundary` is reserved for render-time
  // React errors (the loader doesn't throw).
  if (initialError) {
    return renderStatusError({
      status:    initialError.status,
      requestId: initialError.requestId,
      onRetry:   () => void filesQuery.mutate(),
    });
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
  // reported on the previous successful fetch — no new checkpoint yet.
  // `lastGoodDataRef.current` is updated in a useEffect, so during render
  // it still holds the previous render's data (or null on first load).
  const prevToSha = lastGoodDataRef.current?.meta?.to_sha ?? null;
  const refreshDisabled =
    !!meta.to_sha && prevToSha !== null && prevToSha === meta.to_sha;

  const toolbar = (
    <Toolbar
      changeSummary={{
        totalChanged: meta.total_changed,
        additions: meta.stats.additions,
        deletions: meta.stats.deletions,
      }}
      onRefresh={() => void filesQuery.mutate()}
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
            onRetry={() => void filesQuery.mutate()}
          />
        ) : null}
        <DegradedBanner reason={meta.degraded_reason} />
        <PatchDiff
          patch={meta.patch}
          options={{
            diffStyle,
            theme: "pierre-dark",
          }}
        />
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
          onRetry={() => void filesQuery.mutate()}
        />
      ) : null}
      <div className="flex gap-4">
        {!narrow ? (
          <Suspense fallback={null}>
            <FileTreeSidebar
              files={files}
              selectedPath={hashFile}
              onSelect={handleFileSelect}
            />
          </Suspense>
        ) : null}
        <div className="flex min-w-0 flex-1 flex-col gap-4">
          {body}
        </div>
      </div>
    </div>
  );
}
