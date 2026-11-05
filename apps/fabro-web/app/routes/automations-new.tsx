import { useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { useSWRConfig } from "swr";
import { ChevronRightIcon } from "@heroicons/react/20/solid";
import type { Environment } from "@qltysh/fabro-api-client";

import { ApiError, apiData, automationsApi } from "../lib/api-client";
import { queryKeys } from "../lib/query-keys";
import {
  useEnvironments,
  useRun,
  useRunSettings,
  useRunState,
  useServerSettings,
} from "../lib/queries";
import {
  AutomationFormFields,
  EMPTY_AUTOMATION_FORM,
  automationFormValuesFromRun,
  isFormValid,
  targetFromFormValues,
  triggersFromFormValues,
  workflowSourceFromFormValues,
  type AutomationFormValues,
} from "../components/automation-form";
import {
  ErrorMessage,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "../components/ui";
import { useToast } from "../components/toast";

export function meta() {
  return [{ title: "New automation — Fabro" }];
}

export const handle = { hideHeader: true };

export default function AutomationsNew() {
  const [searchParams] = useSearchParams();
  const fromRunId = searchParams.get("from_run")?.trim() || undefined;
  const runQuery = useRun(fromRunId);
  const runStateQuery = useRunState(fromRunId);
  const settingsQuery = useRunSettings(fromRunId);
  const environmentsQuery = useEnvironments();
  const serverSettingsQuery = useServerSettings();
  const environments = environmentsQuery.data?.data;
  const environmentsPending = environmentsQuery.isLoading && !environmentsQuery.data;
  const environmentsError = Boolean(environmentsQuery.error);

  if (!fromRunId) {
    return (
      <AutomationCreateForm
        key="blank"
        initialValues={EMPTY_AUTOMATION_FORM}
        environments={environments}
        environmentsLoading={environmentsPending}
        environmentsError={environmentsError}
      />
    );
  }

  // Wait for both queries to settle before mounting the form, so the user's
  // edits aren't blown away when settings arrive after the run.
  const runPending = runQuery.isLoading && !runQuery.data;
  const runStatePending = runStateQuery.isLoading && !runStateQuery.data;
  const settingsPending = settingsQuery.isLoading && !settingsQuery.data;
  if (runPending || runStatePending || settingsPending || environmentsPending) {
    return (
      <div className="space-y-6">
        <PageHeader />
        <p className="rounded-lg bg-panel-alt px-4 py-3 text-sm text-fg-3">
          Loading source run…
        </p>
      </div>
    );
  }

  if (!runQuery.data) {
    return (
      <AutomationCreateForm
        key={`missing:${fromRunId}`}
        initialValues={EMPTY_AUTOMATION_FORM}
        environments={environments}
        environmentsError={environmentsError}
        sourceError="The source run could not be loaded. You can still fill it out manually."
      />
    );
  }

  const initialValues = automationFormValuesFromRun(
    runQuery.data,
    runStateQuery.data ?? null,
    settingsQuery.data ?? null,
    environments,
    serverSettingsQuery.data?.server.integrations.github.web_url,
  );

  return (
    <AutomationCreateForm
      key={`from-run:${fromRunId}`}
      initialValues={initialValues}
      environments={environments}
      environmentsError={environmentsError}
    />
  );
}

function AutomationCreateForm({
  initialValues,
  environments = [],
  environmentsLoading = false,
  environmentsError = false,
  sourceError = null,
}: {
  initialValues: AutomationFormValues;
  environments?: Environment[];
  environmentsLoading?: boolean;
  environmentsError?: boolean;
  sourceError?: string | null;
}) {
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [values, setValues] = useState<AutomationFormValues>(initialValues);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSubmit = isFormValid(values)
    && !environmentsLoading
    && !environmentsError
    && !submitting;

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    const trimmedName = values.name.trim();
    try {
      await apiData(() =>
        automationsApi.createAutomation({
          id:          values.id.trim(),
          name:        trimmedName,
          description: values.description.trim() || null,
          environment_id: values.environmentId.trim(),
          target:      targetFromFormValues(values),
          workflow:    values.workflow.trim(),
          workflow_source: workflowSourceFromFormValues(values),
          triggers: triggersFromFormValues(values),
        }),
      );
      await mutate(queryKeys.automations.list());
      toast.push({ message: `Automation “${trimmedName}” created.` });
      navigate("/automations");
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.message
          ? cause.message
          : "Couldn't create the automation. Please try again.",
      );
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={onSubmit} className="space-y-6">
      <PageHeader />

      <AutomationFormFields
        values={values}
        onChange={setValues}
        environments={environments}
        environmentsLoading={environmentsLoading}
        environmentsError={environmentsError}
      />

      {sourceError ? <ErrorMessage message={sourceError} /> : null}
      {error ? <ErrorMessage message={error} /> : null}

      <FormFooter
        submitting={submitting}
        canSubmit={canSubmit}
        onCancel={() => navigate("/automations")}
      />
    </form>
  );
}

function PageHeader() {
  return (
    <div>
      <nav className="mb-4 flex items-center gap-1 text-sm text-fg-muted">
        <Link to="/automations" className="text-fg-3 hover:text-fg">
          Automations
        </Link>
        <ChevronRightIcon className="size-3" aria-hidden="true" />
        <span>New automation</span>
      </nav>
      <h2 className="text-xl font-semibold text-fg">New automation</h2>
      <p className="mt-2 max-w-prose text-sm leading-relaxed text-fg-3">
        Define a workflow that Fabro can run on demand, on a schedule, or via the API.
        You can refine the graph and per-stage prompts after it's created.
      </p>
    </div>
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
        {submitting ? "Creating…" : "Create automation"}
      </button>
    </div>
  );
}
