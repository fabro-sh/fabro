import { useEffect, useRef } from "react";

/**
 * Synchronizes route search params with a one-time local-storage hydration pass.
 * The URL replacement runs at most once per mount and performs no cleanup.
 */
export function useHydrateSearchParamsOnce({
  resolvedSearchParams,
  setSearchParams,
  urlSearchParams,
}: {
  resolvedSearchParams: URLSearchParams;
  setSearchParams: (
    next: URLSearchParams,
    options: { replace: boolean },
  ) => void;
  urlSearchParams: URLSearchParams;
}) {
  const hydratedFromStorage = useRef(false);

  useEffect(() => {
    if (hydratedFromStorage.current) return;
    hydratedFromStorage.current = true;
    if (resolvedSearchParams === urlSearchParams) return;
    setSearchParams(resolvedSearchParams, { replace: true });
  }, [resolvedSearchParams, setSearchParams, urlSearchParams]);
}
