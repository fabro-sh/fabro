import { useState } from "react";
import { useNavigate } from "react-router";
import { AuthLayout } from "../components/auth-layout";
import {
  ErrorMessage,
  INPUT_CLASS,
  PRIMARY_BUTTON_CLASS,
} from "../components/ui";
import { getAuthConfig, loginDevToken } from "../api";

export async function loader() {
  return getAuthConfig();
}

export default function AuthLogin({ loaderData }: any) {
  const methods = loaderData?.methods ?? [];
  const hasDevToken = methods.includes("dev-token");
  const hasGitHub = methods.includes("github");
  const navigate = useNavigate();
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);

  async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setError(null);

    try {
      await loginDevToken(token);
      navigate("/runs");
    } catch {
      setError("Invalid dev token.");
    }
  }

  return (
    <AuthLayout>
      <h1 className="text-center text-2xl font-semibold tracking-tight text-fg text-balance sm:text-[1.75rem]">
        Sign in to Fabro
      </h1>
      <p className="mt-3 text-center text-sm/6 text-fg-3 text-pretty">
        {hasGitHub
          ? "Authenticate with your GitHub account to continue."
          : hasDevToken
            ? "Paste your dev token to continue."
            : "No authentication method is configured on this server."}
      </p>
      <div className="mt-6 space-y-4">
        {hasGitHub ? (
          <a
            href="/auth/login/github"
            className={`${PRIMARY_BUTTON_CLASS} w-full`}
          >
            <GitHubMark />
            Sign in with GitHub
          </a>
        ) : null}
        {hasDevToken && hasGitHub ? (
          <DevTokenCollapsible
            token={token}
            setToken={setToken}
            error={error}
            onSubmit={handleSubmit}
          />
        ) : hasDevToken ? (
          <DevTokenForm
            token={token}
            setToken={setToken}
            error={error}
            onSubmit={handleSubmit}
            showLocation
          />
        ) : null}
      </div>
    </AuthLayout>
  );
}

function DevTokenForm({
  token,
  setToken,
  error,
  onSubmit,
  showLocation = false,
}: {
  token: string;
  setToken: (v: string) => void;
  error: string | null;
  onSubmit: (e: React.FormEvent<HTMLFormElement>) => void;
  showLocation?: boolean;
}) {
  return (
    <form className="space-y-4" onSubmit={onSubmit}>
      <div>
        <label htmlFor="dev-token" className="sr-only">
          Dev token
        </label>
        <input
          id="dev-token"
          type="password"
          name="dev_token"
          value={token}
          onChange={(event) => setToken(event.target.value)}
          placeholder="fabro_dev_…"
          className={`${INPUT_CLASS} font-mono`}
          autoComplete="off"
          autoCapitalize="off"
          spellCheck={false}
        />
      </div>
      {error ? <ErrorMessage message={error} /> : null}
      <button type="submit" className={`${PRIMARY_BUTTON_CLASS} w-full`}>
        Sign in with dev token
      </button>
      {showLocation ? (
        <p className="text-center text-xs text-fg-muted">
          Paste the dev token from your server terminal or install output.
        </p>
      ) : null}
    </form>
  );
}

function DevTokenCollapsible({
  token,
  setToken,
  error,
  onSubmit,
}: {
  token: string;
  setToken: (v: string) => void;
  error: string | null;
  onSubmit: (e: React.FormEvent<HTMLFormElement>) => void;
}) {
  const [open, setOpen] = useState(false);

  return (
    <div className="pt-2">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="mx-auto flex items-center gap-1 rounded text-xs text-fg-3 outline-teal-500 hover:text-fg-2 focus-visible:outline-2 focus-visible:outline-offset-2"
      >
        Use a dev token instead
        <svg
          width="12"
          height="12"
          viewBox="0 0 12 12"
          fill="none"
          className={`transition-transform ${open ? "rotate-180" : ""}`}
          aria-hidden="true"
        >
          <path
            d="M3 4.5L6 7.5L9 4.5"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </button>
      {open ? (
        <div className="mt-4">
          <DevTokenForm
            token={token}
            setToken={setToken}
            error={error}
            onSubmit={onSubmit}
          />
        </div>
      ) : null}
    </div>
  );
}

function GitHubMark() {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" className="size-4 shrink-0" aria-hidden="true">
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z" />
    </svg>
  );
}
