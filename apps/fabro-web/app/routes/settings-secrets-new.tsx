import { useState, type ReactNode } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { useSWRConfig } from "swr";
import { ChevronRightIcon, ChevronDownIcon } from "@heroicons/react/20/solid";
import { SecretType } from "@qltysh/fabro-api-client";

import { ApiError, apiData, secretsApi } from "../lib/api-client";
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
      <PageHeader />
      <CreateSecretForm />
    </div>
  );
}

function PageHeader() {
  return (
    <nav className="flex items-center gap-1 text-sm text-fg-muted">
      <Link to="/settings/secrets" className="text-fg-3 hover:text-fg">
        Secrets
      </Link>
      <ChevronRightIcon className="size-3" aria-hidden="true" />
      <span>New secret</span>
    </nav>
  );
}

function CreateSecretForm() {
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [type, setType] = useState<typeof SecretType.TOKEN | typeof SecretType.FILE>(
    SecretType.TOKEN,
  );
  const [name, setName] = useState(() => searchParams.get("name") ?? "");
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
    <form onSubmit={onSubmit} className="space-y-6">
      <Panel title="Secret">
        <Row title={<Label required>Type</Label>} help="Tokens are exposed as environment variables; files are written to a path inside the sandbox.">
          <SelectInput
            name="type"
            value={type}
            onChange={(next) => {
              if (isFormType(next)) setType(next);
            }}
            options={[
              { value: SecretType.TOKEN, label: "Token (environment variable)" },
              { value: SecretType.FILE,  label: "File" },
            ]}
          />
        </Row>
        <Row title={<Label required>{nameLabel}</Label>} help={nameHelp}>
          <input
            type="text"
            name="name"
            aria-label={nameLabel}
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder={namePlaceholder}
            autoComplete="off"
            spellCheck={false}
            className={`${INPUT_CLASS} font-mono`}
          />
        </Row>
        <Row
          title={<Label required>Value</Label>}
          help="Stored as-is and never shown again. Replace or delete it later if it needs to change."
        >
          <textarea
            name="value"
            aria-label="Secret value"
            value={value}
            onChange={(event) => setValue(event.target.value)}
            rows={isFile ? 6 : 2}
            autoComplete="off"
            spellCheck={false}
            className={`${INPUT_CLASS} resize-y font-mono`}
          />
        </Row>
        <Row title={<Label optional>Description</Label>} help="Helps operators recognize what this secret is for.">
          <input
            type="text"
            name="description"
            aria-label="Secret description"
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
        onCancel={() => navigate("/settings/secrets")}
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

function SelectInput({
  name,
  value,
  onChange,
  options,
}: {
  name: string;
  value: string;
  onChange: (next: string) => void;
  options: ReadonlyArray<{ value: string; label: string }>;
}) {
  return (
    <div className="relative">
      <select
        name={name}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className={`${INPUT_CLASS} appearance-none pr-9`}
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
      <ChevronDownIcon
        className="pointer-events-none absolute right-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted"
        aria-hidden="true"
      />
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
        {submitting ? "Saving…" : "Save secret"}
      </button>
    </div>
  );
}
