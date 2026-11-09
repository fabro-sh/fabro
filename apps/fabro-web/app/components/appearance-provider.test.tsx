import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { useState } from "react";
import TestRenderer, { act } from "react-test-renderer";

import {
  createAppearanceStore,
  type AppearanceEnvironment,
  type AppearanceStore,
} from "../lib/appearance";
import { setupReactTestEnv } from "../lib/test-utils";
import {
  AppearanceProvider,
  useAppearance,
  type AppearanceContextValue,
} from "./appearance-provider";

function memoryStore(initialCookie = ""): {
  store: AppearanceStore;
  listeners: Set<() => void>;
  setOsDark: (dark: boolean) => void;
} {
  let cookie = initialCookie;
  let dark = false;
  const listeners = new Set<() => void>();
  const environment: AppearanceEnvironment = {
    readCookie: () => cookie,
    writeCookie: (value) => {
      cookie = value.split(";")[0]!;
    },
    isSecureContext: () => false,
    systemDarkQuery: () => ({
      get matches() {
        return dark;
      },
      addEventListener: (_type, listener) => listeners.add(listener),
      removeEventListener: (_type, listener) => listeners.delete(listener),
    }),
    applyToDocument: () => {},
    subscribeToFocus: () => () => {},
  };
  return {
    store: createAppearanceStore(environment),
    listeners,
    setOsDark(next) {
      dark = next;
      for (const listener of [...listeners]) listener();
    },
  };
}

let seen: AppearanceContextValue;
let mounts = 0;

function Probe() {
  seen = useAppearance();
  return null;
}

/** Stands in for feature state, such as a builder draft. Counts how often it mounts. */
function FeatureState() {
  const [draft] = useState(() => {
    mounts += 1;
    return "unsaved draft";
  });
  return <span>{draft}</span>;
}

let teardown: () => void;

beforeEach(() => {
  teardown = setupReactTestEnv();
  mounts = 0;
});

afterEach(() => {
  teardown();
});

function mount(store: AppearanceStore, children = <Probe />) {
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(<AppearanceProvider store={store}>{children}</AppearanceProvider>);
  });
  return renderer;
}

describe("useAppearance", () => {
  test("outside a provider it answers with the default look and refuses to save", () => {
    act(() => {
      TestRenderer.create(<Probe />);
    });
    expect(seen.theme).toBe("brutalist");
    expect(seen.displayMode).toBe("light");
    expect(seen.setTheme("graphite")).toBe(false);
    expect(seen.setDisplayMode("dark")).toBe(false);
  });

  test("exposes the resolved palette and its literal colors", () => {
    mount(memoryStore("fabro-appearance=v2.brutalist.dark").store);
    expect(seen.theme).toBe("brutalist");
    expect(seen.displayMode).toBe("dark");
    expect(seen.appearance).toEqual({ theme: "brutalist", displayMode: "dark", mode: "dark" });
    expect(seen.colors.bg).toBe("#121411");
    expect(seen.colors.primary).toBe("#d9f536");
    expect(seen.terminalColors.background).toBe("#1d201b");
  });

  test("setTheme and setDisplayMode change one field and re-render at once", () => {
    mount(memoryStore("fabro-appearance=v2.command.light").store);
    let saved = false;
    act(() => {
      saved = seen.setTheme("lavender");
    });
    expect(saved).toBe(true);
    expect(seen.theme).toBe("lavender");
    expect(seen.displayMode).toBe("light");
    expect(seen.saveStatus).toBe("saved");
    act(() => {
      seen.setDisplayMode("dark");
    });
    expect(seen.theme).toBe("lavender");
    expect(seen.appearance.mode).toBe("dark");
    expect(seen.colors.bg).toBe("#171620");
  });

  test("resetAppearance returns to Brutalist light", () => {
    mount(memoryStore("fabro-appearance=v2.payline.dark").store);
    act(() => {
      seen.resetAppearance();
    });
    expect(seen.theme).toBe("brutalist");
    expect(seen.displayMode).toBe("light");
  });

  test("repaints Match system from OS changes and detaches on unmount", () => {
    const { store, listeners, setOsDark } = memoryStore("fabro-appearance=v2.signal.system");
    const renderer = mount(store);
    expect(listeners.size).toBe(1);
    expect(seen.appearance.mode).toBe("light");
    expect(seen.colors.bg).toBe("#f1f5f9");
    act(() => {
      setOsDark(true);
    });
    expect(seen.appearance.mode).toBe("dark");
    expect(seen.colors.bg).toBe("#0d1122");
    act(() => {
      renderer.unmount();
    });
    expect(listeners.size).toBe(0);
  });

  test("switching palette re-renders in place, without remounting what's below it", () => {
    const { store } = memoryStore();
    const renderer = mount(
      store,
      <>
        <Probe />
        <FeatureState />
      </>,
    );
    const before = renderer.root.findByType("span");
    act(() => {
      seen.setTheme("graphite");
    });
    act(() => {
      seen.setDisplayMode("dark");
    });
    expect(seen.theme).toBe("graphite");
    expect(mounts).toBe(1);
    expect(renderer.root.findByType("span")).toBe(before);
  });
});
