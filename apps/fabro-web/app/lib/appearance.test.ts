import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import {
  parseFactoryPalettes,
  renderAssistantUiCss,
  renderPaletteModule,
  type FactoryTokens,
} from "../../scripts/factory-themes";
import {
  APPEARANCE_COOKIE_MAX_AGE_SECONDS,
  APPEARANCE_COOKIE_NAME,
  DEFAULT_APPEARANCE,
  DISPLAY_MODES,
  SKETCH_FILTER_ID,
  THEMES,
  applyAppearanceToElement,
  createAppearanceStore,
  isDisplayMode,
  isThemeId,
  paletteColors,
  parseAppearanceCookie,
  resolveAppearance,
  serializeAppearanceCookie,
  terminalColorsFor,
  type AppearanceEnvironment,
  type ColorMode,
  type DisplayMode,
  type ResolvedAppearance,
  type ThemeId,
} from "./appearance";
import { contrastRatio, mixColors } from "./color-contrast";
import { FACTORY_PALETTES } from "./factory-palettes.generated";

const APP_ROOT = join(import.meta.dir, "..");
const WEB_ROOT = join(APP_ROOT, "..");
const COLOR_MODES = ["light", "dark"] as const;
const DISPLAY_MODE_IDS = ["light", "dark", "system"] as const;
const THEME_IDS = ["signal", "readability", "command", "payline", "brutalist", "graphite", "lavender"] as const;

const snapshot = readFileSync(join(APP_ROOT, "styles", "factory-themes.css"), "utf8");
const palettes = parseFactoryPalettes(snapshot);
const appCss = readFileSync(join(APP_ROOT, "app.css"), "utf8");
const appearanceCss = readFileSync(join(APP_ROOT, "styles", "appearance.css"), "utf8");
const template = readFileSync(join(WEB_ROOT, "index.template.html"), "utf8");

function fakeEnvironment({
  cookie = "",
  dark = false,
  refuseCookies = false,
  throwOnCookies = false,
  secure = false,
}: {
  cookie?: string;
  dark?: boolean;
  refuseCookies?: boolean;
  throwOnCookies?: boolean;
  secure?: boolean;
} = {}) {
  const queryListeners = new Set<() => void>();
  const focusListeners = new Set<() => void>();
  const applied: ResolvedAppearance[] = [];
  const written: string[] = [];
  const state = { cookie, dark };
  const environment: AppearanceEnvironment = {
    readCookie: () => {
      if (throwOnCookies) throw new Error("SecurityError");
      return state.cookie;
    },
    writeCookie: (value) => {
      if (throwOnCookies) throw new Error("SecurityError");
      written.push(value);
      if (!refuseCookies) state.cookie = value.split(";")[0]!;
    },
    isSecureContext: () => secure,
    systemDarkQuery: () => ({
      get matches() {
        return state.dark;
      },
      addEventListener: (_type, listener) => queryListeners.add(listener),
      removeEventListener: (_type, listener) => queryListeners.delete(listener),
    }),
    applyToDocument: (appearance) => applied.push(appearance),
    subscribeToFocus: (listener) => {
      focusListeners.add(listener);
      return () => focusListeners.delete(listener);
    },
  };
  return {
    environment,
    applied,
    written,
    state,
    queryListeners,
    focusListeners,
    setOsDark(next: boolean) {
      state.dark = next;
      for (const listener of [...queryListeners]) listener();
    },
    focus() {
      for (const listener of [...focusListeners]) listener();
    },
  };
}

