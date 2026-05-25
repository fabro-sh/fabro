import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import {
  ArrowDownTrayIcon,
  ArrowPathIcon,
  ArrowUturnLeftIcon,
  ChevronRightIcon,
} from "@heroicons/react/20/solid";
import {
  FileTree,
  useFileTree,
} from "@pierre/trees/react";
import { themeToTreeStyles } from "@pierre/trees";
import pierreDark from "@pierre/theme/pierre-dark";
import {
  File,
  Virtualizer,
  WorkerPoolContextProvider,
  type FileContents,
} from "@pierre/diffs/react";
import type { SandboxFileEntry } from "@qltysh/fabro-api-client";

import { useSandboxFile, useSandboxFiles } from "../../lib/queries";
import { ApiError } from "../../lib/api-client";
import { EmptyState, ErrorState, LoadingState } from "../../components/state";
import { SECONDARY_BUTTON_CLASS, Tooltip } from "../../components/ui";
import { workerFactory } from "../../lib/pierre-diffs-worker";
import { stringHash } from "../run-files/cache-keys";

export const DEFAULT_DIR = "/";

// Match the run-files/diff caps so previews stay responsive and don't blow up
// the browser on accidentally large logs. Files above the limit fall back to a
// download-only state.
export const TEXT_PREVIEW_BYTE_LIMIT = 256 * 1024;
const BINARY_SAMPLE_BYTES = 8 * 1024;
const pierrePoolOptions = { workerFactory };
const pierreHighlighterOptions = { theme: "pierre-dark" };
const EMPTY_SANDBOX_FILE_ENTRIES: SandboxFileEntry[] = [];

type TreeThemeStyle = CSSProperties & Record<`--${string}`, string | number>;

interface FilesystemPanelProps {
  runId:          string;
  leading?:       React.ReactNode;
  rootDirectory?: string | null;
}

// `path` here is always an absolute sandbox path (e.g. `/src/main.ts`).
// Tree paths fed to @pierre/trees are *relative* to `currentDir` so each
// directory navigation gets a fresh, shallow tree rather than accumulating
// state across navigations.

export function joinPath(parent: string, child: string): string {
  if (!child) return parent;
  if (!parent || parent === "/") return `/${child}`;
  return `${parent}/${child}`;
}

export function parentPath(path: string): string {
  if (!path || path === "/") return "/";
  const trimmed = path.replace(/\/+$/, "");
  const idx = trimmed.lastIndexOf("/");
  if (idx <= 0) return "/";
  return trimmed.substring(0, idx);
}

export function basename(path: string): string {
  if (!path || path === "/") return "/";
  const trimmed = path.replace(/\/+$/, "");
  const idx = trimmed.lastIndexOf("/");
  return idx >= 0 ? trimmed.substring(idx + 1) : trimmed;
}

export interface Breadcrumb {
  name: string;
  path: string;
}

export function buildBreadcrumbs(path: string): Breadcrumb[] {
  const crumbs: Breadcrumb[] = [{ name: "/", path: "/" }];
  if (!path || path === "/") return crumbs;
  const segments = path.split("/").filter((segment) => segment.length > 0);
  let current = "";
  for (const segment of segments) {
    current = `${current}/${segment}`;
    crumbs.push({ name: segment, path: current });
  }
  return crumbs;
}

export function looksLikeBinary(bytes: Uint8Array): boolean {
  const limit = Math.min(bytes.length, BINARY_SAMPLE_BYTES);
  for (let i = 0; i < limit; i += 1) {
    if (bytes[i] === 0) return true;
  }
  return false;
}

