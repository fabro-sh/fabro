import { useEffect, useRef } from "react";

import type {
  FileTree as FileTreeModel,
  GitStatusEntry,
} from "@pierre/trees";

/**
 * Synchronizes Pierre's imperative changed-files tree model with React-owned
 * file paths, git status, and selected-path state. Model mutations run after
 * commit; no external subscription is created.
 */
export function useChangedFilesTreeSync({
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
}: {
  changedPaths: ReadonlySet<string>;
  changedPathsRef: { current: ReadonlySet<string> };
  gitStatus: GitStatusEntry[];
  model: FileTreeModel;
  paths: string[];
  pendingSelectedPathRef: { current: string | null };
  selectedPath: string | null;
  selectedPathRef: { current: string | null };
  selection: readonly string[];
  syncSelection: (
    model: FileTreeModel,
    selection: readonly string[],
    selectedPath: string | null,
  ) => void;
}) {
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
  }, [changedPathsRef, gitStatus, model, paths, pendingSelectedPathRef, selectedPathRef, syncSelection]);

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
  }, [changedPaths, model, pendingSelectedPathRef, selectedPath, selection, syncSelection]);
}
