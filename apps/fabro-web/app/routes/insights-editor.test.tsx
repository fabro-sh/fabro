import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { createMemoryRouter, RouterProvider } from "react-router";

import { setupReactTestEnv } from "../lib/test-utils";
import InsightsEditor from "./insights-editor";

let teardownReactEnv: (() => void) | undefined;
let originalResizeObserver: typeof ResizeObserver | undefined;
const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function textFromNode(
  node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>,
): string {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(textFromNode).join(" ");
  return (node.children ?? []).map(textFromNode).join(" ");
}

function textFromTestNode(node: TestRenderer.ReactTestInstance): string {
  return node.children.map((child) => {
    if (typeof child === "string") return child;
    if (typeof child === "number") return String(child);
    return textFromTestNode(child);
  }).join("");
}

function buttonByText(renderer: TestRenderer.ReactTestRenderer, text: string) {
  return renderer.root.findAll(
    (node) => node.type === "button" && textFromTestNode(node).includes(text),
  )[0];
}

async function renderInsightsEditor() {
  const router = createMemoryRouter(
    [{ path: "/insights", element: <InsightsEditor /> }],
    { initialEntries: ["/insights"] },
  );
  let renderer!: TestRenderer.ReactTestRenderer;
  await act(async () => {
    renderer = TestRenderer.create(<RouterProvider router={router} />);
  });
  mountedRenderers.push(renderer);
  return renderer;
}

beforeEach(() => {
  teardownReactEnv = setupReactTestEnv();
  originalResizeObserver = globalThis.ResizeObserver;
  globalThis.ResizeObserver = class ResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as typeof ResizeObserver;
});

afterEach(() => {
  for (const renderer of mountedRenderers.splice(0)) {
    act(() => renderer.unmount());
  }
  if (originalResizeObserver) {
    globalThis.ResizeObserver = originalResizeObserver;
  } else {
    delete (globalThis as { ResizeObserver?: typeof ResizeObserver }).ResizeObserver;
  }
  teardownReactEnv?.();
  teardownReactEnv = undefined;
});

describe("InsightsEditor", () => {
  test("keeps the newest query result when an earlier run finishes later", async () => {
    const renderer = await renderInsightsEditor();
    const originalSetTimeout = globalThis.setTimeout;
    const timers: Array<() => void> = [];
    globalThis.setTimeout = ((callback: TimerHandler) => {
      if (typeof callback === "function") {
        timers.push(callback as () => void);
      }
      return timers.length as unknown as ReturnType<typeof setTimeout>;
    }) as typeof setTimeout;

    try {
      await act(async () => {
        buttonByText(renderer, "Table")!.props.onClick();
      });

      const textarea = renderer.root.findByProps({ "aria-label": "SQL query" });
      await act(async () => {
        textarea.props.onChange({ target: { value: "SELECT * FROM runs" } });
      });

      await act(async () => {
        buttonByText(renderer, "Run")!.props.onClick();
      });

      const updatedTextarea = renderer.root.findByProps({ "aria-label": "SQL query" });
      await act(async () => {
        updatedTextarea.props.onChange({
          target: { value: "SELECT failed FROM runs" },
        });
      });

      const latestTextarea = renderer.root.findByProps({ "aria-label": "SQL query" });
      await act(async () => {
        latestTextarea.props.onKeyDown({
          key:            "Enter",
          metaKey:        true,
          ctrlKey:        false,
          preventDefault: () => undefined,
        });
      });

      expect(timers).toHaveLength(2);

      await act(async () => {
        timers[1]!();
      });
      expect(textFromNode(renderer.toJSON())).toContain("failure_rate");

      await act(async () => {
        timers[0]!();
      });
      const text = textFromNode(renderer.toJSON());
      expect(text).toContain("failure_rate");
      expect(text).not.toContain("total_additions");
    } finally {
      globalThis.setTimeout = originalSetTimeout;
    }
  });
});
