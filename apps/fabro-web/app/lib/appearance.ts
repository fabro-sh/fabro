// The appearance registry and its browser-independent store.
//
// A user picks a palette and, separately, a display mode. The seven palettes are
// Factory's, and every palette has a light and a dark version, so 14 are possible on
// screen. The colors live in styles/factory-themes.css, preserving Factory's palette
// declarations: this module only says which palette is on and which mode.
//
// Factory keeps this choice in localStorage. Fabro keeps it in its own host-local
// cookie instead, and reads nothing Factory or the Internal Hub saved.
//
// `public/appearance-boot.js` repeats the allow-lists and resolution rules in plain
// JavaScript so the document is themed before first paint. `appearance.test.ts` runs
// that script against this module to keep them in sync.

import { mixColors } from "./color-contrast";
import { FACTORY_PALETTES, type FactoryPaletteColors } from "./factory-palettes.generated";

export const APPEARANCE_COOKIE_NAME = "fabro-appearance";
export const APPEARANCE_COOKIE_VERSION = "v2";
export const APPEARANCE_COOKIE_MAX_AGE_SECONDS = 60 * 60 * 24 * 365;
export const SYSTEM_DARK_QUERY = "(prefers-color-scheme: dark)";

/**
 * No palette draws hand-sketched strokes any more. The workflow builder still imports
 * this name, and without the filter definition its reference has no effect.
 */
export const SKETCH_FILTER_ID = "fabro-sketchy";

export type ThemeId = keyof typeof FACTORY_PALETTES;
export type DisplayMode = "light" | "dark" | "system";
export type ColorMode = "light" | "dark";

export interface ThemeInfo {
  id: ThemeId;
  name: string;
  description: string;
  /** Page, surface, accent, ink: the four colors the chooser shows. */
  swatches: readonly [string, string, string, string];
}

export const DEFAULT_THEME: ThemeId = "brutalist";
export const DEFAULT_DISPLAY_MODE: DisplayMode = "light";

/** Factory's ids, names and swatches, with Fabro's default selected separately. */
export const THEMES: readonly ThemeInfo[] = [
  { id: "signal", name: "Signal navy", description: "Deep navy with orange signals.", swatches: ["#f1f5f9", "#ffffff", "#fe4a23", "#0d1122"] },
  { id: "readability", name: "Spacious warm", description: "Warm neutrals with a restrained blue accent.", swatches: ["#f7f6f2", "#ffffff", "#2756c7", "#242623"] },
  { id: "command", name: "Blue command", description: "Cool surfaces, navy navigation and cobalt accents.", swatches: ["#f2f5fb", "#ffffff", "#2358e8", "#0b2050"] },
  { id: "payline", name: "Rounded Payline", description: "Soft gray surfaces with a fresh lime accent.", swatches: ["#f3f3f5", "#ffffff", "#d0ff00", "#202124"] },
  { id: "brutalist", name: "Brutalist", description: "Paper and ink with acid-lime highlights.", swatches: ["#f0eee5", "#fffef9", "#d9f536", "#171817"] },
  { id: "graphite", name: "Soft graphite", description: "Sculpted gray surfaces with mint accents.", swatches: ["#dcdedf", "#e7eaea", "#cfe6d9", "#244c3b"] },
  { id: "lavender", name: "Lavender workspace", description: "Pale lavender surfaces with violet accents.", swatches: ["#f1effa", "#ffffff", "#6048d8", "#222330"] },
];

export const DISPLAY_MODES: readonly { id: DisplayMode; name: string }[] = [
  { id: "light", name: "Light" },
  { id: "dark", name: "Dark" },
  { id: "system", name: "Match system" },
];

export function isThemeId(value: unknown): value is ThemeId {
  return THEMES.some((theme) => theme.id === value);
}

export function isDisplayMode(value: unknown): value is DisplayMode {
  return DISPLAY_MODES.some((mode) => mode.id === value);
}

export function themeInfo(id: ThemeId): ThemeInfo {
  return THEMES.find((theme) => theme.id === id) ?? THEMES[0]!;
}

// ── Colors for code that can't read CSS ──

/** An xterm.js `ITheme`, kept structural so this module stays free of xterm. */
export interface TerminalColors {
  background: string;
  foreground: string;
  cursor: string;
  cursorAccent: string;
  selectionBackground: string;
  selectionForeground: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
}

export function paletteColors(theme: ThemeId, mode: ColorMode): FactoryPaletteColors {
  return FACTORY_PALETTES[theme][mode];
}

/**
 * Factory has no terminal palette, so this derives one from status roles every palette
 * defines, on the palette's own surface. Bright variants step toward the text color.
 */
