import { describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import { PullRequestChip } from "./pull-request-chip";

describe("PullRequestChip", () => {
  test("renders a generic PR label when the number is unavailable", () => {
    let renderer: TestRenderer.ReactTestRenderer | undefined;
    act(() => {
      renderer = TestRenderer.create(
        <PullRequestChip
          number={undefined}
          url="https://gitlab.com/acme/widgets/-/merge_requests/42"
        />,
      );
    });

    const link = renderer!.root.findByType("a");
    const rendered = JSON.stringify(renderer!.toJSON());
    expect(link.props.href).toBe("https://gitlab.com/acme/widgets/-/merge_requests/42");
    expect(rendered).toContain("PR");
    expect(rendered).not.toContain("#undefined");
  });
});
