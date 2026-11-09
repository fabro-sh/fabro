import { CheckIcon, ChevronRightIcon, StarIcon as StarFilledIcon } from "@heroicons/react/16/solid";
import { StarIcon as StarOutlineIcon } from "@heroicons/react/24/outline";
import { useId, type MouseEvent, type ReactNode } from "react";

import { plural } from "../../lib/plural";
import type { CatalogModel, GroupMatch } from "./model-picker-model";
import type { StarredModel } from "./model-picker-stars";

export function Chip({ tone, children }: { tone: "warn" | "outline"; children: string }) {
  return <span className={`chip chip-${tone} wfb-model-chip`}>{children}</span>;
}

function rowLabel(name: string, id: string, providerName: string, model: CatalogModel | null, listed: boolean): string {
  const parts = [name === id ? id : `${name}, ${id}`, `from ${providerName}`];
  if (model?.default) parts.push("provider default");
  if (model?.small_default) parts.push("small default");
  if (model && !model.configured) parts.push("setup required");
  if (!model && listed) parts.push("not in the catalog");
  return parts.join(", ");
}

/** Brings the selected row into view when it first appears. Stable, so a re-render doesn't scroll. */
function revealRow(row: HTMLElement | null) {
  const pane = row?.closest<HTMLElement>("[data-model-scroll]");
  if (!row || !pane) return;
  const paneBox = pane.getBoundingClientRect();
  const rowBox = row.getBoundingClientRect();
  if (rowBox.top < paneBox.top || rowBox.bottom > paneBox.bottom) {
    pane.scrollTop += rowBox.top - paneBox.top - (paneBox.height - rowBox.height) / 2;
  }
}

/** One model: a button that selects it, beside a button that stars it. Neither sits inside the other. */
export function ModelRow({
  offering,
  model,
  providerName,
  showProvider,
  catalogLoaded,
  current,
  reveal,
  starred,
  onChoose,
  onToggleStar,
}: {
  offering: StarredModel;
  model: CatalogModel | null;
  providerName: string;
  showProvider: boolean;
  catalogLoaded: boolean;
  current: boolean;
  reveal: boolean;
  starred: boolean;
  onChoose: () => void;
  onToggleStar: (event: MouseEvent<HTMLButtonElement>, name: string) => void;
}) {
  const name = model?.display_name || offering.id;
  const notInCatalog = catalogLoaded && !model;
  const setupRequired = model ? !model.configured : false;
  return (
    <li className="wfb-model-row" data-model-row={true} ref={reveal ? revealRow : undefined}>
      <button
        type="button"
        className="wfb-model-select"
        aria-label={rowLabel(name, offering.id, providerName, model, catalogLoaded)}
        aria-current={current ? "true" : undefined}
        data-model-nav={true}
        data-model-select={true}
        onClick={onChoose}
      >
        <span className="wfb-model-check" aria-hidden="true">
          {current ? <CheckIcon className="size-4" /> : null}
        </span>
        <span className="wfb-model-text">
          <span className="wfb-model-name">{name}</span>
          <span className="wfb-model-id wfb-model-mono">
            {showProvider ? `${providerName} · ${offering.id}` : offering.id}
          </span>
          {model?.default || model?.small_default || setupRequired || notInCatalog ? (
            <span className="wfb-model-badges">
              {model?.default ? <Chip tone="outline">Default</Chip> : null}
              {model?.small_default ? <Chip tone="outline">Small default</Chip> : null}
              {setupRequired ? <Chip tone="warn">Setup required</Chip> : null}
              {notInCatalog ? <Chip tone="outline">Not in catalog</Chip> : null}
            </span>
          ) : null}
        </span>
      </button>
      <button
        type="button"
        className="wfb-model-star"
        aria-label={`Star ${name} from ${providerName}`}
        aria-pressed={starred}
        title={starred ? "Remove the star" : "Star this model"}
        data-model-star={true}
        onClick={(event) => onToggleStar(event, name)}
      >
        {starred ? (
          <StarFilledIcon className="size-4" aria-hidden="true" />
        ) : (
          <StarOutlineIcon className="size-4" aria-hidden="true" />
        )}
      </button>
    </li>
  );
}

/** One company: a header that folds it, over its models. */
export function ProviderSection({
  match,
  open,
  searching,
  onToggle,
  onUseCustomId,
  renderRow,
}: {
  match: GroupMatch;
  open: boolean;
  searching: boolean;
  onToggle: () => void;
  onUseCustomId: () => void;
  renderRow: (model: CatalogModel) => ReactNode;
}) {
  const listId = useId();
  const { group, models } = match;
  const total = group.models.length;
  const count =
    searching && models.length !== total
      ? `${models.length} of ${total}`
      : `${total} ${plural(total, "model", "models")}`;
  return (
    <section className="wfb-model-group">
      <h3 className="wfb-model-group-heading">
        <button
          type="button"
          className="wfb-model-group-header"
          aria-expanded={open}
          aria-controls={open ? listId : undefined}
          data-model-nav={true}
          data-model-group={true}
          onClick={onToggle}
        >
          <ChevronRightIcon className="wfb-model-chevron size-4" aria-hidden="true" />
          <span className="wfb-model-group-name">{group.name}</span>
          <span className="wfb-model-group-count">{count}</span>
          {group.ready ? null : <Chip tone="warn">Setup required</Chip>}
        </button>
      </h3>
      {open ? (
        total === 0 ? (
          <p id={listId} className="wfb-model-note">
            This provider lists no models.{" "}
            <button type="button" className="link" onClick={onUseCustomId}>
              Use a custom ID
            </button>
          </p>
        ) : (
          <ul id={listId} className="wfb-model-list">
            {models.map(renderRow)}
          </ul>
        )
      ) : null}
    </section>
  );
}
