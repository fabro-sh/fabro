import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { useEffect } from "react";
import { RouterProvider, createMemoryRouter } from "react-router";
import TestRenderer, { act } from "react-test-renderer";

import { setupReactTestEnv, textContent } from "../lib/test-utils";

const authState: {
  data: Record<string, unknown> | undefined;
  error: unknown;
  isLoading: boolean;
} = { data: undefined, error: null, isLoading: false };

mock.module("../lib/queries", () => ({ useAuthMe: () => authState }));

let wideWindow = true;
let desktopWindow = true;
const realEffects = await import("../hooks/effects");
mock.module("../hooks/effects", () => ({
  ...realEffects,
  useMediaQuery: (query: string) => query === "(min-width: 48rem)" ? desktopWindow : wideWindow,
}));

const AppShell = (await import("./app-shell")).default;
const { createWorkflowHref, isNavigationItemActive, navigation } = await import("./navigation");

const backing = new Map<string, string>();
const previousStorage = globalThis.localStorage;
let teardown: () => void;
let mountedPages = 0;

function Page() {
  useEffect(() => {
    mountedPages += 1;
  }, []);
  return <div>page content</div>;
}

async function renderShell(path: string, handle?: Record<string, unknown>) {
  const router = createMemoryRouter(
    [
      {
        Component: AppShell,
        children: [
          { path: "*", Component: Page, handle },
        ],
      },
    ],
    { initialEntries: [path] },
  );
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  await act(async () => {
    renderer = TestRenderer.create(<RouterProvider router={router} />);
  });
  if (!renderer) throw new Error("not rendered");
  return renderer;
}

function linksTo(renderer: TestRenderer.ReactTestRenderer, href: string) {
  return renderer.root.findAll((node) => node.type === "a" && node.props.href === href);
}

function navLink(renderer: TestRenderer.ReactTestRenderer, href: string) {
  const link = linksTo(renderer, href).find((node) => "aria-describedby" in node.props);
  if (!link) throw new Error(`no navigation link to ${href}`);
  return link;
}

function classOfRoot(renderer: TestRenderer.ReactTestRenderer): string {
  return renderer.root.findAll((node) => node.type === "div" && String(node.props.className).includes("isolate"))[0]
    .props.className;
}

function toggle(renderer: TestRenderer.ReactTestRenderer) {
  return renderer.root.findAll(
    (node) => node.type === "button" && String(node.props["aria-label"]).endsWith("navigation") && "aria-controls" in node.props && node.props["aria-controls"] === "app-sidebar",
  )[0];
}

beforeEach(() => {
  teardown = setupReactTestEnv();
  backing.clear();
  mountedPages = 0;
  authState.data = {
    user: { name: "Ada Admin", email: "ada@example.com", avatarUrl: "/avatar.png" },
    provider: "github",
    demoMode: false,
  };
  authState.error = null;
  authState.isLoading = false;
  wideWindow = true;
  desktopWindow = true;
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => backing.get(key) ?? null,
      setItem: (key: string, value: string) => void backing.set(key, value),
    },
  });
});

afterEach(() => {
  teardown();
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: previousStorage });
});

