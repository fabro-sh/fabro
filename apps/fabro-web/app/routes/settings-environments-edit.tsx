import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useSWRConfig } from "swr";
import { ChevronRightIcon } from "@heroicons/react/20/solid";
import type { Environment } from "@qltysh/fabro-api-client";

import { ApiError, apiData, environmentsApi } from "../lib/api-client";
import { queryKeys } from "../lib/query-keys";
import { useEnvironment } from "../lib/queries";
import {
  EnvironmentFormFields,
  environmentToFormValues,
  isEnvironmentFormValid,
  replaceRequestFromForm,
  type EnvironmentFormValues,
} from "../components/environment-form";
import { Panel, PanelSkeleton } from "../components/settings-panel";
import {
  ErrorMessage,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "../components/ui";
import { useToast } from "../components/toast";

export const handle = { title: "Edit environment" };

export default function SettingsEnvironmentsEdit() {
  const { id } = useParams<{ id: string }>();
  const query = useEnvironment(id);

  return (
    <div className="space-y-6">
      <PageHeader id={id ?? ""} />
      {query.data ? (
        <EditEnvironmentForm key={query.data.revision} environment={query.data} />
      ) : query.error ? (
        <Panel title="Environment">
          <div className="px-4 py-6 text-sm text-fg-2">
            Couldn&apos;t load this environment. It may have been deleted.
          </div>
        </Panel>
      ) : (
        <PanelSkeleton />
      )}
    </div>
  );
}

function PageHeader({ id }: { id: string }) {
  return (
    <nav className="flex items-center gap-1 text-sm text-fg-muted">
      <Link to="/settings/environments" className="text-fg-3 hover:text-fg">
        Environments
      </Link>
      <ChevronRightIcon className="size-3" aria-hidden="true" />
      <span className="font-mono text-fg-2">{id}</span>
    </nav>
  );
}

function EditEnvironmentForm({ environment }: { environment: Environment }) {
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [values, setValues] = useState<EnvironmentFormValues>(() =>
    environmentToFormValues(environment),
  );
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSubmit = isEnvironmentFormValid(values) && !submitting;

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    try {
      await apiData(() =>
        environmentsApi.replaceEnvironment(
          environment.id,
          environment.revision,
          replaceRequestFromForm(values),
        ),
      );
      await mutate(queryKeys.environments.list());
      await mutate(queryKeys.environments.detail(environment.id));
      toast.push({ message: `Environment “${environment.id}” updated.` });
      navigate("/settings/environments");
    } catch (cause) {
      setError(staleAwareMessage(cause));
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={onSubmit} className="space-y-6">
      <EnvironmentFormFields values={values} onChange={setValues} lockId />

      {error ? <ErrorMessage message={error} /> : null}

      <div className="flex items-center justify-end gap-3 pt-2">
        <button
          type="button"
          onClick={() => navigate("/settings/environments")}
          disabled={submitting}
          className={SECONDARY_BUTTON_CLASS}
        >
          Cancel
        </button>
        <button type="submit" disabled={!canSubmit} className={PRIMARY_BUTTON_CLASS}>
          {submitting ? "Saving…" : "Save changes"}
        </button>
      </div>
    </form>
  );
}

function staleAwareMessage(cause: unknown): string {
  if (cause instanceof ApiError && cause.status === 409) {
    return "This environment changed since you opened it. Reload the page to get the latest version, then reapply your edits.";
  }
  if (cause instanceof ApiError && cause.message) {
    return cause.message;
  }
  return "Couldn't update the environment. Please try again.";
}
