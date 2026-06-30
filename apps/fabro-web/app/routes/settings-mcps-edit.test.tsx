import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type { McpServer } from "@qltysh/fabro-api-client";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter, Route, Routes } from "react-router";

import { setupReactTestEnv } from "../lib/test-utils";

let mcpServer: McpServer | null | undefined;
let teardownReactTestEnv: (() => void) | undefined;

mock.module("../lib/queries", () => ({
  useMcpServer: () => ({ data: mcpServer }),
}));

const { default: SettingsMcpsEdit } = await import("./settings-mcps-edit");

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function renderSettingsMcpsEdit() {
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter initialEntries={["/settings/mcps/github/edit"]}>
        <Routes>
          <Route path="/settings/mcps/:id/edit" element={<SettingsMcpsEdit />} />
        </Routes>
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

describe("SettingsMcpsEdit route", () => {
  beforeEach(() => {
    teardownReactTestEnv = setupReactTestEnv();
  });

  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    mcpServer = undefined;
    teardownReactTestEnv?.();
    teardownReactTestEnv = undefined;
  });

  test("renders the write-only value banner and blank existing env values", () => {
    mcpServer = {
      id:                   "github",
      revision:             "rev-1",
      display_name:         "GitHub MCP",
      description:          "GitHub tools",
      startup_timeout_secs: 10,
      tool_timeout_secs:    60,
      transport:            {
        type:     "stdio",
        command:  ["npx", "server"],
        env_keys: ["GITHUB_TOKEN"],
      },
    };

    const renderer = renderSettingsMcpsEdit();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("Existing environment variable and header values are write-only");

    const keyInputs = renderer.root.findAllByProps({ "aria-label": "Key" });
    const valueInputs = renderer.root.findAllByProps({ "aria-label": "Value" });
    expect(keyInputs.map((input) => input.props.value)).toContain("GITHUB_TOKEN");
    expect(valueInputs.map((input) => input.props.value)).toContain("");
  });
});
