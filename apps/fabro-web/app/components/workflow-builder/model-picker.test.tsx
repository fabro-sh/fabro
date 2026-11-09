import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { useSyncExternalStore, type ComponentProps, type MouseEventHandler, type ReactNode } from "react";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter } from "react-router";

import { setupReactTestEnv } from "../../lib/test-utils";
import { ModelPickerBody } from "./model-picker-body";
import { testPickerCatalog } from "./model-picker-fixtures";
import { createModelStarStore, type StarSnapshot } from "./model-picker-stars";

// Headless UI's Dialog needs a DOM. Its focus trap, Escape and return of focus are its own and
// are checked in the browser. These tests cover how the field opens, closes and writes.
let dialogOnClose: () => void = () => {};
const actualHeadless = await import("@headlessui/react");
mock.module("@headlessui/react", () => ({
  ...actualHeadless,
  Dialog: ({ children, onClose }: { children: ReactNode; onClose: () => void }) => {
    dialogOnClose = onClose;
    return <div>{children}</div>;
  },
  DialogPanel: ({
    children,
    className,
    onMouseDown,
  }: {
    children: ReactNode;
    className?: string;
    onMouseDown?: MouseEventHandler;
  }) => (
    <div className={className} onMouseDown={onMouseDown}>
      {children}
    </div>
  ),
}));

const { ModelPickerControl } = await import("./model-picker");

type TestNode = TestRenderer.ReactTestInstance;

const NO_STARS: StarSnapshot = { stars: [], problem: null };
const ACME_LARGE = "Acme Large, acme-large, from Acme AI, provider default";
const ACME_SMALL = "Acme Small, acme-small, from Acme AI, small default";
const ROUTER_LARGE = "Acme Large via Router, acme-large, from RouterCo, setup required";
const STAR_ACME_LARGE = "Star Acme Large from Acme AI";
const STAR_ROUTER_LARGE = "Star Acme Large via Router from RouterCo";

function text(node: TestNode | string): string {
  return typeof node === "string" ? node : node.children.map(text).join("");
}

function hosts(root: TestNode, type: string): TestNode[] {
  return root.findAll((node) => node.type === type);
}

function byLabel(root: TestNode, label: string): TestNode {
  return root.find((node) => typeof node.type === "string" && node.props["aria-label"] === label);
}

function hasLabel(root: TestNode, label: string): boolean {
  return root.findAll((node) => typeof node.type === "string" && node.props["aria-label"] === label).length > 0;
}

function buttonNamed(root: TestNode, name: string): TestNode {
  return root.find((node) => node.type === "button" && text(node) === name);
}

function groupHeaders(root: TestNode): TestNode[] {
  return root.findAll((node) => node.type === "button" && node.props["data-model-group"] === true);
}

function groupNamed(root: TestNode, name: string): TestNode {
  const header = groupHeaders(root).find((node) => text(node).startsWith(name));
  if (!header) throw new Error(`no group ${name}`);
  return header;
}

function modelRows(root: TestNode): string[] {
  return root
    .findAll((node) => node.type === "button" && node.props["data-model-select"] === true)
    .map((node) => node.props["aria-label"] as string);
}

function statusLines(root: TestNode): string[] {
  return root.findAll((node) => node.type === "p" && node.props.role === "status").map(text);
}

function typeInto(input: TestNode, value: string) {
  act(() => input.props.onChange({ target: { value } }));
}

function submitForm(root: TestNode) {
  act(() => hosts(root, "form")[0].props.onSubmit({ preventDefault: () => {} }));
}

let teardown: () => void;

beforeEach(() => {
  teardown = setupReactTestEnv();
});

afterEach(() => {
  teardown();
});

// ── The contents ────────────────────────────────────────────────────────────

function renderBody(overrides: Partial<ComponentProps<typeof ModelPickerBody>> = {}) {
  const onChoose = mock((_choice: unknown) => {});
  const onToggleStar = mock((star: { provider: string; id: string }) => {
    void star;
    return { starred: true, problem: null };
  });
  const props: ComponentProps<typeof ModelPickerBody> = {
    catalog: testPickerCatalog(),
    value: {},
    inherited: {},
    inheritLabel: "Use inherited",
    stars: NO_STARS,
    onToggleStar,
    onChoose,
    ...overrides,
  };
  const body = (next: typeof props) => (
    <MemoryRouter>
      <ModelPickerBody {...next} />
    </MemoryRouter>
  );
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(body(props));
  });
  return {
    root: renderer.root,
    onChoose,
    onToggleStar,
    rerender: (next: Partial<typeof props>) => act(() => renderer.update(body({ ...props, ...next }))),
    search: (query: string) => typeInto(byLabel(renderer.root, "Search models and companies"), query),
  };
}

