import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Synchronizes a user-triggered refresh affordance with the browser timer queue.
 * Any pending minimum-duration timer is cleared before restart and on unmount.
 */
export function useMinimumRefreshSpinner(durationMs: number) {
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [active, setActive] = useState(false);

  const clear = useCallback(() => {
    if (timerRef.current !== null) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const start = useCallback(() => {
    clear();
    setActive(true);
    timerRef.current = setTimeout(() => {
      setActive(false);
      timerRef.current = null;
    }, durationMs);
  }, [clear, durationMs]);

  useEffect(() => clear, [clear]);

  return { active, start };
}
