import type { ReactNode } from "react";
import {
  McpHttpProtocol,
  type CreateMcpServerRequest,
  type McpHttpProtocol as McpHttpProtocolValue,
  type McpServer,
  type McpTransport,
  type ReplaceMcpServerRequest,
} from "@qltysh/fabro-api-client";

import {
  looksLikeCredential,
  secretNameForKey,
  secretReference,
} from "../lib/credential-heuristics";
import { KeyValueEditor, mapFromEntries, type KeyValueEntry } from "./key-value-editor";
import { Badge, Panel, Row } from "./settings-panel";
import { INPUT_CLASS } from "./ui";

export type McpTransportKind = "stdio" | "http" | "sandbox";

export interface McpServerFormValues {
  id: string;
  displayName: string;
  description: string;
  startupTimeoutSecs: number;
  toolTimeoutSecs: number;
  transport: McpTransportKind;
  // stdio + sandbox
  command: string;
  // http + sandbox
  protocol: McpHttpProtocolValue;
  // http
  url: string;
  headers: KeyValueEntry[];
  // sandbox
  port: number;
  // stdio + sandbox env
  env: KeyValueEntry[];
}

export interface CredentialWarning {
  field: "env" | "headers";
  index: number;
}

const DEFAULT_PROTOCOL: McpHttpProtocolValue = McpHttpProtocol.STREAMABLE_HTTP;
const MCP_SERVER_ID_PATTERN = /^[a-z0-9][a-z0-9-]{0,62}$/;
const STARTUP_TIMEOUT_SECS = 10;
const TOOL_TIMEOUT_SECS = 60;
const DEFAULT_SANDBOX_PORT = 3000;

export const MCP_TRANSPORT_KINDS: readonly McpTransportKind[] = [
  "stdio",
  "http",
  "sandbox",
];

export function parseMcpTransportKind(value: string | null): McpTransportKind {
  return value === "http" || value === "sandbox" ? value : "stdio";
}

export function defaultMcpServerFormValues(kind: McpTransportKind): McpServerFormValues {
  return {
    id:                 "",
    displayName:        "",
    description:        "",
    startupTimeoutSecs: STARTUP_TIMEOUT_SECS,
    toolTimeoutSecs:    TOOL_TIMEOUT_SECS,
    transport:          kind,
    command:            "",
    protocol:           DEFAULT_PROTOCOL,
    url:                "",
    headers:            [],
    port:               DEFAULT_SANDBOX_PORT,
    env:                [],
  };
}

export function mcpServerToFormValues(server: McpServer): McpServerFormValues {
  const base = {
    ...defaultMcpServerFormValues(server.transport.type),
    id:                 server.id,
    displayName:        server.display_name,
    description:        server.description ?? "",
    startupTimeoutSecs: server.startup_timeout_secs,
    toolTimeoutSecs:    server.tool_timeout_secs,
  };

  switch (server.transport.type) {
    case "stdio":
      return {
        ...base,
        command: commandToInput(server.transport.command),
        env:     entriesFromKeys(server.transport.env_keys),
      };
    case "http":
      return {
        ...base,
        protocol: server.transport.protocol ?? DEFAULT_PROTOCOL,
        url:      server.transport.url,
        headers:  entriesFromKeys(server.transport.header_keys),
      };
    case "sandbox":
      return {
        ...base,
        protocol: server.transport.protocol ?? DEFAULT_PROTOCOL,
        command:  commandToInput(server.transport.command),
        port:     server.transport.port,
        env:      entriesFromKeys(server.transport.env_keys),
      };
  }
}

function entriesFromKeys(keys: string[]): KeyValueEntry[] {
  return keys.map((key) => ({ key, value: "" }));
}

function commandToInput(command: string[]): string {
  return command.join(" ");
}

export function createRequestFromForm(values: McpServerFormValues): CreateMcpServerRequest {
  return {
    id: values.id.trim(),
    ...settingsFromForm(values),
  };
}

export function replaceRequestFromForm(values: McpServerFormValues): ReplaceMcpServerRequest {
  return settingsFromForm(values);
}

function settingsFromForm(values: McpServerFormValues): ReplaceMcpServerRequest {
  return {
    display_name:         values.displayName.trim(),
    description:          values.description.trim() || null,
    transport:            transportFromForm(values),
    startup_timeout_secs: values.startupTimeoutSecs,
    tool_timeout_secs:    values.toolTimeoutSecs,
  };
}

