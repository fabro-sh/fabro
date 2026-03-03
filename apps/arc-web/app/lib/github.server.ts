import { GitHub, generateState } from "arctic";

export { generateState };

export function getGitHubOAuth() {
  const clientId = process.env.GITHUB_APP_CLIENT_ID;
  const clientSecret = process.env.GITHUB_APP_CLIENT_SECRET;
  if (!clientId || !clientSecret) {
    throw new Error("GitHub App is not configured");
  }
  return new GitHub(clientId, clientSecret, null);
}

export function isGitHubAppConfigured(): boolean {
  return !!process.env.GITHUB_APP_CLIENT_ID;
}
