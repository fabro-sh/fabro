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
      <CollapsibleFile
        file={{ name: "server.json", contents: JSON.stringify(settings, null, 2), lang: "json" }}
      />
    </div>
  );
}
