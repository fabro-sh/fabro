import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { createElement } from "react";
import TestRenderer, { act } from "react-test-renderer";
import { SWRConfig } from "swr";

import { ToastProvider } from "../toast";
import { setupReactTestEnv } from "../../lib/test-utils";
import type { RunWithStatus } from "../../data/runs";
import { RowActionsMenu } from "./row-actions-menu";

// Render Headless UI's Menu primitives inline so menu items are always in the
// tree regardless of open state — we only care which actions the menu offers
// for a given run status, not the open/close interaction.
mock.module("@headlessui/react", () => ({
  Menu: ({ children }: any) =>
    createElement("div", null, typeof children === "function" ? children({ open: true }) : children),
  MenuButton: ({ children, ...props }: any) =>
    createElement("button", props, typeof children === "function" ? children({ open: true }) : children),
  MenuItems: ({ children }: any) =>
    createElement("div", null, typeof children === "function" ? children({ open: true }) : children),
  MenuItem: ({ children }: any) =>
    createElement("div", null, typeof children === "function" ? children({ close: () => {}, active: false }) : children),
  Dialog: ({ open, children }: any) => (open ? createElement("div", { role: "dialog" }, children) : null),
  DialogPanel: ({ children, ...props }: any) => createElement("div", props, children),
  DialogTitle: ({ children, ...props }: any) => createElement("h2", props, children),
}));

let teardownReactEnv: (() => void) | undefined;

function makeRunWithStatus(
  status: { kind: string; reason?: string },
  archived = false,
): RunWithStatus {
  return {
    id:              "run-1",
    title:           "Fix the build",
    lifecycleStatus: archived ? "archived" : status.kind,
    pendingApproval: false,
    lifecycle:       {
      status,
      approval:        null,
      pending_control: null,
      queue_position:  null,
      error:           null,
      archived,
      archived_at:     archived ? "2026-04-20T12:05:00Z" : null,
    },
  } as unknown as RunWithStatus;
}

function render(node: React.ReactNode): TestRenderer.ReactTestRenderer {
  let tree: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    tree = TestRenderer.create(
      <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
        <ToastProvider>{node}</ToastProvider>
      </SWRConfig>,
    );
  });
  return tree!;
}

function instanceText(instance: TestRenderer.ReactTestInstance): string {
  const parts: string[] = [];
  for (const child of instance.children) {
    if (typeof child === "string") parts.push(child);
    else parts.push(instanceText(child));
  }
  return parts.join("");
}

function menuItemLabels(tree: TestRenderer.ReactTestRenderer): string[] {
  return tree.root.findAllByType("button").map((b) => instanceText(b).trim());
}

describe("RowActionsMenu retry gating", () => {
  beforeEach(() => {
    teardownReactEnv = setupReactTestEnv();
  });
  afterEach(() => {
    teardownReactEnv?.();
    teardownReactEnv = undefined;
  });

  test("offers Retry for a succeeded run", () => {
    const tree = render(
      <RowActionsMenu run={makeRunWithStatus({ kind: "succeeded", reason: "completed" })} />,
    );
    expect(menuItemLabels(tree)).toContain("Retry");
  });

  test("still offers Retry for failed and dead runs", () => {
    const failed = render(
      <RowActionsMenu run={makeRunWithStatus({ kind: "failed", reason: "workflow_error" })} />,
    );
    expect(menuItemLabels(failed)).toContain("Retry");

    const dead = render(<RowActionsMenu run={makeRunWithStatus({ kind: "dead" })} />);
    expect(menuItemLabels(dead)).toContain("Retry");
  });

  test("does not offer Retry for an archived (non-retryable) run", () => {
    const tree = render(
      <RowActionsMenu run={makeRunWithStatus({ kind: "succeeded", reason: "completed" }, true)} />,
    );
    expect(menuItemLabels(tree)).not.toContain("Retry");
  });

  test("does not offer Retry for a still-running run", () => {
    const tree = render(<RowActionsMenu run={makeRunWithStatus({ kind: "running" })} />);
    expect(menuItemLabels(tree)).not.toContain("Retry");
  });
});