describe("Factory source", () => {
  test("the palette snapshot matches its recorded version", () => {
    expect(createHash("sha256").update(snapshot).digest("hex")).toBe(
      "0af016dfa47cf43d64cc6bc6864a2427c06c6c43213f3377c0e80339750f198b",
    );
  });

  test("the generated files match the snapshot", () => {
    expect(readFileSync(join(APP_ROOT, "lib", "factory-palettes.generated.ts"), "utf8")).toBe(
      renderPaletteModule(palettes),
    );
    expect(readFileSync(join(APP_ROOT, "styles", "factory-assistant-ui.generated.css"), "utf8")).toBe(
      renderAssistantUiCss(palettes),
    );
  });

  test("registers Factory's seven palettes, in order, with Signal navy first", () => {
    expect(THEMES.map((theme) => [theme.id, theme.name])).toEqual([
      ["signal", "Signal navy"],
      ["readability", "Spacious warm"],
      ["command", "Blue command"],
      ["payline", "Rounded Payline"],
      ["brutalist", "Brutalist"],
      ["graphite", "Soft graphite"],
      ["lavender", "Lavender workspace"],
    ]);
    expect(DISPLAY_MODES.map((mode) => [mode.id, mode.name])).toEqual([
      ["light", "Light"],
      ["dark", "Dark"],
      ["system", "Match system"],
    ]);
    expect(DEFAULT_APPEARANCE).toEqual({ theme: "brutalist", displayMode: "light" });
  });

  test("resolves each palette's real tokens, not its illustrative swatches", () => {
    expect(palettes.signal.light).toMatchObject({ bg: "#f1f5f9", surface: "#ffffff", fg: "#0d1122", primary: "#0d1122", "accent-fill": "#fe4a23", rail: "#0d1122", "rail-selected": "#f1f5f9" });
    expect(palettes.signal.dark).toMatchObject({ bg: "#0d1122", surface: "#151b30", primary: "#fe4a23", "primary-fg": "#0d1122", rail: "#080b17" });
    expect(palettes.readability.light).toMatchObject({ bg: "#f7f6f2", primary: "#2756c7", rail: "#ffffff", "rail-selected-fg": "#2756c7" });
    expect(palettes.readability.dark).toMatchObject({ bg: "#151917", primary: "#a5bdff", ok: "#a2e5b8" });
    expect(palettes.command.light).toMatchObject({ bg: "#f2f5fb", primary: "#2358e8", rail: "#0b2050" });
    expect(palettes.command.dark).toMatchObject({ bg: "#0b1220", primary: "#7da2ff", rail: "#0a1835" });
    // Payline's primary is dark ink. Lime is its pop, not its button color.
    expect(palettes.payline.light).toMatchObject({ primary: "#202124", "accent-pop": "#d0ff00", "rail-selected": "#d0ff00" });
    expect(palettes.payline.dark).toMatchObject({ bg: "#17181b", primary: "#e7e9ef", "accent-fill": "#d0ff00" });
    expect(palettes.brutalist.light).toMatchObject({ bg: "#f0eee5", surface: "#fffef9", primary: "#171817", line: "#292b26" });
    expect(palettes.brutalist.dark).toMatchObject({ bg: "#121411", primary: "#d9f536" });
    expect(palettes.graphite.light).toMatchObject({ bg: "#dcdedf", surface: "#e7eaea", primary: "#244c3b", rail: "#191f1c" });
    expect(palettes.graphite.dark).toMatchObject({ bg: "#252a29", primary: "#b9e3cb", rail: "#141d17" });
    expect(palettes.lavender.light).toMatchObject({ bg: "#f1effa", primary: "#5940d5", rail: "#222330" });
    expect(palettes.lavender.dark).toMatchObject({ bg: "#171620", primary: "#b6a0ff", rail: "#0f1018" });
  });

  test("a dark palette inherits its own geometry, not Signal's", () => {
    expect(palettes.brutalist.dark).toMatchObject({ "radius-md": "3px", "radius-btn": "2px" });
    expect(palettes.payline.dark).toMatchObject({ "radius-md": "18px", "radius-btn": "999px" });
    expect(palettes.signal.dark["font-display"]).toContain("Unbounded");
    expect(palettes.readability.dark["font-display"]).toContain("Manrope");
  });

  test("the runtime colors are the snapshot's tokens", () => {
    for (const theme of THEME_IDS) {
      for (const mode of COLOR_MODES) {
        const colors = FACTORY_PALETTES[theme][mode];
        expect(colors.bg).toBe(palettes[theme][mode].bg!);
        expect(colors.accentSoftInk).toBe(palettes[theme][mode]["accent-soft-ink"]!);
        expect(colors.railSelectedFg).toBe(palettes[theme][mode]["rail-selected-fg"]!);
      }
    }
  });

  test("uses Factory's fonts from this origin, with their licenses", () => {
    expect(appCss).toContain('--font-sans: "Manrope", "IBM Plex Sans"');
    expect(appCss).toContain('--font-mono: "IBM Plex Mono"');
    expect(template).not.toContain("fonts.googleapis.com");
    const fonts = readFileSync(join(APP_ROOT, "styles", "factory-fonts.css"), "utf8");
    for (const match of fonts.matchAll(/url\("\/assets\/fonts\/([^"]+)"\)/g)) {
      expect(readFileSync(join(WEB_ROOT, "public", "assets", "fonts", match[1]!)).length).toBeGreaterThan(0);
    }
    for (const license of ["ofl-manrope.txt", "ofl-unbounded.txt", "ofl-ibm-plex-mono.txt"]) {
      expect(readFileSync(join(WEB_ROOT, "public", "licenses", license), "utf8")).toContain("SIL Open Font License");
    }
    const notices = readFileSync(join(WEB_ROOT, "public", "THIRD_PARTY_NOTICES.txt"), "utf8");
    expect(notices).toContain("Copyright (c) 2025 Cathryn Lavery");
  });

  test("the workflow builder's filter export still exists", () => {
    expect(SKETCH_FILTER_ID).toBe("fabro-sketchy");
  });
});

