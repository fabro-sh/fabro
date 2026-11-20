import { useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { useSWRConfig } from "swr";
import { ChevronRightIcon } from "@heroicons/react/20/solid";

import { ApiError, apiData, environmentsApi } from "../lib/api-client";
import { queryKeys } from "../lib/query-keys";
import {
  EMPTY_ENVIRONMENT_FORM,
  EnvironmentFormFields,
  createRequestFromForm,
  isEnvironmentFormValid,
  parseCreatableProvider,
  type EnvironmentFormValues,
} from "../components/environment-form";
import {
  ErrorMessage,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "../components/ui";
import { useToast } from "../components/toast";

export const handle = { title: "New environment" };

export default function SettingsEnvironmentsNew() {
  return (
    <div className="space-y-6">
      <PageHeader />
      <CreateEnvironmentForm />
    </div>
  );
}

function PageHeader() {
  return (
    <nav className="flex items-center gap-1 text-sm text-fg-muted">
      <Link to="/settings/environments" className="text-fg-3 hover:text-fg">
        Environments
      </Link>
      <ChevronRightIcon className="size-3" aria-hidden="true" />
      <span>New environment</span>
    </nav>
  );
}

function CreateEnvironmentForm() {
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [searchParams] = useSearchParams();
  // The provider is selected via the "New environment" dropdown and arrives as
  // a query param; it's fixed for the lifetime of the environment.
  const [values, setValues] = useState<EnvironmentFormValues>(() => ({
    ...EMPTY_ENVIRONMENT_FORM,
    provider: parseCreatableProvider(searchParams.get("provider")),
  }));
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSubmit = isEnvironmentFormValid(values) && !submitting;

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    const id = values.id.trim();
    try {
      await apiData(() => environmentsApi.createEnvironment(createRequestFromForm(values)));
      await mutate(queryKeys.environments.list());
      toast.push({ message: `Environment “${id}” created.` });
      navigate("/settings/environments");
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.message
          ? cause.message
          : "Couldn't create the environment. Please try again.",
      );
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={onSubmit} className="space-y-6">
      <EnvironmentFormFields values={values} onChange={setValues} />

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
          {submitting ? "Creating…" : "Create environment"}
        </button>
      </div>
    </form>
  );
}
