import { useState } from "react";
import type { ReactNode } from "react";
import type {
  IntegrationStatus,
  SystemIntegrationStatus,
} from "@qltysh/fabro-api-client";
import { useSystemIntegrations } from "../lib/queries";
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
    External services connected to this server, computed from runtime
    configuration, vault credentials, and live connection state.
  </>
);

export default function SettingsIntegrations() {
  const integrationsQuery = useSystemIntegrations();
  const integrations = integrationsQuery.data?.data;
  const github = integrations?.find((status) => status.provider === "github");
  const slack = integrations?.find((status) => status.provider === "slack");

  return (
    <div className="space-y-6">
      <SettingsPageIntro description={DESCRIPTION} />
      {github && slack ? (
        <>
          <GithubPanel status={github} />
          <SlackPanel status={slack} />
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

function GithubPanel({ status }: { status: SystemIntegrationStatus }) {
  return (
    <Panel title="Version Control">
      <IntegrationRow
        slug="github"
        name="GitHub"
        help="App for repo access, checks, and PR automation."
      >
        <IntegrationValue status={status} detail={githubDetail(status)} />
      </IntegrationRow>
    </Panel>
  );
}

function SlackPanel({ status }: { status: SystemIntegrationStatus }) {
  return (
    <Panel title="Communication">
      <IntegrationRow
        slug="slack"
        name="Slack"
        help="Workspace app for run notifications and approvals."
      >
        <IntegrationValue status={status} detail={slackDetail(status)} />
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
  status,
  detail,
}: {
  status: SystemIntegrationStatus;
  detail?: string;
}) {
  const label = status.status === "disabled" ? null : statusLabel(status.status);
  return (
    <span className="inline-flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
      <Toggle on={status.enabled} />
      {label ? (
        <RuntimeStatusLabel status={status.status}>{label}</RuntimeStatusLabel>
      ) : null}
      {detail ? (
        <span
          className="min-w-0 max-w-full truncate font-mono text-xs text-fg-3"
          title={detail}
        >
          {detail}
        </span>
      ) : null}
    </span>
  );
}

function RuntimeStatusLabel({
  status,
  children,
}: {
  status: IntegrationStatus;
  children: ReactNode;
}) {
  const tone =
    status === "error" || status === "missing_credentials"
      ? "text-amber-500"
      : status === "connected"
        ? "text-emerald-500"
        : "text-fg-2";

  return <span className={tone}>{children}</span>;
}

function statusLabel(status: IntegrationStatus): string {
  switch (status) {
    case "connected":
      return "Connected";
    case "connecting":
      return "Connecting";
    case "configured":
      return "Configured";
    case "error":
      return "Error";
    case "missing_credentials":
      return "Missing credentials";
    case "disabled":
      return "Disabled";
  }
}

function githubDetail(status: SystemIntegrationStatus): string | undefined {
  const metadata = status.metadata ?? {};
  if (metadata.slug) return `app: ${metadata.slug}`;
  if (metadata.app_id) return `app id: ${metadata.app_id}`;
  if (metadata.strategy) return `strategy: ${metadata.strategy}`;
  return missingCredentialsDetail(status);
}

function slackDetail(status: SystemIntegrationStatus): string | undefined {
  if (status.connection?.last_error) return status.connection.last_error;
  const missing = missingCredentialsDetail(status);
  if (missing) return missing;
  const defaultChannel = status.metadata?.default_channel;
  if (defaultChannel) return `channel: ${defaultChannel}`;
  if (status.connection?.kind === "socket_mode") return "socket mode";
  return undefined;
}

function missingCredentialsDetail(
  status: SystemIntegrationStatus,
): string | undefined {
  if (status.missing_credentials.length === 0) return undefined;
  return `missing: ${status.missing_credentials.join(", ")}`;
}