describe("resolution", () => {
  test("light and dark are independent of the palette", () => {
    for (const theme of THEME_IDS) {
      expect(resolveAppearance({ theme, displayMode: "light" }, true).mode).toBe("light");
      expect(resolveAppearance({ theme, displayMode: "dark" }, false).mode).toBe("dark");
    }
  });

  test("Match system follows the OS", () => {
    expect(resolveAppearance({ theme: "payline", displayMode: "system" }, false).mode).toBe("light");
    expect(resolveAppearance({ theme: "payline", displayMode: "system" }, true).mode).toBe("dark");
  });
});

describe("cookie", () => {
  test("round-trips every palette and mode", () => {
    for (const theme of THEME_IDS) {
      for (const displayMode of DISPLAY_MODE_IDS) {
        const cookie = serializeAppearanceCookie({ theme, displayMode }, { secure: false });
        expect(parseAppearanceCookie(cookie.split(";")[0])).toEqual({ theme, displayMode });
      }
    }
  });

  test("carries the attributes the preference needs and nothing else", () => {
    expect(serializeAppearanceCookie({ theme: "brutalist", displayMode: "dark" }, { secure: false }).split("; ")).toEqual([
      "fabro-appearance=v2.brutalist.dark",
      "Path=/",
      "Max-Age=31536000",
      "SameSite=Lax",
    ]);
    expect(APPEARANCE_COOKIE_MAX_AGE_SECONDS).toBe(31_536_000);
  });

  test("adds Secure only over HTTPS", () => {
    const saved = { theme: "signal", displayMode: "system" } as const;
    expect(serializeAppearanceCookie(saved, { secure: true })).toEndWith("; Secure");
    expect(serializeAppearanceCookie(saved, { secure: false })).not.toContain("Secure");
  });

  test("finds its value among other cookies and ignores other apps' appearance cookies", () => {
    expect(parseAppearanceCookie("session=abc; fabro-appearance=v2.graphite.light; x=1")).toEqual({ theme: "graphite", displayMode: "light" });
    expect(parseAppearanceCookie('cf-internal-appearance={"version":1,"theme":"lavender","mode":"dark"}')).toEqual(DEFAULT_APPEARANCE);
    expect(parseAppearanceCookie("not-fabro-appearance=v2.payline.dark")).toEqual(DEFAULT_APPEARANCE);
  });

  test.each([
    [""],
    [null],
    [undefined],
    ["fabro-appearance"],
    ["fabro-appearance="],
    ["fabro-appearance=signal"],
    ["fabro-appearance=v2."],
    ["fabro-appearance=v2.signal"],
    ["fabro-appearance=v2.unknown.dark"],
    ["fabro-appearance=v2.signal.sepia"],
    ["fabro-appearance=v2.constructor.dark"],
    ["fabro-appearance=v2.signal.dark.extra"],
    ["fabro-appearance=v3.signal.dark"],
    ["fabro-appearance=V2.signal.dark"],
    ["fabro-appearance=v1.unknown"],
    ["fabro-appearance=v1.__proto__"],
    ["fabro-appearance=v1.constructor"],
    ["fabro-appearance=%76%32.signal.dark"],
    ["fabro-appearance=<script>"],
    ["fabro-appearance=v2.payline.dark; fabro-appearance=v2.graphite.light"],
  ])("falls back to the default look for %p", (cookie) => {
    expect(parseAppearanceCookie(cookie)).toEqual(DEFAULT_APPEARANCE);
  });

  test.each([
    ["system", "system"],
    ["editorial-light", "light"],
    ["full-editorial", "light"],
    ["sketch", "light"],
    ["consultant", "light"],
    ["editorial-dark", "dark"],
    ["terminal", "dark"],
  ] as const)("migrates the old %s preset to Brutalist, %s", (preset, displayMode) => {
    expect(parseAppearanceCookie(`fabro-appearance=v1.${preset}`)).toEqual({ theme: "brutalist", displayMode });
  });

  test("rejects anything outside the allow-lists", () => {
    expect(isThemeId("signal")).toBe(true);
    expect(isThemeId("constructor")).toBe(false);
    expect(isDisplayMode("system")).toBe(true);
    expect(isDisplayMode("sepia")).toBe(false);
  });
});

