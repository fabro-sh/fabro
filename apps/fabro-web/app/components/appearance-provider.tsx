import {
  createContext,
  useContext,
  useMemo,
  useSyncExternalStore,
  type ReactNode,
} from "react";

import {
  DEFAULT_APPEARANCE,
  SYSTEM_DARK_QUERY,
  applyAppearanceToElement,
  createAppearanceStore,
  paletteColors,
  resolveAppearance,
  terminalColorsFor,
  type AppearanceEnvironment,
  type AppearanceSaveStatus,
  type AppearanceSnapshot,
  type AppearanceStore,
  type DisplayMode,
  type ResolvedAppearance,
  type TerminalColors,
  type ThemeId,
} from "../lib/appearance";
import type { FactoryPaletteColors } from "../lib/factory-palettes.generated";

export interface AppearanceContextValue {
  /** The palette the user chose. */
  theme: ThemeId;
  /** The display mode the user chose, including `system`. */
  displayMode: DisplayMode;
  /** That choice resolved: `appearance.mode` is the light or dark on screen now. */
  appearance: ResolvedAppearance;
  /** Literal colors for code that can't read CSS: xterm, canvas, SVG attributes. */
  colors: FactoryPaletteColors;
  terminalColors: TerminalColors;
  /** `blocked` means the choice applies now but will not survive a reload. */
  saveStatus: AppearanceSaveStatus;
  /** Each applies at once and keeps the other field. `false` means it couldn't be saved. */
  setTheme: (theme: ThemeId) => boolean;
  setDisplayMode: (displayMode: DisplayMode) => boolean;
  /** Back to Brutalist in light mode. */
  resetAppearance: () => boolean;
}

function contextValue(
  snapshot: AppearanceSnapshot,
  store: Pick<AppearanceStore, "update">,
): AppearanceContextValue {
  const colors = paletteColors(snapshot.theme, snapshot.mode);
  return {
    theme: snapshot.theme,
    displayMode: snapshot.displayMode,
    appearance: {
      theme: snapshot.theme,
      displayMode: snapshot.displayMode,
      mode: snapshot.mode,
    },
    colors,
    terminalColors: terminalColorsFor(colors, snapshot.mode),
    saveStatus: snapshot.saveStatus,
    setTheme: (theme) => store.update({ theme }),
    setDisplayMode: (displayMode) => store.update({ displayMode }),
    resetAppearance: () => store.update(DEFAULT_APPEARANCE),
  };
}

// Components mounted outside a provider (isolated tests, stories) see the default look
// and can't change it.
const OUTSIDE_PROVIDER_VALUE = contextValue(
  { ...resolveAppearance(DEFAULT_APPEARANCE, false), saveStatus: "idle" },
  { update: () => false },
);

const AppearanceContext = createContext<AppearanceContextValue>(OUTSIDE_PROVIDER_VALUE);

function browserAppearanceEnvironment(): AppearanceEnvironment {
  return {
    readCookie: () => document.cookie,
    writeCookie: (cookie) => {
      document.cookie = cookie;
    },
    isSecureContext: () => window.location.protocol === "https:",
    systemDarkQuery: () =>
      typeof window.matchMedia === "function"
        ? window.matchMedia(SYSTEM_DARK_QUERY)
        : null,
    applyToDocument: (appearance) => {
      applyAppearanceToElement(document.documentElement, appearance);
    },
    subscribeToFocus: (listener) => {
      window.addEventListener("focus", listener);
      document.addEventListener("visibilitychange", listener);
      return () => {
        window.removeEventListener("focus", listener);
        document.removeEventListener("visibilitychange", listener);
      };
    },
  };
}

function detachedAppearanceEnvironment(): AppearanceEnvironment {
  return {
    readCookie: () => "",
    writeCookie: () => {},
    isSecureContext: () => false,
    systemDarkQuery: () => null,
    applyToDocument: () => {},
    subscribeToFocus: () => () => {},
  };
}

let browserStore: AppearanceStore | null = null;

function defaultAppearanceStore(): AppearanceStore {
  if (typeof window === "undefined" || typeof document === "undefined") {
    return createAppearanceStore(detachedAppearanceEnvironment());
  }
  browserStore ??= createAppearanceStore(browserAppearanceEnvironment());
  return browserStore;
}

/**
 * Publishes the current appearance. The store owns the external systems: the cookie,
 * the OS color-scheme media query and the document attributes. Its listeners attach on
 * subscribe and detach on the last unsubscribe, which keeps this safe under Strict Mode
 * remounts. Nothing is keyed on the palette, so switching never remounts a feature.
 */
export function AppearanceProvider({
  children,
  store,
}: {
  children: ReactNode;
  /** Test seam. Browsers use the shared store. */
  store?: AppearanceStore;
}) {
  const activeStore = useMemo(() => store ?? defaultAppearanceStore(), [store]);
  const snapshot = useSyncExternalStore(
    activeStore.subscribe,
    activeStore.getSnapshot,
    activeStore.getSnapshot,
  );
  const value = useMemo(() => contextValue(snapshot, activeStore), [activeStore, snapshot]);

  return <AppearanceContext.Provider value={value}>{children}</AppearanceContext.Provider>;
}

/** The current appearance and the way to change it. */
export function useAppearance(): AppearanceContextValue {
  return useContext(AppearanceContext);
}
