import { afterEach, describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import { InlineMarkdown } from "./inline-markdown";

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

async function render(content: string, className?: string) {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  await act(async () => {
    renderer = TestRenderer.create(
      <InlineMarkdown content={content} className={className} />,
    );
  });
  mountedRenderers.push(renderer!);
  return renderer!;
}

function textFromInstance(node: TestRenderer.ReactTestInstance): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : textFromInstance(child)))
    .join("");
}

describe("InlineMarkdown", () => {
  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  test("renders backtick spans as <code> without visible backticks", async () => {
    const renderer = await render(
      "Move from `[server.integrations.github]` to `[run.integrations.github]`",
    );

    const codes = renderer.root.findAllByType("code");
    expect(codes).toHaveLength(2);
    expect(textFromInstance(codes[0]!)).toBe("[server.integrations.github]");
    expect(textFromInstance(codes[1]!)).toBe("[run.integrations.github]");

    const fullText = textFromInstance(renderer.root);
    expect(fullText).not.toContain("`");
  });

  test("renders **bold** as <strong> and _italic_ as <em>", async () => {
    const renderer = await render("**bold** and _italic_");

    const strongs = renderer.root.findAllByType("strong");
    expect(strongs).toHaveLength(1);
    expect(textFromInstance(strongs[0]!)).toBe("bold");

    const ems = renderer.root.findAllByType("em");
    expect(ems).toHaveLength(1);
    expect(textFromInstance(ems[0]!)).toBe("italic");
  });

  test("renders *italic* (asterisk form) as <em>", async () => {
    const renderer = await render("*italic*");
    const ems = renderer.root.findAllByType("em");
    expect(ems).toHaveLength(1);
    expect(textFromInstance(ems[0]!)).toBe("italic");
  });

  test("block markdown like headings, lists, blockquotes stays text", async () => {
    const renderer = await render("# heading - item > quote");

    expect(renderer.root.findAllByType("h1")).toHaveLength(0);
    expect(renderer.root.findAllByType("h2")).toHaveLength(0);
    expect(renderer.root.findAllByType("ul")).toHaveLength(0);
    expect(renderer.root.findAllByType("li")).toHaveLength(0);
    expect(renderer.root.findAllByType("blockquote")).toHaveLength(0);

    expect(textFromInstance(renderer.root)).toContain("# heading");
    expect(textFromInstance(renderer.root)).toContain("> quote");
  });

  test("link syntax renders the label as text without an <a>", async () => {
    const renderer = await render("[label](javascript:alert(1))");

    expect(renderer.root.findAllByType("a")).toHaveLength(0);
    expect(textFromInstance(renderer.root)).toContain("label");
    expect(textFromInstance(renderer.root)).not.toContain("javascript:");
  });

  test("image syntax renders the alt text without an <img>", async () => {
    const renderer = await render("![alt](x)");

    expect(renderer.root.findAllByType("img")).toHaveLength(0);
    expect(textFromInstance(renderer.root)).toContain("alt");
  });

  test("raw HTML is shown literally as text, not interpreted", async () => {
    const renderer = await render("<script>x</script>");

    expect(renderer.root.findAllByType("script")).toHaveLength(0);
    expect(textFromInstance(renderer.root)).toContain("<script>");
    expect(textFromInstance(renderer.root)).toContain("</script>");
  });

  test("applies a className to the wrapper span", async () => {
    const renderer = await render("hello", "text-fg-2");
    const wrapper = renderer.root.findByType("span");
    expect(wrapper.props.className).toBe("text-fg-2");
  });
});