function transportFromForm(values: McpServerFormValues): McpTransport {
  switch (values.transport) {
    case "stdio":
      return {
        type:    "stdio",
        command: commandFromInput(values.command),
        env:     mapFromEntries(values.env),
      };
    case "http":
      return {
        type:    "http",
        ...protocolProperty(values.protocol),
        url:     values.url.trim(),
        headers: mapFromEntries(values.headers),
      };
    case "sandbox":
      return {
        type:    "sandbox",
        ...protocolProperty(values.protocol),
        command: commandFromInput(values.command),
        port:    values.port,
        env:     mapFromEntries(values.env),
      };
  }
}

function protocolProperty(protocol: McpHttpProtocolValue): { protocol?: McpHttpProtocolValue } {
  return protocol === DEFAULT_PROTOCOL ? {} : { protocol };
}

function commandFromInput(command: string): string[] {
  return command.trim().split(/\s+/).filter(Boolean);
}

export function isMcpServerFormValid(
  values: McpServerFormValues,
  { isEdit }: { isEdit: boolean },
): boolean {
  if (!isEdit && !MCP_SERVER_ID_PATTERN.test(values.id.trim())) return false;
  if (values.displayName.trim() === "") return false;

  switch (values.transport) {
    case "stdio":
      if (values.command.trim() === "") return false;
      break;
    case "http":
      if (values.url.trim() === "") return false;
      break;
    case "sandbox":
      if (values.command.trim() === "") return false;
      if (!Number.isInteger(values.port) || values.port < 1 || values.port > 65_535) {
        return false;
      }
      break;
  }

  if (isEdit && !writeOnlyRowsHaveValues(values)) return false;
  return true;
}

function writeOnlyRowsHaveValues(values: McpServerFormValues): boolean {
  return activeValueEntries(values).every(
    (entry) => entry.key.trim() === "" || entry.value !== "",
  );
}

function activeValueEntries(values: McpServerFormValues): KeyValueEntry[] {
  return values.transport === "http" ? values.headers : values.env;
}

export function credentialWarnings(values: McpServerFormValues): CredentialWarning[] {
  const field = values.transport === "http" ? "headers" : "env";
  const entries = values.transport === "http" ? values.headers : values.env;
  return entries.flatMap((entry, index) =>
    looksLikeCredential(entry.key, entry.value) ? [{ field, index }] : [],
  );
}

interface McpServerFormFieldsProps {
  values: McpServerFormValues;
  onChange: (values: McpServerFormValues) => void;
  lockId?: boolean;
  lockTransport?: boolean;
}

