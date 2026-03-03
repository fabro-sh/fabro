import { redirect } from "react-router";
import { isGitHubAppConfigured } from "../lib/github.server";
import { getUser } from "../lib/session.server";
import type { Route } from "./+types/redirect-home";

export async function loader({ request }: Route.LoaderArgs) {
  if (!isGitHubAppConfigured()) {
    return redirect("/setup");
  }
  const user = await getUser(request);
  if (!user) {
    return redirect("/auth/login");
  }
  return redirect("/start");
}
