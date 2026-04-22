import { describe, expect, test } from "bun:test";
import TestRenderer from "react-test-renderer";

import {
  deriveEmptyKind,
  emptyStateCopy,
  EmptyState,
  InlineErrorBanner,
  LoadingSkeleton,
} from "./states";

function renderToJson(element: React.ReactElement): any {
  return TestRenderer.create(element).toJSON();
}

describe("deriveEmptyKind", () => {
  // Pre-work states → R4(a) "starting"
  test.each(["submitted", "Submitted", "starting", "queued"])(
    "%s maps to R4(a) 'starting'",
    (status) => {
      expect(
        deriveEmptyKind({
          runStatus: status,
          totalChanged: 0,
          degraded: false,
        }),
      ).toBe("starting");
    },
  );

  // Actively-in-progress states → R4(b) "no_changes yet"
  test.each(["running", "blocked", "paused"])(
    "%s with no files yet is R4(b) 'no_changes'",
    (status) => {
      expect(
        deriveEmptyKind({
          runStatus: status,
          totalChanged: 0,
          degraded: false,
        }),
      ).toBe("no_changes");
    },
  );

  // Terminal-failure states → R4(c1) when no degraded patch available
  test.each(["failed", "dead"])(
    "%s without degraded fallback is R4(c1) 'failed_before_checkpoint'",
    (status) => {
      expect(
        deriveEmptyKind({
          runStatus: status,
          totalChanged: 0,
          degraded: false,
        }),
      ).toBe("failed_before_checkpoint");
    },
  );

  // Terminal-success + teardown states → R4(b) or R4(c2) depending on
  // whether files were ever changed
  test.each(["succeeded", "removing", "archived"])(
    "%s with changes but no data is R4(c2) 'diff_lost'",
    (status) => {
      expect(
        deriveEmptyKind({
          runStatus: status,
          totalChanged: 3,
          degraded: false,
        }),
      ).toBe("diff_lost");
    },
  );

  test.each(["succeeded", "removing", "archived"])(
    "%s with no changes is R4(b)",
    (status) => {
      expect(
        deriveEmptyKind({
          runStatus: status,
          totalChanged: 0,
          degraded: false,
        }),
      ).toBe("no_changes");
    },
  );

  test("missing runStatus collapses to 'unknown'", () => {
    expect(
      deriveEmptyKind({
        runStatus: undefined,
        totalChanged: 0,
        degraded: false,
      }),
    ).toBe("unknown");
  });

  test("unknown future status collapses to 'unknown'", () => {
    expect(
      deriveEmptyKind({
        runStatus: "some_future_state",
        totalChanged: 0,
        degraded: false,
      }),
    ).toBe("unknown");
  });

  test("every documented RunStatus gets a non-unknown empty kind", () => {
    // Regression guard sourced from the documented wire discriminators. If
    // a new RunStatus kind lands in the API, this list should be updated
    // alongside the decision table below.
    for (const status of [
      "submitted",
      "queued",
      "starting",
      "running",
      "blocked",
      "paused",
      "removing",
      "succeeded",
      "failed",
      "dead",
      "archived",
    ]) {
      const result = deriveEmptyKind({
        runStatus: status,
        totalChanged: 0,
        degraded: false,
      });
      expect(result).not.toBe("unknown");
    }
  });
});

describe("emptyStateCopy", () => {
  test("every kind resolves to distinct non-empty copy", () => {
    const seen = new Set<string>();
    for (const kind of [
      "starting",
      "no_changes",
      "failed_before_checkpoint",
      "diff_lost",
      "unknown",
    ] as const) {
      const c = emptyStateCopy(kind);
      expect(c.length).toBeGreaterThan(0);
      expect(seen.has(c)).toBe(false);
      seen.add(c);
    }
  });
});

describe("component rendering", () => {
  test("EmptyState wraps message in role=status", () => {
    let tree: TestRenderer.ReactTestRenderer | undefined;
    TestRenderer.act(() => {
      tree = TestRenderer.create(<EmptyState kind="starting" />);
    });
    const statusEl = tree!.root.findAll(
      (node) => node.type === "div" && node.props?.role === "status",
    );
    expect(statusEl.length).toBeGreaterThan(0);
  });

  test("LoadingSkeleton has aria-label", () => {
    let tree: TestRenderer.ReactTestRenderer | undefined;
    TestRenderer.act(() => {
      tree = TestRenderer.create(<LoadingSkeleton />);
    });
    const labeled = tree!.root.findAll(
      (node) => node.props?.["aria-label"] === "Loading files",
    );
    expect(labeled.length).toBeGreaterThan(0);
  });

  test("InlineErrorBanner fires onRetry when clicked", () => {
    let clicked = 0;
    let tree: TestRenderer.ReactTestRenderer | undefined;
    TestRenderer.act(() => {
      tree = TestRenderer.create(
        <InlineErrorBanner message="503" onRetry={() => (clicked += 1)} />,
      );
    });
    const button = tree!.root.findByType("button");
    TestRenderer.act(() => {
      button.props.onClick();
    });
    expect(clicked).toBe(1);
  });
});