export function decodeUtf8Strict(bytes: Uint8Array): string | null {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

export function downloadUrl(runId: string, path: string): string {
  const params = new URLSearchParams({ path });
  return `/api/v1/runs/${encodeURIComponent(runId)}/sandbox/file?${params.toString()}`;
}

export function formatFileSize(bytes: number | undefined): string | null {
  if (bytes == null) return null;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
}

export function sandboxFileCacheKey({
  runId,
  path,
  contents,
}: {
  runId: string;
  path: string;
  contents: string;
}): string {
  return `fabro-sandbox-file:${runId}:${path}:${stringHash(contents)}`;
}

interface BuiltTreeInputs {
  paths: string[];
  fileEntries: Map<string, SandboxFileEntry>;
  directories: Set<string>;
}

// The list endpoint returns flat names like `foo` (depth=1) or `foo/bar.ts`
// (depth=2). We feed those names to @pierre/trees as relative paths. Every
// directories are represented with the tree library's canonical trailing-slash
// form so empty directories appear without synthetic child rows.

export function buildTreeInputs(entries: readonly SandboxFileEntry[]): BuiltTreeInputs {
  const paths: string[] = [];
  const fileEntries = new Map<string, SandboxFileEntry>();
  const directories = new Set<string>();

  for (const entry of entries) {
    if (entry.is_dir) {
      directories.add(entry.name);
      paths.push(`${entry.name}/`);
    } else {
      fileEntries.set(entry.name, entry);
      paths.push(entry.name);
    }
  }

  return { paths, fileEntries, directories };
}

function normalizeDirectorySelection(path: string): string {
  return path.replace(/\/+$/, "");
}

export function classifySelection(
  selectedPath: string,
  fileEntries: Map<string, SandboxFileEntry>,
  directories: Set<string>,
): { kind: "file"; entry: SandboxFileEntry } | { kind: "dir"; relativePath: string } | null {
  const fileEntry = fileEntries.get(selectedPath);
  if (fileEntry) return { kind: "file", entry: fileEntry };
  const directoryPath = normalizeDirectorySelection(selectedPath);
  if (directories.has(directoryPath)) {
    return { kind: "dir", relativePath: directoryPath };
  }
  // Intermediate directory implied by a nested path (depth>1). Treat selection
  // as navigation into that subdir.
  return { kind: "dir", relativePath: directoryPath };
}

interface PreviewState {
  status: "loading" | "text" | "binary" | "too-large" | "error";
  text?: string;
  errorMessage?: string;
  byteLength?: number;
}

export default function FilesystemPanel({
  runId,
  leading,
  rootDirectory,
}: FilesystemPanelProps) {
  const initialDirectory = rootDirectory || DEFAULT_DIR;
  const [currentDir, setCurrentDir] = useState<string>(initialDirectory);
  const [selectedFilePath, setSelectedFilePath] = useState<string | null>(null);
  const [selectedFileSize, setSelectedFileSize] = useState<number | undefined>(undefined);

  const filesQuery = useSandboxFiles(runId, currentDir);
  const entries = filesQuery.data?.data ?? EMPTY_SANDBOX_FILE_ENTRIES;
  const treeInputs = useMemo(() => buildTreeInputs(entries), [entries]);

  const navigate = useCallback((nextDir: string) => {
    setSelectedFilePath(null);
    setSelectedFileSize(undefined);
    setCurrentDir(nextDir);
  }, []);

  return (
    <section
      className="flex h-full min-h-0 flex-col"
      aria-labelledby={`run-filesystem-${runId}`}
    >
      <h2 id={`run-filesystem-${runId}`} className="sr-only">
        Filesystem
      </h2>
      <div className="mb-2 flex shrink-0 flex-wrap items-center gap-3">
        {leading}
        <Breadcrumbs path={currentDir} onNavigate={navigate} />
        <div className="ml-auto flex items-center gap-2">
          <Tooltip label="Up one level">
            <button
              type="button"
              className="inline-flex size-9 items-center justify-center rounded-lg text-fg-2 outline-1 -outline-offset-1 outline-white/10 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-50"
              onClick={() => navigate(parentPath(currentDir))}
              aria-label="Up one level"
              disabled={currentDir === "/"}
            >
              <ArrowUturnLeftIcon className="size-4" aria-hidden="true" />
            </button>
          </Tooltip>
          <Tooltip label="Refresh">
            <button
              type="button"
              className="inline-flex size-9 items-center justify-center rounded-lg text-fg-2 outline-1 -outline-offset-1 outline-white/10 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500"
              onClick={() => void filesQuery.mutate()}
              aria-label="Refresh directory listing"
            >
              <ArrowPathIcon
                className={`size-4 ${filesQuery.isValidating ? "animate-spin" : ""}`}
                aria-hidden="true"
              />
            </button>
          </Tooltip>
        </div>
      </div>
      <div className="grid min-h-0 flex-1 grid-cols-[18rem_1fr] gap-4 overflow-hidden rounded-md border border-line">
        <DirectoryPane
          runId={runId}
          treeInputs={treeInputs}
          currentDir={currentDir}
          isLoading={filesQuery.data === undefined && !filesQuery.error}
          error={filesQuery.error}
          onSelectFile={(entry) => {
            setSelectedFilePath(joinPath(currentDir, entry.name));
            setSelectedFileSize(entry.size);
          }}
          onNavigate={(relPath) => navigate(joinPath(currentDir, relPath))}
        />
        <PreviewPane
          runId={runId}
          filePath={selectedFilePath}
          declaredSize={selectedFileSize}
        />
      </div>
    </section>
  );
}

function Breadcrumbs({
  path,
  onNavigate,
}: {
  path: string;
  onNavigate: (path: string) => void;
}) {
  const crumbs = buildBreadcrumbs(path);
  return (
    <nav
      aria-label="Sandbox path"
      className="flex min-w-0 flex-wrap items-center gap-1 text-xs"
    >
      {crumbs.map((crumb, index) => {
        const isLast = index === crumbs.length - 1;
        return (
          <span key={crumb.path} className="flex items-center gap-1">
            {index > 0 && (
              <ChevronRightIcon
                className="size-3 shrink-0 text-fg-muted"
                aria-hidden="true"
              />
            )}
            {isLast ? (
              <span className="font-mono text-fg" aria-current="location">
                {crumb.name}
              </span>
            ) : (
              <button
                type="button"
                onClick={() => onNavigate(crumb.path)}
                className="font-mono text-fg-3 hover:text-fg hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500"
              >
                {crumb.name}
              </button>
            )}
          </span>
        );
      })}
    </nav>
  );
}

interface DirectoryPaneProps {
  runId: string;
  treeInputs: BuiltTreeInputs;
  currentDir: string;
  isLoading: boolean;
  error: unknown;
  onSelectFile: (entry: SandboxFileEntry) => void;
  onNavigate: (relativePath: string) => void;
}

function DirectoryPane({
  treeInputs,
  currentDir,
  isLoading,
  error,
  onSelectFile,
  onNavigate,
}: DirectoryPaneProps) {
  const onSelectFileRef = useRef(onSelectFile);
  onSelectFileRef.current = onSelectFile;
  const onNavigateRef = useRef(onNavigate);
  onNavigateRef.current = onNavigate;

  const fileEntriesRef = useRef(treeInputs.fileEntries);
  fileEntriesRef.current = treeInputs.fileEntries;
  const directoriesRef = useRef(treeInputs.directories);
  directoriesRef.current = treeInputs.directories;

  // useFileTree only consumes `options.paths` at model construction time, so
  // start with the current snapshot and keep the model in sync via
  // `resetPaths` whenever the listing changes.
  const initialPathsRef = useRef(treeInputs.paths);
  const { model } = useFileTree({
    paths:                   initialPathsRef.current,
    flattenEmptyDirectories: false,
    initialExpansion:        "closed",
    icons:                   "standard",
    density:                 "default",
    onSelectionChange:       (selected) => {
      const last = selected[selected.length - 1];
      if (!last) return;
      const result = classifySelection(
        last,
        fileEntriesRef.current,
        directoriesRef.current,
      );
      if (!result) return;
      if (result.kind === "file") {
        onSelectFileRef.current(result.entry);
      } else {
        onNavigateRef.current(result.relativePath);
      }
    },
  });

  useEffect(() => {
    model.resetPaths(treeInputs.paths);
  }, [model, treeInputs.paths]);

  const themeStyles = useMemo<TreeThemeStyle>(
    () => ({
      ...(themeToTreeStyles(pierreDark) as TreeThemeStyle),
      backgroundColor:                   "transparent",
      "--trees-bg-override":             "transparent",
      "--trees-padding-inline-override": "0px",
    }),
    [],
  );

  return (
    <div
      style={themeStyles}
      className="flex min-h-0 flex-col self-stretch overflow-hidden border-r border-line"
    >
      {error ? (
        <DirectoryError error={error} />
      ) : isLoading ? (
        <div className="flex min-h-0 flex-1 items-center justify-center px-4 py-6">
          <LoadingState label={`Listing ${currentDir}…`} />
        </div>
      ) : treeInputs.paths.length === 0 ? (
        <output className="px-3 py-4 text-sm text-fg-muted">
          Empty directory
        </output>
      ) : (
        <FileTree model={model} className="min-h-0 flex-1 overflow-auto" />
      )}
    </div>
  );
}

function DirectoryError({ error }: { error: unknown }) {
  const isApiError = error instanceof ApiError;
  const description = isApiError
    ? error.message
    : error instanceof Error
      ? error.message
      : "Could not load directory listing.";
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center px-4 py-6">
      <ErrorState title="Listing unavailable" description={description} />
    </div>
  );
}