describe("store", () => {
  test("starts from the cookie and themes the document straight away", () => {
    const env = fakeEnvironment({ cookie: "fabro-appearance=v2.command.dark" });
    const store = createAppearanceStore(env.environment);
    expect(store.getSnapshot()).toEqual({ theme: "command", displayMode: "dark", mode: "dark", saveStatus: "idle" });
    expect(env.applied.at(-1)?.theme).toBe("command");
  });

  test("defaults to Brutalist light even on a dark device, with nothing written", () => {
    const env = fakeEnvironment({ dark: true });
    const store = createAppearanceStore(env.environment);
    expect(store.getSnapshot()).toMatchObject({ theme: "brutalist", displayMode: "light", mode: "light" });
    expect(env.written).toEqual([]);
  });

  test("a palette change keeps the mode, and a mode change keeps the palette", () => {
    const env = fakeEnvironment({ cookie: "fabro-appearance=v2.command.dark" });
    const store = createAppearanceStore(env.environment);
    expect(store.update({ theme: "lavender" })).toBe(true);
    expect(store.getSnapshot()).toMatchObject({ theme: "lavender", displayMode: "dark", mode: "dark", saveStatus: "saved" });
    expect(store.update({ displayMode: "light" })).toBe(true);
    expect(store.getSnapshot()).toMatchObject({ theme: "lavender", displayMode: "light", mode: "light" });
    expect(env.written.at(-1)).toStartWith("fabro-appearance=v2.lavender.light;");
  });

  test("an old v1 cookie moves to v2 at the next choice, keeping its mode", () => {
    const env = fakeEnvironment({ cookie: "fabro-appearance=v1.terminal" });
    const store = createAppearanceStore(env.environment);
    expect(env.written).toEqual([]);
    store.update({ theme: "graphite" });
    expect(env.state.cookie).toBe("fabro-appearance=v2.graphite.dark");
  });

  test("notifies subscribers and writes Secure over HTTPS", () => {
    const env = fakeEnvironment({ secure: true });
    const store = createAppearanceStore(env.environment);
    const notified: string[] = [];
    store.subscribe(() => notified.push(store.getSnapshot().theme));
    store.update({ theme: "payline" });
    expect(notified).toEqual(["payline"]);
    expect(env.written.at(-1)).toContain("Secure");
  });

  test("a new store over the same cookie restores the choice, as a reload does", () => {
    const env = fakeEnvironment();
    createAppearanceStore(env.environment).update({ theme: "brutalist", displayMode: "light" });
    expect(createAppearanceStore(env.environment).getSnapshot()).toMatchObject({ theme: "brutalist", displayMode: "light" });
  });

  test("a refused cookie still applies the choice but reports it won't persist", () => {
    const env = fakeEnvironment({ refuseCookies: true });
    const store = createAppearanceStore(env.environment);
    expect(store.update({ theme: "lavender" })).toBe(false);
    expect(store.getSnapshot()).toMatchObject({ theme: "lavender", saveStatus: "blocked" });
    expect(env.applied.at(-1)?.theme).toBe("lavender");
  });

  test("a refused default isn't mistaken for saved just because it's also the fallback", () => {
    const env = fakeEnvironment({ refuseCookies: true });
    const store = createAppearanceStore(env.environment);
    expect(store.update(DEFAULT_APPEARANCE)).toBe(false);
    expect(store.getSnapshot().saveStatus).toBe("blocked");
  });

  test("a shadowing second cookie counts as not saved", () => {
    const env = fakeEnvironment();
    env.environment.writeCookie = (value) => {
      env.state.cookie = `${value.split(";")[0]}; fabro-appearance=v2.command.dark`;
    };
    const store = createAppearanceStore(env.environment);
    expect(store.update({ theme: "graphite" })).toBe(false);
    expect(store.getSnapshot()).toMatchObject({ theme: "graphite", saveStatus: "blocked" });
  });

  test("a throwing cookie jar degrades to the default instead of crashing", () => {
    const env = fakeEnvironment({ throwOnCookies: true });
    const store = createAppearanceStore(env.environment);
    expect(store.getSnapshot().theme).toBe("brutalist");
    expect(store.update({ theme: "graphite" })).toBe(false);
    expect(store.getSnapshot()).toMatchObject({ theme: "graphite", saveStatus: "blocked" });
  });

  test("ignores values outside the allow-lists", () => {
    const env = fakeEnvironment();
    const store = createAppearanceStore(env.environment);
    expect(store.update({ theme: "hacker" as ThemeId })).toBe(false);
    expect(store.update({ displayMode: "sepia" as DisplayMode })).toBe(false);
    expect(store.getSnapshot().theme).toBe("brutalist");
    expect(env.written).toEqual([]);
  });

  test("Match system repaints when the OS changes, without writing", () => {
    const env = fakeEnvironment({ dark: false, cookie: "fabro-appearance=v2.signal.system" });
    const store = createAppearanceStore(env.environment);
    const unsubscribe = store.subscribe(() => {});
    expect(store.getSnapshot().mode).toBe("light");
    env.setOsDark(true);
    expect(store.getSnapshot().mode).toBe("dark");
    expect(env.applied.at(-1)?.mode).toBe("dark");
    expect(env.written).toEqual([]);
    unsubscribe();
  });

  test.each(["light", "dark"] as const)("%s ignores OS changes", (displayMode) => {
    const env = fakeEnvironment({ cookie: `fabro-appearance=v2.graphite.${displayMode}` });
    const store = createAppearanceStore(env.environment);
    const unsubscribe = store.subscribe(() => {});
    const before = store.getSnapshot();
    const appliedBefore = env.applied.length;
    env.setOsDark(displayMode === "light");
    expect(store.getSnapshot()).toBe(before);
    expect(env.applied.length).toBe(appliedBefore);
    unsubscribe();
  });

  test("picks up a choice made in another tab when this one regains focus", () => {
    const env = fakeEnvironment();
    const store = createAppearanceStore(env.environment);
    const unsubscribe = store.subscribe(() => {});
    env.state.cookie = "fabro-appearance=v2.lavender.dark";
    env.focus();
    expect(store.getSnapshot()).toMatchObject({ theme: "lavender", displayMode: "dark", mode: "dark" });
    unsubscribe();
  });

  test("focus doesn't discard a choice the browser refused to store", () => {
    const env = fakeEnvironment({ refuseCookies: true });
    const store = createAppearanceStore(env.environment);
    const unsubscribe = store.subscribe(() => {});
    store.update({ theme: "payline" });
    env.focus();
    expect(store.getSnapshot().theme).toBe("payline");
    unsubscribe();
  });

  test("detaches every listener once the last subscriber leaves", () => {
    const env = fakeEnvironment();
    const store = createAppearanceStore(env.environment);
    const first = store.subscribe(() => {});
    const second = store.subscribe(() => {});
    expect(env.queryListeners.size).toBe(1);
    expect(env.focusListeners.size).toBe(1);
    first();
    expect(env.queryListeners.size).toBe(1);
    second();
    expect(env.queryListeners.size).toBe(0);
    expect(env.focusListeners.size).toBe(0);
  });

  test("survives Strict Mode's subscribe, unsubscribe, subscribe", () => {
    const env = fakeEnvironment({ cookie: "fabro-appearance=v2.signal.system" });
    const store = createAppearanceStore(env.environment);
    store.subscribe(() => {})();
    const unsubscribe = store.subscribe(() => {});
    expect(env.queryListeners.size).toBe(1);
    env.setOsDark(true);
    expect(store.getSnapshot().mode).toBe("dark");
    unsubscribe();
    expect(env.queryListeners.size).toBe(0);
  });

  test("catches an OS change that landed before anyone subscribed", () => {
    const env = fakeEnvironment({ dark: false, cookie: "fabro-appearance=v2.signal.system" });
    const store = createAppearanceStore(env.environment);
    env.state.dark = true;
    const unsubscribe = store.subscribe(() => {});
    expect(store.getSnapshot().mode).toBe("dark");
    unsubscribe();
  });
});