export function terminalColorsFor(colors: FactoryPaletteColors, mode: ColorMode): TerminalColors {
  const bright = (color: string) => mixColors(color, colors.fg, 0.25);
  const cyan = mixColors(colors.info, colors.ok, 0.5);
  return {
    background: colors.surface,
    foreground: colors.fg,
    cursor: colors.accentFill,
    cursorAccent: colors.surface,
    selectionBackground: colors.selectionBg,
    selectionForeground: colors.selectionFg,
    black: mode === "light" ? colors.fg : colors.bg,
    red: colors.bad,
    green: colors.ok,
    yellow: colors.warn,
    blue: colors.info,
    magenta: colors.violet,
    cyan,
    white: mode === "light" ? colors.muted : colors.fg2,
    brightBlack: colors.muted,
    brightRed: bright(colors.bad),
    brightGreen: bright(colors.ok),
    brightYellow: bright(colors.warn),
    brightBlue: bright(colors.info),
    brightMagenta: bright(colors.violet),
    brightCyan: bright(cyan),
    brightWhite: colors.fg,
  };
}

// ── Resolution ──

/** What the user chose. */
export interface SavedAppearance {
  theme: ThemeId;
  displayMode: DisplayMode;
}

/** That choice, resolved against the device: `mode` is what's on screen. */
export interface ResolvedAppearance extends SavedAppearance {
  mode: ColorMode;
}

export const DEFAULT_APPEARANCE: SavedAppearance = {
  theme: DEFAULT_THEME,
  displayMode: DEFAULT_DISPLAY_MODE,
};

export function resolveAppearance(
  saved: SavedAppearance,
  systemPrefersDark: boolean,
): ResolvedAppearance {
  const dark =
    saved.displayMode === "dark" || (saved.displayMode === "system" && systemPrefersDark);
  return { theme: saved.theme, displayMode: saved.displayMode, mode: dark ? "dark" : "light" };
}

// ── Cookie ──

const COOKIE_VALUE_RE = /^v2\.([a-z]+)\.([a-z]+)$/;
const LEGACY_COOKIE_VALUE_RE = /^v1\.([a-z-]+)$/;

/**
 * The experimental presets an earlier build saved as `v1.<preset>`. None of them is a
 * palette any more, so each keeps only its light or dark intent on the default palette.
 */
const LEGACY_PRESET_MODES = new Map<string, DisplayMode>([
  ["system", "system"],
  ["editorial-light", "light"],
  ["editorial-dark", "dark"],
  ["full-editorial", "light"],
  ["terminal", "dark"],
  ["sketch", "light"],
  ["consultant", "light"],
]);

function appearanceCookieValues(cookieHeader: string | null | undefined): string[] {
  if (!cookieHeader) return [];
  const values: string[] = [];
  for (const part of cookieHeader.split(";")) {
    const separator = part.indexOf("=");
    if (separator < 0) continue;
    if (part.slice(0, separator).trim() === APPEARANCE_COOKIE_NAME) {
      values.push(part.slice(separator + 1).trim());
    }
  }
  return values;
}

/**
 * Reads the choice out of a `document.cookie` string. Only a known version with both
 * fields on their allow-lists counts. Anything missing, malformed or from a future
 * version, or two cookies of this name (a path-shadowed copy is ambiguous), means the
 * default look.
 */
export function parseAppearanceCookie(
  cookieHeader: string | null | undefined,
): SavedAppearance {
  const values = appearanceCookieValues(cookieHeader);
  if (values.length !== 1) return DEFAULT_APPEARANCE;
  const value = values[0]!;
  const current = value.match(COOKIE_VALUE_RE);
  if (current) {
    return isThemeId(current[1]) && isDisplayMode(current[2])
      ? { theme: current[1], displayMode: current[2] }
      : DEFAULT_APPEARANCE;
  }
  const legacyMode = LEGACY_PRESET_MODES.get(value.match(LEGACY_COOKIE_VALUE_RE)?.[1] ?? "");
  return legacyMode ? { theme: DEFAULT_THEME, displayMode: legacyMode } : DEFAULT_APPEARANCE;
}

function appearanceCookieValue(saved: SavedAppearance): string {
  return `${APPEARANCE_COOKIE_VERSION}.${saved.theme}.${saved.displayMode}`;
}

/** The exact string assigned to `document.cookie`. It only ever holds these two choices. */
export function serializeAppearanceCookie(
  saved: SavedAppearance,
  { secure }: { secure: boolean },
): string {
  const attributes = [
    `${APPEARANCE_COOKIE_NAME}=${appearanceCookieValue(saved)}`,
    "Path=/",
    `Max-Age=${APPEARANCE_COOKIE_MAX_AGE_SECONDS}`,
    "SameSite=Lax",
  ];
  if (secure) attributes.push("Secure");
  return attributes.join("; ");
}

// ── Store ──

/** Whether the current choice will survive a reload. */
export type AppearanceSaveStatus = "idle" | "saved" | "blocked";

export interface AppearanceSnapshot extends ResolvedAppearance {
  saveStatus: AppearanceSaveStatus;
}

