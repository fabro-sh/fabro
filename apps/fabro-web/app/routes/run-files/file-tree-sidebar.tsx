import {
  useMemo,
  useRef,
  type CSSProperties,
} from "react";
import {
  FileTree,
  useFileTree,
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
import { useChangedFilesTreeSync } from "../../hooks/use-changed-files-tree-sync";

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
    onSelectionChange:       (selected) => {
      const selectedFile = lastSelectedFile(selected, changedPathsRef.current);
      if (!selectedFile) return;
      pendingSelectedPathRef.current = selectedFile;
      onSelectRef.current(selectedFile);
    },
  });

  const selection = useFileTreeSelection(model);
  useChangedFilesTreeSync({
    changedPaths,
    changedPathsRef,
    gitStatus,
    model,
    paths,
    pendingSelectedPathRef,
    selectedPath,
    selectedPathRef,
    selection,
    syncSelection,
  });

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
    <aside
      aria-label="Changed files"
      style={themeStyles}
      className="-ml-0.5 flex min-h-0 w-72 shrink-0 flex-col self-stretch"
    >
      {paths.length > 0 ? (
        <FileTree model={model} className="min-h-0 flex-1 overflow-hidden" />
      ) : (
        <output className="min-h-0 flex-1 px-3 py-2 text-sm text-fg-muted">
          No changed files
        </output>
      )}
    </aside>
  );
}
