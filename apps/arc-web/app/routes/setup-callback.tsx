import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { randomBytes } from "node:crypto";
import type { Route } from "./+types/setup-callback";

const ENV_PATH = resolve(import.meta.dirname, "../../../../.env");

export async function loader({ request }: Route.LoaderArgs) {
  const url = new URL(request.url);
  const code = url.searchParams.get("code");
  if (!code) {
    return { success: false, error: "Missing code parameter" };
  }

  const response = await fetch(
    `https://api.github.com/app-manifests/${code}/conversions`,
    { method: "POST", headers: { Accept: "application/vnd.github+json" } }
  );

  if (!response.ok) {
    const body = await response.text();
    return {
      success: false,
      error: `GitHub API error: ${response.status} ${body}`,
    };
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
    `export GITHUB_APP_PRIVATE_KEY="${data.pem}"`,
  ].join("\n");

  const envContent = existing ? `${existing.trimEnd()}\n\n${newVars}\n` : `${newVars}\n`;
  await writeFile(ENV_PATH, envContent, "utf-8");

  return { success: true, error: null };
}

export default function SetupCallback({ loaderData }: Route.ComponentProps) {
  const { success, error } = loaderData;

  return (
    <div className="flex min-h-screen items-center justify-center bg-page">
      <div className="w-full max-w-md rounded-lg border border-line-strong bg-panel p-8 shadow-sm">
        {success ? (
          <>
            <h1 className="text-xl font-semibold text-fg">
              GitHub App registered
            </h1>
            <p className="mt-2 text-sm text-fg-muted">
              Credentials have been written to <code>.env</code>. Restart the
              app to pick up the new configuration.
            </p>
          </>
        ) : (
          <>
            <h1 className="text-xl font-semibold text-red-500">
              Setup failed
            </h1>
            <p className="mt-2 text-sm text-fg-muted">{error}</p>
          </>
        )}
      </div>
    </div>
  );
}