/** The browser surface the store touches, injected so tests need no DOM. */
export interface AppearanceEnvironment {
  readCookie(): string;
  writeCookie(cookie: string): void;
  isSecureContext(): boolean;
  /** `null` when media queries are unavailable: Match system then resolves to light. */
  systemDarkQuery(): {
    matches: boolean;
    addEventListener(type: "change", listener: () => void): void;
    removeEventListener(type: "change", listener: () => void): void;
  } | null;
  applyToDocument(appearance: ResolvedAppearance): void;
  /** Subscribes to the moments another tab's cookie write may have landed. */
  subscribeToFocus(listener: () => void): () => void;
}

export interface AppearanceStore {
  getSnapshot(): AppearanceSnapshot;
  subscribe(listener: () => void): () => void;
  /**
   * Applies at once, changing only the fields given. Returns `false` when the browser
   * refused to keep the cookie, or for a value outside the allow-lists.
   */
  update(change: Partial<SavedAppearance>): boolean;
}

function safelyReadCookie(env: AppearanceEnvironment): string | null {
  try {
    return env.readCookie();
  } catch {
    return null;
  }
}

export function createAppearanceStore(env: AppearanceEnvironment): AppearanceStore {
  const listeners = new Set<() => void>();
  let releaseSources: (() => void) | undefined;
  let snapshot: AppearanceSnapshot = {
    ...resolveAppearance(
      parseAppearanceCookie(safelyReadCookie(env)),
      env.systemDarkQuery()?.matches ?? false,
    ),
    saveStatus: "idle",
  };
  env.applyToDocument(snapshot);

  function publish(next: AppearanceSnapshot) {
    const changedAppearance =
      next.theme !== snapshot.theme ||
      next.displayMode !== snapshot.displayMode ||
      next.mode !== snapshot.mode;
    if (!changedAppearance && next.saveStatus === snapshot.saveStatus) return;
    snapshot = next;
    if (changedAppearance) env.applyToDocument(snapshot);
    for (const listener of listeners) listener();
  }

  function systemPrefersDark() {
    return env.systemDarkQuery()?.matches ?? false;
  }

  function handleSystemChange() {
    if (snapshot.displayMode !== "system") return;
    publish({
      ...resolveAppearance(snapshot, systemPrefersDark()),
      saveStatus: snapshot.saveStatus,
    });
  }

  function handleFocus() {
    const cookie = safelyReadCookie(env);
    // A blocked cookie says nothing about the user's choice. Keep what they picked.
    if (cookie === null || snapshot.saveStatus === "blocked") return;
    const saved = parseAppearanceCookie(cookie);
    if (saved.theme === snapshot.theme && saved.displayMode === snapshot.displayMode) return;
    publish({ ...resolveAppearance(saved, systemPrefersDark()), saveStatus: "idle" });
  }

  return {
    getSnapshot: () => snapshot,

    subscribe(listener) {
      listeners.add(listener);
      if (!releaseSources) {
        const query = env.systemDarkQuery();
        query?.addEventListener("change", handleSystemChange);
        const unsubscribeFocus = env.subscribeToFocus(handleFocus);
        releaseSources = () => {
          query?.removeEventListener("change", handleSystemChange);
          unsubscribeFocus();
        };
        // The OS or another tab may have moved between creating the store and
        // subscribing to it, so catch up once instead of waiting for the next event.
        handleSystemChange();
        handleFocus();
      }
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0) {
          releaseSources?.();
          releaseSources = undefined;
        }
      };
    },

    update(change) {
      const next: SavedAppearance = {
        theme: change.theme ?? snapshot.theme,
        displayMode: change.displayMode ?? snapshot.displayMode,
      };
      if (!isThemeId(next.theme) || !isDisplayMode(next.displayMode)) return false;
      let saved = false;
      try {
        env.writeCookie(serializeAppearanceCookie(next, { secure: env.isSecureContext() }));
        // Browsers drop a refused cookie silently, so only a read-back proves it. A
        // second value means a path-shadowed copy would still win on the next load.
        const values = appearanceCookieValues(env.readCookie());
        saved = values.length === 1 && values[0] === appearanceCookieValue(next);
      } catch {
        saved = false;
      }
      publish({
        ...resolveAppearance(next, systemPrefersDark()),
        saveStatus: saved ? "saved" : "blocked",
      });
      return saved;
    },
  };
}

// ── Document ──

/** Writes what the stylesheet keys on. The boot script writes the same four things. */
export function applyAppearanceToElement(
  root: {
    setAttribute(name: string, value: string): void;
    classList: { toggle(name: string, force: boolean): unknown };
    style: { colorScheme: string };
  },
  appearance: ResolvedAppearance,
): void {
  root.setAttribute("data-theme", appearance.theme);
  root.setAttribute("data-mode", appearance.displayMode);
  root.classList.toggle("dark", appearance.mode === "dark");
  root.style.colorScheme = appearance.mode;
}
