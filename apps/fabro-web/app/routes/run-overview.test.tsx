import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter, Route, Routes } from "react-router";

import { ApiError } from "../lib/api-client";
import { setupReactTestEnv } from "../lib/test-utils";

let currentGraphData: string | null | undefined;
let currentGraphError: Error | undefined;
let currentGraphLoading = false;

const graphMutateMock = mock(() => Promise.resolve(currentGraphData));

mock.module("../lib/queries", () => ({
  useRun: () => ({ data: undefined }),
  useRunStages: () => ({ data: undefined }),
  useRunGraph: () => ({
    data:      currentGraphData,
    error:     currentGraphError,
    isLoading: currentGraphLoading,
    mutate:    graphMutateMock,
  }),
  useRunGraphSource: () => ({ data: undefined }),
  useRunState: () => ({ data: undefined }),
  useRunStream: () => ({ data: undefined }),
}));

mock.module("../components/run-summary-panel", () => ({
  RunSummaryPanel: () => <div>summary</div>,
}));

mock.module("../components/stage-sidebar", () => ({
  StageSidebar: () => <nav>stages</nav>,
}));

const { default: RunOverview } = await import("./run-overview");

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function textFromNode(
  node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>,
): string {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(textFromNode).join(" ");
  return (node.children ?? []).map(textFromNode).join(" ");
}

// Zooming writes to useRememberedGraphView's module-scoped store, so tests that
// change the viewport must each use a run id no other test zooms.
function renderAt(entry: string): TestRenderer.ReactTestRenderer {
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter initialEntries={[entry]}>
        <Routes>
          <Route path="/runs/:id" element={<RunOverview />} />
        </Routes>
      </MemoryRouter>,
    );
  });
  mountedRenderers.push(renderer);
  return renderer;
}

function render(): TestRenderer.ReactTestRenderer {
  return renderAt("/runs/run-1");
}

// The inner graph node carries `transform: translate(...) scale(...)` reflecting view.zoom/pan.
function graphTransform(renderer: TestRenderer.ReactTestRenderer): string {
  const node = renderer.root.findAll(
    (n) => (n.props?.style as { transformOrigin?: string })?.transformOrigin === "center center",
  )[0];
  return (node.props.style as { transform: string }).transform;
}

function clickTitle(renderer: TestRenderer.ReactTestRenderer, title: string): void {
  act(() => {
    renderer.root.findAll((n) => n.props?.title === title)[0].props.onClick();
  });
}

let teardownReactTestEnv: () => void;

beforeEach(() => {
  teardownReactTestEnv = setupReactTestEnv();
});

afterEach(() => {
  for (const renderer of mountedRenderers.splice(0)) {
    act(() => renderer.unmount());
  }
  currentGraphData = undefined;
  currentGraphError = undefined;
  currentGraphLoading = false;
  graphMutateMock.mockClear();
  teardownReactTestEnv();
});

describe("RunOverview", () => {
  test("shows graph render errors instead of the empty graph state", () => {
    currentGraphError = new ApiError({
      status:    400,
      message:   "failed to parse DOT source",
      requestId: "req_123",
      body:      null,
    });

    const renderer = render();
    const text = textFromNode(renderer.toJSON());

    expect(text).toContain("Couldn't render workflow graph");
    expect(text).toContain("failed to parse DOT source");
    expect(text).not.toContain("No workflow graph");
  });

  test("restores graph zoom/pan when the route remounts for the same run", () => {
    currentGraphData = "<svg viewBox='0 0 100 100'></svg>";

    const first = renderAt("/runs/zoom-persist");
    const initial = graphTransform(first);
    clickTitle(first, "Zoom in");
    const zoomed = graphTransform(first);
    expect(zoomed).not.toBe(initial);

    // Switching tabs unmounts this route; returning remounts it for the same run.
    act(() => first.unmount());
    const second = renderAt("/runs/zoom-persist");
    expect(graphTransform(second)).toBe(zoomed);
  });

  test("does not carry one run's zoom over to a different run", () => {
    currentGraphData = "<svg viewBox='0 0 100 100'></svg>";

    const renderer = renderAt("/runs/run-a");
    const initial = graphTransform(renderer);
    clickTitle(renderer, "Zoom in");
    expect(graphTransform(renderer)).not.toBe(initial);

    act(() => renderer.unmount());
    const other = renderAt("/runs/run-b");
    expect(graphTransform(other)).toBe(initial);
  });
});
