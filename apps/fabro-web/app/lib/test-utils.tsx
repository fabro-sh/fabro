import { createElement, type ReactNode } from "react";
import TestRenderer, { act } from "react-test-renderer";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const originalConsoleError = console.error;
console.error = ((...args: unknown[]) => {
  if (
    typeof args[0] === "string" &&
    args[0].startsWith("react-test-renderer is deprecated")
  ) {
    return;
  }
  originalConsoleError(...args);
}) as typeof console.error;

export function renderHook<T>(
  hook: () => T,
  options: { wrapper: React.ComponentType<{ children: ReactNode }> },
): { result: { current: T } } {
  const result = { current: undefined as unknown as T };
  function HookHost() {
    result.current = hook();
    return null;
  }
  act(() => {
    TestRenderer.create(
      createElement(options.wrapper, null, createElement(HookHost)),
    );
  });
  return { result };
}
