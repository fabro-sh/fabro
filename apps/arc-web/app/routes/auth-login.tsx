import { redirect } from "react-router";
import { getGitHubOAuth, generateState } from "../lib/github.server";
import type { Route } from "./+types/auth-login";

export function action({ request }: Route.ActionArgs) {
  const github = getGitHubOAuth();
  const state = generateState();
  const authUrl = github.createAuthorizationURL(state, ["read:user", "user:email"]);

  return redirect(authUrl.toString(), {
    headers: {
      "Set-Cookie": `arc_oauth_state=${state}; HttpOnly; Path=/; Max-Age=600; SameSite=Lax`,
    },
  });
}

export default function AuthLogin() {
  return (
    <div className="flex min-h-screen items-center justify-center bg-page">
      <div className="w-full max-w-md rounded-lg border border-line-strong bg-panel p-8 shadow-sm">
        <h1 className="text-xl font-semibold text-fg">Sign in to Arc</h1>
        <p className="mt-2 text-sm text-fg-muted">
          Authenticate with your GitHub account to continue.
        </p>
        <form method="POST" className="mt-6">
          <button
            type="submit"
            className="w-full rounded-md bg-teal-600 px-4 py-2 text-sm font-medium text-white hover:bg-teal-500 focus:outline-2 focus:outline-offset-2 focus:outline-teal-500"
          >
            Sign in with GitHub
          </button>
        </form>
      </div>
    </div>
  );
}
