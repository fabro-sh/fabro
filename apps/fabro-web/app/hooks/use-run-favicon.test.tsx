import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import { useRunFavicon } from "./use-run-favicon";
import { setupReactTestEnv } from "../lib/test-utils";

const LOGO = '<svg viewBox="0 0 272 348"><path d="M0 0"/></svg>';

/** A `<link>` with just what the hook reads and writes. */
class FakeLink {
  attributes: Record<string, string>;

  constructor(attributes: Record<string, string>) {
    this.attributes = { ...attributes };
  }

  get href() {
    return this.attributes.href ?? "";
  }

  set href(value: string) {
    this.attributes.href = value;
  }

  getAttribute(name: string) {
    return this.attributes[name] ?? null;
  }

  setAttribute(name: string, value: string) {
    this.attributes[name] = value;
  }

  removeAttribute(name: string) {
    delete this.attributes[name];
  }
}

/** Matches `link[name="value"]…` selectors against the fake links. */
function select(links: FakeLink[], selector: string) {
  const wanted = Array.from(selector.matchAll(/\[([a-z-]+)="([^"]*)"\]/g), ([, name, value]) => [name!, value!]);
  return links.filter((link) => wanted.every(([name, value]) => link.getAttribute(name) === value));
}

const globals = globalThis as Record<string, unknown>;
const STUBBED = ["document", "getComputedStyle", "fetch"] as const;

describe("useRunFavicon", () => {
  let teardown: () => void;
  let saved: Array<[string, boolean, unknown]>;
  let svgLink: FakeLink;
  let icoLink: FakeLink;
  let fetches: number;
  const mounted: TestRenderer.ReactTestRenderer[] = [];

  beforeEach(() => {
    teardown = setupReactTestEnv();
    saved = STUBBED.map((name) => [name, name in globals, globals[name]]);
    svgLink = new FakeLink({ rel: "icon", type: "image/svg+xml", href: "/images/favicon.svg" });
    icoLink = new FakeLink({ rel: "icon", sizes: "32x32", href: "/images/favicon.ico" });
    fetches = 0;
    globals.document = {
      documentElement: {},
      querySelectorAll: (selector: string) => select([svgLink, icoLink], selector),
    };
    globals.getComputedStyle = () => ({
      getPropertyValue: (name: string) => ({ "--color-coral": "#E86B6B", "--color-page": "#0F1729" })[name] ?? "",
    });
    globals.fetch = async () => {
      fetches += 1;
      return new Response(LOGO);
    };
  });

  afterEach(() => {
    for (const renderer of mounted.splice(0)) act(() => renderer.unmount());
    for (const [name, had, value] of saved) {
      if (had) globals[name] = value;
      else delete globals[name];
    }
    teardown();
  });

  function Favicon({ status }: { status: string }) {
    useRunFavicon(status);
    return null;
  }

  async function render(status: string) {
    let renderer!: TestRenderer.ReactTestRenderer;
    await act(async () => {
      renderer = TestRenderer.create(<Favicon status={status} />);
    });
    mounted.push(renderer);
    return renderer;
  }

  test("draws the dot on the SVG icon and leaves the .ico fallback alone", async () => {
    const renderer = await render("failed");

    expect(svgLink.href.startsWith("data:image/svg+xml,")).toBe(true);
    expect(decodeURIComponent(svgLink.href)).toContain('fill="#E86B6B"');
    expect(icoLink.href).toBe("/images/favicon.ico");

    await act(async () => renderer.unmount());
    mounted.splice(mounted.indexOf(renderer), 1);
    expect(svgLink.href).toBe("/images/favicon.svg");
  });

  test("keeps the plain icon, and fetches nothing, for a muted status", async () => {
    await render("pending");

    expect(svgLink.href).toBe("/images/favicon.svg");
    expect(fetches).toBe(0);
  });
});
