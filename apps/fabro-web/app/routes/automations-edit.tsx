import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useSWRConfig } from "swr";
import { ChevronRightIcon } from "@heroicons/react/20/solid";
import type { Automation } from "@qltysh/fabro-api-client";

import { ApiError, apiData, automationsApi } from "../lib/api-client";
import { useAutomation } from "../lib/queries";
import { queryKeys } from "../lib/query-keys";
import {
  AutomationFormFields,
  automationToFormValues,
  isFormValid,
  triggersFromFormValues,
  type AutomationFormValues,
} from "../components/automation-form";
import { Panel, PanelSkeleton } from "../components/settings-panel";
import {
  ErrorMessage,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "../components/ui";
import { useToast } from "../components/toast";

export function meta() {
  return [{ title: "Edit automation — Fabro" }];
}

export const handle = { hideHeader: true };

export default function AutomationsEdit() {
  const { id } = useParams<{ id: string }>();
  const query = useAutomation(id);

  return (
    <div className="space-y-6">
      <PageHeader id={id ?? ""} name={query.data?.name} />
      {query.data ? (
        <EditAutomationForm key={query.data.id} automation={query.data} />
      ) : query.error ? (
        <Panel title="Automation">
          <div className="px-4 py-6 text-sm text-fg-2">
            Couldn&apos;t load this automation. It may have been deleted.
          </div>
        </Panel>
      ) : (
        <PanelSkeleton />
      )}
    </div>
  );
}

function PageHeader({ id, name }: { id: string; name: string | undefined }) {
  return (
    <nav className="flex items-center gap-1 text-sm text-fg-muted">
      <Link to="/automations" className="text-fg-3 hover:text-fg">
        Automations
      </Link>
      <ChevronRightIcon className="size-3" aria-hidden="true" />
      <span>{name ?? id}</span>
    </nav>
  );
}

function EditAutomationForm({ automation }: { automation: Automation }) {
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [values, setValues] = useState<AutomationFormValues>(
    automationToFormValues(automation),
  );
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSubmit = isFormValid(values) && !submitting;

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    const trimmedName = values.name.trim();
    try {
      await apiData(() =>
        automationsApi.replaceAutomation(automation.id, automation.revision, {
          name:        trimmedName,
          description: values.description.trim() || null,
          target:      {
            repository: values.repository.trim(),
            ref:        values.ref.trim(),
            workflow:   values.workflow.trim(),
          },
          triggers: triggersFromFormValues(values),
        }),
      );
      await mutate(queryKeys.automations.list());
      await mutate(queryKeys.automations.detail(automation.id));
      toast.push({ message: `Automation “${trimmedName}” updated.` });
      navigate("/automations");
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.message
          ? cause.message
          : "Couldn't update the automation. Please try again.",
      );
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={onSubmit} className="space-y-6">
      <AutomationFormFields values={values} onChange={setValues} lockIdAndTarget />

      {error ? <ErrorMessage message={error} /> : null}

      <FormFooter
        submitting={submitting}
        canSubmit={canSubmit}
        onCancel={() => navigate("/automations")}
      />
    </form>
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
