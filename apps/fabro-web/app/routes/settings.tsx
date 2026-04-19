import { apiJson } from "../api";
import { CollapsibleFile } from "../components/collapsible-file";

/**
 * Opaque server settings payload returned by `/api/v1/settings`. Mirrors the
 * v2 `SettingsFile` shape with secret-bearing subtrees dropped before
 * serialization. The UI only renders it as JSON.
 */
type ServerSettings = Record<string, unknown>;

export function meta({}: any) {
  return [{ title: "Settings — Fabro" }];
}

export const handle = { hideHeader: true };

export async function loader({ request }: any) {
  const settings = await apiJson<ServerSettings>("/settings", { request });
  return { settings };
}

export default function Settings({ loaderData }: any) {
  const { settings } = loaderData;

  return (
    <div className="mx-auto max-w-4xl">
      <header className="mb-6">
        <h1 className="text-2xl font-semibold tracking-tight text-fg">
          Settings
        </h1>
        <p className="mt-2 max-w-[60ch] text-sm/6 text-fg-3 text-pretty">
          Redacted snapshot of the server configuration. Edit values with the
          Fabro CLI; changes take effect on the next server restart.
        </p>
      </header>
      <CollapsibleFile
        file={{ name: "server.json", contents: JSON.stringify(settings, null, 2), lang: "json" }}
      />
    </div>
  );
}
