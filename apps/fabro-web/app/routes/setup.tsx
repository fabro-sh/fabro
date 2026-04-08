import { useMemo } from "react";
import { AuthLayout } from "../components/auth-layout";
export default function Setup() {
  const manifest = useMemo(() => {
    const baseUrl = window.location.origin;
    const suffix = Math.random().toString(16).slice(2, 8);
    return JSON.stringify({
      name: `Fabro-${suffix}`,
      url: "https://fabro.sh",
      redirect_url: `${baseUrl}/setup/complete`,
      callback_urls: [`${baseUrl}/auth/callback/github`],
      setup_url: `${baseUrl}/setup/complete`,
      public: false,
      default_permissions: {
        contents: "write",
        metadata: "read",
        pull_requests: "write",
        checks: "write",
        issues: "write",
        emails: "read",
      },
      default_events: [],
    });
  }, []);

  return (
    <AuthLayout>
      <h1 className="text-center text-lg font-semibold text-fg">
        Set up Fabro
      </h1>
      <p className="mt-2 text-center text-sm text-fg-3">
        Register a GitHub App to enable OAuth login and repository access.
      </p>
      <form
        method="POST"
        action="https://github.com/settings/apps/new"
        className="mt-6"
      >
        <input type="hidden" name="manifest" value={manifest} />
        <button
          type="submit"
          className="flex w-full items-center justify-center gap-2 rounded-lg bg-teal-500 px-4 py-2.5 text-sm font-medium text-white transition-colors hover:bg-teal-300"
        >
          <GitHubMark />
          Register GitHub App
        </button>
      </form>
    </AuthLayout>
  );
}

function GitHubMark() {
  return (
    <svg width="18" height="18" viewBox="0 0 16 16" fill="currentColor">
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z" />
    </svg>
  );
}
