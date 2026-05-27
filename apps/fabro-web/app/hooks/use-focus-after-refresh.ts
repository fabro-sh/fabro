import { useEffect, useRef, type RefObject } from "react";

/**
 * Synchronizes refresh completion with browser focus so keyboard users return to
 * the refresh control. No cleanup is required because focus is a one-shot DOM
 * operation and duplicate Strict Mode calls do not change persisted state.
 */
export function useFocusAfterRefreshCompletes(
  refreshing: boolean,
  targetRef: RefObject<HTMLElement | null>,
) {
  const refreshingPrev = useRef(false);

  useEffect(() => {
    if (refreshingPrev.current && !refreshing) {
      targetRef.current?.focus({ preventScroll: true });
    }
    refreshingPrev.current = refreshing;
  }, [refreshing, targetRef]);
}
