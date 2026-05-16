import { createElement, type ReactNode } from "react";
import TestRenderer, { act } from "react-test-renderer";

const IS_REACT_ACT_ENV = "IS_REACT_ACT_ENVIRONMENT" as const;

/**
 * Per-test setup for code that uses react-test-renderer:
 * - Sets IS_REACT_ACT_ENVIRONMENT (required by act()).
 * - Silences react-test-renderer's deprecation warning.
 *
 * Returns a teardown function; pair with beforeEach/afterEach so the global
 * state is scoped to the test rather than leaking process-wide.
 */
export function setupReactTestEnv(): () => void {
  type Globals = { [IS_REACT_ACT_ENV]?: boolean };
  const globals = globalThis as Globals;
  const hadEnv = IS_REACT_ACT_ENV in globals;
  const previousEnv = globals[IS_REACT_ACT_ENV];
  globals[IS_REACT_ACT_ENV] = true;

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

  return () => {
    console.error = originalConsoleError;
    if (hadEnv) {
      globals[IS_REACT_ACT_ENV] = previousEnv;
    } else {
      delete globals[IS_REACT_ACT_ENV];
    }
  };
}

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
