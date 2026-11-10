import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import { AppearanceProvider } from "../components/appearance-provider";
import {
  THEMES,
  createAppearanceStore,
  type AppearanceEnvironment,
  type AppearanceStore,
} from "../lib/appearance";
import { setupReactTestEnv } from "../lib/test-utils";
import { navSections } from "./settings";
import SettingsAppearance from "./settings-appearance";

function storeWith({
  cookie = "",
  refuse = false,
}: { cookie?: string; refuse?: boolean } = {}): AppearanceStore {
  let jar = cookie;
  const environment: AppearanceEnvironment = {
    readCookie: () => jar,
    writeCookie: (value) => {
      if (!refuse) jar = value.split(";")[0]!;
    },
    isSecureContext: () => false,
    systemDarkQuery: () => null,
    applyToDocument: () => {},
    subscribeToFocus: () => () => {},
  };
  return createAppearanceStore(environment);
}

function render(store: AppearanceStore) {
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(
      <AppearanceProvider store={store}>
        <SettingsAppearance />
      </AppearanceProvider>,
    );
  });
  return renderer;
}

function radios(renderer: TestRenderer.ReactTestRenderer, name: string) {
  return renderer.root.findAll(
    (node) => node.type === "input" && node.props.type === "radio" && node.props.name === name,
  );
}

function text(renderer: TestRenderer.ReactTestRenderer): string {
  const walk = (node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>): string => {
    if (!node) return "";
    if (typeof node === "string") return node;
    if (Array.isArray(node)) return node.map(walk).join("");
    return (node.children ?? []).map((child) => walk(child as typeof node)).join("");
  };
  return walk(renderer.toJSON());
}

function status(renderer: TestRenderer.ReactTestRenderer): string {
  const node = renderer.root.findByProps({ role: "status" });
  return node.children.filter((child) => typeof child === "string").join("");
}

let teardown: () => void;

beforeEach(() => {
  teardown = setupReactTestEnv();
});

afterEach(() => {
  teardown();
});

describe("SettingsAppearance", () => {
  test("offers the seven palettes as one radio group, with the three modes as another", () => {
    const renderer = render(storeWith());
    expect(radios(renderer, "appearance-theme").map((input) => input.props.value)).toEqual(
      THEMES.map((theme) => theme.id),
    );
    expect(radios(renderer, "appearance-mode").map((input) => input.props.value)).toEqual([
      "light",
      "dark",
      "system",
    ]);
    expect(renderer.root.findAll((node) => node.type === "fieldset")).toHaveLength(2);
    expect(renderer.root.findAll((node) => node.type === "legend").map((node) => node.children[0])).toEqual([
      "Palette",
      "Display mode",
    ]);
  });

  test("shows each palette's four swatches and marks only Brutalist as the default", () => {
    const renderer = render(storeWith());
    expect(renderer.root.findAll((node) => node.props.className === "theme-swatch")).toHaveLength(THEMES.length * 4);
    const page = text(renderer);
    for (const theme of THEMES) {
      expect(page).toContain(theme.name);
      expect(page).toContain(theme.description);
    }
    expect(page.match(/Default/g)).toHaveLength(1);
    expect(page).toContain("BrutalistDefault");
    expect(page).toContain("Match system");
  });

  test("defaults to Brutalist light", () => {
    const renderer = render(storeWith());
    expect(radios(renderer, "appearance-theme").filter((input) => input.props.checked).map((input) => input.props.value)).toEqual(["brutalist"]);
    expect(radios(renderer, "appearance-mode").filter((input) => input.props.checked).map((input) => input.props.value)).toEqual(["light"]);
    expect(status(renderer)).toBe("Brutalist · light.");
  });

  test("palette and mode change independently", () => {
    const store = storeWith({ cookie: "fabro-appearance=v2.command.dark" });
    const renderer = render(store);
    const lavender = radios(renderer, "appearance-theme").find((input) => input.props.value === "lavender")!;
    act(() => lavender.props.onChange());
    expect(store.getSnapshot()).toMatchObject({ theme: "lavender", displayMode: "dark" });
    expect(status(renderer)).toBe("Lavender workspace · dark.");
    const light = radios(renderer, "appearance-mode").find((input) => input.props.value === "light")!;
    act(() => light.props.onChange());
    expect(store.getSnapshot()).toMatchObject({ theme: "lavender", displayMode: "light" });
    expect(status(renderer)).toBe("Lavender workspace · light.");
  });

  test("says so plainly when the browser refuses the cookie", () => {
    const store = storeWith({ refuse: true });
    const renderer = render(store);
    const graphite = radios(renderer, "appearance-theme").find((input) => input.props.value === "graphite")!;
    act(() => graphite.props.onChange());
    expect(store.getSnapshot().theme).toBe("graphite");
    expect(status(renderer)).toContain("This browser would not save it, so it lasts for this visit.");
  });

  test("reset returns to Brutalist light", () => {
    const store = storeWith({ cookie: "fabro-appearance=v2.payline.dark" });
    const renderer = render(store);
    const button = renderer.root.findByProps({ id: "appearance-reset" });
    expect(button.children).toEqual(["Reset to Brutalist, light mode"]);
    act(() => button.props.onClick());
    expect(store.getSnapshot()).toMatchObject({ theme: "brutalist", displayMode: "light" });
  });

  test("has a settings nav entry", () => {
    const items = navSections.flatMap((section) => section.items);
    const entry = items.find((item) => item.href === "/settings/appearance");
    expect(entry?.name).toBe("Appearance");
    expect(entry?.match("/settings/appearance")).toBe(true);
  });
});
