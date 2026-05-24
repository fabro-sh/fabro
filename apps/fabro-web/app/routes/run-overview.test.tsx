import { afterEach, describe, expect, mock, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter, Route, Routes } from "react-router";

import { ApiError } from "../lib/api-client";

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
  useRunStageEvents: () => ({ data: [] }),
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

function render(): TestRenderer.ReactTestRenderer {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter initialEntries={["/runs/run-1"]}>
        <Routes>
          <Route path="/runs/:id" element={<RunOverview />} />
        </Routes>
      </MemoryRouter>,
    );
  });
  mountedRenderers.push(renderer);
  return renderer;
}

afterEach(() => {
  for (const renderer of mountedRenderers.splice(0)) {
    act(() => renderer.unmount());
  }
  currentGraphData = undefined;
  currentGraphError = undefined;
  currentGraphLoading = false;
  graphMutateMock.mockClear();
  delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
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
});
