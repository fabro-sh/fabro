import { useState } from "react";
import type {
  ServerListenSettings,
  ServerSettings,
} from "@qltysh/fabro-api-client";
import { useServerSettings } from "../lib/queries";
import { CollapsibleFile } from "../components/collapsible-file";
import {
  Badge,
  Count,
  Mono,
  Muted,
  NumberValue,
  ObjectStoreValue,
  Panel,
  PanelSkeleton,
  Row,
  type SettingsView,
  Toggle,
  UrlValue,
  UsernameList,
  ViewToggle,
  plural,
} from "../components/settings-panel";

export function meta({}: any) {
  return [{ title: "Settings — Fabro" }];
}

export default function Settings() {
  const settingsQuery = useServerSettings();
  const settings = settingsQuery.data;
  const [view, setView] = useState<SettingsView>("settings");

  if (!settings) {
    return (
      <div className="space-y-6">
        <PageIntro view={view} setView={setView} />
        <PanelSkeleton />
        <PanelSkeleton />
        <PanelSkeleton />
        <PanelSkeleton />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <PageIntro view={view} setView={setView} />
      {view === "settings" ? (
        <>
          <ServerPanel settings={settings} />
          <DataPanel settings={settings} />
          <SecurityPanel settings={settings} />
          <IntegrationsPanel settings={settings} />
        </>
      ) : (
        <CollapsibleFile
          file={{
            name: "server-settings.json",
            contents: JSON.stringify(settings, null, 2),
            lang: "json",
          }}
        />
      )}
    </div>
  );
}

function PageIntro({
  view,
  setView,
}: {
  view: SettingsView;
  setView: (v: SettingsView) => void;
}) {
  return (
    <div className="flex items-start justify-between gap-6">
      <p className="max-w-[64ch] text-sm/6 text-fg-3 text-pretty">
        Snapshot of the server configuration. Edit via{" "}
        <code className="font-mono text-fg-2">settings.toml</code>; changes take
        effect on the next server restart.
      </p>
      <ViewToggle view={view} setView={setView} />
    </div>
  );
}

function ServerPanel({ settings }: { settings: ServerSettings }) {
  const { listen, web, api, scheduler } = settings.server;
  return (
    <Panel title="Server">
      <Row title="Web URL" help="Public URL for the browser UI.">
        {web.enabled ? <UrlValue url={web.url} /> : <Toggle on={false} />}
      </Row>
      <Row title="API URL" help="Base URL advertised to API clients.">
        {api.url ? <UrlValue url={api.url} /> : <Muted>Same origin</Muted>}
      </Row>
      <Row title="Listen" help="Address the API server is bound to.">
        <ListenValue listen={listen} />
      </Row>
      <Row title="Max concurrent runs" help="Scheduler ceiling on simultaneous runs.">
        <NumberValue value={scheduler.max_concurrent_runs} />
      </Row>
    </Panel>
  );
}

function DataPanel({ settings }: { settings: ServerSettings }) {
  const { storage, slatedb, artifacts } = settings.server;
  return (
    <Panel title="Data">
      <Row title="Storage root" help="Filesystem path for run state and logs.">
        <Mono>{storage.root}</Mono>
      </Row>
      <Row title="SlateDB" help="Object store backing the embedded database.">
        <ObjectStoreValue store={slatedb.store} prefix={slatedb.prefix} />
      </Row>
      <Row title="Artifacts" help="Where run artifacts are persisted.">
        <ObjectStoreValue store={artifacts.store} prefix={artifacts.prefix} />
      </Row>
    </Panel>
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

function IntegrationsPanel({ settings }: { settings: ServerSettings }) {
  const { integrations } = settings.server;
  return (
    <Panel title="Integrations">
      <Row title="GitHub" help="App for repo access, checks, and PR automation.">
        <IntegrationValue
          enabled={integrations.github.enabled}
          detail={
            integrations.github.slug
              ? `app: ${integrations.github.slug}`
              : integrations.github.app_id
                ? `app id: ${integrations.github.app_id}`
                : undefined
          }
        />
      </Row>
    </Panel>
  );
}

function ListenValue({ listen }: { listen: ServerListenSettings }) {
  if (listen.type === "tcp") {
    return (
      <span className="inline-flex items-center gap-2">
        <Badge>tcp</Badge>
        <span className="truncate font-mono text-xs text-fg-2" title={listen.address}>
          {listen.address}
        </span>
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-2">
      <Badge>unix</Badge>
      <span className="truncate font-mono text-xs text-fg-2" title={listen.path}>
        {listen.path}
      </span>
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

