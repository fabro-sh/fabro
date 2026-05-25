import {
  lazy,
  memo,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactElement,
  type RefObject,
} from "react";
import { useLocation, useNavigate, useParams } from "react-router";
import {
  MultiFileDiff,
  PatchDiff,
  type FileContents,
} from "@pierre/diffs/react";
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
  FileTreeSidebarSkeleton,
  InlineErrorBanner,
  LoadingSkeleton,
  renderStatusError,
  RunFilesErrorBoundary,
} from "./run-files/states";
import { useFileKeyboardNav } from "./run-files/keyboard";
import {
  Toolbar,
  type DiffPickerValue,
  type DiffStyle,
} from "./run-files/toolbar";
import { fileCacheKey, stringHash } from "./run-files/cache-keys";
import { buildRunCommitOptions } from "./run-files/commit-options";
import { VirtualizedDiffList } from "./run-files/virtualized-diff-list";
import { ApiError, extractRequestId } from "../lib/api-client";
import { useRun, useRunCommits, useRunFiles } from "../lib/queries";
import {
  runFileScopeSelection,
  type RunFileScope,
  type RunFileSelection,
} from "../lib/query-keys";

export { extractRequestId };

const FileTreeSidebar = lazy(() =>
  import("./run-files/file-tree-sidebar").then((module) => ({
    default: module.FileTreeSidebar,
  })),
);

export const handle = { wide: true, fullHeight: true };

const MD_BREAKPOINT_PX = 768;
const DIFF_STYLE_STORAGE_KEY = "fabro.run-files.diff-style";
// Minimum time the Refresh button keeps spinning after a click. SWR can
// resolve a cached/304 refetch in tens of ms, leaving the user unsure
// whether the click registered.
const MIN_REFRESH_SPIN_MS = 500;

export const ErrorBoundary = RunFilesErrorBoundary;

export function normalizeRunFileScope(value: string | null): RunFileScope {
  if (value === "all" || value === "uncommitted" || value === "committed") {
    return value;
  }
  return "committed";
}

