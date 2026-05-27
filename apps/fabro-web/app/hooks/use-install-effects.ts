import { startTransition, useEffect, type Dispatch, type SetStateAction } from "react";
import type { NavigateFunction } from "react-router";

import {
  type InstallFinishResponse,
  type InstallSessionResponse,
  getInstallSession,
  persistInstallToken,
} from "../install-api";
import { shouldRedirectAfterHealthPoll } from "../install-flow";
import {
  consumeInstallGithubErrorFromUrl,
  consumeInstallTokenFromUrl,
  shouldConsumeInstallGithubErrorForPath,
} from "../mode";

type InstallSessionAction =
  | { type: "sessionCleared" }
  | { type: "sessionRequested" }
  | { type: "sessionReady"; session: InstallSessionResponse }
  | { type: "sessionFailed"; message: string };

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
  useEffect(() => {
    if (shouldConsumeInstallGithubErrorForPath(pathname)) {
      const { error, sanitizedUrl } = consumeInstallGithubErrorFromUrl(window.location.href);
      if (error) {
        dispatchInstall({ type: "saveErrorChanged", message: error });
        window.history.replaceState(window.history.state, "", sanitizedUrl);
        return;
      }
    }
    dispatchInstall({ type: "saveErrorChanged", message: null });
  }, [dispatchInstall, pathname]);
}

/**
 * Drives the install session state machine from the current install token. The
 * in-flight session request is ignored after token changes or unmounts.
 */
export function useInstallSessionLoader({
  dispatchInstall,
  installToken,
}: {
  dispatchInstall: (action: InstallSessionAction) => void;
  installToken: string | null;
}) {
  useEffect(() => {
    if (!installToken) {
      dispatchInstall({ type: "sessionCleared" });
      return;
    }

    let cancelled = false;
    dispatchInstall({ type: "sessionRequested" });
    getInstallSession(installToken)
      .then((nextSession) => {
        if (cancelled) return;
        dispatchInstall({ type: "sessionReady", session: nextSession });
      })
      .catch((error) => {
        if (cancelled) return;
        dispatchInstall({
          type:    "sessionFailed",
          message: error instanceof Error ? error.message : "Install session failed",
        });
      });

    return () => {
      cancelled = true;
    };
  }, [dispatchInstall, installToken]);
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

/**
 * Synchronizes the install root route with the loaded install session by
 * replacing the URL once the async session is ready. Duplicate development calls
 * are harmless because React Router replaces to the same destination.
 */
export function useInstallRootRedirect({
  finishState,
  installToken,
  navigate,
  pathname,
  session,
}: {
  finishState: InstallFinishResponse | null;
  installToken: string | null;
  navigate: NavigateFunction;
  pathname: string;
  session: InstallSessionResponse | null;
}) {
  useEffect(() => {
    if (!installToken || !session) return;
    if ((pathname === "/" || pathname === "/install") && !finishState) {
      startTransition(() => {
        navigate("/install/welcome", { replace: true });
      });
    }
  }, [finishState, installToken, navigate, pathname, session]);
}
