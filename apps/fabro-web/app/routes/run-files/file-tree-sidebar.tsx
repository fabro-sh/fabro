import {
  useEffect,
  useMemo,
  useRef,
  type ChangeEvent,
  type CSSProperties,
} from "react";
import {
  FileTree,
  useFileTree,
  useFileTreeSearch,
  useFileTreeSelection,
} from "@pierre/trees/react";
import {
  themeToTreeStyles,
  type FileTree as FileTreeModel,
  type GitStatus,
  type GitStatusEntry,
} from "@pierre/trees";
import pierreDark from "@pierre/theme/pierre-dark";
import type { FileDiff } from "@qltysh/fabro-api-client";

type TreeThemeStyle = CSSProperties & Record<`--${string}`, string | number>;

const CHANGE_KIND_TO_GIT_STATUS: Record<NonNullable<FileDiff["change_kind"]>, GitStatus> = {
  added:     "added",
  modified:  "modified",
  deleted:   "deleted",
  renamed:   "renamed",
  symlink:   "modified",
  submodule: "modified",
};

function filePath(file: FileDiff): string {
  return file.new_file.name || file.old_file.name;
}

function gitStatusFor(file: FileDiff): GitStatus {
  return file.change_kind
    ? CHANGE_KIND_TO_GIT_STATUS[file.change_kind] ?? "modified"
    : "modified";
}

function lastSelectedFile(
  selected: readonly string[],
  changedPaths: ReadonlySet<string>,
): string | null {
  for (let index = selected.length - 1; index >= 0; index -= 1) {
    const path = selected[index];
    if (path && changedPaths.has(path)) return path;
  }
  return null;
}

function syncSelection(
  model: FileTreeModel,
  selection: readonly string[],
  selectedPath: string | null,
) {
  for (const path of selection) {
    if (path !== selectedPath) {
      model.getItem(path)?.deselect();
    }
  }
  if (!selectedPath || (selection.length === 1 && selection[0] === selectedPath)) {
    return;
  }
  const item = model.getItem(selectedPath);
  if (item && !item.isSelected()) item.select();
}

interface FileTreeSidebarProps {
  files: readonly FileDiff[];
  selectedPath: string | null;
  onSelect: (path: string) => void;
}

export function FileTreeSidebar({
  files,
  selectedPath,
  onSelect,
}: FileTreeSidebarProps) {
  const paths = useMemo(() => files.map(filePath), [files]);
  const changedPaths = useMemo(() => new Set(paths), [paths]);

  const gitStatus = useMemo<GitStatusEntry[]>(
    () =>
      files.map((file) => ({
        path:   filePath(file),
        status: gitStatusFor(file),
      })),
    [files],
  );

  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;

  const selectedPathRef = useRef(selectedPath);
  selectedPathRef.current = selectedPath;

  const changedPathsRef = useRef<ReadonlySet<string>>(changedPaths);
  changedPathsRef.current = changedPaths;

  const pendingSelectedPathRef = useRef<string | null>(null);

  const { model } = useFileTree({
    paths,
    flattenEmptyDirectories: true,
    initialExpansion:        "open",
    initialSelectedPaths:    selectedPath ? [selectedPath] : undefined,
    gitStatus,
    icons:                   "standard",
    density:                 "default",
    search:                  true,
    fileTreeSearchMode:      "hide-non-matches",
    onSelectionChange:       (selected) => {
      const selectedFile = lastSelectedFile(selected, changedPathsRef.current);
      if (!selectedFile) return;
      pendingSelectedPathRef.current = selectedFile;
      onSelectRef.current(selectedFile);
    },
  });

  const didSyncModelRef = useRef(false);
  useEffect(() => {
    if (!didSyncModelRef.current) {
      didSyncModelRef.current = true;
      return;
    }
    model.resetPaths(paths);
    model.setGitStatus(gitStatus);
    pendingSelectedPathRef.current = null;
    const currentSelectedPath = selectedPathRef.current;
    syncSelection(
      model,
      model.getSelectedPaths(),
      currentSelectedPath && changedPathsRef.current.has(currentSelectedPath)
        ? currentSelectedPath
        : null,
    );
  }, [gitStatus, model, paths]);

  const selection = useFileTreeSelection(model);
  useEffect(() => {
    const pendingSelectedPath = pendingSelectedPathRef.current;
    if (pendingSelectedPath === selectedPath) {
      pendingSelectedPathRef.current = null;
    }
    const nextSelectedPath = pendingSelectedPath ?? selectedPath;
    syncSelection(
      model,
      selection,
      nextSelectedPath && changedPaths.has(nextSelectedPath) ? nextSelectedPath : null,
    );
  }, [changedPaths, model, selectedPath, selection]);

  const search = useFileTreeSearch(model);
  const handleSearchChange = (event: ChangeEvent<HTMLInputElement>) =>
    search.setValue(event.target.value);

  const themeStyles = useMemo(
    () => themeToTreeStyles(pierreDark) as TreeThemeStyle,
    [],
  );

  return (
    <aside
      aria-label="Changed files"
      style={themeStyles}
      className="sticky top-4 flex max-h-[calc(100vh-6rem)] w-72 shrink-0 flex-col gap-2 self-start"
    >
      <input
        type="search"
        value={search.value}
        onChange={handleSearchChange}
        placeholder="Filter changed files…"
        aria-label="Filter changed files"
        className="w-full rounded-md border border-line bg-panel px-2 py-1.5 text-sm text-fg placeholder:text-fg-muted focus:outline-2 focus:outline-focus focus:outline-offset-2"
      />
      <FileTree
        model={model}
        className="min-h-0 flex-1 overflow-hidden rounded-md border border-line bg-panel"
      />
    </aside>
  );
}
