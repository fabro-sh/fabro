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
  return (
    <Row
      title={provider.display_name || provider.id}
      help={`${provider.model_count} ${plural(provider.model_count, "model", "models")}`}
    >
      <ProviderStatus provider={provider} />
    </Row>
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
