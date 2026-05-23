import { useState } from "react";
import type { ReactNode } from "react";
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
      <IntegrationRow
        slug="linear"
        name="Linear"
        help="Sync runs with Linear issues and projects."
      >
        <span className="text-sm text-fg-muted">Coming Soon</span>
      </IntegrationRow>
      <IntegrationRow
        slug="jira"
        name="Jira"
        help="Sync runs with Jira issues and projects."
      >
        <span className="text-sm text-fg-muted">Coming Soon</span>
      </IntegrationRow>
    </Panel>
  );
}

function GithubPanel({ settings }: { settings: ServerSettings }) {
  const { github } = settings.server.integrations;
  return (
    <Panel title="Version Control">
      <IntegrationRow
        slug="github"
        name="GitHub"
        help="App for repo access, checks, and PR automation."
      >
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
      </IntegrationRow>
    </Panel>
  );
}

function SlackPanel({ settings }: { settings: ServerSettings }) {
  const { slack } = settings.server.integrations;
  return (
    <Panel title="Communication">
      <IntegrationRow
        slug="slack"
        name="Slack"
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
      </IntegrationRow>
      <IntegrationRow
        slug="microsoft-teams"
        name="Microsoft Teams"
        help="Channel app for run notifications and approvals."
      >
        <span className="text-sm text-fg-muted">Coming Soon</span>
      </IntegrationRow>
      <IntegrationRow
        slug="discord"
        name="Discord"
        help="Server app for run notifications and approvals."
      >
        <span className="text-sm text-fg-muted">Coming Soon</span>
      </IntegrationRow>
    </Panel>
  );
}

function IntegrationRow({
  slug,
  name,
  help,
  children,
}: {
  slug: string;
  name: string;
  help: string;
  children: ReactNode;
}) {
  return (
    <Row
      title={
        <span className="flex items-center gap-4">
          <IntegrationLogo slug={slug} name={name} />
          <span className="flex min-w-0 flex-col">
            <span className="text-sm text-fg-2">{name}</span>
            <span className="text-xs/5 text-fg-3">{help}</span>
          </span>
        </span>
      }
    >
      {children}
    </Row>
  );
}

function IntegrationLogo({ slug, name }: { slug: string; name: string }) {
  const [failed, setFailed] = useState(false);
  const chip =
    "grid size-10 shrink-0 place-items-center rounded-md bg-ice-50 ring-1 ring-line-strong";

  if (failed) {
    const initial = name.charAt(0).toUpperCase() || "?";
    return (
      <span className={`${chip} text-base font-medium text-page`}>
        {initial}
      </span>
    );
  }

  return (
    <span className={chip}>
      <img
        alt=""
        src={`/images/integrations/${slug}.svg`}
        onError={() => setFailed(true)}
        className="size-7"
      />
    </span>
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
