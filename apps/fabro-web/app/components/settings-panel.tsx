import type { ReactNode } from "react";
import type { ObjectStoreSettings } from "@qltysh/fabro-api-client";

export type SettingsView = "settings" | "json";

export function Panel({ title, children }: { title: string; children: ReactNode }) {
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

export function PanelSkeleton() {
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

export function Row({
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

export function ViewToggle({
  view,
  setView,
}: {
  view: SettingsView;
  setView: (v: SettingsView) => void;
}) {
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

export function Mono({ children }: { children: ReactNode }) {
  return (
    <div
      className="truncate font-mono text-xs text-fg-2"
      title={typeof children === "string" ? children : undefined}
    >
      {children}
    </div>
  );
}

export function Muted({ children }: { children: ReactNode }) {
  return <span className="text-fg-muted">{children}</span>;
}

export function Badge({ children }: { children: ReactNode }) {
  return (
    <span className="inline-flex items-center rounded-sm bg-overlay-strong px-1.5 py-0.5 font-mono text-[11px] text-fg-2">
      {children}
    </span>
  );
}

export function NumberValue({ value }: { value: number }) {
  return <span className="font-mono tabular-nums text-fg">{value}</span>;
}

export function Dot({ on }: { on: boolean }) {
  return (
    <span
      className={`size-1.5 rounded-full ${on ? "bg-emerald-400" : "bg-fg-muted"}`}
      aria-hidden="true"
    />
  );
}

export function Toggle({ on }: { on: boolean }) {
  return (
    <span className="inline-flex items-center gap-2">
      <Dot on={on} />
      <span className={on ? "text-fg" : "text-fg-muted"}>
        {on ? "Enabled" : "Disabled"}
      </span>
    </span>
  );
}

export function UrlValue({ url }: { url: string }) {
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

export function ObjectStoreValue({
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
        <span className="font-mono text-[11px] text-fg-muted">{store.region}</span>
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

export function UsernameList({ names }: { names: string[] }) {
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

export function Count({
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

export function plural(n: number, singular: string, pluralForm: string) {
  return n === 1 ? singular : pluralForm;
}
