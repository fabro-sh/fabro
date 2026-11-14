import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { createMemoryRouter, Outlet, RouterProvider, type RouteObject } from "react-router";

import { documentTitle, PageTitleProvider, routeTitle, usePageTitle } from "./page-title";
import { setupReactTestEnv } from "./test-utils";
import { routes } from "../router";

describe("documentTitle", () => {
  test("joins the parts in order and ends with Fabro", () => {
    expect(documentTitle(["trinsic", "Add lead score", "spec"])).toBe("trinsic — Add lead score — spec — Fabro");
    expect(documentTitle(["Models", null, " ", undefined])).toBe("Models — Fabro");
    expect(documentTitle([])).toBe("Fabro");
  });
});

describe("routeTitle", () => {
  test("takes the deepest route that declares a title", () => {
    expect(routeTitle([{ handle: undefined }, { handle: { title: "Settings" } }, { handle: { title: "Models" } }])).toBe("Models");
    expect(routeTitle([{ handle: { title: "Run" } }, { handle: { wide: true } }])).toBe("Run");
    expect(routeTitle([{ handle: undefined }])).toBeNull();
  });
});

describe("app routes", () => {
  /** Every page's path, with the titles its routes declare, outermost first. */
  function pages(
    objects: RouteObject[],
    parentPath = "",
    titles: Array<string | undefined> = [],
  ): Array<{ path: string; titles: Array<string | undefined> }> {
    return objects.flatMap((object) => {
      const path = object.index
        ? parentPath
        : [parentPath, object.path].filter(Boolean).join("/").replace(/\/+/g, "/");
      const chain = [...titles, (object.handle as { title?: string } | undefined)?.title];
      if (object.children?.length) return pages(object.children, path, chain);
      return [{ path, titles: chain }];
    });
  }

  test("every page names its tab, so none reads just Fabro", () => {
    const untitled = pages(routes)
      // The index redirects to /runs before anything renders.
      .filter((page) => page.path !== "/")
      .filter((page) => !page.titles.some(Boolean))
      .map((page) => page.path);
    expect(untitled).toEqual([]);
  });
});

describe("PageTitleProvider", () => {
  let teardown: () => void;
  let previousDocument: unknown;
  let hadDocument: boolean;
  const mounted: TestRenderer.ReactTestRenderer[] = [];

  beforeEach(() => {
    teardown = setupReactTestEnv();
    hadDocument = "document" in globalThis;
    previousDocument = (globalThis as { document?: unknown }).document;
    (globalThis as { document?: unknown }).document = { title: "" };
  });

  afterEach(() => {
    for (const renderer of mounted.splice(0)) act(() => renderer.unmount());
    if (hadDocument) (globalThis as { document?: unknown }).document = previousDocument;
    else delete (globalThis as { document?: unknown }).document;
    teardown();
  });

  function NamedRun() {
    usePageTitle(["trinsic", "Add lead score"]);
    return null;
  }

  async function render(path: string) {
    const router = createMemoryRouter(
      [
        {
          path: "/",
          element: (
            <PageTitleProvider>
              <Outlet />
            </PageTitleProvider>
          ),
          children: [
            {
              path: "settings",
              handle: { title: "Settings" },
              element: <Outlet />,
              children: [
                { index: true, element: null },
                { path: "models", handle: { title: "Models" }, element: null },
              ],
            },
            { path: "runs/:id", handle: { title: "Run" }, element: <NamedRun /> },
          ],
        },
      ],
      { initialEntries: [path] },
    );
    let renderer!: TestRenderer.ReactTestRenderer;
    await act(async () => {
      renderer = TestRenderer.create(<RouterProvider router={router} />);
    });
    mounted.push(renderer);
    return router;
  }

  const title = () => (globalThis as unknown as { document: { title: string } }).document.title;

  test("a child route's title wins over its layout's", async () => {
    const router = await render("/settings/models");
    expect(title()).toBe("Models — Fabro");

    await act(() => router.navigate("/settings"));
    expect(title()).toBe("Settings — Fabro");
  });

  test("a page's own title wins over its route's, and goes when the page does", async () => {
    const router = await render("/runs/01M4");
    expect(title()).toBe("trinsic — Add lead score — Fabro");

    await act(() => router.navigate("/settings/models"));
    expect(title()).toBe("Models — Fabro");
  });
});
