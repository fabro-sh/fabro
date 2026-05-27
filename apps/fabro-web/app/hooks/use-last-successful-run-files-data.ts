import { useEffect, useRef } from "react";

import type { PaginatedRunFileList } from "@qltysh/fabro-api-client";
import type { ToastInput } from "../components/toast";

/**
 * Maintains the last committed run-files payload so failed SWR revalidations can
 * keep rendering prior file data. The refs intentionally update after render so
 * callers can compare the current payload to the previous committed snapshot;
 * empty-transition toasts are emitted once from that commit path.
 */
export function useLastSuccessfulRunFilesData({
  currentData,
  emptyTransitionMessage,
  push,
}: {
  currentData: PaginatedRunFileList | null | undefined;
  emptyTransitionMessage: (
    previousFileCount: number | null,
    nextFileCount: number,
  ) => string | null;
  push: (toast: ToastInput) => string;
}) {
  const lastGoodDataRef = useRef<PaginatedRunFileList | null>(null);
  const lastFetchedAtRef = useRef<number | null>(null);
  const previousData = lastGoodDataRef.current;

  useEffect(() => {
    if (!currentData) return;
    const message = emptyTransitionMessage(
      lastGoodDataRef.current?.data.length ?? null,
      currentData.data.length,
    );
    if (message) {
      push({ message });
    }
    lastGoodDataRef.current = currentData;
    lastFetchedAtRef.current = Date.now();
  }, [currentData, emptyTransitionMessage, push]);

  return {
    data: currentData ?? lastGoodDataRef.current,
    hasLastGoodData: lastGoodDataRef.current !== null,
    lastFetchedAt: lastFetchedAtRef.current,
    previousToSha: previousData?.meta?.to_sha ?? null,
  };
}
