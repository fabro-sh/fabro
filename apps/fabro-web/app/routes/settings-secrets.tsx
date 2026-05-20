import { useState } from "react";
import { useSWRConfig } from "swr";
import type { SecretMetadata } from "@qltysh/fabro-api-client";

import { ApiError, apiData, secretsApi } from "../lib/api-client";
import { useSecrets } from "../lib/queries";
import { queryKeys } from "../lib/query-keys";
import { Badge, Panel, PanelSkeleton } from "../components/settings-panel";
import { ConfirmDialog } from "../components/ui";
import { useToast } from "../components/toast";
import { formatAbsoluteTs, formatRelativeTime } from "../lib/format";

export function meta() {
  return [{ title: "Secrets — Fabro" }];
}

export const handle = {
  description:
    "Secrets are values stored on this Fabro server and made available to workflow runs. Values are write-only — they can be replaced or deleted, but never read back through the UI.",
  headerAction: { to: "/settings/secrets/new", label: "New secret" },
};

export default function SettingsSecrets() {
  const query = useSecrets();

  return (
    <div className="space-y-6">
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
  const [pendingDelete, setPendingDelete] = useState<SecretMetadata | null>(null);
  const [deleting, setDeleting] = useState(false);

  const sorted = [...secrets].sort((a, b) => a.name.localeCompare(b.name));

  async function confirmDelete() {
    if (!pendingDelete) return;
    const target = pendingDelete;
    setDeleting(true);
    try {
      await apiData(() => secretsApi.deleteSecretByName({ name: target.name }));
      await mutate(queryKeys.secrets.list());
      toast.push({ message: `Secret “${target.name}” deleted.` });
      setPendingDelete(null);
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
        {sorted.length === 0 ? (
          <div className="px-4 py-6 text-sm text-fg-muted">
            No secrets stored yet.
          </div>
        ) : (
          sorted.map((secret) => (
            <SecretRow
              key={secret.name}
              secret={secret}
              disabled={deleting}
              onDelete={() => setPendingDelete(secret)}
            />
          ))
        )}
      </Panel>
      <ConfirmDialog
        open={pendingDelete !== null}
        title="Delete secret"
        description={
          <>
            Delete{" "}
            <span className="font-mono text-fg-2">{pendingDelete?.name}</span>? Workflow
            runs that depend on it will no longer have access.
          </>
        }
        confirmLabel="Delete"
        pendingLabel="Deleting…"
        pending={deleting}
        onConfirm={confirmDelete}
        onCancel={() => {
          if (!deleting) setPendingDelete(null);
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
        className="rounded-md border border-line bg-overlay px-2.5 py-1 text-xs text-fg-2 transition-colors hover:bg-overlay-strong hover:text-fg disabled:cursor-not-allowed disabled:opacity-50"
      >
        Delete
      </button>
    </div>
  );
}
