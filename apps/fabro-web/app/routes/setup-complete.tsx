import { useEffect, useMemo, useRef, useState } from "react";
import { AuthLayout } from "../components/auth-layout";

type SetupState = "registering" | "done" | "error";

export default function SetupComplete() {
  const code = useMemo(() => new URLSearchParams(window.location.search).get("code"), []);
  const [state, setState] = useState<SetupState>(code ? "registering" : "done");
  const [error, setError] = useState<string | null>(null);
  const registeredRef = useRef(false);

  useEffect(() => {
    if (registeredRef.current) return;
    registeredRef.current = true;

    async function register() {
      if (!code) {
        setState("done");
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
        setError(payload.error ?? "Setup registration failed.");
        setState("error");
        return;
      }

      setState("done");
      window.history.replaceState({}, "", "/setup/complete");
    }

    void register();
  }, [code]);

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

      {state === "done" && (
        <>
          <h1 className="text-center text-lg font-semibold text-fg">
            Setup complete
          </h1>
          <p className="mt-2 text-center text-sm text-fg-3">
            Your GitHub App has been registered and configured.
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