function useNarrowViewport(): boolean {
  const [narrow, setNarrow] = useState(() => {
    if (typeof window === "undefined") return false;
    return window.matchMedia(`(max-width: ${MD_BREAKPOINT_PX - 1}px)`).matches;
  });
  useEffect(() => {
    if (typeof window === "undefined") return;
    const mql = window.matchMedia(`(max-width: ${MD_BREAKPOINT_PX - 1}px)`);
    const apply = () => setNarrow(mql.matches);
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

interface RunFileRowProps {
  file: ApiFileDiff;
  diffStyle: DiffStyle;
  isDeepLinkTarget: boolean;
  runId: string;
  toSha: string | null | undefined;
}

function fileDiffRenderKey({
  file,
  index,
  scope,
  toSha,
}: {
  file: ApiFileDiff;
  index: number;
  scope: string;
  toSha: string | null | undefined;
}): string {
  const display = file.new_file.name || file.old_file.name || `file-${index}`;
  const oldContents = file.old_file.contents ?? "";
  const newContents = file.new_file.contents ?? "";
  const contentFingerprint = file.unified_patch
    ? `patch:${stringHash(file.unified_patch)}`
    : `contents:${stringHash(oldContents)}:${stringHash(newContents)}`;
  return [
    scope,
    toSha ?? "no-sha",
    display,
    index,
    file.change_kind ?? "modified",
    contentFingerprint,
  ].join(":");
}

const RunFileRow = memo(function RunFileRow({
  file,
  diffStyle,
  isDeepLinkTarget,
  runId,
  toSha,
}: RunFileRowProps) {
  const display = file.new_file.name || file.old_file.name;
  const placeholder = pickPlaceholder(file);

  const oldContents = file.old_file.contents;
  const newContents = file.new_file.contents;
  const oldPath = file.old_file.name || display;
  const newPath = file.new_file.name || display;

  const oldFile = useMemo<FileContents | null>(() => {
    if (oldContents == null) return null;
    return {
      ...file.old_file,
      name:     oldPath,
      contents: oldContents,
      cacheKey: fileCacheKey({
        runId,
        toSha,
        side: "old",
        path: oldPath,
        contents: oldContents,
      }),
    };
  }, [file.old_file, oldContents, oldPath, runId, toSha]);

  const newFile = useMemo<FileContents | null>(() => {
    if (newContents == null) return null;
    return {
      ...file.new_file,
      name:     newPath,
      contents: newContents,
      cacheKey: fileCacheKey({
        runId,
        toSha,
        side: "new",
        path: newPath,
        contents: newContents,
      }),
    };
  }, [file.new_file, newContents, newPath, runId, toSha]);

  const multiFileOptions = useMemo(
    () => ({
      diffStyle,
      expandUnchanged: isDeepLinkTarget ? true : undefined,
    }),
    [diffStyle, isDeepLinkTarget],
  );
  const patchOptions = useMemo(() => ({ diffStyle }), [diffStyle]);
  const patch = useMemo(() => file.unified_patch ?? null, [file.unified_patch]);

  let body: ReactElement | null = null;
  if (placeholder) {
    body = placeholder;
  } else if (oldFile && newFile) {
    body = (
      <MultiFileDiff
        oldFile={oldFile}
        newFile={newFile}
        options={multiFileOptions}
      />
    );
  } else if (patch) {
    body = (
      <PatchDiff
        key={stringHash(patch)}
        patch={patch}
        options={patchOptions}
      />
    );
  }

  return (
    <section
      id={fileRowId(display)}
      tabIndex={-1}
      data-run-file-row="true"
      aria-label={`${file.change_kind ?? "modified"}: ${display}`}
      className="focus:outline-2 focus:outline-focus focus:outline-offset-2 rounded-md"
    >
      {body}
    </section>
  );
});

function RunFilesLoaded({
  containerRef,
  toolbar,
  files,
  meta,
  revalidationError,
  onRetry,
  narrow,
  hashFile,
  onFileSelect,
  effectiveScope,
  diffStyle,
  runId,
  runStatus,
}: {
  containerRef: RefObject<HTMLDivElement | null>;
  toolbar: ReactElement;
  files: ApiFileDiff[];
  meta: PaginatedRunFileList["meta"];
  revalidationError: string | null;
  onRetry: () => void;
  narrow: boolean;
  hashFile: string | null;
  onFileSelect: (path: string) => void;
  effectiveScope: string;
  diffStyle: DiffStyle;
  runId: string;
  runStatus: string | undefined;
}) {
  if (files.length === 0) {
    return (
      <div ref={containerRef} className="flex min-h-0 flex-1 flex-col gap-4">
        {toolbar}
        {meta.degraded ? (
          <DegradedBanner reason={meta.degraded_reason} />
        ) : null}
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

  return (
    <div ref={containerRef} className="flex min-h-0 flex-1 flex-col gap-4">
      <div className="shrink-0 space-y-4">
        {toolbar}
        {revalidationError ? (
          <InlineErrorBanner message={revalidationError} onRetry={onRetry} />
        ) : null}
        {meta.degraded ? (
          <DegradedBanner reason={meta.degraded_reason} />
        ) : null}
      </div>
      <div className="flex min-h-0 flex-1 gap-4">
        {!narrow ? (
          <Suspense fallback={<FileTreeSidebarSkeleton />}>
            <FileTreeSidebar
              files={files}
              selectedPath={hashFile}
              onSelect={onFileSelect}
            />
          </Suspense>
        ) : null}
        <div className="flex min-w-0 min-h-0 flex-1 flex-col">
          <VirtualizedDiffList>
            {files.map((file, idx) => {
              const display = file.new_file.name || file.old_file.name;
              const isDeepLinkTarget =
                !!hashFile &&
                (file.new_file.name === hashFile || file.old_file.name === hashFile);
              return (
                <RunFileRow
                  key={fileDiffRenderKey({
                    file,
                    index: idx,
                    scope: effectiveScope,
                    toSha: meta.to_sha,
                  })}
                  file={file}
                  diffStyle={diffStyle}
                  isDeepLinkTarget={isDeepLinkTarget}
                  runId={runId}
                  toSha={meta.to_sha}
                />
              );
            })}
          </VirtualizedDiffList>
        </div>
      </div>
    </div>
  );
}

export default function RunFiles() {
  const params = useParams();
  const routeLocation = useLocation();
  const navigate = useNavigate();
  const searchParams = useMemo(
    () => new URLSearchParams(routeLocation.search),
    [routeLocation.search],
  );
  const selectedScope = normalizeRunFileScope(searchParams.get("scope"));
  const selectedCommitSha = searchParams.get("commit");
  const commitsQuery = useRunCommits(params.id);
  const commitOptions = useMemo(
    () => buildRunCommitOptions(commitsQuery.data?.data ?? []),
    [commitsQuery.data],
  );
  const selectedCommit = selectedCommitSha
    ? commitOptions.find((commit) => commit.sha === selectedCommitSha)
    : undefined;
  const waitingForCommitSelection =
    !!selectedCommitSha && commitsQuery.data === undefined && !commitsQuery.error;
  const fileSelection: RunFileSelection =
    selectedCommit && selectedCommit.fromSha
      ? {
          kind:    "commit",
          fromSha: selectedCommit.fromSha,
          toSha:   selectedCommit.toSha,
        }
      : runFileScopeSelection(selectedScope);
  const filesQuery = useRunFiles(
    waitingForCommitSelection ? undefined : params.id,
    fileSelection,
  );
  const runQuery = useRun(params.id);
  const { push } = useToast();
  const narrow = useNarrowViewport();
  const runStatus = runQuery.data?.lifecycle.status.kind;

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

  const isInitialLoading = (waitingForCommitSelection || filesQuery.isLoading) && !data;
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

  const minRefreshTimerRef = useRef<number | null>(null);
  const [minRefreshActive, setMinRefreshActive] = useState(false);
  const clearMinRefreshTimer = useCallback(() => {
    if (minRefreshTimerRef.current !== null) {
      window.clearTimeout(minRefreshTimerRef.current);
      minRefreshTimerRef.current = null;
    }
  }, []);
  const handleRefresh = useCallback(() => {
    clearMinRefreshTimer();
    setMinRefreshActive(true);
    minRefreshTimerRef.current = window.setTimeout(() => {
      setMinRefreshActive(false);
      minRefreshTimerRef.current = null;
    }, MIN_REFRESH_SPIN_MS);
    void filesQuery.mutate();
  }, [clearMinRefreshTimer, filesQuery]);
  const handlePickerChange = useCallback(
    (selection: DiffPickerValue) => {
      const search = new URLSearchParams(routeLocation.search);
      if (selection.kind === "commit") {
        search.set("commit", selection.sha);
        search.delete("scope");
      } else {
        search.set("scope", selection.scope);
        search.delete("commit");
      }
      navigate({
        pathname: routeLocation.pathname,
        search:   `?${search.toString()}`,
        hash:     routeLocation.hash,
      });
    },
    [routeLocation.hash, routeLocation.pathname, routeLocation.search, navigate],
  );
  useEffect(() => clearMinRefreshTimer, [clearMinRefreshTimer]);
  // react-doctor-disable-next-line react-doctor/no-event-handler -- The refresh spinner is driven by both SWR revalidation and the click-owned minimum timer.
  const showRefreshing = isRevalidating || minRefreshActive;

  // Return focus to the Refresh button after a refresh visibly completes so
  // keyboard-first users stay oriented.
  const refreshingPrev = useRef(false);
  useEffect(() => {
    // react-doctor-disable-next-line react-doctor/no-event-handler -- Returning focus after async refresh completion is an accessibility sync effect.
    if (refreshingPrev.current && !showRefreshing) {
      refreshButtonRef.current?.focus({ preventScroll: true });
    }
    refreshingPrev.current = showRefreshing;
  }, [showRefreshing]);

  const fileCount = data?.data.length ?? 0;
  useFileKeyboardNav(containerRef, fileCount);

  // Deep-link handling: scroll + focus the matching row. Expansion is
  // handled by passing `expandUnchanged: true` to the targeted MultiFileDiff
  // via per-file options on `RunFileRow` — @pierre/diffs 1.1.x
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

  // react-doctor-disable-next-line react-doctor/no-event-handler -- Deep-link focus has to run after URL hash and file data have both rendered matching DOM rows.
  useEffect(() => {
    // react-doctor-disable-next-line react-doctor/no-event-handler -- Toasting missing deep links also depends on resolved file data.
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

  if (isInitialLoading) {
    return <LoadingSkeleton reserveSidebar={!narrow} />;
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
  const showScopePicker = data.meta.source === "sandbox";
  const pickerSelection: DiffPickerValue =
    selectedCommit && selectedCommit.fromSha
      ? { kind: "commit", sha: selectedCommit.sha }
      : { kind: "scope", scope: showScopePicker ? selectedScope : "committed" };
  const effectiveScope = fileSelection.kind === "commit"
    ? `commit:${fileSelection.toSha}`
    : fileSelection.scope;

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
      selection={pickerSelection}
      commits={commitOptions}
      showScopePicker={showScopePicker}
      onPickerChange={handlePickerChange}
      onRefresh={handleRefresh}
      refreshing={showRefreshing}
      refreshDisabled={refreshDisabled}
      freshness={freshness}
      refreshButtonRef={refreshButtonRef}
      diffStyle={diffStyle}
      onDiffStyleChange={handleDiffStyleChange}
      diffStyleForced={diffStyleForced}
    />
  );

  return (
    <RunFilesLoaded
      containerRef={containerRef}
      toolbar={toolbar}
      files={files}
      meta={meta}
      revalidationError={revalidationError}
      onRetry={() => void filesQuery.mutate()}
      narrow={narrow}
      hashFile={hashFile}
      onFileSelect={handleFileSelect}
      effectiveScope={effectiveScope}
      diffStyle={diffStyle}
      runId={params.id ?? "unknown-run"}
      runStatus={runStatus}
    />
  );
}
