import type { ServerSettings } from "@qltysh/fabro-api-client";
import { useServerSettings } from "../lib/queries";
import {
  Badge,
  Muted,
  Panel,
  PanelSkeleton,
  Row,
  SettingsPageIntro,
  UsernameList,
} from "../components/settings-panel";

export function meta() {
  return [{ title: "Security — Fabro" }];
}

const DESCRIPTION = (
  <>
    Authentication methods and permitted GitHub or GitLab identities. Edit via{" "}
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
  const { auth } = settings.server;
  const githubUsers = auth.github.allowed_usernames;
  const gitlabEnabled = auth.methods.includes("gitlab");
  const gitlabUsers = auth.gitlab.allowed_usernames;
  const gitlabGroups = auth.gitlab.allowed_groups;
  const gitlabAllowlistsEmpty =
    gitlabUsers.length === 0 && gitlabGroups.length === 0;

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
      {gitlabEnabled ? (
        <>
          <Row
            title="GitLab allowed usernames"
            help="GitLab usernames permitted to authenticate."
          >
            {gitlabUsers.length === 0 ? (
              <Muted>
                {gitlabAllowlistsEmpty
                  ? "No GitLab users can authenticate until an allowed username or group is configured."
                  : "None"}
              </Muted>
            ) : (
              <UsernameList names={gitlabUsers} />
            )}
          </Row>
          <Row
            title="GitLab allowed groups"
            help="GitLab groups permitted to authenticate."
          >
            {gitlabGroups.length === 0 ? (
              <Muted>None</Muted>
            ) : (
              <UsernameList names={gitlabGroups} />
            )}
          </Row>
        </>
      ) : null}
    </Panel>
  );
}