export function McpServerFormFields({
  values,
  onChange,
  lockId = false,
  lockTransport = false,
}: McpServerFormFieldsProps) {
  function patch(partial: Partial<McpServerFormValues>) {
    onChange({ ...values, ...partial });
  }

  function setEntryValue(field: "env" | "headers", index: number, value: string) {
    patch({
      [field]: values[field].map((entry, i) => (i === index ? { ...entry, value } : entry)),
    });
  }

  const idValid = MCP_SERVER_ID_PATTERN.test(values.id.trim());
  const requireWriteOnlyValues = lockId && lockTransport;

  return (
    <>
      <Panel title="General">
        <Row
          title={<Label required>ID</Label>}
          help="Lowercase identifier (letters, digits, hyphens). Workflows enable this MCP server by id. Cannot be changed after creation."
        >
          {lockId ? (
            <div className="font-mono text-sm text-fg">{values.id}</div>
          ) : (
            <input
              type="text"
              name="id"
              aria-label="MCP server ID"
              value={values.id}
              onChange={(e) => patch({ id: e.target.value })}
              placeholder="github"
              autoComplete="off"
              spellCheck={false}
              className={`${INPUT_CLASS} font-mono`}
            />
          )}
        </Row>
        <Row title={<Label required>Display name</Label>} help="Human-readable name shown in this settings catalog.">
          <input
            type="text"
            name="display_name"
            aria-label="Display name"
            value={values.displayName}
            onChange={(e) => patch({ displayName: e.target.value })}
            placeholder="GitHub MCP"
            autoComplete="off"
            spellCheck={false}
            className={INPUT_CLASS}
          />
        </Row>
        <Row title={<Label optional>Description</Label>} help="Optional note to help operators recognize this server.">
          <input
            type="text"
            name="description"
            aria-label="Description"
            value={values.description}
            onChange={(e) => patch({ description: e.target.value })}
            className={INPUT_CLASS}
          />
        </Row>
        <Row
          title={<Label required>Transport</Label>}
          help="Choose how Fabro connects to this MCP server. The transport is fixed after creation."
        >
          {lockTransport ? (
            <Badge>{values.transport}</Badge>
          ) : (
            <select
              name="transport"
              aria-label="Transport"
              value={values.transport}
              onChange={(e) => patch({ transport: parseMcpTransportKind(e.target.value) })}
              className={INPUT_CLASS}
            >
              <option value="stdio">stdio</option>
              <option value="http">http</option>
              <option value="sandbox">sandbox</option>
            </select>
          )}
        </Row>
        <Row title="Startup timeout" help="Seconds to wait for the MCP server to become ready.">
          <input
            type="number"
            name="startup_timeout_secs"
            aria-label="Startup timeout"
            min={1}
            value={values.startupTimeoutSecs}
            onChange={(e) => patch({ startupTimeoutSecs: Number(e.target.value) })}
            className={`${INPUT_CLASS} font-mono`}
          />
        </Row>
        <Row title="Tool timeout" help="Seconds to allow each MCP tool call before timing out.">
          <input
            type="number"
            name="tool_timeout_secs"
            aria-label="Tool timeout"
            min={1}
            value={values.toolTimeoutSecs}
            onChange={(e) => patch({ toolTimeoutSecs: Number(e.target.value) })}
            className={`${INPUT_CLASS} font-mono`}
          />
        </Row>
      </Panel>

      <Panel title="Transport">
        {values.transport === "stdio" ? (
          <StdioTransportFields
            values={values}
            patch={patch}
            requireWriteOnlyValues={requireWriteOnlyValues}
            setEntryValue={setEntryValue}
          />
        ) : values.transport === "http" ? (
          <HttpTransportFields
            values={values}
            patch={patch}
            requireWriteOnlyValues={requireWriteOnlyValues}
            setEntryValue={setEntryValue}
          />
        ) : (
          <SandboxTransportFields
            values={values}
            patch={patch}
            requireWriteOnlyValues={requireWriteOnlyValues}
            setEntryValue={setEntryValue}
          />
        )}
      </Panel>

      {!lockId && values.id.trim() !== "" && !idValid ? (
        <p className="text-xs text-coral">
          ID must be lowercase letters, digits, or hyphens and start with a letter or digit.
        </p>
      ) : null}
    </>
  );
}

function StdioTransportFields({
  values,
  patch,
  requireWriteOnlyValues,
  setEntryValue,
}: TransportFieldsProps) {
  return (
    <>
      <Row title={<Label required>Command</Label>} help="Command and arguments used to launch the MCP server.">
        <input
          type="text"
          name="command"
          aria-label="Command"
          value={values.command}
          onChange={(e) => patch({ command: e.target.value })}
          placeholder="npx -y @modelcontextprotocol/server-github"
          autoComplete="off"
          spellCheck={false}
          className={`${INPUT_CLASS} font-mono`}
        />
      </Row>
      <KeyValueRows
        field="env"
        entries={values.env}
        onChange={(env) => patch({ env })}
        requireWriteOnlyValues={requireWriteOnlyValues}
        setEntryValue={setEntryValue}
      />
    </>
  );
}

function HttpTransportFields({
  values,
  patch,
  requireWriteOnlyValues,
  setEntryValue,
}: TransportFieldsProps) {
  return (
    <>
      <ProtocolRow values={values} patch={patch} />
      <Row title={<Label required>URL</Label>} help="Remote MCP endpoint URL.">
        <input
          type="text"
          name="url"
          aria-label="URL"
          value={values.url}
          onChange={(e) => patch({ url: e.target.value })}
          placeholder="https://example.com/mcp"
          autoComplete="off"
          spellCheck={false}
          className={`${INPUT_CLASS} font-mono`}
        />
      </Row>
      <KeyValueRows
        field="headers"
        entries={values.headers}
        onChange={(headers) => patch({ headers })}
        requireWriteOnlyValues={requireWriteOnlyValues}
        setEntryValue={setEntryValue}
      />
    </>
  );
}

function SandboxTransportFields({
  values,
  patch,
  requireWriteOnlyValues,
  setEntryValue,
}: TransportFieldsProps) {
  return (
    <>
      <ProtocolRow values={values} patch={patch} />
      <Row title={<Label required>Command</Label>} help="Command and arguments used to launch the MCP server inside the run sandbox.">
        <input
          type="text"
          name="command"
          aria-label="Command"
          value={values.command}
          onChange={(e) => patch({ command: e.target.value })}
          placeholder="python server.py"
          autoComplete="off"
          spellCheck={false}
          className={`${INPUT_CLASS} font-mono`}
        />
      </Row>
      <Row title={<Label required>Port</Label>} help="Port where the in-sandbox MCP server listens.">
        <input
          type="number"
          name="port"
          aria-label="Port"
          min={1}
          max={65_535}
          value={values.port}
          onChange={(e) => patch({ port: Number(e.target.value) })}
          className={`${INPUT_CLASS} font-mono`}
        />
      </Row>
      <KeyValueRows
        field="env"
        entries={values.env}
        onChange={(env) => patch({ env })}
        requireWriteOnlyValues={requireWriteOnlyValues}
        setEntryValue={setEntryValue}
      />
    </>
  );
}