describe("boot script", () => {
  const source = readFileSync(join(WEB_ROOT, "public", "appearance-boot.js"), "utf8");

  function elementStub() {
    const attributes = new Map<string, string>();
    const classes = new Set<string>();
    const style = { colorScheme: "" };
    return {
      attributes,
      classes,
      style,
      element: {
        setAttribute: (name: string, value: string) => attributes.set(name, value),
        classList: {
          toggle: (name: string, force: boolean) => (force ? classes.add(name) : classes.delete(name)),
        },
        style,
      },
    };
  }

  function runBoot({ cookie, dark, throwOnCookie = false }: { cookie: string; dark: boolean; throwOnCookie?: boolean }) {
    const stub = elementStub();
    const documentStub = {
      get cookie() {
        if (throwOnCookie) throw new Error("SecurityError");
        return cookie;
      },
      documentElement: stub.element,
    };
    new Function("document", "window", source)(documentStub, { matchMedia: () => ({ matches: dark }) });
    return stub;
  }

  function expectParity(cookie: string, dark: boolean) {
    const booted = runBoot({ cookie, dark });
    const expected = elementStub();
    applyAppearanceToElement(expected.element, resolveAppearance(parseAppearanceCookie(cookie), dark));
    expect([...booted.attributes]).toEqual([...expected.attributes]);
    expect([...booted.classes]).toEqual([...expected.classes]);
    expect(booted.style.colorScheme).toBe(expected.style.colorScheme);
  }

  test("agrees with the store for every palette, mode and OS setting", () => {
    for (const theme of THEME_IDS) {
      for (const displayMode of DISPLAY_MODE_IDS) {
        for (const dark of [false, true]) {
          expectParity(`other=1; ${APPEARANCE_COOKIE_NAME}=v2.${theme}.${displayMode}`, dark);
        }
      }
    }
  });

  test("agrees with the store about every old preset", () => {
    for (const preset of ["system", "editorial-light", "editorial-dark", "full-editorial", "terminal", "sketch", "consultant"]) {
      for (const dark of [false, true]) expectParity(`fabro-appearance=v1.${preset}`, dark);
    }
  });

  test("agrees with the store about every malformed cookie", () => {
    const malformed = [
      "",
      "fabro-appearance=",
      "fabro-appearance=signal",
      "fabro-appearance=v2.signal",
      "fabro-appearance=v2.unknown.dark",
      "fabro-appearance=v2.signal.sepia",
      "fabro-appearance=v2.constructor.dark",
      "fabro-appearance=v2.signal.dark.extra",
      "fabro-appearance=v3.signal.dark",
      "fabro-appearance=v1.unknown",
      "fabro-appearance=v1.__proto__",
      "fabro-appearance=v2.payline.dark; fabro-appearance=v2.graphite.light",
      "not-fabro-appearance=v2.payline.dark",
      'cf-internal-appearance={"version":1,"theme":"lavender","mode":"dark"}',
    ];
    for (const cookie of malformed) {
      for (const dark of [false, true]) {
        expectParity(cookie, dark);
        expect(runBoot({ cookie, dark }).attributes.get("data-theme")).toBe("brutalist");
      }
    }
  });

  test("a throwing cookie jar still produces a themed document", () => {
    const booted = runBoot({ cookie: "", dark: false, throwOnCookie: true });
    expect(booted.attributes.get("data-theme")).toBe("brutalist");
    expect(booted.attributes.get("data-mode")).toBe("light");
  });

  test("never writes a cookie or reads another app's storage", () => {
    expect(source).not.toMatch(/document\.cookie\s*=[^=]/);
    expect(source).not.toContain("localStorage");
    expect(source).not.toContain("cf-internal");
    expect(source).not.toContain("factory:appearance");
  });

  test("lists exactly the registry's palettes", () => {
    const listed = source.match(/var THEMES = \[([^\]]+)\]/)![1]!.match(/"([a-z]+)"/g)!.map((id) => id.replaceAll('"', ""));
    expect(listed).toEqual(THEMES.map((theme) => theme.id));
  });

  test("loads as a blocking external script, before the stylesheet paints", () => {
    const script = template.indexOf('<script src="/appearance-boot.js"></script>');
    expect(script).toBeGreaterThan(0);
    expect(script).toBeLessThan(template.indexOf("{{styles}}"));
    // An inline script would need a CSP change that split-web deployments enforce.
    expect(template).not.toMatch(/<script>/);
  });
});

