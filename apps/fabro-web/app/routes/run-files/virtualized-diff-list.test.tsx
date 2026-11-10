import { describe, expect, mock, test } from "bun:test";
import type { ReactNode } from "react";
import TestRenderer, { act } from "react-test-renderer";

const providerCalls: any[] = [];
const virtualizerCalls: any[] = [];

mock.module("@pierre/diffs/react", () => ({
  MultiFileDiff: (props: any) => <div>{props.newFile?.name}</div>,
  PatchDiff: (props: any) => <div>{props.patch}</div>,
  WorkerPoolContextProvider: (props: any) => {
    providerCalls.push(props);
    return <div data-provider="true">{props.children}</div>;
  },
  Virtualizer: (props: any) => {
    virtualizerCalls.push(props);
    return <div data-virtualizer="true">{props.children}</div>;
  },
}));

const { workerFactory } = await import("../../lib/pierre-diffs-worker");
const { VirtualizedDiffList } = await import("./virtualized-diff-list");

function render(children: ReactNode) {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(
      <VirtualizedDiffList>{children}</VirtualizedDiffList>,
    );
  });
  return renderer!;
}

describe("VirtualizedDiffList", () => {
  test("configures Pierre worker pool and virtualizer layout", () => {
    providerCalls.length = 0;
    virtualizerCalls.length = 0;

    render(<div>row</div>);

    expect(providerCalls).toHaveLength(1);
    expect(providerCalls[0].poolOptions.workerFactory).toBe(workerFactory);
    expect(providerCalls[0].highlighterOptions).toEqual({
      theme: { dark: "pierre-dark", light: "pierre-light" },
    });

    expect(virtualizerCalls).toHaveLength(1);
    expect(virtualizerCalls[0].className).toContain("min-h-0");
    expect(virtualizerCalls[0].className).toContain("flex-1");
    expect(virtualizerCalls[0].className).toContain("overflow-auto");
    expect(virtualizerCalls[0].contentClassName).toContain("flex");
    expect(virtualizerCalls[0].contentClassName).toContain("flex-col");
    expect(virtualizerCalls[0].contentClassName).toContain("gap-2");
    expect(virtualizerCalls[0].contentClassName).toContain(
      "--fabro-interview-dock-clearance",
    );
  });
});