describe("browsing the tiers", () => {
  test("an empty search lists companies, folded, with their counts", () => {
    const { root } = renderBody();
    expect(groupHeaders(root).map(text)).toEqual([
      "Acme AI2 models",
      "RouterCo2 modelsSetup required",
      "unlisted1 modelSetup required",
      "Local Gateway0 modelsSetup required",
    ]);
    expect(modelRows(root)).toEqual([]);
    expect(statusLines(root)[0]).toBe("5 models from 3 companies");
  });

  test("a company opens and folds", () => {
    const { root } = renderBody();
    act(() => groupNamed(root, "Acme AI").props.onClick());
    expect(groupNamed(root, "Acme AI").props["aria-expanded"]).toBe(true);
    expect(modelRows(root)).toEqual([ACME_LARGE, ACME_SMALL]);

    act(() => groupNamed(root, "Acme AI").props.onClick());
    expect(groupNamed(root, "Acme AI").props["aria-expanded"]).toBe(false);
    expect(modelRows(root)).toEqual([]);
  });

  test("the company holding the current model starts open, and the exact offering is marked", () => {
    const { root } = renderBody({ value: { model: "acme-large", provider: "router" } });
    expect(groupNamed(root, "RouterCo").props["aria-expanded"]).toBe(true);
    expect(groupNamed(root, "Acme AI").props["aria-expanded"]).toBe(false);
    expect(byLabel(root, ROUTER_LARGE).props["aria-current"]).toBe("true");
  });

  test("the same ID under another company isn't marked", () => {
    const { root } = renderBody({ value: { model: "acme-large", provider: "router" } });
    act(() => groupNamed(root, "Acme AI").props.onClick());
    expect(byLabel(root, ACME_LARGE).props["aria-current"]).toBeUndefined();
  });

  test("a catalog with one company opens it", () => {
    const { root } = renderBody({
      catalog: testPickerCatalog({
        models: testPickerCatalog().models.filter((model) => model.provider === "acme"),
        providers: testPickerCatalog().providers.filter((provider) => provider.id === "acme"),
      }),
    });
    expect(modelRows(root)).toEqual([ACME_LARGE, ACME_SMALL]);
  });

  test("badges say default, small default and setup required", () => {
    const { root } = renderBody();
    act(() => groupNamed(root, "Acme AI").props.onClick());
    act(() => groupNamed(root, "RouterCo").props.onClick());
    expect(text(byLabel(root, ACME_LARGE))).toContain("Default");
    expect(text(byLabel(root, ACME_SMALL))).toContain("Small default");
    expect(text(byLabel(root, ROUTER_LARGE))).toContain("Setup required");
    expect(text(byLabel(root, ACME_LARGE))).not.toContain("Setup required");
  });

  test("each row shows the friendly name and exact ID", () => {
    const { root } = renderBody();
    act(() => groupNamed(root, "RouterCo").props.onClick());
    const row = byLabel(root, ROUTER_LARGE);
    expect(text(row)).toContain("Acme Large via Router");
    expect(text(row)).toContain("acme-large");
  });

  test("no control sits inside another", () => {
    const { root } = renderBody({
      value: { model: "acme-large", provider: "acme" },
      stars: { stars: [{ provider: "router", id: "router-fast" }], problem: null },
      catalog: testPickerCatalog({ models: testPickerCatalog().models.map((model) => ({ ...model, configured: false })) }),
    });
    const interactive = root.findAll(
      (node) => typeof node.type === "string" && ["button", "a", "input", "select", "textarea"].includes(node.type),
    );
    expect(interactive.length).toBeGreaterThan(5);
    for (const control of interactive) {
      const nested = control.findAll(
        (node) =>
          node !== control &&
          typeof node.type === "string" &&
          ["button", "a", "input", "select", "textarea"].includes(node.type),
      );
      expect(nested).toEqual([]);
    }
  });
});