/** Fabro's real token mappings, read from the stylesheets that declare them. */
function adapterDeclarations(): Map<string, string> {
  const declarations = new Map<string, string>();
  const sources = [
    appCss.slice(appCss.indexOf("@theme static {"), appCss.indexOf("@custom-variant dark")),
    appearanceCss.slice(appearanceCss.indexOf(":root {"), appearanceCss.indexOf(":root.dark {")),
  ];
  for (const source of sources) {
    for (const match of source.matchAll(/(--[a-z0-9-]+):\s*([^;]+);/g)) {
      declarations.set(match[1]!, match[2]!.trim());
    }
  }
  return declarations;
}

const adapter = adapterDeclarations();

/** What a Fabro or Factory variable computes to on one palette, as an opaque-or-rgba color. */
function resolveColor(value: string, tokens: FactoryTokens): string {
  const direct = value.match(/^var\(--([a-z0-9-]+)\)$/);
  if (direct) {
    const name = direct[1]!;
    const fromAdapter = adapter.get(`--${name}`);
    if (fromAdapter) return resolveColor(fromAdapter, tokens);
    const token = tokens[name];
    if (!token) throw new Error(`Unknown token --${name}`);
    return token;
  }
  const mix = value.match(/^color-mix\(in srgb, (var\(--[a-z0-9-]+\)) (\d+)%, (var\(--[a-z0-9-]+\)|transparent)\)$/);
  if (mix) {
    const first = resolveColor(mix[1]!, tokens);
    const share = Number(mix[2]) / 100;
    if (mix[3] === "transparent") {
      const hex = first.slice(1);
      const [r, g, b] = [0, 2, 4].map((start) => parseInt(hex.slice(start, start + 2), 16));
      return `rgba(${r}, ${g}, ${b}, ${share})`;
    }
    return mixColors(first, resolveColor(mix[3]!, tokens), 1 - share);
  }
  return value;
}

