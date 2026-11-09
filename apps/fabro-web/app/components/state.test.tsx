import { afterAll, beforeAll, describe, expect, mock, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import { setupReactTestEnv, textContent } from "../lib/test-utils";
import { EmptyState, ErrorState, LoadingState } from "./state";

let restore: () => void;
beforeAll(() => {
  restore = setupReactTestEnv();
});
afterAll(() => restore());

function create(element: Parameters<typeof TestRenderer.create>[0]) {
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(element);
  });
  return renderer;
}

const FakeIcon = (props: object) => <svg data-icon="yes" {...props} />;

describe("EmptyState", () => {
  test("shows its title, description, icon and action", () => {
    const renderer = create(
      <EmptyState
        icon={FakeIcon}
        title="No runs"
        description="Start one."
        action={<button type="button">New run</button>}
      />,
    );
    const words = textContent(renderer.root);
    expect(words).toContain("No runs");
    expect(words).toContain("Start one.");
    expect(renderer.root.findByType("button")).toBeDefined();
    expect(renderer.root.findByType("svg").props["aria-hidden"]).toBe("true");
  });

  test("leaves out what it was not given", () => {
    const renderer = create(<EmptyState title="No runs" />);
    expect(renderer.root.findAllByType("svg")).toHaveLength(0);
    expect(renderer.root.findAllByType("button")).toHaveLength(0);
  });
});

describe("ErrorState", () => {
  test("is announced as an alert and retries when asked", () => {
    const onRetry = mock(() => {});
    const renderer = create(
      <ErrorState title="Couldn't load" description="Server said no." onRetry={onRetry} />,
    );
    expect(renderer.root.findByProps({ role: "alert" })).toBeDefined();
    expect(textContent(renderer.root)).toContain("Server said no.");
    act(() => renderer.root.findByType("button").props.onClick());
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  test("offers no retry when there is nothing to retry, and has a default title", () => {
    const renderer = create(<ErrorState />);
    expect(renderer.root.findAllByType("button")).toHaveLength(0);
    expect(textContent(renderer.root)).toContain("Something went wrong");
  });
});

describe("LoadingState", () => {
  test("is announced as busy and names what is loading", () => {
    const renderer = create(<LoadingState label="Loading waterfall…" />);
    const status = renderer.root.findByProps({ role: "status" });
    expect(status.props["aria-busy"]).toBe("true");
    expect(textContent(status)).toBe("Loading waterfall…");
  });
});
