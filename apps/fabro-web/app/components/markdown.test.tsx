import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";

import { Markdown } from "./markdown";

describe("Markdown", () => {
  test("renders GitHub-flavored Markdown", () => {
    const html = renderToStaticMarkup(
      <Markdown content={"# Plan\n\n- [x] Review this"} />,
    );

    expect(html).toContain("<h1>Plan</h1>");
    expect(html).toContain('type="checkbox"');
    expect(html).toContain("Review this");
  });

  test("strips raw HTML and unsafe URLs", () => {
    const html = renderToStaticMarkup(
      <Markdown
        content={'<script>alert("no")</script>\n\n[bad](javascript:alert("no"))'}
      />,
    );

    expect(html).not.toContain("<script>");
    expect(html).not.toContain("javascript:");
  });
});