describe("searching", () => {
  test("the box is what the dialog focuses when the window opens, not the grip before it", () => {
    const { root } = renderBody();
    expect(byLabel(root, "Search models and companies").props["data-autofocus"]).toBe(true);
    const marked = root.findAll((node) => typeof node.type === "string" && node.props["data-autofocus"] !== undefined);
    expect(marked).toHaveLength(1);
  });

  test("matches show without opening anything, across companies", () => {
    const { root, search } = renderBody();
    search("large");
    expect(modelRows(root)).toEqual([ACME_LARGE, ROUTER_LARGE]);
    expect(statusLines(root)[0]).toBe("2 models from 2 companies");
  });

  test("a company name lists its models and the count says how many of them matched", () => {
    const { root, search } = renderBody();
    search("acme ai");
    expect(modelRows(root)).toEqual([ACME_LARGE, ACME_SMALL]);
    expect(groupHeaders(root).map(text)).toEqual(["Acme AI2 models"]);

    search("bolt");
    expect(groupHeaders(root).map(text)).toEqual(["RouterCo1 of 2Setup required"]);
  });

  test("a company can be folded during a search, and the next search opens it again", () => {
    const { root, search } = renderBody();
    search("large");
    act(() => groupNamed(root, "Acme AI").props.onClick());
    expect(modelRows(root)).toEqual([ROUTER_LARGE]);
    search("larg");
    expect(modelRows(root)).toEqual([ACME_LARGE, ROUTER_LARGE]);
  });

  test("folding during a search doesn't change what browsing shows afterward", () => {
    const { root, search } = renderBody();
    search("large");
    act(() => groupNamed(root, "Acme AI").props.onClick());
    search("");
    expect(modelRows(root)).toEqual([]);
  });

  test("no match says so and offers the text as a custom ID", () => {
    const { root, search } = renderBody();
    search("brand-new-model");
    expect(statusLines(root)[0]).toBe("No matches");
    act(() => buttonNamed(root, "Use “brand-new-model” as a custom model ID").props.onClick());
    expect(byLabel(root, "Search models and companies")).toBeDefined();
    expect(hosts(root, "input")[1].props.value).toBe("brand-new-model");
  });
});

describe("choosing", () => {
  test("a row sends its model and provider together", () => {
    const { root, onChoose } = renderBody();
    act(() => groupNamed(root, "Acme AI").props.onClick());
    act(() => byLabel(root, ACME_SMALL).props.onClick());
    expect(onChoose).toHaveBeenCalledWith({ model: "acme-small", provider: "acme" });
  });

  test("the same ID under two companies sends the company of the row clicked", () => {
    const { root, onChoose, search } = renderBody();
    search("large");
    act(() => byLabel(root, ROUTER_LARGE).props.onClick());
    expect(onChoose).toHaveBeenLastCalledWith({ model: "acme-large", provider: "router" });
    act(() => byLabel(root, ACME_LARGE).props.onClick());
    expect(onChoose).toHaveBeenLastCalledWith({ model: "acme-large", provider: "acme" });
  });

  test("unconfigured offerings can still be chosen", () => {
    const { root, onChoose } = renderBody();
    act(() => groupNamed(root, "RouterCo").props.onClick());
    act(() => byLabel(root, ROUTER_LARGE).props.onClick());
    expect(onChoose).toHaveBeenCalledTimes(1);
  });

  test("nothing is sent by opening, re-rendering or the catalog arriving", () => {
    const { onChoose, rerender } = renderBody({
      value: { model: "vendor/experimental-9" },
      catalog: testPickerCatalog({ status: "loading", models: [], providers: [] }),
    });
    rerender({ catalog: testPickerCatalog() });
    rerender({ catalog: testPickerCatalog() });
    expect(onChoose).not.toHaveBeenCalled();
  });

  test("an imported model the catalog doesn't list is named and left alone", () => {
    const { root, onChoose } = renderBody({ value: { model: "vendor/experimental-9", provider: "vendor" } });
    const notes = root.findAll((node) => node.type === "p" && text(node).includes("is not in the catalog"));
    expect(text(notes[0])).toContain("vendor/experimental-9");
    expect(onChoose).not.toHaveBeenCalled();
  });
});

