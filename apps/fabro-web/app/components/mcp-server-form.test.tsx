import { describe, expect, test } from "bun:test";
import type { McpServer } from "@qltysh/fabro-api-client";

import {
  credentialWarnings,
  createRequestFromForm,
  defaultMcpServerFormValues,
  isMcpServerFormValid,
  mcpServerToFormValues,
  replaceRequestFromForm,
  type McpServerFormValues,
} from "./mcp-server-form";

function values(overrides: Partial<McpServerFormValues> = {}): McpServerFormValues {
  return {
    ...defaultMcpServerFormValues("stdio"),
    id:          "github",
    displayName: "GitHub MCP",
    command:     "npx -y @modelcontextprotocol/server-github",
    ...overrides,
  };
}

function server(overrides: Partial<McpServer> = {}): McpServer {
  return {
    id:                   "github",
    revision:             "rev-1",
    display_name:         "GitHub MCP",
    description:          "Tools for GitHub",
    startup_timeout_secs: 10,
    tool_timeout_secs:    60,
    transport:            {
      type:     "stdio",
      command:  ["npx", "server"],
      env_keys: ["GITHUB_TOKEN"],
    },
    ...overrides,
  };
}

describe("MCP server form helpers", () => {
  test("uses MCP server defaults", () => {
    expect(defaultMcpServerFormValues("http")).toMatchObject({
      transport:          "http",
      protocol:           "streamable_http",
      startupTimeoutSecs: 10,
      toolTimeoutSecs:    60,
      headers:            [],
      env:                [],
    });
  });

  test("maps read models to form values with write-only values left empty", () => {
    const form = mcpServerToFormValues(server({
      transport: {
        type:        "http",
        protocol:    "sse",
        url:         "https://example.com/mcp",
        header_keys: ["Authorization", "X-API-Key"],
      },
    }));

    expect(form).toMatchObject({
      id:          "github",
      displayName: "GitHub MCP",
      description: "Tools for GitHub",
      transport:   "http",
      protocol:    "sse",
      url:         "https://example.com/mcp",
      headers:     [
        { key: "Authorization", value: "" },
        { key: "X-API-Key", value: "" },
      ],
    });
  });

  test("builds stdio create requests", () => {
    const request = createRequestFromForm(values({
      id:          " github ",
      displayName: " GitHub MCP ",
      description: " Tools ",
      command:     "npx server --flag",
      env:         [
        { key: " GITHUB_TOKEN ", value: "{{ secrets.GITHUB_TOKEN }}" },
        { key: "", value: "ignored" },
      ],
    }));

    expect(request).toEqual({
      id:                   "github",
      display_name:         "GitHub MCP",
      description:          "Tools",
      startup_timeout_secs: 10,
      tool_timeout_secs:    60,
      transport:            {
        type:    "stdio",
        command: ["npx", "server", "--flag"],
        env:     { GITHUB_TOKEN: "{{ secrets.GITHUB_TOKEN }}" },
      },
    });
  });

  test("builds http replace requests and omits the default protocol", () => {
    const request = replaceRequestFromForm(values({
      transport: "http",
      protocol:  "streamable_http",
      url:       " https://example.com/mcp ",
      headers:   [{ key: " Authorization ", value: "Bearer token" }],
    }));

    expect(request.transport).toEqual({
      type:    "http",
      url:     "https://example.com/mcp",
      headers: { Authorization: "Bearer token" },
    });
  });

  test("builds sandbox requests with an explicit non-default protocol", () => {
    const request = replaceRequestFromForm(values({
      transport: "sandbox",
      protocol:  "sse",
      command:   "python server.py",
      port:      7777,
      env:       [{ key: "NODE_ENV", value: "production" }],
    }));

    expect(request.transport).toEqual({
      type:     "sandbox",
      protocol: "sse",
      command:  ["python", "server.py"],
      port:     7777,
      env:      { NODE_ENV: "production" },
    });
  });

  test("validates create fields per transport", () => {
    expect(isMcpServerFormValid(values({ id: "bad_id" }), { isEdit: false }))
      .toBe(false);
    expect(isMcpServerFormValid(values({ displayName: "" }), { isEdit: false }))
      .toBe(false);
    expect(isMcpServerFormValid(values({ transport: "stdio", command: "" }), { isEdit: false }))
      .toBe(false);
    expect(isMcpServerFormValid(values({ transport: "http", url: "" }), { isEdit: false }))
      .toBe(false);
    expect(isMcpServerFormValid(values({ transport: "sandbox", port: 0 }), { isEdit: false }))
      .toBe(false);
    expect(isMcpServerFormValid(values({ transport: "sandbox", port: 65535 }), { isEdit: false }))
      .toBe(true);
  });

  test("requires values for existing write-only rows on edit", () => {
    const editValues = values({
      env: [{ key: "GITHUB_TOKEN", value: "" }],
    });

    expect(isMcpServerFormValid(editValues, { isEdit: false }))
      .toBe(true);
    expect(isMcpServerFormValid(editValues, { isEdit: true }))
      .toBe(false);
    expect(
      isMcpServerFormValid(
        { ...editValues, env: [{ key: "GITHUB_TOKEN", value: "{{ secrets.GITHUB_TOKEN }}" }] },
        { isEdit: true },
      ),
    ).toBe(true);
  });

  test("reports credential warnings for the active key-value field only", () => {
    expect(
      credentialWarnings(values({
        env:     [{ key: "API_KEY", value: "literal-secret" }],
        headers: [{ key: "Authorization", value: "Bearer literal-secret" }],
      })),
    ).toEqual([{ field: "env", index: 0 }]);

    expect(
      credentialWarnings(values({
        transport: "http",
        env:       [{ key: "API_KEY", value: "literal-secret" }],
        headers:   [{ key: "Authorization", value: "Bearer literal-secret" }],
      })),
    ).toEqual([{ field: "headers", index: 0 }]);
  });

  test("round-trips editable values while documenting omitted write-only values", () => {
    const form = mcpServerToFormValues(server({
      transport: {
        type:     "sandbox",
        command:  ["node", "server.js"],
        port:     3000,
        env_keys: ["API_KEY"],
      },
    }));
    const request = replaceRequestFromForm(form);

    expect(form.id).toBe("github");
    expect(form.env).toEqual([{ key: "API_KEY", value: "" }]);
    expect(request.display_name).toBe("GitHub MCP");
    expect(request.transport).toEqual({
      type:    "sandbox",
      command: ["node", "server.js"],
      port:    3000,
      env:     { API_KEY: "" },
    });
  });
});
