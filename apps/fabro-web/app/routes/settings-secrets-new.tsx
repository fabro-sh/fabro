import { useState } from "react";
import { Link, useNavigate } from "react-router";
import { useSWRConfig } from "swr";
import { ArrowLeftIcon } from "@heroicons/react/16/solid";
import { SecretType } from "@qltysh/fabro-api-client";

import { ApiError, apiData, secretsApi } from "../lib/api-client";
import { queryKeys } from "../lib/query-keys";
import { Panel, SettingsPageIntro } from "../components/settings-panel";
import {
  ErrorMessage,
  FormField,
  INPUT_CLASS,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "../components/ui";
import { useToast } from "../components/toast";

export function meta() {
  return [{ title: "New secret — Fabro" }];
}

// The create form only offers Token and File. OAuth secrets are written by
// provider sign-in flows, never typed by hand.
function isFormType(
  value: string,
): value is typeof SecretType.TOKEN | typeof SecretType.FILE {
  return value === SecretType.TOKEN || value === SecretType.FILE;
}

export default function SettingsSecretsNew() {
  return (
    <div className="space-y-6">
      <Link
        to="/settings/secrets"
        className="inline-flex items-center gap-1 text-sm text-fg-3 transition-colors hover:text-fg"
      >
        <ArrowLeftIcon className="size-4" aria-hidden="true" />
        Back to secrets
      </Link>
      <SettingsPageIntro description="Store a new token or file secret on this Fabro server. The value is write-only — it can be replaced or deleted later, but never read back through the UI." />
      <CreateSecretForm />
    </div>
  );
}

function CreateSecretForm() {
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [type, setType] = useState<typeof SecretType.TOKEN | typeof SecretType.FILE>(
    SecretType.TOKEN,
  );
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [description, setDescription] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const isFile = type === SecretType.FILE;
  const nameLabel = isFile ? "Destination path" : "Name";
  const nameHelp = isFile
    ? "Absolute path where the file is written inside the run sandbox."
    : "Environment variable name exposed to runs (letters, digits, underscores).";
  const namePlaceholder = isFile ? "/home/fabro/.netrc" : "OPENAI_API_KEY";
  const canSubmit = name.trim() !== "" && value !== "" && !submitting;

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    const trimmedName = name.trim();
    try {
      await apiData(() =>
        secretsApi.createSecret({
          name: trimmedName,
          value,
          type,
          description: description.trim() || undefined,
        }),
      );
      await mutate(queryKeys.secrets.list());
      toast.push({ message: `Secret “${trimmedName}” saved.` });
      navigate("/settings/secrets");
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.message
          ? cause.message
          : "Couldn't save the secret. Please try again.",
      );
      setSubmitting(false);
    }
  }

  return (
    <Panel title="New secret">
      <form onSubmit={onSubmit} className="space-y-4 px-4 py-4">
        <div className="grid gap-4 sm:grid-cols-2">
          <FormField label="Type" htmlFor="secret-type">
            <select
              id="secret-type"
              value={type}
              onChange={(event) => {
                if (isFormType(event.target.value)) setType(event.target.value);
              }}
              className={INPUT_CLASS}
            >
              <option value={SecretType.TOKEN}>Token (environment variable)</option>
              <option value={SecretType.FILE}>File</option>
            </select>
          </FormField>
          <FormField label={nameLabel} htmlFor="secret-name" help={nameHelp}>
            <input
              id="secret-name"
              type="text"
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder={namePlaceholder}
              autoComplete="off"
              spellCheck={false}
              className={`${INPUT_CLASS} font-mono`}
            />
          </FormField>
        </div>
        <FormField
          label="Value"
          htmlFor="secret-value"
          help={
            isFile
              ? "File contents. Stored as-is and never shown again."
              : "The secret value. Stored as-is and never shown again."
          }
        >
          <textarea
            id="secret-value"
            value={value}
            onChange={(event) => setValue(event.target.value)}
            rows={isFile ? 4 : 2}
            autoComplete="off"
            spellCheck={false}
            className={`${INPUT_CLASS} resize-y font-mono`}
          />
        </FormField>
        <FormField
          label="Description"
          htmlFor="secret-description"
          help="Optional. Helps operators recognize what this secret is for."
        >
          <input
            id="secret-description"
            type="text"
            value={description}
            onChange={(event) => setDescription(event.target.value)}
            placeholder="Optional"
            className={INPUT_CLASS}
          />
        </FormField>
        {error ? <ErrorMessage message={error} /> : null}
        <div className="flex justify-end gap-2">
          <button
            type="button"
            onClick={() => navigate("/settings/secrets")}
            disabled={submitting}
            className={SECONDARY_BUTTON_CLASS}
          >
            Cancel
          </button>
          <button type="submit" disabled={!canSubmit} className={PRIMARY_BUTTON_CLASS}>
            {submitting ? "Saving…" : "Save secret"}
          </button>
        </div>
      </form>
    </Panel>
  );
}
