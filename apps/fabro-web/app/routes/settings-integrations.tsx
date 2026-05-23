import type { ServerSettings } from "@qltysh/fabro-api-client";
import { useServerSettings } from "../lib/queries";
import {
  Panel,
  PanelSkeleton,
  Row,
  SettingsPageIntro,
  Toggle,
} from "../components/settings-panel";

export function meta() {
  return [{ title: "Integrations — Fabro" }];
}

const DESCRIPTION = (
  <>
    External services connected to this server. Edit via{" "}
    <code className="font-mono text-fg-2">settings.toml</code>; changes take
    effect on the next server restart.
  </>
);

export default function SettingsIntegrations() {
  const settingsQuery = useServerSettings();
  const settings = settingsQuery.data;

  return (
    <div className="space-y-6">
      <SettingsPageIntro description={DESCRIPTION} />
      {settings ? (
        <>
          <GithubPanel settings={settings} />
          <SlackPanel settings={settings} />
        </>
      ) : (
        <>
          <PanelSkeleton />
          <PanelSkeleton />
        </>
      )}
      <ProjectManagementPanel />
    </div>
  );
}

function ProjectManagementPanel() {
  return (
    <Panel title="Project Management">
      <Row title="Linear" help="Sync runs with Linear issues and projects.">
        <span className="text-sm text-fg-muted">Coming Soon</span>
      </Row>
      <Row title="Jira" help="Sync runs with Jira issues and projects.">
        <span className="text-sm text-fg-muted">Coming Soon</span>
      </Row>
    </Panel>
  );
}

function GithubPanel({ settings }: { settings: ServerSettings }) {
  const { github } = settings.server.integrations;
  return (
    <Panel title="Version Control">
      <Row title="GitHub" help="App for repo access, checks, and PR automation.">
        <IntegrationValue
          enabled={github.enabled}
          detail={
            github.slug
              ? `app: ${github.slug}`
              : github.app_id
                ? `app id: ${github.app_id}`
                : undefined
          }
        />
      </Row>
    </Panel>
  );
}

function SlackPanel({ settings }: { settings: ServerSettings }) {
  const { slack } = settings.server.integrations;
  return (
    <Panel title="Communication">
      <Row
        title="Slack"
        help="Workspace app for run notifications and approvals."
      >
        <IntegrationValue
          enabled={slack.enabled}
          detail={
            slack.default_channel
              ? `channel: ${slack.default_channel}`
              : undefined
          }
        />
      </Row>
      <Row
        title="Microsoft Teams"
        help="Channel app for run notifications and approvals."
      >
        <span className="text-sm text-fg-muted">Coming Soon</span>
      </Row>
      <Row
        title="Discord"
        help="Server app for run notifications and approvals."
      >
        <span className="text-sm text-fg-muted">Coming Soon</span>
      </Row>
    </Panel>
  );
}

function IntegrationValue({
  enabled,
  detail,
}: {
  enabled: boolean;
  detail?: string;
}) {
  if (!enabled) return <Toggle on={false} />;
  return (
    <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">
      <Toggle on={true} />
      {detail ? (
        <span className="font-mono text-xs text-fg-3">{detail}</span>
      ) : null}
    </span>
  );
}
