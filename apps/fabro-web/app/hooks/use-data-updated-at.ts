import { useMemo } from "react";

/**
 * Captures a stable wall-clock timestamp for the current async data identity.
 * The value is derived during render and stays stable until that identity
 * changes.
 */
export function useDataUpdatedAt<T>(data: T | null | undefined): number | null {
  return useMemo(() => data != null ? Date.now() : null, [data]);
}
