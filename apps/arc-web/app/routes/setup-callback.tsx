import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { randomBytes } from "node:crypto";
import { redirect } from "react-router";
import type { Route } from "./+types/setup-callback";

const ENV_PATH = resolve(import.meta.dirname, "../../../../.env");

export async function loader({ request }: Route.LoaderArgs) {
  const url = new URL(request.url);
  const code = url.searchParams.get("code");
  if (!code) {
    return { error: "Missing code parameter" };
  }

  const response = await fetch(
    `https://api.github.com/app-manifests/${code}/conversions`,
    { method: "POST", headers: { Accept: "application/vnd.github+json" } }
  );

  if (!response.ok) {
    const body = await response.text();
    return { error: `GitHub API error: ${response.status} ${body}` };
  }

  const data = (await response.json()) as {
    id: number;
    client_id: string;
    client_secret: string;
    webhook_secret: string;
    pem: string;
  };

  const sessionSecret = randomBytes(32).toString("hex");

  let existing = "";
  try {
    existing = await readFile(ENV_PATH, "utf-8");
  } catch {
    // file doesn't exist yet
  }

  const newVars = [
    `export SESSION_SECRET=${sessionSecret}`,
    `export GITHUB_APP_ID=${data.id}`,
    `export GITHUB_APP_CLIENT_ID=${data.client_id}`,
    `export GITHUB_APP_CLIENT_SECRET=${data.client_secret}`,
    `export GITHUB_APP_WEBHOOK_SECRET=${data.webhook_secret}`,
    `export GITHUB_APP_PRIVATE_KEY=${Buffer.from(data.pem).toString("base64")}`,
  ].join("\n");

  const envContent = existing ? `${existing.trimEnd()}\n\n${newVars}\n` : `${newVars}\n`;
  await writeFile(ENV_PATH, envContent, "utf-8");

  process.env.SESSION_SECRET = sessionSecret;
  process.env.GITHUB_APP_ID = String(data.id);
  process.env.GITHUB_APP_CLIENT_ID = data.client_id;
  process.env.GITHUB_APP_CLIENT_SECRET = data.client_secret;
  process.env.GITHUB_APP_WEBHOOK_SECRET = data.webhook_secret;
  process.env.GITHUB_APP_PRIVATE_KEY = Buffer.from(data.pem).toString("base64");

  throw redirect("/auth/login");
}

export default function SetupCallback({ loaderData }: Route.ComponentProps) {
  const { error } = loaderData;

  return (
    <div className="flex min-h-screen flex-col items-center justify-center bg-atmosphere px-4">
      <div className="w-full max-w-sm">
        <div className="mb-8 flex justify-center">
          <img src="/logo.svg" alt="Arc" className="h-12 w-12" draggable={false} />
        </div>
        <div className="rounded-xl border border-line bg-panel/80 p-8 shadow-lg backdrop-blur-sm">
          <div className="mx-auto mb-4 flex h-10 w-10 items-center justify-center rounded-full bg-coral/10">
            <svg width="20" height="20" viewBox="0 0 20 20" fill="none" className="text-coral">
              <path d="M7 7L13 13M13 7L7 13" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
            </svg>
          </div>
          <h1 className="text-center text-lg font-semibold text-fg">
            Setup failed
          </h1>
          <p className="mt-2 text-center text-sm text-fg-3">{error}</p>
          <a
            href="/setup"
            className="mt-6 flex w-full items-center justify-center rounded-lg border border-line-strong px-4 py-2.5 text-sm font-medium text-fg-2 transition-colors hover:bg-overlay-strong"
          >
            Try again
          </a>
        </div>
      </div>
    </div>
  );
}