describe("going back to what is inherited", () => {
  test("the clear row says what applies, and sends null", () => {
    const { root, onChoose } = renderBody({
      value: { model: "acme-small", provider: "acme" },
      inherited: {
        model: { value: "acme-large", source: "Workflow default" },
        provider: { value: "acme", source: "Workflow default" },
      },
    });
    const row = buttonNamed(root, "Use inheritedWorkflow default · Acme Large");
    expect(row.props["aria-current"]).toBeUndefined();
    act(() => row.props.onClick());
    expect(onChoose).toHaveBeenCalledWith(null);
  });

  test("it reads as current when the element sets nothing", () => {
    const { root } = renderBody();
    expect(buttonNamed(root, "Use inheritedServer default").props["aria-current"]).toBe("true");
  });

  test("the workflow default's row can carry its own hint", () => {
    const { root } = renderBody({
      inheritLabel: "Use server default",
      inheritHint: "This workflow sets no default model.",
    });
    expect(buttonNamed(root, "Use server defaultThis workflow sets no default model.")).toBeDefined();
  });

  test("it steps aside during a search, which is only about models", () => {
    const { root, search } = renderBody();
    search("acme");
    expect(root.findAll((node) => node.type === "button" && text(node).startsWith("Use inherited"))).toEqual([]);
  });
});

describe("stars", () => {
  let harnessStore: ReturnType<typeof createModelStarStore>;

  function StarHarness({ onChoose }: { onChoose: (choice: unknown) => void }) {
    const stars = useSyncExternalStore(harnessStore.subscribe, harnessStore.getSnapshot);
    return (
      <MemoryRouter>
        <ModelPickerBody
          catalog={testPickerCatalog()}
          value={{}}
          inherited={{}}
          inheritLabel="Use inherited"
          stars={stars}
          onToggleStar={harnessStore.toggle}
          onChoose={onChoose}
        />
      </MemoryRouter>
    );
  }

  function renderWithStore() {
    const backing = new Map<string, string>();
    harnessStore = createModelStarStore(() => ({
      getItem: (name) => backing.get(name) ?? null,
      setItem: (name, value) => void backing.set(name, value),
    }));
    const onChoose = mock((_choice: unknown) => {});
    let renderer!: TestRenderer.ReactTestRenderer;
    act(() => {
      renderer = TestRenderer.create(<StarHarness onChoose={onChoose} />);
    });
    act(() => groupNamed(renderer.root, "Acme AI").props.onClick());
    return { root: renderer.root, onChoose };
  }

  const star = (root: TestNode, label: string) => act(() => byLabel(root, label).props.onClick({}));

  test("starring doesn't choose, and the button reports its state", () => {
    const { root, onChoose } = renderWithStore();
    expect(byLabel(root, STAR_ACME_LARGE).props["aria-pressed"]).toBe(false);
    star(root, STAR_ACME_LARGE);
    expect(onChoose).not.toHaveBeenCalled();
    expect(root.findAll((node) => node.type === "button" && node.props["aria-label"] === STAR_ACME_LARGE)[0].props["aria-pressed"]).toBe(true);
  });

  test("a star pins the model at the top and leaves its row where it was", () => {
    const { root } = renderWithStore();
    star(root, STAR_ACME_LARGE);
    expect(modelRows(root)).toEqual([ACME_LARGE, ACME_LARGE, ACME_SMALL]);
    expect(text(root.findAll((node) => node.type === "h3")[0])).toBe("Starred1");
  });

  test("the pinned row names its company and chooses like any other", () => {
    const { root, onChoose } = renderWithStore();
    star(root, STAR_ACME_LARGE);
    const [pinned] = root.findAll((node) => node.type === "button" && node.props["aria-label"] === ACME_LARGE);
    expect(text(pinned)).toContain("Acme AI · acme-large");
    act(() => pinned.props.onClick());
    expect(onChoose).toHaveBeenCalledWith({ model: "acme-large", provider: "acme" });
  });

  test("a star on one company's copy of an ID doesn't mark the other's", () => {
    const { root } = renderWithStore();
    act(() => groupNamed(root, "RouterCo").props.onClick());
    star(root, STAR_ACME_LARGE);
    const routerStars = root.findAll((node) => node.type === "button" && node.props["aria-label"] === STAR_ROUTER_LARGE);
    expect(routerStars.map((node) => node.props["aria-pressed"])).toEqual([false]);
  });

  test("removing the star from the pinned row takes the pin away and keeps the row", () => {
    const { root } = renderWithStore();
    star(root, STAR_ACME_LARGE);
    const [pinnedStar] = root.findAll((node) => node.type === "button" && node.props["aria-label"] === STAR_ACME_LARGE);
    act(() => pinnedStar.props.onClick({}));
    expect(modelRows(root)).toEqual([ACME_LARGE, ACME_SMALL]);
    expect(root.findAll((node) => node.type === "h3").map(text)).not.toContain("Starred0");
  });

  test("saving is reported truthfully", () => {
    const { root } = renderWithStore();
    expect(statusLines(root).at(-1)).toBe("Stars are saved in this browser.");
    star(root, STAR_ACME_LARGE);
    expect(statusLines(root).at(-1)).toBe("Starred Acme Large. Saved in this browser.");
  });

  test("a star that didn't save says so instead of claiming it did", () => {
    const { root } = renderBody({
      stars: { stars: [], problem: "write-failed" },
      onToggleStar: () => ({ starred: true, problem: "write-failed" }),
    });
    act(() => groupNamed(root, "Acme AI").props.onClick());
    act(() => byLabel(root, STAR_ACME_LARGE).props.onClick({}));
    expect(statusLines(root).at(-1)).toBe("That star was not saved. It lasts until this page reloads.");
  });

  test("a starred model the catalog dropped stays visible and marked", () => {
    const { root, onChoose } = renderBody({ stars: { stars: [{ provider: "gone", id: "retired" }], problem: null } });
    const row = byLabel(root, "retired, from gone, not in the catalog");
    expect(text(row)).toContain("Not in catalog");
    act(() => row.props.onClick());
    expect(onChoose).toHaveBeenCalledWith({ model: "retired", provider: "gone" });
  });

  test("stars still list while the catalog is away, without claiming anything about it", () => {
    const { root } = renderBody({
      catalog: testPickerCatalog({ status: "error", models: [], providers: [] }),
      stars: { stars: [{ provider: "acme", id: "acme-large" }], problem: null },
    });
    expect(modelRows(root)).toEqual(["acme-large, from acme"]);
  });
});

