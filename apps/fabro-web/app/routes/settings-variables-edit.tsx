import { useState, type ReactNode } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useSWRConfig } from "swr";
import { ChevronRightIcon } from "@heroicons/react/20/solid";

import { ApiError, apiData, variablesApi } from "../lib/api-client";
import { queryKeys } from "../lib/query-keys";
import { Panel, PanelSkeleton, Row } from "../components/settings-panel";
import {
  ErrorMessage,
  INPUT_CLASS,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "../components/ui";
import { useToast } from "../components/toast";
import { useVariable } from "../lib/queries";

export function meta() {
  return [{ title: "Edit variable — Fabro" }];
}

export default function SettingsVariablesEdit() {
  const { name } = useParams<{ name: string }>();
  const query = useVariable(name);

  return (
    <div className="space-y-6">
      <PageHeader name={name ?? ""} />
      {query.data ? (
        <EditVariableForm
          key={query.data.name}
          name={query.data.name}
          initialValue={query.data.value}
          initialDescription={query.data.description ?? ""}
        />
      ) : query.error ? (
        <Panel title="Variable">
          <div className="px-4 py-6 text-sm text-fg-2">
            Couldn&apos;t load this variable. It may have been deleted.
          </div>
        </Panel>
      ) : (
        <PanelSkeleton />
      )}
    </div>
  );
}

function PageHeader({ name }: { name: string }) {
  return (
    <nav className="flex items-center gap-1 text-sm text-fg-muted">
      <Link to="/settings/variables" className="text-fg-3 hover:text-fg">
        Variables
      </Link>
      <ChevronRightIcon className="size-3" aria-hidden="true" />
      <span className="font-mono text-fg-2">{name}</span>
    </nav>
  );
}

function EditVariableForm({
  name,
  initialValue,
  initialDescription,
}: {
  name: string;
  initialValue: string;
  initialDescription: string;
}) {
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [value, setValue] = useState(initialValue);
  const [description, setDescription] = useState(initialDescription);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const dirty = value !== initialValue || description !== initialDescription;
  const canSubmit = dirty && !submitting;

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    try {
      await apiData(() =>
        variablesApi.updateVariable(name, {
          value,
          description: description.trim() || undefined,
        }),
      );
      await mutate(queryKeys.variables.list());
      await mutate(queryKeys.variables.detail(name));
      toast.push({ message: `Variable “${name}” updated.` });
      navigate("/settings/variables");
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.message
          ? cause.message
          : "Couldn't update the variable. Please try again.",
      );
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={onSubmit} className="space-y-6">
      <Panel title="Variable">
        <Row title={<Label>Name</Label>} help="The variable name cannot be changed. Delete and recreate to rename.">
          <div className="font-mono text-sm text-fg">{name}</div>
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
  optional,
}: {
  children: ReactNode;
  optional?: boolean;
}) {
  return (
    <span className="inline-flex items-baseline gap-1.5">
      <span>{children}</span>
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
        {submitting ? "Saving…" : "Save changes"}
      </button>
    </div>
  );
}
