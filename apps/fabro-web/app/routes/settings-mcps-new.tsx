import { useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { useSWRConfig } from "swr";
import { ChevronRightIcon } from "@heroicons/react/20/solid";

import {
  McpServerFormFields,
  createRequestFromForm,
  defaultMcpServerFormValues,
  isMcpServerFormValid,
  type McpServerFormValues,
} from "../components/mcp-server-form";
import {
  ErrorMessage,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "../components/ui";
import { useToast } from "../components/toast";
import { ApiError, apiData, mcpServersApi } from "../lib/api-client";
import { upsertMcpServerInList } from "../lib/mcp-server-cache";
import { parseMcpTransportKind } from "../lib/mcp-transport-kinds";
import { queryKeys } from "../lib/query-keys";

export function meta() {
  return [{ title: "New MCP server — Fabro" }];
}

export default function SettingsMcpsNew() {
  return (
    <div className="space-y-6">
      <PageHeader />
      <CreateMcpServerForm />
    </div>
  );
}

function PageHeader() {
  return (
    <nav className="flex items-center gap-1 text-sm text-fg-muted">
      <Link to="/settings/mcps" className="text-fg-3 hover:text-fg">
        MCP servers
      </Link>
      <ChevronRightIcon className="size-3" aria-hidden="true" />
      <span>New MCP server</span>
    </nav>
  );
}

function CreateMcpServerForm() {
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [searchParams] = useSearchParams();
  const [values, setValues] = useState<McpServerFormValues>(() =>
    defaultMcpServerFormValues(parseMcpTransportKind(searchParams.get("type"))),
  );
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSubmit = isMcpServerFormValid(values, { isEdit: false }) && !submitting;

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    const id = values.id.trim();
    try {
      const created = await apiData(() =>
        mcpServersApi.createMcpServer(createRequestFromForm(values)),
      );
      await mutate(
        queryKeys.mcpServers.list(),
        (current) => upsertMcpServerInList(current, created),
        { revalidate: false },
      );
      toast.push({ message: `MCP server “${id}” created.` });
      navigate("/settings/mcps");
      void mutate(queryKeys.mcpServers.list());
    } catch (cause) {
      setError(
        cause instanceof ApiError && cause.message
          ? cause.message
          : "Couldn't create the MCP server. Please try again.",
      );
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={onSubmit} className="space-y-6">
      <McpServerFormFields values={values} onChange={setValues} />

      {error ? <ErrorMessage message={error} /> : null}

      <div className="flex items-center justify-end gap-3 pt-2">
        <button
          type="button"
          onClick={() => navigate("/settings/mcps")}
          disabled={submitting}
          className={SECONDARY_BUTTON_CLASS}
        >
          Cancel
        </button>
        <button type="submit" disabled={!canSubmit} className={PRIMARY_BUTTON_CLASS}>
          {submitting ? "Creating…" : "Create MCP server"}
        </button>
      </div>
    </form>
  );
}
