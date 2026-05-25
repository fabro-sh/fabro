import { createContext, use, useMemo, useState } from "react";

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
  /**
   * True while the user is dragging the sidebar's resize handle. Consumers
   * that animate off `sidebarWidth` drop their transition while this is set so
   * the layout tracks the cursor instead of trailing it by the ease duration.
   */
  isResizing: boolean;
  setIsResizing: (resizing: boolean) => void;
}

const NOOP_LAYOUT: AskFabroLayout = {
  sidebarWidth: 0,
  setSidebarWidth: () => {},
  isResizing: false,
  setIsResizing: () => {},
};

const AskFabroLayoutContext = createContext<AskFabroLayout>(NOOP_LAYOUT);

export function AskFabroLayoutProvider({
  children,
}: {
  children: React.ReactNode;
}) {
  const [sidebarWidth, setSidebarWidth] = useState(0);
  const [isResizing, setIsResizing] = useState(false);
  const value = useMemo(
    () => ({ sidebarWidth, setSidebarWidth, isResizing, setIsResizing }),
    [sidebarWidth, isResizing],
  );
  return (
    <AskFabroLayoutContext.Provider value={value}>
      {children}
    </AskFabroLayoutContext.Provider>
  );
}

export function useAskFabroLayout(): AskFabroLayout {
  return use(AskFabroLayoutContext);
}
