import { createContext, useContext, useMemo, useState } from "react";

/**
 * Layout coordination for the docked "Ask Fabro" sidebar. The run detail page
 * owns the open/closed state and publishes the sidebar's current width here;
 * the app shell reads it and insets `<main>` by that amount so the page
 * content shifts left instead of being covered by the fixed sidebar.
 */
interface AskFabroLayout {
  /** Width in px the docked sidebar currently occupies; 0 when closed. */
  sidebarWidth: number;
  setSidebarWidth: (width: number) => void;
}

const NOOP_LAYOUT: AskFabroLayout = {
  sidebarWidth: 0,
  setSidebarWidth: () => {},
};

const AskFabroLayoutContext = createContext<AskFabroLayout>(NOOP_LAYOUT);

export function AskFabroLayoutProvider({
  children,
}: {
  children: React.ReactNode;
}) {
  const [sidebarWidth, setSidebarWidth] = useState(0);
  const value = useMemo(
    () => ({ sidebarWidth, setSidebarWidth }),
    [sidebarWidth],
  );
  return (
    <AskFabroLayoutContext.Provider value={value}>
      {children}
    </AskFabroLayoutContext.Provider>
  );
}

export function useAskFabroLayout(): AskFabroLayout {
  return useContext(AskFabroLayoutContext);
}
