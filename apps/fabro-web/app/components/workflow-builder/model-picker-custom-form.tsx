import { useId, useState } from "react";

import { parseCustomChoice, type CatalogProvider, type ModelChoice } from "./model-picker-model";

/** A model the catalog may not list yet. What is typed goes into the source as written. */
export function CustomModelForm({
  initial,
  providers,
  onSubmit,
  onCancel,
}: {
  initial: { model: string; provider: string };
  providers: readonly CatalogProvider[];
  onSubmit: (choice: ModelChoice) => void;
  onCancel: () => void;
}) {
  const modelId = useId();
  const providerId = useId();
  const listId = useId();
  const [model, setModel] = useState(initial.model);
  const [provider, setProvider] = useState(initial.provider);
  const [error, setError] = useState<string | null>(null);

  return (
    <form
      className="wfb-model-custom"
      data-model-custom={true}
      noValidate
      onSubmit={(event) => {
        event.preventDefault();
        const parsed = parseCustomChoice(model, provider);
        if ("error" in parsed) setError(parsed.error);
        else onSubmit(parsed.choice);
      }}
    >
      <div>
        <label htmlFor={modelId} className="field-label">
          Model ID
        </label>
        <input
          id={modelId}
          className="input wfb-model-mono"
          autoFocus
          value={model}
          spellCheck={false}
          autoComplete="off"
          autoCapitalize="off"
          aria-invalid={error ? true : undefined}
          onChange={(event) => {
            setModel(event.target.value);
            setError(null);
          }}
        />
      </div>
      <div>
        <label htmlFor={providerId} className="field-label">
          Provider ID
        </label>
        <input
          id={providerId}
          className="input wfb-model-mono"
          list={listId}
          value={provider}
          spellCheck={false}
          autoComplete="off"
          autoCapitalize="off"
          onChange={(event) => {
            setProvider(event.target.value);
            setError(null);
          }}
        />
        <datalist id={listId}>
          {providers.map((option) => (
            <option key={option.id} value={option.id}>
              {option.display_name}
            </option>
          ))}
        </datalist>
      </div>
      <p className="wfb-model-hint">
        Written as typed and not checked. Leave the provider blank only if the catalog can work it out.
      </p>
      {error ? (
        <p role="alert" className="wfb-model-error">
          {error}
        </p>
      ) : null}
      <div className="wfb-model-custom-actions">
        <button type="button" className="btn btn-sm" onClick={onCancel}>
          Cancel
        </button>
        <button type="submit" className="btn btn-sm btn-primary">
          Use this ID
        </button>
      </div>
    </form>
  );
}