describe("a custom ID", () => {
  function openCustom(root: TestNode) {
    act(() => buttonNamed(root, "Custom ID").props.onClick());
  }

  function customFields(root: TestNode): { model: TestNode; provider: TestNode } {
    const form = hosts(root, "form")[0];
    const [model, provider] = hosts(form, "input");
    return { model, provider };
  }

  test("sends the model and provider as typed", () => {
    const { root, onChoose } = renderBody();
    openCustom(root);
    typeInto(customFields(root).model, "  vendor/new-1 ");
    typeInto(customFields(root).provider, "vendor");
    submitForm(root);
    expect(onChoose).toHaveBeenCalledWith({ model: "vendor/new-1", provider: "vendor" });
  });

  test("a blank provider sends none, so a stale one is removed", () => {
    const { root, onChoose } = renderBody();
    openCustom(root);
    typeInto(customFields(root).model, "vendor/new-1");
    submitForm(root);
    expect(onChoose).toHaveBeenCalledWith({ model: "vendor/new-1" });
  });

  test("an empty model is refused, with a reason, and sends nothing", () => {
    const { root, onChoose } = renderBody();
    openCustom(root);
    submitForm(root);
    expect(onChoose).not.toHaveBeenCalled();
    expect(text(root.find((node) => node.props.role === "alert"))).toBe("Enter a model ID.");
    expect(customFields(root).model.props["aria-invalid"]).toBe(true);
  });

  test("starts from an imported value, so it can be corrected", () => {
    const { root } = renderBody({ value: { model: "vendor/experimental-9", provider: "vendor" } });
    openCustom(root);
    expect(customFields(root).model.props.value).toBe("vendor/experimental-9");
    expect(customFields(root).provider.props.value).toBe("vendor");
  });

  test("starts empty over a model the catalog knows", () => {
    const { root } = renderBody({ value: { model: "acme-large", provider: "acme" } });
    openCustom(root);
    expect(customFields(root).model.props.value).toBe("");
  });

  test("a provider that lists no models offers it with the provider filled in", () => {
    const { root } = renderBody();
    act(() => groupNamed(root, "Local Gateway").props.onClick());
    act(() => buttonNamed(root, "Use a custom ID").props.onClick());
    expect(customFields(root).provider.props.value).toBe("local");
  });

  test("provider suggestions come from the catalog", () => {
    const { root } = renderBody();
    openCustom(root);
    expect(hosts(hosts(root, "datalist")[0], "option").map((node) => node.props.value)).toEqual(["acme", "router", "local"]);
  });

  test("an explicit provider preset replaces an already open custom form", () => {
    const { root } = renderBody({ value: { model: "old", provider: "acme" } });
    openCustom(root);
    typeInto(customFields(root).model, "typed-model");
    act(() => groupNamed(root, "Local Gateway").props.onClick());
    act(() => buttonNamed(root, "Use a custom ID").props.onClick());
    expect(customFields(root).model.props.value).toBe("");
    expect(customFields(root).provider.props.value).toBe("local");
  });

  test("search preserves typing until its custom-ID preset is explicitly chosen", () => {
    const { root } = renderBody();
    openCustom(root);
    typeInto(customFields(root).model, "unfinished-model");
    typeInto(byLabel(root, "Search models and companies"), "brand-new-model");
    expect(customFields(root).model.props.value).toBe("unfinished-model");
    act(() => buttonNamed(root, "Use “brand-new-model” as a custom model ID").props.onClick());
    expect(customFields(root).model.props.value).toBe("brand-new-model");
  });

  test("cancel closes the form and chooses nothing", () => {
    const { root, onChoose } = renderBody();
    openCustom(root);
    act(() => buttonNamed(root, "Cancel").props.onClick());
    expect(hosts(root, "form")).toEqual([]);
    expect(onChoose).not.toHaveBeenCalled();
  });
});

