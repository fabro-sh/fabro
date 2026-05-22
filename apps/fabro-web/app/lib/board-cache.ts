import type { Key } from "swr";

import { queryKeys } from "./query-keys";

type MutateBoardRuns = (key: Key) => unknown;

export function boardRunCacheKeys(): Key[] {
  return [queryKeys.boards.runs(false), queryKeys.boards.runs(true)];
}

export function mutateBoardRunCaches(mutate: MutateBoardRuns) {
  for (const key of boardRunCacheKeys()) {
    void mutate(key);
  }
}