function PreviewPane({
  runId,
  filePath,
  declaredSize,
}: {
  runId: string;
  filePath: string | null;
  declaredSize: number | undefined;
}) {
  const tooLarge = declaredSize != null && declaredSize > TEXT_PREVIEW_BYTE_LIMIT;
  const fileQuery = useSandboxFile(runId, tooLarge ? null : filePath);

  const preview = useMemo<PreviewState | null>(() => {
    if (!filePath) return null;
    if (tooLarge) {
      return {
        status:     "too-large",
        byteLength: declaredSize,
      };
    }
    if (fileQuery.error) {
      const description =
        fileQuery.error instanceof ApiError
          ? fileQuery.error.message
          : fileQuery.error instanceof Error
            ? fileQuery.error.message
            : "Could not load file.";
      return { status: "error", errorMessage: description };
    }
    const buffer = fileQuery.data;
    if (!buffer) {
      return { status: "loading" };
    }
    const bytes = new Uint8Array(buffer);
    if (bytes.byteLength > TEXT_PREVIEW_BYTE_LIMIT) {
      return { status: "too-large", byteLength: bytes.byteLength };
    }
    if (looksLikeBinary(bytes)) {
      return { status: "binary", byteLength: bytes.byteLength };
    }
    const text = decodeUtf8Strict(bytes);
    if (text == null) {
      return { status: "binary", byteLength: bytes.byteLength };
    }
    return { status: "text", text, byteLength: bytes.byteLength };
  }, [declaredSize, fileQuery.data, fileQuery.error, filePath, tooLarge]);

  if (!filePath || !preview) {
    return (
      <div className="flex min-h-0 items-center justify-center p-6">
        <EmptyState
          title="No file selected"
          description="Select a file from the directory listing to preview its contents."
        />
      </div>
    );
  }

  const name = basename(filePath);
  const sizeLabel = formatFileSize(preview.byteLength);

  return (
    <div className="flex min-h-0 min-w-0 flex-col">
      <header className="flex shrink-0 items-center gap-3 border-b border-line bg-panel/60 px-4 py-2.5">
        <span
          className="min-w-0 truncate font-mono text-xs text-fg-2"
          title={filePath}
        >
          {name}
        </span>
        {sizeLabel && (
          <span className="font-mono text-xs text-fg-muted">{sizeLabel}</span>
        )}
        <div className="ml-auto flex items-center gap-2">
          <a
            href={downloadUrl(runId, filePath)}
            download={name}
            target="_blank"
            rel="noreferrer"
            className={SECONDARY_BUTTON_CLASS}
            aria-label={`Download ${name}`}
          >
            <ArrowDownTrayIcon className="size-4" aria-hidden="true" />
            Download
          </a>
        </div>
      </header>
      <div className="min-h-0 flex-1">
        <PreviewBody
          preview={preview}
          runId={runId}
          fileName={name}
          filePath={filePath}
        />
      </div>
    </div>
  );
}

