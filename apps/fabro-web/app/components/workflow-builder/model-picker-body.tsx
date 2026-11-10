import { CheckIcon, MagnifyingGlassIcon, StarIcon as StarFilledIcon } from "@heroicons/react/16/solid";
import { useMemo, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import { Link } from "react-router";

import { modelOfferingKey } from "../../lib/model-offerings";
import { plural } from "../../lib/plural";
import { Spinner } from "../state";
import { CustomModelForm } from "./model-picker-custom-form";
import {
  buildProviderGroups,
  choiceOf,
  nextNavIndex,
  searchGroups,
  starRows,
  summarizeChoice,
  type CatalogModel,
  type GroupMatch,
  type InheritedModel,
  type ModelChoice,
  type PickerCatalog,
} from "./model-picker-model";
import { ModelRow, ProviderSection } from "./model-picker-row";
import { starProblemMessage, type StarredModel, type StarSnapshot, type StarToggleOutcome } from "./model-picker-stars";

const NO_OVERRIDES: ReadonlyMap<string, boolean> = new Map();

function SettingsLink() {
  return (
    <Link to="/settings/models" className="link">
      Settings › Models
    </Link>
  );
}

/** A row can leave the list when its star is removed, so focus moves first rather than falling out of the panel. */
function moveFocusOffPinnedRow(button: HTMLElement | null | undefined) {
  const panel = button?.closest?.("[data-model-picker]");
  if (!button || !panel) return;
  const stops = [...panel.querySelectorAll<HTMLElement>("[data-model-star]")];
  const index = stops.indexOf(button);
  const next = stops[index + 1] ?? stops[index - 1] ?? panel.querySelector<HTMLElement>("input");
  next?.focus();
}

/** Arrows walk the list, left and right move within a row or fold a company, Enter leaves the search for the first result. */
function onPickerKeyDown(event: KeyboardEvent<HTMLDivElement>) {
  const target = event.target as HTMLElement;
  // The custom ID form keeps its own arrow keys.
  if (target.closest("[data-model-custom]")) return;
  const root = event.currentTarget;
  const inSearch = target.matches("input");

  if (inSearch && event.key === "Enter") {
    event.preventDefault();
    root.querySelector<HTMLElement>("[data-model-select]")?.focus();
    return;
  }
  if (inSearch && (event.key === "Home" || event.key === "End")) return;

  const row = target.closest("[data-model-row]");
  if (!inSearch && (event.key === "ArrowLeft" || event.key === "ArrowRight")) {
    const toward = event.key === "ArrowRight";
    if (row) {
      row.querySelector<HTMLElement>(toward ? "[data-model-star]" : "[data-model-select]")?.focus();
      event.preventDefault();
    } else if (target.matches("[data-model-group]") && (target.getAttribute("aria-expanded") === "true") !== toward) {
      target.click();
      event.preventDefault();
    }
    return;
  }

  const stops = [...root.querySelectorAll<HTMLElement>("[data-model-nav]")];
  const from = stops.indexOf(row?.querySelector<HTMLElement>("[data-model-nav]") ?? target);
  // Buttons outside the list, like Custom ID, aren't stops.
  if (from < 0) return;
  const next = nextNavIndex(event.key, from, stops.length);
  if (next === null) return;
  event.preventDefault();
  stops[next].focus();
}

export interface ModelPickerBodyProps {
  catalog: PickerCatalog;
  value: ModelChoice;
  inherited: InheritedModel;
  inheritLabel: string;
  /** Replaces the inherited value on the clear row, when there is nothing to inherit but the server. */
  inheritHint?: string;
  stars: StarSnapshot;
  onToggleStar: (star: StarredModel) => StarToggleOutcome;
  /** `null` removes the choice, which returns to what is inherited. */
  onChoose: (choice: ModelChoice | null) => void;
}

/**
 * What the popup shows. Mounted only while it is open, so its search and folding start
 * fresh each time. Nothing here writes unless the author picks something.
 */
export function ModelPickerBody({
  catalog,
  value,
  inherited,
  inheritLabel,
  inheritHint,
  stars,
  onToggleStar,
  onChoose,
}: ModelPickerBodyProps) {
  const [query, setQuery] = useState("");
  const [browseOverrides, setBrowseOverrides] = useState(NO_OVERRIDES);
  const [searchOverrides, setSearchOverrides] = useState(NO_OVERRIDES);
  const [custom, setCustom] = useState<{ model: string; provider: string; revision: number } | null>(null);
  const [starNote, setStarNote] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  const ready = catalog.status === "ready";
  const loaded = ready ? catalog : null;
  const { models, providers } = catalog;
  const groups = useMemo(
    () => (ready ? buildProviderGroups({ models, providers }) : []),
    [ready, models, providers],
  );
  const matches = useMemo(() => searchGroups(groups, providers, query), [groups, providers, query]);
  const starredKeys = useMemo(() => new Set(stars.stars.map(modelOfferingKey)), [stars.stars]);

  const searching = query.trim() !== "";
  const summary = summarizeChoice(loaded, value, inherited);
  const inheritedSummary = summarizeChoice(loaded, {}, inherited);
  const pinned = starRows(stars.stars, loaded, query);
  const hasOwnChoice = Boolean(value.model || value.provider);

  const shownModels = matches.reduce((sum, match) => sum + match.models.length, 0);
  const shownCompanies = matches.filter((match) => match.models.length > 0).length;
  const nothingMatches = searching && matches.length === 0 && pinned.length === 0;

  const isOpen = (match: GroupMatch): boolean => {
    if (searching) return searchOverrides.get(match.group.id) ?? true;
    const holdsCurrent = match.models.some((model) => modelOfferingKey(model) === summary.currentKey);
    return browseOverrides.get(match.group.id) ?? (holdsCurrent || groups.length === 1);
  };

  const fold = (match: GroupMatch) => {
    const set = searching ? setSearchOverrides : setBrowseOverrides;
    const next = !isOpen(match);
    set((previous) => new Map(previous).set(match.group.id, next));
  };

  const toggleStar = (
    event: MouseEvent<HTMLButtonElement>,
    star: StarredModel,
    name: string,
    fromPinned: boolean,
  ) => {
    if (fromPinned) moveFocusOffPinnedRow(event.currentTarget);
    const outcome = onToggleStar(star);
    if (outcome.problem) setStarNote(null);
    else setStarNote(`${outcome.starred ? "Starred" : "Removed the star from"} ${name}. Saved in this browser.`);
  };

  const openCustom = (preset: { model?: string; provider?: string }) => {
    const typed = searching ? query.trim() : summary.currentKey ? "" : (value.model ?? "");
    setCustom((previous) => ({
      model: preset.model ?? typed,
      provider: preset.provider ?? value.provider ?? "",
      revision: (previous?.revision ?? 0) + 1,
    }));
  };

  // These buttons leave the DOM when they work, and focus must not fall out of the panel with them.
  const retry = () => {
    searchRef.current?.focus();
    catalog.retry();
  };
  const closeCustom = () => {
    searchRef.current?.focus();
    setCustom(null);
  };

  const renderGroupRow = (model: CatalogModel, providerName: string) => {
    const key = modelOfferingKey(model);
    const current = key === summary.currentKey;
    const starred = starredKeys.has(key);
    const offering = { provider: model.provider, id: model.id };
    return (
      <ModelRow
        key={key}
        offering={offering}
        model={model}
        providerName={providerName}
        showProvider={false}
        catalogLoaded={ready}
        current={current}
        reveal={current && !starred}
        starred={starred}
        onChoose={() => onChoose(choiceOf(model))}
        onToggleStar={(event, name) => toggleStar(event, offering, name, false)}
      />
    );
  };

  return (
    <div className="wfb-model-body" data-model-picker={true} onKeyDown={onPickerKeyDown}>
      <div className="wfb-model-search">
        <MagnifyingGlassIcon className="wfb-model-search-icon size-4" aria-hidden="true" />
        <input
          ref={searchRef}
          type="search"
          className="input wfb-model-search-input"
          placeholder="Search models or companies"
          aria-label="Search models and companies"
          value={query}
          // The dialog's focus trap reads this. A plain autoFocus runs before its portal is in the page.
          data-autofocus={true}
          spellCheck={false}
          autoComplete="off"
          autoCapitalize="off"
          enterKeyHint="search"
          data-model-nav={true}
          onChange={(event) => {
            setQuery(event.target.value);
            setSearchOverrides(NO_OVERRIDES);
          }}
        />
      </div>
      <p role="status" className="wfb-model-count">
        {ready
          ? shownModels === 0
            ? searching
              ? "No matches"
              : "No models"
            : `${shownModels} ${plural(shownModels, "model", "models")} from ${shownCompanies} ${plural(shownCompanies, "company", "companies")}`
          : ""}
      </p>

      <div className="wfb-model-scroll scroll-pane" data-model-scroll={true}>
        {catalog.status === "loading" ? (
          <p role="status" className="wfb-model-note wfb-model-note-loading">
            <Spinner className="size-4" />
            Loading the model catalog…
          </p>
        ) : null}
        {catalog.status === "error" ? (
          <div role="alert" className="wfb-model-note wfb-model-note-bad">
            <span>The model catalog did not load. You can still use a custom ID.</span>
            <button type="button" className="btn btn-sm" disabled={catalog.retrying} onClick={retry}>
              {catalog.retrying ? "Retrying…" : "Retry"}
            </button>
          </div>
        ) : null}
        {ready && catalog.providersFailed ? (
          <div role="alert" className="wfb-model-note wfb-model-note-warn">
            <span>Company names did not load, so provider IDs are shown.</span>
            <button type="button" className="btn btn-sm" disabled={catalog.retrying} onClick={retry}>
              {catalog.retrying ? "Retrying…" : "Retry"}
            </button>
          </div>
        ) : null}
        {ready && models.length === 0 ? (
          <p className="wfb-model-note">
            <span>
              This server lists no models. Check <SettingsLink />, or use a custom ID.
            </span>
          </p>
        ) : null}
        {ready && models.length > 0 && !models.some((model) => model.configured) ? (
          <p className="wfb-model-note wfb-model-note-warn">
            <span>
              No provider is set up yet. You can still choose a model, but a run needs a provider set up in{" "}
              <SettingsLink />.
            </span>
          </p>
        ) : null}
        {value.model && summary.notInCatalog ? (
          <p className="wfb-model-note">
            <span>
              <span className="wfb-model-mono">{value.model}</span> is not in the catalog. It stays as written until you
              choose another.
            </span>
          </p>
        ) : null}

        {searching ? null : (
          <button
            type="button"
            className="wfb-model-inherit"
            aria-current={hasOwnChoice ? undefined : "true"}
            data-model-nav={true}
            onClick={() => onChoose(null)}
          >
            <span className="wfb-model-check" aria-hidden="true">
              {hasOwnChoice ? null : <CheckIcon className="size-4" />}
            </span>
            <span className="wfb-model-text">
              <span className="wfb-model-name">{inheritLabel}</span>
              <span className="wfb-model-id">
                {inheritHint ??
                  (inheritedSummary.source
                    ? `${inheritedSummary.source} · ${inheritedSummary.title}`
                    : inheritedSummary.title)}
              </span>
            </span>
          </button>
        )}

        {pinned.length > 0 ? (
          <section className="wfb-model-group">
            <h3 className="wfb-model-group-heading">
              <span className="wfb-model-pinned-header">
                <StarFilledIcon className="wfb-model-pinned-icon size-3" aria-hidden="true" />
                <span className="wfb-model-group-name">Starred</span>
                <span className="wfb-model-group-count">{pinned.length}</span>
              </span>
            </h3>
            <ul className="wfb-model-list">
              {pinned.map(({ star, model, providerName }) => {
                const key = modelOfferingKey(star);
                const current = key === summary.currentKey;
                return (
                  <ModelRow
                    key={key}
                    offering={star}
                    model={model}
                    providerName={providerName}
                    showProvider={true}
                    catalogLoaded={ready}
                    current={current}
                    reveal={current}
                    starred={true}
                    onChoose={() => onChoose(choiceOf(star))}
                    onToggleStar={(event, name) => toggleStar(event, star, name, true)}
                  />
                );
              })}
            </ul>
          </section>
        ) : null}

        {matches.map((match) => (
          <ProviderSection
            key={match.group.id}
            match={match}
            open={isOpen(match)}
            searching={searching}
            onToggle={() => fold(match)}
            onUseCustomId={() => openCustom({ model: "", provider: match.group.id })}
            renderRow={(model) => renderGroupRow(model, match.group.name)}
          />
        ))}

        {nothingMatches && ready ? (
          <div className="wfb-model-empty">
            <p>No model or company matches “{query.trim()}”.</p>
            <button type="button" className="link" onClick={() => openCustom({ model: query.trim() })}>
              Use “{query.trim()}” as a custom model ID
            </button>
          </div>
        ) : null}
      </div>

      <div className="wfb-model-footer">
        {custom ? (
          <CustomModelForm key={custom.revision} initial={custom} providers={providers} onSubmit={onChoose} onCancel={closeCustom} />
        ) : null}
        <div className="wfb-model-footer-line">
          <p
            role="status"
            className={`wfb-model-star-status${stars.problem ? " wfb-model-star-status-problem" : ""}`}
          >
            {stars.problem ? starProblemMessage(stars.problem) : (starNote ?? starProblemMessage(null))}
          </p>
          {custom ? null : (
            <button type="button" className="btn btn-sm btn-ghost shrink-0" onClick={() => openCustom({})}>
              Custom ID
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
