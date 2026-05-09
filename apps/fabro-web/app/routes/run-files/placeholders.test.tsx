import { describe, expect, test } from "bun:test";
import TestRenderer from "react-test-renderer";

import {
  bannerCopyForReason,
  BinaryPlaceholder,
  DegradedBanner,
  pickPlaceholder,
  SensitivePlaceholder,
  SymlinkOrSubmodulePlaceholder,
  TruncatedPlaceholder,
} from "./placeholders";
import type { FileDiff as ApiFileDiff } from "@qltysh/fabro-api-client";

/**
 * Render the element and return a flat string of every text leaf the tree
 * produced, for substring assertions. Wraps in `TestRenderer.act` to keep
 * React 19 from unmounting the tree before we can walk it.
 */
function renderedText(element: React.ReactElement): string {
  let tree: TestRenderer.ReactTestRenderer | undefined;
  TestRenderer.act(() => {
    tree = TestRenderer.create(element);
  });
  const parts: string[] = [];
  tree!.root.findAll((node) => {
    const children = Array.isArray(node.children) ? node.children : [];
    for (const child of children) {
      if (typeof child === "string") parts.push(child);
    }
    return false;
  });
  return parts.join(" ");
}

function baseFile(overrides: Partial<ApiFileDiff> = {}): ApiFileDiff {
  return {
    old_file: { name: "src/foo.rs", contents: "" },
    new_file: { name: "src/foo.rs", contents: "" },
    change_kind: "modified",
    ...overrides,
  } as ApiFileDiff;
}

describe("pickPlaceholder priority", () => {
  test("sensitive > binary > symlink > truncated", () => {
    const all = baseFile({
      sensitive: true,
      binary: true,
      change_kind: "symlink",
      truncated: true,
    });
    const text = renderedText(pickPlaceholder(all)!);
    expect(text).toContain("sensitive");
    expect(text).not.toContain("binary");
  });

  test("binary flags take priority over symlink/submodule and truncated", () => {
    const bin = baseFile({
      binary: true,
      change_kind: "symlink",
      truncated: true,
    });
    const text = renderedText(pickPlaceholder(bin)!);
    expect(text).toContain("binary");
    expect(text).not.toContain("symlink");
  });

  test("symlink/submodule take priority over truncated", () => {
    const link = baseFile({ change_kind: "symlink", truncated: true });
    const text = renderedText(pickPlaceholder(link)!);
    expect(text).toContain("symlink");
    expect(text).not.toContain("too large");
  });

  test("truncated fires last and respects reason", () => {
    const tooLarge = baseFile({
      truncated: true,
      truncation_reason: "file_too_large",
    });
    expect(renderedText(pickPlaceholder(tooLarge)!)).toContain("too large");

    const budget = baseFile({
      truncated: true,
      truncation_reason: "budget_exhausted",
    });
    expect(renderedText(pickPlaceholder(budget)!)).toContain("too many files");
  });

  test("plain modified file gets no placeholder", () => {
    expect(pickPlaceholder(baseFile())).toBeNull();
  });
});

describe("bannerCopyForReason", () => {
  test("sandbox_gone is suppressed", () => {
    expect(bannerCopyForReason("sandbox_gone")).toBeNull();
  });

  test("provider_unsupported and sandbox_unreachable get distinct copy", () => {
    const b = bannerCopyForReason("provider_unsupported");
    const c = bannerCopyForReason("sandbox_unreachable");
    expect(b).not.toBe(c);
    expect(b).toContain("provider");
    expect(c).toContain("refresh");
  });

  test("unknown / undefined reason falls back to sandbox_unreachable copy", () => {
    expect(bannerCopyForReason(undefined)).toBe(
      bannerCopyForReason("sandbox_unreachable"),
    );
    expect(bannerCopyForReason("made-up")).toBe(
      bannerCopyForReason("sandbox_unreachable"),
    );
  });
});

describe("placeholder rendering", () => {
  test("SensitivePlaceholder includes file name", () => {
    expect(
      renderedText(<SensitivePlaceholder name=".env.production" />),
    ).toContain(".env.production");
  });

  test("BinaryPlaceholder includes file name", () => {
    const text = renderedText(<BinaryPlaceholder name="assets/logo.png" />);
    expect(text).toContain("assets/logo.png");
    expect(text).toContain("binary");
  });

  test("TruncatedPlaceholder maps reason to copy", () => {
    expect(
      renderedText(
        <TruncatedPlaceholder name="big.rs" reason="budget_exhausted" />,
      ),
    ).toContain("too many files");
    expect(renderedText(<TruncatedPlaceholder name="big.rs" />)).toContain(
      "too large",
    );
  });

  test("SymlinkOrSubmodulePlaceholder shows the kind", () => {
    expect(
      renderedText(
        <SymlinkOrSubmodulePlaceholder name="lnk" kind="submodule" />,
      ),
    ).toContain("submodule");
  });

  test("DegradedBanner renders nothing for sandbox_gone", () => {
    expect(renderedText(<DegradedBanner reason="sandbox_gone" />)).toBe("");
  });

  test("DegradedBanner picks copy based on reason", () => {
    expect(
      renderedText(<DegradedBanner reason="provider_unsupported" />),
    ).toContain("provider");
  });
});
