import { createContext, use } from "react";

const DemoModeContext = createContext(false);

export function DemoModeProvider({
  value,
  children,
}: {
  value: boolean;
  children: React.ReactNode;
}) {
  return (
    <DemoModeContext.Provider value={value}>
      {children}
    </DemoModeContext.Provider>
  );
}

export function useDemoMode(): boolean {
  return use(DemoModeContext);
}
