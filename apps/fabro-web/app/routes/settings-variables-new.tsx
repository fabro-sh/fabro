import { useState, type ReactNode } from "react";
import { Link, useNavigate } from "react-router";
import { useSWRConfig } from "swr";
import { ChevronRightIcon } from "@heroicons/react/20/solid";

import { ApiError, apiData, variablesApi } from "../lib/api-client";
import { queryKeys } from "../lib/query-keys";
import { Panel, Row } from "../components/settings-panel";
import {
  ErrorMessage,
  INPUT_CLASS,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "../components/ui";
import { useToast } from "../components/toast";

export function meta() {
  return [{ title: "New variable — Fabro" }];
}

export default function SettingsVariablesNew() {
  return (
    <div className="space-y-6">
      <PageHeader />
      <CreateVariableForm />
    </div>
  );
}

function PageHeader() {
  return (
    <nav className="flex items-center gap-1 text-sm text-fg-muted">
      <Link to="/settings/variables" className="text-fg-3 hover:text-fg">
        Variables
      </Link>
      <ChevronRightIcon className="size-3" aria-hidden="true" />
      <span>New variable</span>
    </nav>
  );
}

function CreateVariableForm() {
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [description, setDescription] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSubmit = name.trim() !== "" && !submitting;

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    const trimmedName = name.trim();
    try {
      await apiData(() =>
        variablesApi.createVariable({
          name: trimmedName,
          value,
          description: description.trim() || undefined,
        }),
      );
      await mutate(queryKeys.variables.list());
      toast.push({ message: `Variable “${trimmedName}” saved.` });
      navigate("/settings/variables");
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.message
          ? cause.message
          : "Couldn't save the variable. Please try again.",
      );
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={onSubmit} className="space-y-6">
      <Panel title="Variable">
        <Row
          title={<Label required>Name</Label>}
          help="Env-style variable name (letters, digits, underscores). Referenced in run config as {{ vars.NAME }}."
        >
          <input
            type="text"
            name="name"
            aria-label="Variable name"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="DEFAULT_BRANCH"
            autoComplete="off"
            spellCheck={false}
            className={`${INPUT_CLASS} font-mono`}
          />
        </Row>
        <Row
          title={<Label optional>Value</Label>}
          help="Stored as-is. Empty values are allowed."
        >
          <textarea
            name="value"
            aria-label="Variable value"
            value={value}
            onChange={(event) => setValue(event.target.value)}
            rows={2}
            autoComplete="off"
            spellCheck={false}
            className={`${INPUT_CLASS} resize-y font-mono`}
          />
        </Row>
        <Row
          title={<Label optional>Description</Label>}
          help="Helps operators recognize what this variable is for."
        >
          <input
            type="text"
            name="description"
            aria-label="Variable description"
            value={description}
            onChange={(event) => setDescription(event.target.value)}
            className={INPUT_CLASS}
          />
        </Row>
      </Panel>

      {error ? <ErrorMessage message={error} /> : null}

      <FormFooter
        submitting={submitting}
        canSubmit={canSubmit}
        onCancel={() => navigate("/settings/variables")}
      />
    </form>
  );
}

function Label({
  children,
  required,
  optional,
}: {
  children: ReactNode;
  required?: boolean;
  optional?: boolean;
}) {
  return (
    <span className="inline-flex items-baseline gap-1.5">
      <span>{children}</span>
      {required ? (
        <span aria-label="required" className="text-coral">
          *
        </span>
      ) : null}
      {optional ? <span className="text-xs font-normal text-fg-muted">Optional</span> : null}
    </span>
  );
}

function FormFooter({
  submitting,
  canSubmit,
  onCancel,
}: {
  submitting: boolean;
  canSubmit: boolean;
  onCancel: () => void;
}) {
  return (
    <div className="flex items-center justify-end gap-3 pt-2">
      <button
        type="button"
        onClick={onCancel}
        disabled={submitting}
        className={SECONDARY_BUTTON_CLASS}
      >
        Cancel
      </button>
      <button type="submit" disabled={!canSubmit} className={PRIMARY_BUTTON_CLASS}>
        {submitting ? "Saving…" : "Save variable"}
      </button>
    </div>
  );
}