describe("catalog states", () => {
  test("loading says so, and the clear row and custom ID stay usable", () => {
    const { root } = renderBody({ catalog: testPickerCatalog({ status: "loading", models: [], providers: [] }) });
    expect(statusLines(root)).toContain("Loading the model catalog…");
    expect(groupHeaders(root)).toEqual([]);
    expect(buttonNamed(root, "Custom ID")).toBeDefined();
  });

  test("an error says so and offers a retry", () => {
    const retry = mock(() => {});
    const { root } = renderBody({ catalog: testPickerCatalog({ status: "error", models: [], providers: [], retry }) });
    expect(text(root.find((node) => node.props.role === "alert"))).toContain("did not load");
    act(() => buttonNamed(root, "Retry").props.onClick());
    expect(retry).toHaveBeenCalledTimes(1);
  });

  test("retry can't be pressed twice while it runs", () => {
    const { root } = renderBody({
      catalog: testPickerCatalog({ status: "error", models: [], providers: [], retrying: true }),
    });
    expect(buttonNamed(root, "Retrying…").props.disabled).toBe(true);
  });

  test("losing only the company names shows the IDs and says why", () => {
    const { root } = renderBody({ catalog: testPickerCatalog({ providers: [], providersFailed: true }) });
    expect(text(root.find((node) => node.props.role === "alert"))).toContain("Company names did not load");
    expect(groupHeaders(root).map(text)[0]).toContain("acme");
  });

  test("an empty catalog points at Settings › Models", () => {
    const { root } = renderBody({ catalog: testPickerCatalog({ models: [], providers: [] }) });
    expect(statusLines(root)[0]).toBe("No models");
    expect(hosts(root, "a")[0].props.href).toBe("/settings/models");
  });

  test("when nothing is set up it points at Settings › Models and still lets a model be chosen", () => {
    const models = testPickerCatalog().models.map((model) => ({ ...model, configured: false }));
    const { root, onChoose } = renderBody({ catalog: testPickerCatalog({ models }) });
    expect(hosts(root, "a")[0].props.href).toBe("/settings/models");
    act(() => groupNamed(root, "Acme AI").props.onClick());
    act(() => byLabel(root, "Acme Large, acme-large, from Acme AI, provider default, setup required").props.onClick());
    expect(onChoose).toHaveBeenCalledWith({ model: "acme-large", provider: "acme" });
  });

  test("when something is set up there is no such note", () => {
    const { root } = renderBody();
    expect(hosts(root, "a")).toEqual([]);
  });
});

