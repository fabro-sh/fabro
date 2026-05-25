import { useMemo, useState } from "react";
import { Link } from "react-router";
import { ChevronDownIcon } from "@heroicons/react/16/solid";
import { ComputerDesktopIcon } from "@heroicons/react/24/outline";
import type { ServerSandboxProvidersSettings } from "@qltysh/fabro-api-client";
import { useServerSettings } from "../lib/queries";
import {
  Dot,
  Panel,
  PanelSkeleton,
  Row,
  SettingsPageIntro,
} from "../components/settings-panel";
import { plural } from "../lib/plural";

export function meta() {
  return [{ title: "Sandboxes — Fabro" }];
}

type SandboxProviderId = "local" | "docker" | "daytona";

type SandboxProvider = {
  id: SandboxProviderId;
  name: string;
  description: string;
  enabled: boolean;
  secretName?: string;
};

const DESCRIPTION =
  "Runtime environments where workflow stages execute. Configured via settings.toml.";

export default function SettingsSandboxes() {
  const query = useServerSettings();
  const settings = query.data;

  return (
    <div className="space-y-6">
      <SettingsPageIntro description={DESCRIPTION} />
      {settings ? <ProvidersPanel settings={settings.server.sandbox.providers} /> : <PanelSkeleton />}
    </div>
  );
}

function ProvidersPanel({ settings }: { settings: ServerSandboxProvidersSettings }) {
  const providers: SandboxProvider[] = useMemo(
    () => [
      {
        id: "local",
        name: "Local",
        description: "Run stages directly on the Fabro host.",
        enabled: settings.local.enabled,
      },
      {
        id: "docker",
        name: "Docker",
        description: "Run stages in isolated Docker containers on the host daemon.",
        enabled: settings.docker.enabled,
      },
      {
        id: "daytona",
        name: "Daytona",
        description: "Run stages in cloud sandboxes managed by Daytona.",
        enabled: settings.daytona.enabled,
        secretName: "DAYTONA_API_KEY",
      },
    ],
    [settings],
  );

  const { enabled, disabled } = useMemo(() => {
    const enabled: SandboxProvider[] = [];
    const disabled: SandboxProvider[] = [];
    for (const provider of providers) {
      if (provider.enabled) {
        enabled.push(provider);
      } else {
        disabled.push(provider);
      }
    }
    return { enabled, disabled };
  }, [providers]);

  const [showDisabled, setShowDisabled] = useState(false);
  const showDisabledRows = enabled.length === 0 || showDisabled;

  return (
    <Panel title="Providers">
      {enabled.map((provider) => (
        <ProviderRow key={provider.id} provider={provider} />
      ))}
      {showDisabledRows
        ? disabled.map((provider) => (
            <ProviderRow key={provider.id} provider={provider} />
          ))
        : null}
      {enabled.length > 0 && disabled.length > 0 ? (
        <button
          type="button"
          onClick={() => setShowDisabled((v) => !v)}
          aria-expanded={showDisabled}
          className="flex w-full items-center gap-1.5 px-4 py-3 text-left text-xs font-medium text-fg-muted hover:text-fg-3"
        >
          <ChevronDownIcon
            className={`size-4 h-lh shrink-0 transition-transform ${
              showDisabled ? "rotate-180" : ""
            }`}
          />
          {showDisabled ? "Hide" : "Show"} {disabled.length} disabled{" "}
          {plural(disabled.length, "provider", "providers")}
        </button>
      ) : null}
    </Panel>
  );
}

function ProviderRow({ provider }: { provider: SandboxProvider }) {
  return (
    <Row
      title={
        <span className="flex items-center gap-4">
          <ProviderLogo provider={provider} />
          <span className="flex min-w-0 flex-col">
            <span className="text-sm text-fg-2">{provider.name}</span>
            <span className="text-xs/5 text-fg-3">{provider.description}</span>
          </span>
        </span>
      }
    >
      <ProviderStatus provider={provider} />
    </Row>
  );
}

function ProviderLogo({ provider }: { provider: SandboxProvider }) {
  const [failed, setFailed] = useState(false);
  const chip =
    "grid size-10 shrink-0 place-items-center rounded-md bg-ice-50 ring-1 ring-line-strong";
  const dim = provider.enabled ? "" : "opacity-60";

  if (provider.id === "local") {
    return (
      <span className={`${chip} text-page ${dim}`}>
        <ComputerDesktopIcon className="size-6" aria-hidden="true" />
      </span>
    );
  }

  if (failed) {
    return (
      <span className={`${chip} text-base font-medium text-page ${dim}`}>
        {provider.name.charAt(0)}
      </span>
    );
  }

  return (
    <span className={`${chip} text-page ${dim}`}>
      <img
        alt=""
        src={`/images/sandboxes/${provider.id}.svg`}
        onError={() => setFailed(true)}
        className="size-7"
      />
    </span>
  );
}

function ProviderStatus({ provider }: { provider: SandboxProvider }) {
  return (
    <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">
      <span className="inline-flex items-center gap-2">
        <Dot on={provider.enabled} />
        <span className={provider.enabled ? "text-fg" : "text-fg-muted"}>
          {provider.enabled ? "Enabled" : "Disabled"}
        </span>
      </span>
      {!provider.enabled && provider.secretName ? (
        <Link
          to={`/settings/secrets/new?name=${encodeURIComponent(provider.secretName)}`}
          className="text-xs text-teal-500 hover:underline"
        >
          Add secret →
        </Link>
      ) : null}
    </span>
  );
}
