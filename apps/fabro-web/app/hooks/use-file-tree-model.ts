import { useEffect } from "react";

import type { FileTree as FileTreeModel } from "@pierre/trees";

/**
 * Synchronizes Pierre's imperative file-tree model with the latest path list.
 * The model owns no subscription here, so no cleanup is required.
 */
export function useResetFileTreePaths(
  model: FileTreeModel,
  paths: readonly string[],
) {
  useEffect(() => {
    model.resetPaths(paths);
  }, [model, paths]);
}
