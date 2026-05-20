import { useState } from "react";
import { useSWRConfig } from "swr";
import type { AuthSession } from "@qltysh/fabro-api-client";

import { ApiError, apiData, authApi } from "../lib/api-client";
import { useAuthSessions } from "../lib/queries";
import { queryKeys } from "../lib/query-keys";
import {
  Muted,
  Panel,
  PanelSkeleton,
  Row,
} from "../components/settings-panel";
import { COMPACT_SECONDARY_BUTTON_CLASS } from "../components/ui";
import { formatAbsoluteTs, formatRelativeTime } from "../lib/format";

export default function ProfileSessions() {
  const { data, error } = useAuthSessions();
  const { mutate } = useSWRConfig();
  const [revokingId, setRevokingId] = useState<string | null>(null);
  const [revokeError, setRevokeError] = useState<string | null>(null);

  if (error) {
    return (
      <div className="space-y-6">
        <Panel title="Sessions">
          <div className="px-4 py-6 text-sm text-fg-2">
            Couldn&apos;t load sessions. Please try again.
          </div>
        </Panel>
      </div>
    );
  }

  if (!data) {
    return (
      <div className="space-y-6">
        <PanelSkeleton />
        <PanelSkeleton />
      </div>
    );
  }

  const browser = data.sessions.find((s) => s.kind === "browser") ?? null;
  const cli = data.sessions
    .filter((s) => s.kind === "cli")
    .sort((a, b) => Date.parse(b.lastSeenAt) - Date.parse(a.lastSeenAt));

  async function revoke(id: string) {
    setRevokeError(null);
    setRevokingId(id);
    try {
      await apiData(() => authApi.deleteAuthSession(id));
      await mutate(queryKeys.auth.sessions());
    } catch (e) {
      const message =
        e instanceof ApiError && e.message
          ? e.message
          : "Couldn't revoke this session. Please try again.";
      setRevokeError(message);
    } finally {
      setRevokingId(null);
    }
  }

  return (
    <div className="space-y-6">
      <Panel title="Browser">
        {browser ? (
          <>
            <Row title="Signed in">
              <span title={formatAbsoluteTs(browser.createdAt)}>
                {formatRelativeTime(browser.createdAt)}
              </span>
            </Row>
            <Row title="Expires">{formatAbsoluteTs(browser.expiresAt)}</Row>
          </>
        ) : (
          <div className="px-4 py-6 text-sm text-fg-muted">
            No browser session.
          </div>
        )}
      </Panel>

      <Panel title="CLI">
        {cli.length === 0 ? (
          <div className="px-4 py-6 text-sm text-fg-muted">
            No CLI sessions.
          </div>
        ) : (
          cli.map((session) => (
            <CliRow
              key={session.id}
              session={session}
              onRevoke={revoke}
              pending={revokingId === session.id}
              disabled={revokingId !== null}
            />
          ))
        )}
      </Panel>

      {revokeError ? (
        <div
          role="alert"
          className="text-sm text-rose-400"
          data-testid="revoke-error"
        >
          {revokeError}
        </div>
      ) : null}
    </div>
  );
}

function CliRow({
  session,
  onRevoke,
  pending,
  disabled,
}: {
  session: AuthSession;
  onRevoke: (id: string) => void;
  pending: boolean;
  disabled: boolean;
}) {
  return (
    <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-4 px-4 py-3.5">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-fg-2">
        <span>
          <Muted>Last active</Muted>{" "}
          <span title={formatAbsoluteTs(session.lastSeenAt)}>
            {formatRelativeTime(session.lastSeenAt)}
          </span>
        </span>
        <span>
          <Muted>Expires</Muted> {formatAbsoluteTs(session.expiresAt)}
        </span>
      </div>
      <button
        type="button"
        onClick={() => onRevoke(session.id)}
        disabled={disabled}
        aria-label="Revoke CLI session"
        className={COMPACT_SECONDARY_BUTTON_CLASS}
      >
        {pending ? "Revoking…" : "Revoke"}
      </button>
    </div>
  );
}
