import { useEffect, useState } from "react";

/**
 * Captures wall-clock time when an async data identity becomes available. The
 * timestamp update is ignored for nullish values and has no cleanup.
 */
export function useDataUpdatedAt<T>(data: T | null | undefined): number | null {
  const [updatedAt, setUpdatedAt] = useState<number | null>(null);

  useEffect(() => {
    if (data != null) setUpdatedAt(Date.now());
  }, [data]);

  return updatedAt;
}
