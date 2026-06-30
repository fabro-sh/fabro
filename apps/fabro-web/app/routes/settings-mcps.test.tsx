import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type { McpServerListResponse } from "@qltysh/fabro-api-client";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter } from "react-router";

import { setupReactTestEnv } from "../lib/test-utils";

let mcpServers: McpServerListResponse | undefined;
let teardownReactTestEnv: (() => void) | undefined;

mock.module("../lib/queries", () => ({
  useMcpServers: () => ({ data: mcpServers }),
}));

const { default: SettingsMcps } = await import("./settings-mcps");

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function renderSettingsMcps() {
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter initialEntries={["/settings/mcps"]}>
        <SettingsMcps />
      </MemoryRouter>,
    );
  });
  mountedRenderers.push(renderer!);
  return renderer!;
}

function textContent(node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textContent).join("");
  return node.children?.map(textContent).join("") ?? "";
}

describe("SettingsMcps route", () => {
  beforeEach(() => {
    teardownReactTestEnv = setupReactTestEnv();
  });

  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    mcpServers = undefined;
    teardownReactTestEnv?.();
    teardownReactTestEnv = undefined;
  });

  test("renders MCP server rows", () => {
    mcpServers = {
      data: [
        {
          id:                   "github",
          revision:             "rev-1",
          display_name:         "GitHub MCP",
          description:          "GitHub tools",
          startup_timeout_secs: 10,
          tool_timeout_secs:    60,
          transport:            {
            type:        "http",
            url:         "https://example.com/mcp",
            header_keys: ["Authorization"],
          },
        },
        {
          id:                   "filesystem",
          revision:             "rev-2",
          display_name:         "Filesystem MCP",
          description:          null,
          startup_timeout_secs: 10,
          tool_timeout_secs:    60,
          transport:            {
            type:     "stdio",
            command:  ["npx", "server"],
            env_keys: [],
          },
        },
      ],
      meta: { total: 2 },
    };

    const renderer = renderSettingsMcps();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("GitHub MCP");
    expect(text).toContain("github");
    expect(text).toContain("http");
    expect(text).toContain("Filesystem MCP");
    expect(text).toContain("stdio");
  });

  test("renders an empty state", () => {
    mcpServers = { data: [], meta: { total: 0 } };

    const renderer = renderSettingsMcps();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("No MCP servers defined yet.");
  });
});
