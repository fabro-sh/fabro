import { describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import { PullRequestChip } from "./pull-request-chip";

describe("PullRequestChip", () => {
  test("renders a linked pull request number", () => {
    let renderer: TestRenderer.ReactTestRenderer | undefined;
    act(() => {
      renderer = TestRenderer.create(
        <PullRequestChip
          number={42}
          url="https://github.com/acme/widgets/pull/42"
        />,
      );
    });

    const link = renderer!.root.findByType("a");
    const rendered = JSON.stringify(renderer!.toJSON());
    expect(link.props.href).toBe("https://github.com/acme/widgets/pull/42");
    expect(rendered).toContain("#42");
  });
});
