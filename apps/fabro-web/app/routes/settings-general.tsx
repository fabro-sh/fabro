import { useState } from "react";
import type {
  ServerListenSettings,
  ServerSettings,
  SystemInfoResponse,
} from "@qltysh/fabro-api-client";
import { formatDurationSecs } from "../lib/format";
import { useServerSettings, useSystemInfo } from "../lib/queries";
import { CollapsibleFile } from "../components/collapsible-file";
import {
  Badge,
  Muted,
  NumberValue,
  Panel,
  PanelSkeleton,
  Row,
  SettingsPageIntro,
  type SettingsView,
  Toggle,
  UrlValue,
} from "../components/settings-panel";

export function meta() {
  return [{ title: "Server settings — Fabro" }];
}

const DESCRIPTION = (
  <>
    Server URLs, listen address, and scheduler limits. Edit via{" "}
    <code className="font-mono text-fg-2">settings.toml</code>; changes take
    effect on the next server restart.
  </>
);

export default function SettingsGeneral() {
  const settingsQuery = useServerSettings();
  const settings = settingsQuery.data;
  const [view, setView] = useState<SettingsView>("settings");

  if (!settings) {
    return (
      <div className="space-y-6">
        <SettingsPageIntro description={DESCRIPTION} view={view} setView={setView} />
        <PanelSkeleton />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SettingsPageIntro description={DESCRIPTION} view={view} setView={setView} />
      {view === "settings" ? (
        <>
          <ServerPanel settings={settings} />
          <SystemPanel />
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

function SystemPanel() {
  const infoQuery = useSystemInfo();
  const info = infoQuery.data;
  if (!info) {
    return <PanelSkeleton />;
  }
  return (
    <Panel title="System">
      <Row title="Fabro version" help="Version of the running Fabro server.">
        {info.version ? info.version : <Muted>Unknown</Muted>}
      </Row>
      <Row
        title="Operating system"
        help="Operating system and CPU architecture reported by the server binary."
      >
        <OperatingSystemValue info={info} />
      </Row>
      <Row title="Uptime" help="Elapsed time since this server process started.">
        {info.uptime_secs != null ? (
          formatDurationSecs(info.uptime_secs)
        ) : (
          <Muted>Unknown</Muted>
        )}
      </Row>
    </Panel>
  );
}

function OperatingSystemValue({ info }: { info: SystemInfoResponse }) {
  return (
    <span className="inline-flex items-center gap-2">
      <Badge>{info.os ?? "unknown"}</Badge>
      <Badge>{info.arch ?? "unknown"}</Badge>
    </span>
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
