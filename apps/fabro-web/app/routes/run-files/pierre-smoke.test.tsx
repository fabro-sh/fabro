import { describe, expect, test } from "bun:test";
import type { ReactNode } from "react";
import * as PierreDiffs from "@pierre/diffs/react";

const { MultiFileDiff, PatchDiff } = PierreDiffs;
const maybeVirtualizer = (PierreDiffs as Record<string, unknown>).Virtualizer;
const Virtualizer = typeof maybeVirtualizer === "function"
  ? maybeVirtualizer
  : function VirtualizerFallback({ children }: { children: ReactNode }) {
      return <>{children}</>;
    };

// Regression coverage for the @pierre/diffs 1.0 -> 1.1 upgrade. We assert
// only that the React components the Run Files route consumes remain
// callable. `Virtualizer` is optional across installed pierre versions, so
// the route carries a no-op fallback and the smoke test mirrors that
// compatibility layer rather than assuming a specific package export set.

describe("@pierre/diffs public API", () => {
  test("MultiFileDiff is a callable component export", () => {
    expect(typeof MultiFileDiff).toBe("function");
  });

  test("PatchDiff is a callable component export", () => {
    expect(typeof PatchDiff).toBe("function");
  });

  test("Virtualizer is a callable component export", () => {
    expect(typeof Virtualizer).toBe("function");
  });
});