// ── The field and its window ────────────────────────────────────────────────

describe("the field", () => {
  function renderField(overrides: Partial<ComponentProps<typeof ModelPickerControl>> = {}) {
    const onChange = mock((_choice: unknown) => {});
    const retry = mock(() => {});
    const props: ComponentProps<typeof ModelPickerControl> = {
      label: "Model",
      value: { model: "acme-large", provider: "acme" },
      inheritLabel: "Use inherited",
      catalog: testPickerCatalog({ retry }),
      stars: NO_STARS,
      onToggleStar: () => ({ starred: true, problem: null }),
      onChange,
      ...overrides,
    };
    const field = (next: typeof props) => (
      <MemoryRouter>
        <ModelPickerControl {...next} />
      </MemoryRouter>
    );
    let renderer!: TestRenderer.ReactTestRenderer;
    act(() => {
      renderer = TestRenderer.create(field(props), {
        createNodeMock: (element) =>
          element.type === "button" && element.props["aria-haspopup"] === "dialog"
            ? { getBoundingClientRect: () => ({ left: 900, top: 100, width: 300, height: 44 }) }
            : null,
      });
    });
    const { root } = renderer;
    const trigger = () => root.find((node) => node.type === "button" && node.props["aria-haspopup"] === "dialog");
    const isOpen = () => root.findAll((node) => node.type === "div" && node.props.className === "wfb-popup").length > 0;
    const open = () => act(() => trigger().props.onClick());
    return { root, renderer, onChange, retry, trigger, isOpen, open, field, props };
  }

  test("starts closed and names what is chosen", () => {
    const { trigger, isOpen } = renderField();
    expect(isOpen()).toBe(false);
    expect(trigger().props["aria-expanded"]).toBe(false);
    expect(trigger().props["aria-label"]).toBe("Model: Acme Large, Acme AI · acme-large");
  });

  test("the label points at the trigger", () => {
    const { root, trigger } = renderField();
    expect(hosts(root, "label")[0].props.htmlFor).toBe(trigger().props.id);
  });

  test("opening writes nothing and asks the catalog again, since the shared hooks never refetch", () => {
    const { trigger, isOpen, open, onChange, retry } = renderField();
    open();
    expect(isOpen()).toBe(true);
    expect(trigger().props["aria-expanded"]).toBe(true);
    expect(onChange).not.toHaveBeenCalled();
    expect(retry).toHaveBeenCalledTimes(1);
  });

  test("the X closes it and writes nothing", () => {
    const { root, isOpen, open, onChange } = renderField();
    open();
    act(() => byLabel(root, "Close model picker").props.onClick());
    expect(isOpen()).toBe(false);
    expect(onChange).not.toHaveBeenCalled();
  });

  test("the dialog's close, which Escape calls, closes it and writes nothing", () => {
    const { isOpen, open, onChange } = renderField();
    open();
    act(() => dialogOnClose());
    expect(isOpen()).toBe(false);
    expect(onChange).not.toHaveBeenCalled();
  });

  test("a press on the empty background leaves it open and writes nothing", () => {
    const { root, isOpen, open, onChange } = renderField();
    open();
    const boundary = root.find((node) => node.type === "div" && node.props.className === "wfb-popup-boundary");
    act(() => boundary.props.onMouseDown({ target: boundary, currentTarget: boundary, preventDefault: () => {} }));
    expect(isOpen()).toBe(true);
    expect(onChange).not.toHaveBeenCalled();
  });

  test("choosing a model writes the pair and closes", () => {
    const { root, isOpen, open, onChange } = renderField();
    open();
    act(() => byLabel(root, ACME_SMALL).props.onClick());
    expect(onChange).toHaveBeenCalledWith({ model: "acme-small", provider: "acme" });
    expect(isOpen()).toBe(false);
  });

  test("the clear row removes the choice and closes", () => {
    const { root, isOpen, open, onChange } = renderField();
    open();
    act(() => root.find((node) => node.type === "button" && text(node).startsWith("Use inherited")).props.onClick());
    expect(onChange).toHaveBeenCalledWith(null);
    expect(isOpen()).toBe(false);
  });

  test("a custom ID writes and closes", () => {
    const { root, isOpen, open, onChange } = renderField();
    open();
    act(() => buttonNamed(root, "Custom ID").props.onClick());
    typeInto(hosts(hosts(root, "form")[0], "input")[0], "acme-next");
    submitForm(root);
    // The current provider is filled in, visibly, so a newer model from the same company is one field.
    expect(onChange).toHaveBeenCalledWith({ model: "acme-next", provider: "acme" });
    expect(isOpen()).toBe(false);
  });

  test("a refused custom ID stays open with its reason", () => {
    const { root, isOpen, open, onChange } = renderField();
    open();
    act(() => buttonNamed(root, "Custom ID").props.onClick());
    submitForm(root);
    expect(onChange).not.toHaveBeenCalled();
    expect(isOpen()).toBe(true);
  });

  test("a star, a search and a fold all leave it open", () => {
    const { root, isOpen, open, onChange } = renderField();
    open();
    act(() => byLabel(root, STAR_ACME_LARGE).props.onClick({}));
    typeInto(byLabel(root, "Search models and companies"), "large");
    act(() => groupNamed(root, "Acme AI").props.onClick());
    expect(isOpen()).toBe(true);
    expect(onChange).not.toHaveBeenCalled();
  });

  test("the catalog refreshing doesn't disturb what the author is doing", () => {
    const { root, renderer, field, props, open, isOpen } = renderField();
    open();
    typeInto(byLabel(root, "Search models and companies"), "large");
    act(() => renderer.update(field({ ...props, catalog: testPickerCatalog() })));
    expect(isOpen()).toBe(true);
    expect(byLabel(root, "Search models and companies").props.value).toBe("large");
  });

  test("a choice goes to the latest onChange, never one kept from when it opened", () => {
    const { root, renderer, field, props, open, onChange } = renderField();
    open();
    const latest = mock((_choice: unknown) => {});
    act(() => renderer.update(field({ ...props, onChange: latest })));
    act(() => byLabel(root, ACME_SMALL).props.onClick());
    expect(latest).toHaveBeenCalledWith({ model: "acme-small", provider: "acme" });
    expect(onChange).not.toHaveBeenCalled();
  });

  test("opening again starts fresh", () => {
    const { root, open } = renderField();
    open();
    typeInto(byLabel(root, "Search models and companies"), "large");
    act(() => byLabel(root, "Close model picker").props.onClick());
    open();
    expect(byLabel(root, "Search models and companies").props.value).toBe("");
  });

  test("a locked field can't open, and locking one hides its window", () => {
    const locked = renderField({ disabled: true });
    expect(locked.trigger().props.disabled).toBe(true);

    const { renderer, field, props, open, isOpen } = renderField();
    open();
    act(() => renderer.update(field({ ...props, disabled: true })));
    expect(isOpen()).toBe(false);
  });

  test("names the grip and the X the way the author asked", () => {
    const { root, open } = renderField();
    open();
    expect(hasLabel(root, "Move model picker")).toBe(true);
    expect(hasLabel(root, "Close model picker")).toBe(true);
  });

  test("warns about a model paired with a provider that doesn't serve it, and changes nothing", () => {
    const { root, onChange } = renderField({ value: { model: "router-fast", provider: "acme" } });
    const warning = root.find((node) => node.type === "p" && node.props.className === "wfb-field-warning");
    expect(text(warning)).toContain("router-fast is not listed under acme");
    expect(onChange).not.toHaveBeenCalled();
  });

  test("shows setup required and not-in-catalog on the field itself", () => {
    expect(text(renderField({ value: { model: "router-fast", provider: "router" } }).trigger())).toContain("Setup required");
    expect(text(renderField({ value: { model: "vendor/x" } }).trigger())).toContain("Not in catalog");
  });

  test("shows where an inherited model comes from", () => {
    const { trigger } = renderField({
      value: {},
      inherited: {
        model: { value: "acme-large", source: "Workflow default" },
        provider: { value: "acme", source: "Workflow default" },
      },
    });
    expect(trigger().props["aria-label"]).toBe(
      "Model: Acme Large, inherited from Workflow default, Acme AI · acme-large",
    );
  });
});
