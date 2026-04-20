import { describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import { BlockedRunNotice } from "./blocked-run-notice";

function textFromNode(node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>): string {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(textFromNode).join("");
  return (node.children ?? []).map(textFromNode).join("");
}

describe("BlockedRunNotice", () => {
  test("renders the question text when provided", () => {
    let tree: TestRenderer.ReactTestRenderer | undefined;
    act(() => {
      tree = TestRenderer.create(
        <BlockedRunNotice
          questionText="Approve the deployment target?"
          onCancel={() => {}}
        />,
      );
    });

    expect(textFromNode(tree!.toJSON())).toContain("Approve the deployment target?");
  });

  test("renders fallback copy when no question is available", () => {
    let tree: TestRenderer.ReactTestRenderer | undefined;
    act(() => {
      tree = TestRenderer.create(<BlockedRunNotice onCancel={() => {}} />);
    });

    expect(textFromNode(tree!.toJSON())).toContain("Fabro is blocked on a human-in-the-loop question.");
  });

  test("fires the secondary cancel action", () => {
    let cancelled = 0;
    let tree: TestRenderer.ReactTestRenderer | undefined;
    act(() => {
      tree = TestRenderer.create(
        <BlockedRunNotice onCancel={() => {
          cancelled += 1;
        }}
        />,
      );
    });

    const button = tree!.root.findByType("button");
    act(() => {
      button.props.onClick();
    });

    expect(cancelled).toBe(1);
  });
});