interface TransportFieldsProps {
  values: McpServerFormValues;
  patch: (partial: Partial<McpServerFormValues>) => void;
  requireWriteOnlyValues: boolean;
  setEntryValue: (field: "env" | "headers", index: number, value: string) => void;
}

function ProtocolRow({
  values,
  patch,
}: {
  values: McpServerFormValues;
  patch: (partial: Partial<McpServerFormValues>) => void;
}) {
  return (
    <Row title="Protocol" help="HTTP wire protocol used for the MCP connection.">
      <select
        name="protocol"
        aria-label="Protocol"
        value={values.protocol}
        onChange={(e) => patch({ protocol: parseProtocol(e.target.value) })}
        className={INPUT_CLASS}
      >
        <option value={McpHttpProtocol.STREAMABLE_HTTP}>streamable_http</option>
        <option value={McpHttpProtocol.SSE}>sse</option>
      </select>
    </Row>
  );
}

function parseProtocol(value: string): McpHttpProtocolValue {
  return value === McpHttpProtocol.SSE ? McpHttpProtocol.SSE : DEFAULT_PROTOCOL;
}

function KeyValueRows({
  field,
  entries,
  onChange,
  requireWriteOnlyValues,
  setEntryValue,
}: {
  field: "env" | "headers";
  entries: KeyValueEntry[];
  onChange: (entries: KeyValueEntry[]) => void;
  requireWriteOnlyValues: boolean;
  setEntryValue: (field: "env" | "headers", index: number, value: string) => void;
}) {
  const isHeaders = field === "headers";
  return (
    <Row
      title={isHeaders ? "Headers" : "Environment variables"}
      help={isHeaders ? "HTTP headers sent with every request." : "Variables injected into the MCP server process."}
    >
      <KeyValueEditor
        entries={entries}
        onChange={onChange}
        keyPlaceholder={isHeaders ? "Authorization" : "GITHUB_TOKEN"}
        valuePlaceholder={isHeaders ? "Bearer token" : "{{ secrets.GITHUB_TOKEN }}"}
        addLabel={isHeaders ? "Add header" : "Add variable"}
        renderEntryHint={(entry, index) => (
          <EntryHint
            field={field}
            index={index}
            entry={entry}
            requireWriteOnlyValue={requireWriteOnlyValues}
            setEntryValue={setEntryValue}
          />
        )}
      />
    </Row>
  );
}

function EntryHint({
  field,
  index,
  entry,
  requireWriteOnlyValue,
  setEntryValue,
}: {
  field: "env" | "headers";
  index: number;
  entry: KeyValueEntry;
  requireWriteOnlyValue: boolean;
  setEntryValue: (field: "env" | "headers", index: number, value: string) => void;
}) {
  const missingWriteOnlyValue = requireWriteOnlyValue && entry.key.trim() !== "" && entry.value === "";
  const credentialWarning = looksLikeCredential(entry.key, entry.value);
  if (!missingWriteOnlyValue && !credentialWarning) return null;

  const secretName = secretNameForKey(entry.key);
  return (
    <div className="ml-0.5 space-y-1 text-xs/5">
      {missingWriteOnlyValue ? (
        <p className="text-coral">
          Enter a value for this existing write-only setting, or remove the row before saving.
        </p>
      ) : null}
      {credentialWarning ? (
        <p className="text-amber">
          This looks like a credential.{" "}
          <button
            type="button"
            onClick={() => {
              setEntryValue(field, index, secretReference(secretName));
              openSecretCreateTab(secretName);
            }}
            className="font-medium text-amber underline underline-offset-2 hover:text-fg"
          >
            Store as secret
          </button>
        </p>
      ) : null}
    </div>
  );
}

function openSecretCreateTab(secretName: string) {
  if (typeof window === "undefined") return;
  window.open(
    `/settings/secrets/new?name=${encodeURIComponent(secretName)}`,
    "_blank",
    "noopener,noreferrer",
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
