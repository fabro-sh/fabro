import { useEffect, useRef, type Dispatch, type SetStateAction } from "react";

import {
  type InstallFinishResponse,
  persistInstallToken,
} from "../install-api";
import { shouldRedirectAfterHealthPoll } from "../install-flow";
import {
  consumeInstallGithubErrorFromUrl,
  consumeInstallTokenFromUrl,
  shouldConsumeInstallGithubErrorForPath,
} from "../mode";

type InstallGithubCallbackAction =
  | { type: "saveErrorChanged"; message: string | null };

type InstallRestartPollingAction =
  | { type: "timedOutChanged"; timedOut: boolean };

/**
 * Synchronizes install mode with the browser URL and sessionStorage. A token in
 * the URL is persisted, promoted into React state, and scrubbed from history on
 * mount; there is no resource to clean up.
 */
export function useInstallTokenFromUrl({
  setInstallToken,
}: {
  setInstallToken: Dispatch<SetStateAction<string | null>>;
}) {
  useEffect(() => {
    const { token, sanitizedUrl } = consumeInstallTokenFromUrl(window.location.href);
    if (!token) return;

    persistInstallToken(token);
    setInstallToken(token);
    window.history.replaceState(window.history.state, "", sanitizedUrl);
  }, [setInstallToken]);
}

/**
 * Synchronizes GitHub App callback errors from the browser URL into the install
 * state machine. The error query parameter is scrubbed from history after it is
 * consumed; there is no resource to clean up.
 */
export function useInstallGithubCallbackError({
  dispatchInstall,
  pathname,
}: {
  dispatchInstall: (action: InstallGithubCallbackAction) => void;
  pathname: string;
}) {
  const consumedErrorPathRef = useRef<string | null>(null);

  useEffect(() => {
    if (shouldConsumeInstallGithubErrorForPath(pathname)) {
      const { error, sanitizedUrl } = consumeInstallGithubErrorFromUrl(window.location.href);
      if (error) {
        consumedErrorPathRef.current = pathname;
        dispatchInstall({ type: "saveErrorChanged", message: error });
        window.history.replaceState(window.history.state, "", sanitizedUrl);
        return;
      }
      if (consumedErrorPathRef.current === pathname) {
        return;
      }
    }
    consumedErrorPathRef.current = null;
    dispatchInstall({ type: "saveErrorChanged", message: null });
  }, [dispatchInstall, pathname]);
}

/**
 * Synchronizes install finishing with browser timers, fetch health polling, and
 * `window.location`. The deadline timer, polling interval, and in-flight fetch
 * are cancelled when finishing stops or the component unmounts.
 */
export function useInstallRestartHealthPolling({
  dispatchInstall,
  finishState,
}: {
  dispatchInstall: (action: InstallRestartPollingAction) => void;
  finishState: InstallFinishResponse | null;
}) {
  useEffect(() => {
    if (!finishState) return;

    dispatchInstall({ type: "timedOutChanged", timedOut: false });
    const deadline = window.setTimeout(() => {
      dispatchInstall({ type: "timedOutChanged", timedOut: true });
    }, 30_000);

    const controller = new AbortController();
    let inFlight = false;
    const poll = async () => {
      if (inFlight || controller.signal.aborted) return;
      inFlight = true;
      try {
        const response = await fetch("/health", { signal: controller.signal });
        const body = response.ok
          ? ((await response.json()) as { mode?: string })
          : undefined;
        if (
          shouldRedirectAfterHealthPoll({
            kind: "response",
            ok: response.ok,
            mode: body?.mode,
          })
        ) {
          window.location.href = finishState.restart_url;
        }
      } catch {
        if (controller.signal.aborted) return;
        if (shouldRedirectAfterHealthPoll({ kind: "error" })) {
          window.location.href = finishState.restart_url;
        }
      } finally {
        inFlight = false;
      }
    };
    const interval = window.setInterval(poll, 2_000);

    return () => {
      controller.abort();
      window.clearTimeout(deadline);
      window.clearInterval(interval);
    };
  }, [dispatchInstall, finishState]);
}
