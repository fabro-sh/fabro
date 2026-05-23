import { useMemo, useState } from "react";
import { ChevronDownIcon } from "@heroicons/react/16/solid";
import type { Provider } from "@qltysh/fabro-api-client";
import { useProviders } from "../lib/queries";
import {
  Dot,
  Panel,
  PanelSkeleton,
  Row,
  SettingsPageIntro,
  plural,
} from "../components/settings-panel";

export function meta() {
  return [{ title: "Models — Fabro" }];
}

export default function SettingsModels() {
  const query = useProviders();

  return (
    <div className="space-y-6">
      <SettingsPageIntro description="LLM providers configured on this Fabro server." />
      {query.data ? (
        <ProvidersPanel providers={query.data.data} />
      ) : (
        <PanelSkeleton />
      )}
    </div>
  );
}

function ProvidersPanel({ providers }: { providers: Provider[] }) {
  const { configured, unconfigured } = useMemo(() => {
    const configured: Provider[] = [];
    const unconfigured: Provider[] = [];
    for (const provider of providers) {
      if (provider.configured) {
        configured.push(provider);
      } else {
        unconfigured.push(provider);
      }
    }
    return { configured, unconfigured };
  }, [providers]);
  const [showUnconfigured, setShowUnconfigured] = useState(false);
  const showUnconfiguredRows = configured.length === 0 || showUnconfigured;

  if (providers.length === 0) {
    return (
      <Panel title="Providers">
        <div className="px-4 py-6 text-sm text-fg-muted">
          No LLM providers in the catalog.
        </div>
      </Panel>
    );
  }

  return (
    <Panel title="Providers">
      {configured.map((provider) => (
        <ProviderRow key={provider.id} provider={provider} />
      ))}
      {showUnconfiguredRows
        ? unconfigured.map((provider) => (
            <ProviderRow key={provider.id} provider={provider} />
          ))
        : null}
      {configured.length > 0 && unconfigured.length > 0 ? (
        <button
          type="button"
          onClick={() => setShowUnconfigured((v) => !v)}
          aria-expanded={showUnconfigured}
          className="flex w-full items-center gap-1.5 px-4 py-3 text-left text-xs font-medium text-fg-muted hover:text-fg-3"
        >
          <ChevronDownIcon
            className={`size-4 h-lh shrink-0 transition-transform ${
              showUnconfigured ? "rotate-180" : ""
            }`}
          />
          {showUnconfigured ? "Hide" : "Show"} {unconfigured.length}{" "}
          unconfigured {plural(unconfigured.length, "provider", "providers")}
        </button>
      ) : null}
    </Panel>
  );
}

function ProviderRow({ provider }: { provider: Provider }) {
  const name = provider.display_name || provider.id;
  const modelCount = `${provider.model_count} ${plural(provider.model_count, "model", "models")}`;
  return (
    <Row
      title={
        <span className="flex items-center gap-4">
          <ProviderLogo provider={provider} />
          <span className="flex min-w-0 flex-col">
            <span className="text-sm text-fg-2">{name}</span>
            <span className="text-xs/5 text-fg-3">{modelCount}</span>
          </span>
        </span>
      }
    >
      <ProviderStatus provider={provider} />
    </Row>
  );
}

function ProviderLogo({ provider }: { provider: Provider }) {
  const [failed, setFailed] = useState(false);
  const name = provider.display_name || provider.id;
  const chip =
    "grid size-10 shrink-0 place-items-center rounded-md bg-ice-50 ring-1 ring-line-strong";
  const dim = provider.configured ? "" : "opacity-60";

  if (failed) {
    const initial = name.charAt(0).toUpperCase() || "?";
    return (
      <span className={`${chip} text-base font-medium text-page ${dim}`}>
        {initial}
      </span>
    );
  }

  return (
    <span className={`${chip} ${dim}`}>
      <img
        alt=""
        src={`/images/providers/${provider.id}.svg`}
        onError={() => setFailed(true)}
        className="size-7"
      />
    </span>
  );
}

function ProviderStatus({ provider }: { provider: Provider }) {
  return (
    <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">
      <span className="inline-flex items-center gap-2">
        <Dot on={provider.configured} />
        <span className={provider.configured ? "text-fg" : "text-fg-muted"}>
          {provider.configured ? "Configured" : "Not configured"}
        </span>
      </span>
      {!provider.configured && provider.api_key_url ? (
        <a
          href={provider.api_key_url}
          target="_blank"
          rel="noreferrer"
          className="text-xs text-teal-500 hover:underline"
        >
          Get API key →
        </a>
      ) : null}
    </span>
  );
}
