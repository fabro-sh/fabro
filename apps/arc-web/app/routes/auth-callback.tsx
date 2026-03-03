import { redirect } from "react-router";
import { getGitHubOAuth } from "../lib/github.server";
import { getSession, commitSession } from "../lib/session.server";
import type { Route } from "./+types/auth-callback";

export async function loader({ request }: Route.LoaderArgs) {
  const url = new URL(request.url);
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");

  const cookies = request.headers.get("Cookie") ?? "";
  const stateMatch = cookies.match(/arc_oauth_state=([^;]+)/);
  const storedState = stateMatch?.[1];

  if (!code || !state || state !== storedState) {
    throw redirect("/auth/login");
  }

  const github = getGitHubOAuth();
  const tokens = await github.validateAuthorizationCode(code);
  const accessToken = tokens.accessToken();

  const userResponse = await fetch("https://api.github.com/user", {
    headers: { Authorization: `Bearer ${accessToken}` },
  });
  const profile = (await userResponse.json()) as {
    login: string;
    name: string | null;
    email: string | null;
    avatar_url: string;
  };

  const session = await getSession(request);
  session.set("githubLogin", profile.login);
  session.set("name", profile.name ?? profile.login);
  session.set("email", profile.email ?? "");
  session.set("avatarUrl", profile.avatar_url);
  session.set("accessToken", accessToken);

  const headers = new Headers();
  headers.append("Set-Cookie", await commitSession(session));
  headers.append("Set-Cookie", "arc_oauth_state=; HttpOnly; Path=/; Max-Age=0");

  return redirect("/start", { headers });
}
