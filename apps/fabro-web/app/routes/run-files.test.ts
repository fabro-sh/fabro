import { describe, expect, test } from "bun:test";

import {
  deepLinkToastMessage,
  emptyTransitionToastMessage,
  extractRequestId,
} from "./run-files";

function buildRunFilesPayload({
  files = [],
  degraded = false,
}: {
  files?: string[];
  degraded?: boolean;
}) {
  return {
    data: files.map((name, index) => ({
      change_kind: "modified",
      old_file: { name, contents: degraded ? null : `old ${index}` },
      new_file: { name, contents: degraded ? null : `new ${index}` },
      ...(degraded ? { unified_patch: `diff --git a/${name} b/${name}` } : {}),
    })),
    meta: {
      degraded,
      total_changed: files.length,
      stats: { additions: 0, deletions: 0 },
      truncated: false,
    },
  } as any;
}

describe("extractRequestId", () => {
  test("reads `request_id` from the top level of the error body", () => {
    expect(extractRequestId({ request_id: "abc-123" })).toBe("abc-123");
  });

  test("reads `request_id` from errors[0] under the uniform envelope", () => {
    expect(
      extractRequestId({
        errors: [
          { status: "500", title: "Internal", request_id: "evt_42" },
        ],
      }),
    ).toBe("evt_42");
  });

  test("parses `Request ID: xyz` out of errors[0].detail", () => {
    expect(
      extractRequestId({
        errors: [
          {
            status: "500",
            title:  "Internal Server Error",
            detail: "Run files failed. Request ID: req_999 on shard 2.",
          },
        ],
      }),
    ).toBe("req_999");
  });

  test("returns null for bodies without any request_id", () => {
    expect(extractRequestId(null)).toBe(null);
    expect(extractRequestId(undefined)).toBe(null);
    expect(extractRequestId("not an object")).toBe(null);
    expect(extractRequestId({ errors: [] })).toBe(null);
    expect(extractRequestId({ errors: [{ detail: "no id in here" }] })).toBe(
      null,
    );
  });

  test("handles request_id values with hyphens and underscores", () => {
    expect(
      extractRequestId({
        errors: [
          { detail: "Something failed. request_id: RX-1A_2B-3C4D" },
        ],
      }),
    ).toBe("RX-1A_2B-3C4D");
  });
});

describe("emptyTransitionToastMessage", () => {
  test("returns the no-changes toast when a populated diff becomes empty", () => {
    expect(emptyTransitionToastMessage(3, 0)).toBe("No changes in this run.");
  });

  test("returns null when the diff was already empty", () => {
    expect(emptyTransitionToastMessage(0, 0)).toBeNull();
    expect(emptyTransitionToastMessage(null, 0)).toBeNull();
    expect(emptyTransitionToastMessage(2, 1)).toBeNull();
  });
});

describe("deepLinkToastMessage", () => {
  test("returns the missing-file message when the requested file is absent", () => {
    expect(
      deepLinkToastMessage(
        "src/missing.ts",
        buildRunFilesPayload({ files: ["src/present.ts"] }),
      ),
    ).toBe("File src/missing.ts is not in this run.");
  });

  test("returns null when the deep-linked file exists", () => {
    expect(
      deepLinkToastMessage(
        "src/present.ts",
        buildRunFilesPayload({ files: ["src/present.ts"] }),
      ),
    ).toBeNull();
  });

  test("returns null for degraded file-shaped payloads when the file exists", () => {
    expect(
      deepLinkToastMessage(
        "src/present.ts",
        buildRunFilesPayload({
          degraded: true,
          files: ["src/present.ts"],
        }),
      ),
    ).toBeNull();
  });
});
