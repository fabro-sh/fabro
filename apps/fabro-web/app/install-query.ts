import useSWR, { type SWRConfiguration } from "swr";

import { getInstallSession, type InstallSessionResponse } from "./install-api";

type InstallSessionKey = readonly ["install", "session", string];

function installSessionKey(token: string | null): InstallSessionKey | null {
  return token ? ["install", "session", token] : null;
}

/**
 * Reads the install session through SWR so server state is owned by the query
 * layer instead of a component effect. Revalidation is explicit because install
 * setup writes refresh the session from their submit path.
 */
export function useInstallSessionQuery(
  token: string | null,
  options: SWRConfiguration<InstallSessionResponse, Error> = {},
) {
  return useSWR<InstallSessionResponse, Error, InstallSessionKey | null>(
    installSessionKey(token),
    ([, , currentToken]) => getInstallSession(currentToken),
    {
      dedupingInterval:       0,
      revalidateOnFocus:      false,
      revalidateOnReconnect:  false,
      shouldRetryOnError:     false,
      ...options,
    },
  );
}
