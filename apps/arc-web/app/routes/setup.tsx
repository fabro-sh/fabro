import { redirect } from "react-router";
import { isGitHubAppConfigured } from "../lib/github.server";
import type { Route } from "./+types/setup";

export function loader({ request }: Route.LoaderArgs) {
  if (isGitHubAppConfigured()) {
    return redirect("/");
  }

  const url = new URL(request.url);
  const baseUrl = `${url.protocol}//${url.host}`;
  const suffix = Math.random().toString(16).slice(2, 8);

  const manifest = {
    name: `Arc-${suffix}`,
    url: baseUrl,
    redirect_url: `${baseUrl}/setup/callback`,
    callback_urls: [`${baseUrl}/auth/callback`],
    setup_url: `${baseUrl}/setup/callback`,
    public: false,
    default_permissions: {
      contents: "write",
      metadata: "read",
      pull_requests: "write",
      checks: "write",
      issues: "write",
    },
    default_events: [] as string[],
  };

  return { manifest: JSON.stringify(manifest), baseUrl };
}

export default function Setup({ loaderData }: Route.ComponentProps) {
  const { manifest } = loaderData;

  return (
    <div className="flex min-h-screen items-center justify-center bg-page">
      <div className="w-full max-w-md rounded-lg border border-line-strong bg-panel p-8 shadow-sm">
        <h1 className="text-xl font-semibold text-fg">Set up Arc</h1>
        <p className="mt-2 text-sm text-fg-muted">
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
            className="w-full rounded-md bg-teal-600 px-4 py-2 text-sm font-medium text-white hover:bg-teal-500 focus:outline-2 focus:outline-offset-2 focus:outline-teal-500"
          >
            Register GitHub App
          </button>
        </form>
      </div>
    </div>
  );
}
