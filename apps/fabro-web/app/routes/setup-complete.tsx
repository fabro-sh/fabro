import { useEffect, useMemo, useState } from "react";
import { AuthLayout } from "../components/auth-layout";

type SetupState = "registering" | "restart_required" | "ready" | "error";

export default function SetupComplete() {
  const code = useMemo(() => new URLSearchParams(window.location.search).get("code"), []);
  const [state, setState] = useState<SetupState>(code ? "registering" : "restart_required");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function register() {
      if (!code) {
        setState("restart_required");
        return;
      }

      const response = await fetch("/api/v1/setup/register", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ code }),
      });

      if (!response.ok) {
        const payload = await response.json().catch(() => ({}));
        if (!cancelled) {
          setError(payload.error ?? "Setup registration failed.");
          setState("error");
        }
        return;
      }

      if (!cancelled) {
        setState("restart_required");
        window.history.replaceState({}, "", "/setup/complete");
      }
    }

    void register();

    return () => {
      cancelled = true;
    };
  }, [code]);

  useEffect(() => {
    if (state !== "restart_required") {
      return;
    }

    const timer = window.setInterval(async () => {
      const response = await fetch("/api/v1/setup/status", { credentials: "include" });
      if (!response.ok) {
        return;
      }

      const payload = await response.json().catch(() => ({ configured: false }));
      if (payload.configured) {
        setState("ready");
      }
    }, 2000);

    return () => {
      window.clearInterval(timer);
    };
  }, [state]);

  return (
    <AuthLayout>
      {state === "registering" && (
        <>
          <h1 className="text-center text-lg font-semibold text-fg">
            Finishing setup
          </h1>
          <p className="mt-2 text-center text-sm text-fg-3">
            Registering your GitHub App and writing local configuration.
          </p>
        </>
      )}

      {state === "restart_required" && (
        <>
          <h1 className="text-center text-lg font-semibold text-fg">
            Setup complete
          </h1>
          <p className="mt-2 text-center text-sm text-fg-3">
            Fabro loads auth configuration at process start, so a restart is required before sign-in.
          </p>
          <pre className="mt-6 rounded-lg border border-line bg-panel/80 px-4 py-3 text-center font-mono text-sm text-fg">
            fabro server start
          </pre>
          <p className="mt-4 text-center text-xs text-fg-muted">
            Waiting for the restarted server to come back...
          </p>
        </>
      )}

      {state === "ready" && (
        <>
          <h1 className="text-center text-lg font-semibold text-fg">
            Restart detected
          </h1>
          <p className="mt-2 text-center text-sm text-fg-3">
            Authentication is ready.
          </p>
          <a
            href="/login"
            className="mt-6 flex w-full items-center justify-center rounded-lg bg-teal-500 px-4 py-2.5 text-sm font-medium text-white transition-colors hover:bg-teal-300"
          >
            Continue to sign in
          </a>
        </>
      )}

      {state === "error" && (
        <>
          <h1 className="text-center text-lg font-semibold text-fg">
            Setup failed
          </h1>
          <p className="mt-2 text-center text-sm text-fg-3">
            {error}
          </p>
          <a
            href="/setup"
            className="mt-6 flex w-full items-center justify-center rounded-lg border border-line-strong px-4 py-2.5 text-sm font-medium text-fg-2 transition-colors hover:bg-overlay-strong"
          >
            Try again
          </a>
        </>
      )}
    </AuthLayout>
  );
}
