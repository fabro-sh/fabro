import { useState, type ReactNode } from "react";
import type {
  ObjectStoreSettings,
  ServerListenSettings,
  ServerSettings,
} from "@qltysh/fabro-api-client";
import { useServerSettings } from "../lib/queries";
import { CollapsibleFile } from "../components/collapsible-file";

export function meta({}: any) {
  return [{ title: "Settings — Fabro" }];
}

type View = "settings" | "json";

export default function Settings() {
  const settingsQuery = useServerSettings();
  const settings = settingsQuery.data;
  const [view, setView] = useState<View>("settings");

  if (!settings) {
    return (
      <div className="space-y-6">
        <PageIntro view={view} setView={setView} />
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
          <AccessPanel settings={settings} />
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

function PageIntro({ view, setView }: { view: View; setView: (v: View) => void }) {
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

function ViewToggle({ view, setView }: { view: View; setView: (v: View) => void }) {
  const btn = "rounded px-3 py-1.5 text-xs font-medium transition-colors";
  return (
    <div
      role="group"
      aria-label="Settings view"
      className="inline-flex shrink-0 rounded-md border border-line bg-panel/80 p-0.5"
    >
      <button
        type="button"
        onClick={() => setView("settings")}
        aria-pressed={view === "settings"}
        className={`${btn} ${view === "settings" ? "bg-overlay text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
      >
        Settings
      </button>
      <button
        type="button"
        onClick={() => setView("json")}
        aria-pressed={view === "json"}
        className={`${btn} ${view === "json" ? "bg-overlay text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
      >
        JSON
      </button>
    </div>
  );
}

function ServerPanel({ settings }: { settings: ServerSettings }) {
  const { listen, web, api, storage } = settings.server;
  return (
    <Panel title="Server">
      <Row title="Listen" help="Address the API server is bound to.">
        <ListenValue listen={listen} />
      </Row>
      <Row title="Web" help="Public URL for the browser UI.">
        {web.enabled ? <UrlValue url={web.url} /> : <Toggle on={false} />}
      </Row>
      <Row title="API" help="Base URL advertised to API clients.">
        {api.url ? <UrlValue url={api.url} /> : <Muted>Same origin</Muted>}
      </Row>
      <Row title="Storage root" help="Filesystem path for run state and logs.">
        <Mono>{storage.root}</Mono>
      </Row>
    </Panel>
  );
}

function AccessPanel({ settings }: { settings: ServerSettings }) {
  const { auth, ip_allowlist, scheduler } = settings.server;
  const githubUsers = auth.github.allowed_usernames;
  return (
    <Panel title="Access & Capacity">
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
        title="GitHub allowlist"
        help="Usernames permitted to authenticate via GitHub."
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
      <Row title="Max concurrent runs" help="Scheduler ceiling on simultaneous runs.">
        <Number value={scheduler.max_concurrent_runs} />
      </Row>
    </Panel>
  );
}

function IntegrationsPanel({ settings }: { settings: ServerSettings }) {
  const { integrations, artifacts } = settings.server;
  return (
    <Panel title="Integrations & Artifacts">
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
      <Row title="Artifacts store" help="Where run artifacts are persisted.">
        <ObjectStoreValue store={artifacts.store} prefix={artifacts.prefix} />
      </Row>
    </Panel>
  );
}

function Panel({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="overflow-hidden rounded-md border border-line bg-panel/40">
      <header className="border-b border-line bg-overlay px-4 py-2.5">
        <h2 className="text-xs font-medium uppercase tracking-wider text-fg-muted">
          {title}
        </h2>
      </header>
      <div className="divide-y divide-line">{children}</div>
    </section>
  );
}

function PanelSkeleton() {
  return (
    <div className="overflow-hidden rounded-md border border-line bg-panel/40">
      <div className="h-10 border-b border-line bg-overlay" />
      <div className="space-y-4 px-4 py-6">
        <div className="h-3 w-40 rounded bg-overlay-strong" />
        <div className="h-3 w-64 rounded bg-overlay" />
        <div className="h-3 w-52 rounded bg-overlay" />
      </div>
    </div>
  );
}

function Row({
  title,
  help,
  children,
}: {
  title: string;
  help?: string;
  children: ReactNode;
}) {
  return (
    <div className="grid grid-cols-[minmax(0,5fr)_minmax(0,7fr)] items-start gap-x-6 gap-y-1 px-4 py-3.5">
      <div className="min-w-0">
        <div className="text-sm text-fg-2">{title}</div>
        {help ? (
          <div className="mt-0.5 text-xs/5 text-fg-3 text-pretty">{help}</div>
        ) : null}
      </div>
      <div className="min-w-0 self-center text-sm text-fg">{children}</div>
    </div>
  );
}

function Mono({ children }: { children: ReactNode }) {
  return (
    <div className="truncate font-mono text-xs text-fg-2" title={typeof children === "string" ? children : undefined}>
      {children}
    </div>
  );
}

function Muted({ children }: { children: ReactNode }) {
  return <span className="text-fg-muted">{children}</span>;
}

function Badge({ children }: { children: ReactNode }) {
  return (
    <span className="inline-flex items-center rounded-sm bg-overlay-strong px-1.5 py-0.5 font-mono text-[11px] text-fg-2">
      {children}
    </span>
  );
}

function Number({ value }: { value: number }) {
  return <span className="font-mono tabular-nums text-fg">{value}</span>;
}

function Dot({ on }: { on: boolean }) {
  return (
    <span
      className={`size-1.5 rounded-full ${on ? "bg-emerald-400" : "bg-fg-muted"}`}
      aria-hidden="true"
    />
  );
}

function Toggle({ on }: { on: boolean }) {
  return (
    <span className="inline-flex items-center gap-2">
      <Dot on={on} />
      <span className={on ? "text-fg" : "text-fg-muted"}>
        {on ? "Enabled" : "Disabled"}
      </span>
    </span>
  );
}

function UrlValue({ url }: { url: string }) {
  return (
    <a
      href={url}
      target="_blank"
      rel="noreferrer"
      className="truncate font-mono text-xs text-fg-2 hover:text-fg hover:underline"
      title={url}
    >
      {url}
    </a>
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

function ObjectStoreValue({
  store,
  prefix,
}: {
  store: ObjectStoreSettings;
  prefix: string;
}) {
  if (store.type === "s3") {
    const target = `s3://${store.bucket}${prefix ? `/${prefix}` : ""}`;
    return (
      <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">
        <Badge>s3</Badge>
        <span className="truncate font-mono text-xs text-fg-2" title={target}>
          {target}
        </span>
        <span className="font-mono text-[11px] text-fg-muted">
          {store.region}
        </span>
      </span>
    );
  }
  const target = `${store.root}${prefix ? `/${prefix}` : ""}`;
  return (
    <span className="inline-flex items-center gap-2">
      <Badge>local</Badge>
      <span className="truncate font-mono text-xs text-fg-2" title={target}>
        {target}
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

function UsernameList({ names }: { names: string[] }) {
  const visible = names.slice(0, 3);
  const remaining = names.length - visible.length;
  return (
    <span className="inline-flex flex-wrap items-center gap-1.5">
      {visible.map((n) => (
        <Badge key={n}>{n}</Badge>
      ))}
      {remaining > 0 ? (
        <span className="text-xs text-fg-muted">+{remaining} more</span>
      ) : null}
    </span>
  );
}

function Count({
  n,
  singular,
  plural: pluralLabel,
  suffix,
}: {
  n: number;
  singular: string;
  plural: string;
  suffix?: string;
}) {
  if (n === 0) return <Muted>None</Muted>;
  return (
    <span className="text-fg-2">
      <span className="font-mono tabular-nums text-fg">{n}</span>{" "}
      {n === 1 ? singular : pluralLabel}
      {suffix ? <span className="ml-1 text-fg-muted">{suffix}</span> : null}
    </span>
  );
}

function plural(n: number, singular: string, pluralForm: string) {
  return n === 1 ? singular : pluralForm;
}
