import { ChevronDownIcon } from "@heroicons/react/16/solid";
import { useId, useRef, useState } from "react";

import { useModels, useProviders } from "../../lib/queries";
import { DraggablePopup } from "./draggable-popup";
import { FieldShell } from "./fields";
import { ModelPickerBody } from "./model-picker-body";
import {
  NO_INHERITED_MODEL,
  pickerCatalogStatus,
  summarizeChoice,
  type CatalogModel,
  type CatalogProvider,
  type ChoiceSummary,
  type InheritedModel,
  type ModelChoice,
  type PickerCatalog,
} from "./model-picker-model";
import { Chip } from "./model-picker-row";
import {
  toggleModelStar,
  useModelStars,
  type StarredModel,
  type StarSnapshot,
  type StarToggleOutcome,
} from "./model-picker-stars";

const NO_MODELS: readonly CatalogModel[] = [];
const NO_PROVIDERS: readonly CatalogProvider[] = [];
const PICKER_SIZE = { width: 400, height: 544 };

/**
 * The server's catalog, read through the shared hooks. Those hooks don't refetch on
 * their own, so a provider set up in Settings would still read "Setup required". The
 * picker asks again each time it opens, and that fetch only reads.
 */
function usePickerCatalog(): PickerCatalog {
  const models = useModels("", "");
  const providers = useProviders();
  const { status, providersFailed } = pickerCatalogStatus({
    hasModels: models.data !== undefined,
    modelsFailed: models.error !== undefined,
    hasProviders: providers.data !== undefined,
    providersFailed: providers.error !== undefined,
  });
  return {
    status,
    providersFailed,
    models: models.data?.data ?? NO_MODELS,
    providers: providers.data?.data ?? NO_PROVIDERS,
    retrying: models.isValidating || providers.isValidating,
    retry: () => {
      void models.mutate();
      void providers.mutate();
    },
  };
}

function triggerLabel(label: string, summary: ChoiceSummary): string {
  return [
    `${label}: ${summary.title}`,
    summary.source ? `inherited from ${summary.source}` : null,
    summary.detail,
    summary.setupRequired ? "setup required" : null,
    summary.notInCatalog ? "not in the catalog" : null,
  ]
    .filter(Boolean)
    .join(", ");
}

export interface ModelPickerProps {
  label: string;
  description?: string;
  value: ModelChoice;
  inherited?: InheritedModel;
  inheritLabel: string;
  inheritHint?: string;
  disabled?: boolean;
  onChange: (choice: ModelChoice | null) => void;
}

/**
 * The field and its window, given a catalog. The value is only ever written from
 * `onChange`, so opening this, loading the catalog or re-rendering leaves the source
 * alone. The window closes on the X, on Escape and on a choice, and the dialog returns
 * focus here. Nothing behind it can be used meanwhile, so this field can't change under it.
 */
export function ModelPickerControl({
  label,
  description,
  value,
  inherited = NO_INHERITED_MODEL,
  inheritLabel,
  inheritHint,
  disabled,
  onChange,
  catalog,
  stars,
  onToggleStar,
}: ModelPickerProps & {
  catalog: PickerCatalog;
  stars: StarSnapshot;
  onToggleStar: (star: StarredModel) => StarToggleOutcome;
}) {
  const buttonId = useId();
  const buttonRef = useRef<HTMLButtonElement>(null);
  const [isOpen, setIsOpen] = useState(false);
  const open = isOpen && !disabled;
  const summary = summarizeChoice(catalog.status === "ready" ? catalog : null, value, inherited);

  return (
    <FieldShell htmlFor={buttonId} label={label} description={description}>
      <div className="wfb-model-field">
        <button
          ref={buttonRef}
          id={buttonId}
          type="button"
          className="wfb-input wfb-model-trigger"
          disabled={disabled}
          aria-haspopup="dialog"
          aria-expanded={open}
          aria-label={triggerLabel(label, summary)}
          onClick={() => {
            catalog.retry();
            setIsOpen(true);
          }}
        >
          <span className="wfb-model-text">
            <span className="wfb-model-trigger-title">
              <span className="wfb-model-name">{summary.title}</span>
              {summary.setupRequired ? <Chip tone="warn">Setup required</Chip> : null}
              {summary.notInCatalog ? <Chip tone="outline">Not in catalog</Chip> : null}
            </span>
            {summary.source || summary.detail ? (
              <span className="wfb-model-id">
                {summary.source ? <span>{summary.source}</span> : null}
                {summary.source && summary.detail ? " · " : null}
                {summary.detail ? <span className="wfb-model-mono">{summary.detail}</span> : null}
              </span>
            ) : null}
          </span>
          <ChevronDownIcon className="wfb-model-trigger-icon size-4" aria-hidden="true" />
        </button>
        {open ? (
          <DraggablePopup
            label={`Choose ${label.toLowerCase()}`}
            moveLabel="Move model picker"
            closeLabel="Close model picker"
            anchorRef={buttonRef}
            preferredSize={PICKER_SIZE}
            onClose={() => setIsOpen(false)}
          >
            <ModelPickerBody
              catalog={catalog}
              value={value}
              inherited={inherited}
              inheritLabel={inheritLabel}
              inheritHint={inheritHint}
              stars={stars}
              onToggleStar={onToggleStar}
              onChoose={(choice) => {
                onChange(choice);
                setIsOpen(false);
              }}
            />
          </DraggablePopup>
        ) : null}
      </div>
      {summary.warning ? <p className="wfb-field-warning">{summary.warning}</p> : null}
    </FieldShell>
  );
}

/** Chooses a model and its provider from the server's catalog. */
export function ModelPickerField(props: ModelPickerProps) {
  const catalog = usePickerCatalog();
  const stars = useModelStars();
  return <ModelPickerControl {...props} catalog={catalog} stars={stars} onToggleStar={toggleModelStar} />;
}
