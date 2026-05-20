import { useState } from "react";
import { Link } from "react-router";
import { useSWRConfig } from "swr";
import { PlusIcon } from "@heroicons/react/16/solid";
import type { SecretMetadata } from "@qltysh/fabro-api-client";

import { ApiError, apiData, secretsApi } from "../lib/api-client";
import { useSecrets } from "../lib/queries";
import { queryKeys } from "../lib/query-keys";
import {
  Badge,
  Panel,
  PanelSkeleton,
  SettingsPageIntro,
} from "../components/settings-panel";
import { COMPACT_SECONDARY_BUTTON_CLASS, ConfirmDialog } from "../components/ui";
import { useToast } from "../components/toast";
import { formatAbsoluteTs, formatRelativeTime } from "../lib/format";

export function meta() {
  return [{ title: "Secrets — Fabro" }];
}

const DESCRIPTION =
  "Secrets are values stored on this Fabro server and made available to workflow runs. Values are write-only — they can be replaced or deleted, but never read back through the UI.";

export default function SettingsSecrets() {
  const query = useSecrets();

  return (
    <div className="space-y-6">
      <SettingsPageIntro
        description={DESCRIPTION}
        action={
          <Link
            to="/settings/secrets/new"
            className="inline-flex items-center gap-1.5 rounded-md border border-line bg-panel/80 px-2.5 py-1 text-sm font-medium text-fg-3 transition-colors hover:border-line-strong hover:bg-panel hover:text-fg"
          >
            <PlusIcon className="size-3.5" aria-hidden="true" />
            New secret
          </Link>
        }
      />
      {query.data ? (
        <SecretsPanel secrets={query.data.data} />
      ) : query.error ? (
        <Panel title="Stored secrets">
          <div className="px-4 py-6 text-sm text-fg-2">
            Couldn&apos;t load secrets. Please try again.
          </div>
        </Panel>
      ) : (
        <PanelSkeleton />
      )}
    </div>
  );
}

function SecretsPanel({ secrets }: { secrets: SecretMetadata[] }) {
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [pendingDeleteName, setPendingDeleteName] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);

  async function confirmDelete() {
    if (!pendingDeleteName) return;
    const name = pendingDeleteName;
    setDeleting(true);
    try {
      await apiData(() => secretsApi.deleteSecretByName({ name }));
      await mutate(queryKeys.secrets.list());
      toast.push({ message: `Secret “${name}” deleted.` });
      setPendingDeleteName(null);
    } catch (cause) {
      toast.push({
        tone: "error",
        message:
          cause instanceof ApiError && cause.message
            ? cause.message
            : "Couldn't delete the secret. Please try again.",
      });
    } finally {
      setDeleting(false);
    }
  }

  return (
    <>
      <Panel title="Stored secrets">
        {secrets.length === 0 ? (
          <div className="px-4 py-6 text-sm text-fg-muted">
            No secrets stored yet.
          </div>
        ) : (
          secrets.map((secret) => (
            <SecretRow
              key={secret.name}
              secret={secret}
              disabled={deleting}
              onDelete={() => setPendingDeleteName(secret.name)}
            />
          ))
        )}
      </Panel>
      <ConfirmDialog
        open={pendingDeleteName !== null}
        title="Delete secret"
        description={
          <>
            Delete{" "}
            <span className="font-mono text-fg-2">{pendingDeleteName}</span>? Workflow
            runs that depend on it will no longer have access.
          </>
        }
        confirmLabel="Delete"
        pendingLabel="Deleting…"
        pending={deleting}
        onConfirm={confirmDelete}
        onCancel={() => {
          if (!deleting) setPendingDeleteName(null);
        }}
      />
    </>
  );
}

function SecretRow({
  secret,
  disabled,
  onDelete,
}: {
  secret: SecretMetadata;
  disabled: boolean;
  onDelete: () => void;
}) {
  return (
    <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-4 px-4 py-3.5">
      <div className="min-w-0">
        <div className="flex items-center gap-2">
          <span className="truncate font-mono text-sm text-fg" title={secret.name}>
            {secret.name}
          </span>
          <Badge>{secret.type}</Badge>
        </div>
        <div className="mt-0.5 text-xs/5 text-fg-3">
          {secret.description ? <span>{secret.description} · </span> : null}
          <span title={formatAbsoluteTs(secret.updated_at)}>
            Updated {formatRelativeTime(secret.updated_at)}
          </span>
        </div>
      </div>
      <button
        type="button"
        onClick={onDelete}
        disabled={disabled}
        aria-label={`Delete secret ${secret.name}`}
        className={COMPACT_SECONDARY_BUTTON_CLASS}
      >
        Delete
      </button>
    </div>
  );
}
