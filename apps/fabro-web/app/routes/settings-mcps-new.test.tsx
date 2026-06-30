import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter } from "react-router";

import { setupReactTestEnv } from "../lib/test-utils";

const { default: SettingsMcpsNew } = await import("./settings-mcps-new");

let teardownReactTestEnv: (() => void) | undefined;
const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function renderSettingsMcpsNew(initialEntry = "/settings/mcps/new") {
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter initialEntries={[initialEntry]}>
        <SettingsMcpsNew />
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

describe("SettingsMcpsNew route", () => {
  beforeEach(() => {
    teardownReactTestEnv = setupReactTestEnv();
  });

  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    teardownReactTestEnv?.();
    teardownReactTestEnv = undefined;
  });

  test("renders the create form for the requested transport", () => {
    const renderer = renderSettingsMcpsNew("/settings/mcps/new?type=http");
    const text = textContent(renderer.toJSON());

    expect(text).toContain("New MCP server");
    expect(renderer.root.findByProps({ "aria-label": "Transport" }).props.value)
      .toBe("http");
    expect(renderer.root.findByProps({ "aria-label": "URL" })).toBeDefined();
  });
});
