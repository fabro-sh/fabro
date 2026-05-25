import { useCallback, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import { Link } from "react-router";
import { ChevronDownIcon } from "@heroicons/react/16/solid";
import type { Model, Provider } from "@qltysh/fabro-api-client";
import { useModels, useProviders } from "../lib/queries";
import {
  Dot,
  Panel,
  PanelSkeleton,
  Row,
  SettingsPageIntro,
} from "../components/settings-panel";
import { plural } from "../lib/plural";
import {
  FilterButton,
  type FilterOption,
} from "../components/runs-list/filter-button";
import {
  SortHeader,
  type SortDirection,
} from "../components/runs-list/sort-header";
import { formatContextWindow, formatTokensPerSecond } from "../lib/format";

export function meta() {
  return [{ title: "Models — Fabro" }];
}

export default function SettingsModels() {
  const query = useProviders();

  return (
    <div className="space-y-8">
      <SettingsPageIntro description="LLM providers configured on this Fabro server." />
      {query.data ? (
        <>
          <ProvidersPanel providers={query.data.data} />
          <ModelsSection providers={query.data.data} />
        </>
      ) : (
        <PanelSkeleton />
      )}
    </div>
  );
}

function ProvidersPanel({ providers }: { providers: Provider[] }) {
  const { configured, unconfigured, priorityProviderId } = useMemo(() => {
    const configured: Provider[] = [];
    const unconfigured: Provider[] = [];
    for (const provider of providers) {
      if (provider.configured) {
        configured.push(provider);
      } else {
        unconfigured.push(provider);
      }
    }
    const top = configured.reduce<Provider | null>(
      (best, p) => (!best || p.priority > best.priority ? p : best),
      null,
    );
    return { configured, unconfigured, priorityProviderId: top?.id ?? null };
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
        <ProviderRow
          key={provider.id}
          provider={provider}
          isPriority={provider.id === priorityProviderId}
        />
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

function ProviderRow({
  provider,
  isPriority = false,
}: {
  provider:    Provider;
  isPriority?: boolean;
}) {
  const name = provider.display_name || provider.id;
  const modelCount = `${provider.model_count} ${plural(provider.model_count, "model", "models")}`;
  return (
    <Row
      title={
        <span className="flex items-center gap-4">
          <ProviderLogo provider={provider} />
          <span className="flex min-w-0 flex-col">
            <span className="flex items-center gap-2 text-sm text-fg-2">
              {name}
              {isPriority ? <Label>Priority</Label> : null}
            </span>
            <span className="text-xs/5 text-fg-3">{modelCount}</span>
          </span>
        </span>
      }
    >
      <ProviderStatus provider={provider} />
    </Row>
  );
}

function Label({ children }: { children: ReactNode }) {
  return (
    <span className="inline-flex items-center rounded-md bg-overlay-strong px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-fg-3">
      {children}
    </span>
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
      {!provider.configured && provider.expected_secret_name ? (
        <Link
          to={`/settings/secrets/new?name=${encodeURIComponent(provider.expected_secret_name)}`}
          className="text-xs text-teal-500 hover:underline"
        >
          Add secret →
        </Link>
      ) : null}
    </span>
  );
}

type ModelSortKey = "provider" | "model" | "context" | "speed";

function ModelsSection({ providers }: { providers: Provider[] }) {
  const [providerFilter, setProviderFilter] = useState<string>("");
  const [searchInput, setSearchInput] = useState("");
  const debouncedSearch = useDebouncedValue(searchInput, 250);
  const [sortKey, setSortKey] = useState<ModelSortKey>("provider");
  const [direction, setDirection] = useState<SortDirection>("asc");

  const { data, isLoading } = useModels(providerFilter, debouncedSearch);

  const providerNameById = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of providers) map.set(p.id, p.display_name || p.id);
    return map;
  }, [providers]);

  const rows = useMemo(() => {
    const all = (data?.data ?? []).filter((m) => m.configured);
    return sortModels(all, sortKey, direction, providerNameById);
  }, [data, sortKey, direction, providerNameById]);

  const providerOptions: FilterOption<string>[] = useMemo(() => {
    const options: FilterOption<string>[] = [{ value: "", label: "All providers" }];
    for (const provider of providers) {
      if (provider.configured) {
        options.push({ value: provider.id, label: provider.display_name || provider.id });
      }
    }
    return options;
  }, [providers]);

  const onSort = useCallback(
    (key: ModelSortKey) => {
      if (sortKey === key) {
        setDirection((dir) => (dir === "asc" ? "desc" : "asc"));
      } else {
        setSortKey(key);
        setDirection("asc");
      }
    },
    [sortKey],
  );

  const showEmpty = !isLoading && rows.length === 0;

  return (
    <section className="space-y-3">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <h2 className="text-sm font-medium text-fg-2">Models</h2>
        <div className="flex flex-wrap items-center gap-2">
          <FilterButton
            label="Provider"
            value={providerFilter}
            allValue=""
            options={providerOptions}
            onChange={setProviderFilter}
          />
          <input
            type="search"
            name="model-search"
            aria-label="Search models"
            placeholder="Search models…"
            value={searchInput}
            onChange={(e) => setSearchInput(e.target.value)}
            className="w-44 rounded-md border border-line bg-panel/80 px-3 py-2 text-xs text-fg-2 placeholder:text-fg-muted focus:border-line-strong focus:outline-none"
          />
        </div>
      </header>
      <div className="-mx-4 -my-2 overflow-x-auto whitespace-nowrap sm:-mx-6 lg:-mx-8">
        <div className="inline-block min-w-full px-4 py-2 align-middle sm:px-6 lg:px-8">
          <table className="w-full text-sm [&_td:first-child]:pl-0 [&_td:last-child]:pr-0 [&_th:first-child]:pl-0 [&_th:last-child]:pr-0">
            <thead>
              <tr className="border-b border-line text-xs font-medium text-fg-3">
                <SortHeader<ModelSortKey>
                  label="Provider"
                  sortKey="provider"
                  activeSort={sortKey}
                  direction={direction}
                  onClick={onSort}
                />
                <SortHeader<ModelSortKey>
                  label="Model"
                  sortKey="model"
                  activeSort={sortKey}
                  direction={direction}
                  onClick={onSort}
                />
                <SortHeader<ModelSortKey>
                  label="Context"
                  sortKey="context"
                  activeSort={sortKey}
                  direction={direction}
                  align="right"
                  onClick={onSort}
                />
                <SortHeader<ModelSortKey>
                  label="Speed"
                  sortKey="speed"
                  activeSort={sortKey}
                  direction={direction}
                  align="right"
                  onClick={onSort}
                />
              </tr>
            </thead>
            <tbody>
              {rows.map((model) => (
                <ModelTableRow
                  key={model.id}
                  model={model}
                  providerLabel={
                    providerNameById.get(model.provider) ?? model.provider
                  }
                />
              ))}
            </tbody>
          </table>
        </div>
      </div>
      {showEmpty && (
        <div className="py-6 text-sm text-fg-muted">
          {debouncedSearch || providerFilter
            ? "No matching models from configured providers."
            : "No configured providers yet — add a provider above to enable models."}
        </div>
      )}
    </section>
  );
}

function ModelTableRow({
  model,
  providerLabel,
}: {
  model:         Model;
  providerLabel: string;
}) {
  return (
    <tr className="border-b border-line transition-colors last:border-b-0 hover:bg-overlay/40">
      <td className="whitespace-nowrap px-3 py-2.5 text-fg-3">
        {providerLabel}
      </td>
      <td className="whitespace-nowrap px-3 py-2.5">
        <ModelNameCell model={model} />
      </td>
      <td className="whitespace-nowrap px-3 py-2.5 text-right font-mono text-xs text-fg-muted tabular-nums">
        {formatContextWindow(model.limits.context_window)}
      </td>
      <td className="whitespace-nowrap px-3 py-2.5 text-right font-mono text-xs text-fg-muted tabular-nums">
        {formatTokensPerSecond(model.estimated_output_tps)}
      </td>
    </tr>
  );
}

function ModelNameCell({ model }: { model: Model }) {
  const hasAliases = model.aliases.length > 0;
  const nameNode = hasAliases ? (
    <span className="group/aliases relative inline-flex">
      <button
        type="button"
        aria-describedby={`aliases-${model.id}`}
        className="cursor-default font-mono text-xs text-fg-2 underline decoration-dotted decoration-fg-muted underline-offset-4 hover:decoration-fg-3 focus:outline-none focus-visible:text-fg"
      >
        {model.id}
      </button>
      <span
        role="tooltip"
        id={`aliases-${model.id}`}
        className="pointer-events-none invisible absolute left-0 top-full z-30 mt-1.5 min-w-[10rem] rounded-md bg-panel p-2 text-xs opacity-0 shadow-2xl shadow-black/40 ring-1 ring-line-strong transition-opacity duration-100 group-hover/aliases:visible group-hover/aliases:opacity-100 group-focus-within/aliases:visible group-focus-within/aliases:opacity-100"
      >
        <span className="mb-1 block text-[10px] font-medium uppercase tracking-wider text-fg-muted">
          Aliases
        </span>
        {model.aliases.map((alias) => (
          <span key={alias} className="block font-mono text-fg-2">
            {alias}
          </span>
        ))}
      </span>
    </span>
  ) : (
    <span className="font-mono text-xs text-fg-2">{model.id}</span>
  );

  if (!model.default && !model.small_default) {
    return nameNode;
  }

  return (
    <span className="inline-flex items-center gap-2">
      {nameNode}
      {model.default ? <Label>Default</Label> : null}
      {model.small_default ? <Label>Small</Label> : null}
    </span>
  );
}

function sortModels(
  models:           Model[],
  key:              ModelSortKey,
  direction:        SortDirection,
  providerNameById: Map<string, string>,
): Model[] {
  const sign = direction === "asc" ? 1 : -1;
  const providerLabel = (m: Model) =>
    providerNameById.get(m.provider) ?? m.provider;
  const sorted = Array.from(models);
  sorted.sort((a, b) => {
    let cmp = 0;
    switch (key) {
      case "provider":
        cmp = providerLabel(a).localeCompare(providerLabel(b));
        if (cmp === 0) cmp = a.id.localeCompare(b.id);
        break;
      case "model":
        cmp = a.id.localeCompare(b.id);
        break;
      case "context":
        cmp = a.limits.context_window - b.limits.context_window;
        if (cmp === 0) cmp = a.id.localeCompare(b.id);
        break;
      case "speed": {
        const ta = a.estimated_output_tps ?? -Infinity;
        const tb = b.estimated_output_tps ?? -Infinity;
        cmp = ta - tb;
        if (cmp === 0) cmp = a.id.localeCompare(b.id);
        break;
      }
    }
    return cmp * sign;
  });
  return sorted;
}

function useDebouncedValue<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const id = setTimeout(() => setDebounced(value), delayMs);
    return () => clearTimeout(id);
  }, [value, delayMs]);
  return debounced;
}
