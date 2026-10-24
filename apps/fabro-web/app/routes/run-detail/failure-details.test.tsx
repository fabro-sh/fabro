import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { FailureDetail } from "@qltysh/fabro-api-client";
import { RunFailureDetails } from "./failure-details";

const failure = {
  message: "Could not publish the run branch",
  category: "deterministic",
} as FailureDetail;

describe("run failure details", () => {
  test("older runs without output keep their existing display", () => {
    expect(renderToStaticMarkup(<RunFailureDetails failure={failure} />)).toBe("");
    expect(renderToStaticMarkup(<RunFailureDetails />)).toBe("");
  });

  test("renders escaped, copyable output with a keyboard accessible disclosure", () => {
    const html = renderToStaticMarkup(<RunFailureDetails failure={{
      ...failure,
      exec_output_tail: {
        stderr: "remote: GH013\n<script>not markup</script>",
        stdout: "command output",
        stderr_truncated: true,
        stdout_truncated: false,
      },
    }} />);
    expect(html).toContain("<summary");
    expect(html).toContain("Show error details");
    expect(html).toContain("Earlier output was omitted.");
    expect(html).toContain("remote: GH013\n&lt;script&gt;not markup&lt;/script&gt;");
    expect(html).not.toContain("<script>");
    expect(html).toContain('aria-label="Copy standard error"');
    expect(html).toContain('aria-label="Copy standard output"');
  });

  test("does not imply complete output when only the truncation flag remains", () => {
    const html = renderToStaticMarkup(<RunFailureDetails failure={{
      ...failure,
      exec_output_tail: { stderr_truncated: true, stdout_truncated: false },
    }} />);
    expect(html).toContain("Earlier output was omitted.");
    expect(html).not.toContain("Copy standard error");
  });
});