describe("adapter", () => {
  const cases = THEME_IDS.flatMap((theme) =>
    COLOR_MODES.map((mode) => [`${theme} ${mode}`, theme, mode] as const),
  );

  function fabro(name: string, tokens: FactoryTokens): string {
    return resolveColor(`var(--${name})`, tokens);
  }

  test("every contract name the app and its owners depend on is defined", () => {
    const contract = [
      "color-page", "color-panel", "color-panel-alt", "color-fg", "color-fg-2", "color-fg-3", "color-fg-muted",
      "color-overlay", "color-overlay-strong", "color-line", "color-line-strong", "color-focus", "color-on-primary",
      "color-primary", "color-primary-hover", "color-backdrop", "color-teal-300", "color-teal-500", "color-teal-700",
      "color-mint", "color-amber", "color-coral", "color-ice-50",
      "diagram-paper", "diagram-surface", "diagram-ink", "diagram-muted", "diagram-soft", "diagram-rule",
      "diagram-accent", "diagram-accent-tint", "diagram-edge", "diagram-grid", "diagram-success", "diagram-danger",
      "diagram-warning", "diagram-node-radius", "diagram-stroke", "diagram-edge-stroke",
      "nav-bg", "nav-text", "nav-muted", "nav-brand", "nav-border", "nav-divider", "nav-hover", "nav-hover-text",
      "nav-selected", "nav-selected-text", "nav-selected-border", "nav-ring",
      "color-rail", "color-rail-fg", "font-serif", "font-sans", "font-mono", "font-display",
      "radius-sm", "radius-md", "radius-lg", "radius-btn", "radius-chip", "radius-badge",
    ];
    for (const name of contract) expect(adapter.has(`--${name}`)).toBe(true);
  });

  test("the exact roles read Factory's tokens directly", () => {
    const exact: Array<[string, string]> = [
      ["color-page", "bg"], ["color-panel", "surface"], ["color-panel-alt", "surface-2"],
      ["color-fg", "fg"], ["color-fg-2", "fg-2"], ["color-fg-muted", "muted"],
      ["color-line", "line"], ["color-line-strong", "line-strong"], ["color-focus", "ring"],
      ["color-primary", "primary"], ["color-on-primary", "primary-fg"], ["color-teal-500", "accent-ink"],
      ["color-mint", "ok"], ["color-amber", "warn"], ["color-coral", "bad"],
      ["diagram-paper", "bg"], ["diagram-surface", "surface"], ["diagram-ink", "fg"], ["diagram-accent", "accent-fill"],
      ["nav-bg", "rail"], ["nav-text", "rail-fg"], ["nav-selected", "rail-selected"], ["nav-selected-text", "rail-selected-fg"],
      ["nav-selected-border", "rail-indicator"], ["nav-ring", "rail-ring"], ["nav-hover", "rail-hover"],
    ];
    for (const [name, token] of exact) expect(adapter.get(`--${name}`)).toBe(`var(--${token})`);
  });

  test("no adapter hard-codes a color, apart from the fixed logo tile", () => {
    const literals = [...adapter].filter(([, value]) => /#[0-9a-f]{3,8}\b|rgba?\(/i.test(value));
    expect(literals.map(([name]) => name)).toEqual(["--color-ice-50"]);
  });

  describe.each(cases)("%s", (_label, theme, mode) => {
    const tokens = palettes[theme][mode];
    const surfaces = [fabro("color-page", tokens), fabro("color-panel", tokens), fabro("color-panel-alt", tokens)];

    test.each([
      "color-fg", "color-fg-2", "color-fg-3", "color-fg-muted",
      "color-teal-300", "color-teal-500",
      "color-mint", "color-amber", "color-coral",
    ])("%s reads on every UI surface", (name) => {
      for (const surface of surfaces) {
        expect(contrastRatio(fabro(name, tokens), surface)).toBeGreaterThanOrEqual(4.5);
      }
    });

    test("focus and control boundaries stand out on every UI surface", () => {
      for (const surface of surfaces) {
        expect(contrastRatio(fabro("color-focus", tokens), surface)).toBeGreaterThanOrEqual(3);
      }
      expect(contrastRatio(tokens.input!, fabro("color-panel", tokens))).toBeGreaterThanOrEqual(3);
    });

    test("filled controls keep their text readable, including markup still on the accent", () => {
      const onPrimary = fabro("color-on-primary", tokens);
      for (const fill of ["color-primary", "color-primary-hover", "color-teal-500", "color-teal-300", "color-coral"]) {
        expect(contrastRatio(onPrimary, fabro(fill, tokens))).toBeGreaterThanOrEqual(4.5);
      }
      expect(contrastRatio(tokens["accent-pop-fg"]!, tokens["accent-pop"]!)).toBeGreaterThanOrEqual(4.5);
    });

    test("status text reads on its own surface", () => {
      for (const status of ["ok", "warn", "info", "violet", "bad"]) {
        expect(contrastRatio(tokens[status]!, tokens[`${status}-bg`]!)).toBeGreaterThanOrEqual(4.5);
      }
      expect(contrastRatio(tokens["bad-solid-fg"]!, tokens["bad-solid"]!)).toBeGreaterThanOrEqual(4.5);
      expect(contrastRatio(tokens["accent-soft-ink"]!, tokens["accent-soft"]!)).toBeGreaterThanOrEqual(4.5);
    });

    test("diagram text reads on paper and node surfaces", () => {
      for (const surface of [fabro("diagram-paper", tokens), fabro("diagram-surface", tokens)]) {
        for (const text of ["diagram-ink", "diagram-muted", "diagram-soft"]) {
          expect(contrastRatio(fabro(text, tokens), surface)).toBeGreaterThanOrEqual(4.5);
        }
      }
    });

    test("connectors, node strokes and state strokes hold 3:1 where they're drawn", () => {
      for (const surface of [fabro("diagram-paper", tokens), fabro("diagram-surface", tokens)]) {
        for (const mark of ["diagram-edge", "diagram-ink", "diagram-accent", "diagram-danger", "diagram-success", "diagram-warning"]) {
          expect(contrastRatio(fabro(mark, tokens), surface)).toBeGreaterThanOrEqual(3);
        }
      }
    });

    test("the rail reads on its own ground", () => {
      for (const name of ["nav-text", "nav-muted", "nav-brand"]) {
        expect(contrastRatio(fabro(name, tokens), fabro("nav-bg", tokens))).toBeGreaterThanOrEqual(4.5);
      }
      expect(contrastRatio(fabro("nav-hover-text", tokens), fabro("nav-hover", tokens))).toBeGreaterThanOrEqual(4.5);
      expect(contrastRatio(fabro("nav-selected-text", tokens), fabro("nav-selected", tokens))).toBeGreaterThanOrEqual(4.5);
      expect(contrastRatio(fabro("nav-ring", tokens), fabro("nav-bg", tokens))).toBeGreaterThanOrEqual(3);
    });

    test("chart series stand out on the panel", () => {
      for (const series of [1, 2, 3, 4, 5]) {
        expect(contrastRatio(fabro(`chart-series-${series}`, tokens), fabro("color-panel", tokens))).toBeGreaterThanOrEqual(3);
      }
      expect(contrastRatio(fabro("chart-axis-label", tokens), fabro("color-panel", tokens))).toBeGreaterThanOrEqual(4.5);
    });

    test("diff numbers and markers read on the code surfaces", () => {
      for (const surface of ["diffs-bg-context-override", "diffs-bg-buffer-override", "diffs-bg-hover-override"]) {
        for (const text of ["diffs-fg-number-override", "diffs-addition-color-override", "diffs-deletion-color-override"]) {
          expect(contrastRatio(fabro(text, tokens), fabro(surface, tokens))).toBeGreaterThanOrEqual(4.5);
        }
      }
    });

    test("terminal text reads on the palette's terminal", () => {
      const colors = paletteColors(theme, mode);
      const terminal = terminalColorsFor(colors, mode as ColorMode);
      expect(terminal.background).toBe(tokens.surface!);
      expect(contrastRatio(terminal.foreground, terminal.background)).toBeGreaterThanOrEqual(4.5);
      for (const name of [
        "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
      ] as const) {
        expect(contrastRatio(terminal[name], terminal.background)).toBeGreaterThanOrEqual(4.5);
      }
    });
  });
});

describe("stylesheet contract", () => {
  test("imports the snapshot, and keeps the builder's stylesheet", () => {
    expect(appCss).toContain('@import "./styles/factory-themes.css"');
    expect(appCss).toContain('@import "./styles/factory-fonts.css"');
    expect(appCss).toContain('@import "./styles/factory-assistant-ui.generated.css"');
    expect(appCss).toContain('@import "./components/workflow-builder/builder.css"');
  });

  test("the snapshot keys on what the store and boot script write", () => {
    expect(snapshot).toContain(":root.dark {");
    for (const theme of THEME_IDS.slice(1)) {
      expect(snapshot).toContain(`:root[data-theme="${theme}"] {`);
      expect(snapshot).toContain(`:root[data-theme="${theme}"].dark {`);
    }
  });

  test("declares each color scheme, for scrollbars and form controls", () => {
    expect(appearanceCss).toMatch(/:root \{\s*color-scheme: light;/);
    expect(appearanceCss).toMatch(/:root\.dark \{\s*color-scheme: dark;/);
  });

  test("never lets pulses run under reduced motion", () => {
    expect(appearanceCss).toMatch(/prefers-reduced-motion: reduce\)[\s\S]*\.animate-pulse[\s\S]*animation: none/);
  });

  test("the old preset system is gone", () => {
    for (const css of [appCss, appearanceCss]) {
      expect(css).not.toContain("data-treatment");
      expect(css).not.toContain("data-preset");
      expect(css).not.toContain("radial-gradient");
    }
  });
});