describe("the sidebar", () => {
  test("lists every destination, each with its purpose available to screen readers", async () => {
    const renderer = await renderShell("/runs");
    const content = textContent(renderer.root);
    for (const item of navigation) {
      expect(linksTo(renderer, item.href).length).toBeGreaterThan(0);
      expect(content).toContain(item.purpose);
    }
    // The logo also links to /runs in this mode, so pick the destination's own link.
    expect(navLink(renderer, "/runs").props["aria-describedby"]).toBeTruthy();
  });

  test("marks only the current destination", async () => {
    const renderer = await renderShell("/workflows/abc");
    const current = renderer.root.findAll((node) => node.type === "a" && node.props["aria-current"] === "page");
    expect(new Set(current.map((node) => node.props.href))).toEqual(new Set(["/workflows"]));
  });

  test("Create is on the sidebar and the mobile bar", async () => {
    const renderer = await renderShell("/runs");
    expect(linksTo(renderer, createWorkflowHref).length).toBe(2);
    for (const link of linksTo(renderer, createWorkflowHref)) {
      expect(link.props["aria-label"]).toBe("Create workflow");
    }
  });

  test("starts expanded on a wide screen, with a toggle that says so", async () => {
    const renderer = await renderShell("/runs");
    const button = toggle(renderer);
    expect(button.props["aria-expanded"]).toBe(true);
    expect(button.props["aria-label"]).toBe("Collapse navigation");
    expect(classOfRoot(renderer)).toContain("md:[--fabro-nav-width:15.5rem]");
  });

  test("collapses to a rail, keeps labels for screen readers, and remembers", async () => {
    const renderer = await renderShell("/runs");
    await act(async () => {
      toggle(renderer).props.onClick();
    });
    const button = toggle(renderer);
    expect(button.props["aria-expanded"]).toBe(false);
    expect(button.props["aria-label"]).toBe("Expand navigation");
    expect(classOfRoot(renderer)).toContain("md:[--fabro-nav-width:3.5rem]");
    expect(backing.get("fabro.sidebar.collapsed")).toBe("true");
    // The destination's name is still in the link: visually hidden, not removed.
    const label = navLink(renderer, "/runs").findAll((node) => node.type === "span" && node.props.className === "sr-only");
    expect(label.map(textContent)).toContain("Runs");
  });

  test("a remembered choice wins over the window width", async () => {
    backing.set("fabro.sidebar.collapsed", "true");
    const renderer = await renderShell("/runs");
    expect(toggle(renderer).props["aria-expanded"]).toBe(false);
  });

  test("a medium window with no choice yet starts on the rail", async () => {
    wideWindow = false;
    const renderer = await renderShell("/runs");
    expect(toggle(renderer).props["aria-expanded"]).toBe(false);
  });

  test("switching to mobile removes the desktop navigation and its portal owners", async () => {
    const renderer = await renderShell("/runs");
    expect(renderer.root.findAll((node) => node.type === "aside")).toHaveLength(1);
    desktopWindow = false;
    await act(async () => {
      toggle(renderer).props.onClick();
    });
    expect(renderer.root.findAll((node) => node.type === "aside")).toHaveLength(0);
    expect(linksTo(renderer, createWorkflowHref)).toHaveLength(1);
    expect(mountedPages).toBe(1);
  });

  test("blocked storage doesn't stop the toggle", async () => {
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      value: {
        getItem: () => {
          throw new Error("blocked");
        },
        setItem: () => {
          throw new Error("blocked");
        },
      },
    });
    const renderer = await renderShell("/runs");
    await act(async () => {
      toggle(renderer).props.onClick();
    });
    expect(toggle(renderer).props["aria-expanded"]).toBe(false);
  });

  test("collapsing doesn't remount the page", async () => {
    const renderer = await renderShell("/runs");
    await act(async () => {
      toggle(renderer).props.onClick();
    });
    expect(mountedPages).toBe(1);
  });

  test("offers a way to skip the navigation", async () => {
    const renderer = await renderShell("/runs");
    expect(linksTo(renderer, "#main").map(textContent)).toEqual(["Skip to content"]);
    expect(renderer.root.findByType("main").props.id).toBe("main");
  });

  test("the home link follows demo mode", async () => {
    authState.data = { ...authState.data, demoMode: true };
    const renderer = await renderShell("/runs");
    const home = renderer.root.findAll((node) => node.type === "a" && node.props["aria-label"] === "Fabro home");
    expect(new Set(home.map((node) => node.props.href))).toEqual(new Set(["/start"]));
  });
});

describe("the page frame", () => {
  test("a full-height page fills the window beside the sidebar", async () => {
    const renderer = await renderShell("/workflows/new", { fullHeight: true, hideHeader: true, wide: true });
    expect(classOfRoot(renderer)).toContain("h-dvh");
    expect(classOfRoot(renderer)).not.toContain("flex-col");
    const main = renderer.root.findByType("main");
    expect(main.props.className).toContain("flex-1");
  });

  test("a normal page keeps its title and header actions", async () => {
    const renderer = await renderShell("/runs", { headerExtra: <span>extra action</span> });
    expect(renderer.root.findByType("h1").children).toEqual(["Runs"]);
    expect(textContent(renderer.root)).toContain("extra action");
    expect(classOfRoot(renderer)).toContain("min-h-full");
  });

  test("hideHeader removes the title row", async () => {
    const renderer = await renderShell("/runs", { hideHeader: true });
    expect(renderer.root.findAllByType("h1")).toHaveLength(0);
  });

  test("a failed session still says so", async () => {
    authState.data = undefined;
    authState.error = new Error("nope");
    const renderer = await renderShell("/runs");
    expect(textContent(renderer.root)).toContain("Couldn't load your session");
  });
});

describe("navigation data", () => {
  test("matches a destination and its children, not a lookalike", () => {
    const runs = navigation.find((item) => item.href === "/runs");
    if (!runs) throw new Error("no runs item");
    expect(isNavigationItemActive(runs, "/runs")).toBe(true);
    expect(isNavigationItemActive(runs, "/runs/abc/stages")).toBe(true);
    expect(isNavigationItemActive(runs, "/runsheets")).toBe(false);
  });
});
