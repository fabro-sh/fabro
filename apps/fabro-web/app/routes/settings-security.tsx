import type { ServerSettings } from "@qltysh/fabro-api-client";
import { useServerSettings } from "../lib/queries";
import {
  Badge,
  Count,
  Muted,
  Panel,
  PanelSkeleton,
  Row,
  SettingsPageIntro,
  UsernameList,
  plural,
} from "../components/settings-panel";

export function meta() {
  return [{ title: "Security — Fabro" }];
}

const DESCRIPTION = (
  <>
    Authentication methods and network allowlist. Edit via{" "}
    <code className="font-mono text-fg-2">settings.toml</code>; changes take
    effect on the next server restart.
  </>
);

export default function SettingsSecurity() {
  const settingsQuery = useServerSettings();
  const settings = settingsQuery.data;

  return (
    <div className="space-y-6">
      <SettingsPageIntro description={DESCRIPTION} />
      {settings ? <SecurityPanel settings={settings} /> : <PanelSkeleton />}
    </div>
  );
}

function SecurityPanel({ settings }: { settings: ServerSettings }) {
  const { auth, ip_allowlist } = settings.server;
  const githubUsers = auth.github.allowed_usernames;
  return (
    <Panel title="Security">
      <Row title="Auth methods" help="How users may sign in to this server.">
        {auth.methods.length === 0 ? (
          <Muted>None configured</Muted>
        ) : (
          <div className="flex flex-wrap gap-1.5">
            {auth.methods.map((m) => (
              <Badge key={m}>{m}</Badge>
            ))}
          </div>
        )}
      </Row>
      <Row
        title="Allowed usernames"
        help="GitHub usernames permitted to authenticate."
      >
        {githubUsers.length === 0 ? (
          <Muted>Anyone</Muted>
        ) : (
          <UsernameList names={githubUsers} />
        )}
      </Row>
      <Row title="IP allowlist" help="Network sources permitted to reach the API.">
        <Count
          n={ip_allowlist.entries.length}
          singular="entry"
          plural="entries"
          suffix={
            ip_allowlist.trusted_proxy_count > 0
              ? `· ${ip_allowlist.trusted_proxy_count} trusted ${plural(ip_allowlist.trusted_proxy_count, "proxy", "proxies")}`
              : undefined
          }
        />
      </Row>
    </Panel>
  );
}
