import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";

/**
 * App-level state for the Ask-Fabro right sidebar. Lifted out of any single
 * route so the sidebar can be rendered at the App layout level and span the
 * full window height (above the top nav and main content).
 */
type AskFabroContextValue = {
  isOpen: boolean;
  open: () => void;
  close: () => void;
  toggle: () => void;
};

const AskFabroContext = createContext<AskFabroContextValue | null>(null);

export function AskFabroProvider({ children }: { children: ReactNode }) {
  const [isOpen, setIsOpen] = useState(false);
  const open = useCallback(() => setIsOpen(true), []);
  const close = useCallback(() => setIsOpen(false), []);
  const toggle = useCallback(() => setIsOpen((v) => !v), []);
  const value = useMemo(
    () => ({ isOpen, open, close, toggle }),
    [isOpen, open, close, toggle],
  );
  return <AskFabroContext.Provider value={value}>{children}</AskFabroContext.Provider>;
}

export function useAskFabro(): AskFabroContextValue {
  const value = useContext(AskFabroContext);
  if (!value) throw new Error("useAskFabro must be used inside <AskFabroProvider>");
  return value;
}