function PreviewBody({
  preview,
  runId,
  fileName,
  filePath,
}: {
  preview: PreviewState;
  runId: string;
  fileName: string;
  filePath: string;
}) {
  if (preview.status === "loading") {
    return (
      <div className="flex h-full items-center justify-center p-6">
        <LoadingState label="Loading file…" />
      </div>
    );
  }
  if (preview.status === "error") {
    return (
      <div className="flex h-full items-center justify-center p-6">
        <ErrorState title="File unavailable" description={preview.errorMessage} />
      </div>
    );
  }
  if (preview.status === "too-large") {
    return (
      <div className="flex h-full items-center justify-center p-6">
        <EmptyState
          title="File too large to preview"
          description={
            preview.byteLength != null
              ? `${formatFileSize(preview.byteLength) ?? `${preview.byteLength} B`} exceeds the inline preview limit. Use the Download button to fetch the raw file.`
              : "Use the Download button to fetch the raw file."
          }
        />
      </div>
    );
  }
  if (preview.status === "binary") {
    return (
      <div className="flex h-full items-center justify-center p-6">
        <EmptyState
          title="Binary file"
          description="No inline preview is available for this file. Use the Download button to fetch the raw bytes."
        />
      </div>
    );
  }
  if ((preview.text ?? "").length === 0) {
    return (
      <div className="flex h-full items-center justify-center p-6">
        <EmptyState
          title="Empty file"
          description="This file has no contents."
        />
      </div>
    );
  }
  const file: FileContents = {
    name:     fileName,
    contents: preview.text ?? "",
    cacheKey: sandboxFileCacheKey({
      runId,
      path:     filePath,
      contents: preview.text ?? "",
    }),
  };
  return (
    <WorkerPoolContextProvider
      poolOptions={pierrePoolOptions}
      highlighterOptions={pierreHighlighterOptions}
    >
      <Virtualizer
        className="h-full min-h-0 overflow-auto"
        contentClassName="min-w-0 pb-4"
      >
        <File
          file={file}
          options={{ theme: "pierre-dark", disableFileHeader: true }}
        />
      </Virtualizer>
    </WorkerPoolContextProvider>
  );
}
