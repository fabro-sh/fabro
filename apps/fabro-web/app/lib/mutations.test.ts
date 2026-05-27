import { describe, expect, mock, test, beforeEach } from "bun:test";

const mutateMock = mock((..._args: unknown[]) => Promise.resolve(undefined));
let lastMutationOptions: { onSuccess?: (result: unknown) => void } | null = null;

const useSWRMutationMock = mock((_key: unknown, _fetcher: unknown, options: unknown) => {
  lastMutationOptions = options as { onSuccess?: (result: unknown) => void };
  return {
    trigger: mock(),
    isMutating: false,
    reset: mock(),
  };
});

mock.module("swr", () => ({
  useSWRConfig: () => ({ mutate: mutateMock }),
}));

mock.module("swr/mutation", () => ({
  default: useSWRMutationMock,
}));

const { useArchiveRun } = await import("./mutations");
mock.restore();

beforeEach(() => {
  mutateMock.mockClear();
  useSWRMutationMock.mockClear();
  lastMutationOptions = null;
});

describe("lifecycle mutations", () => {
  test("successful archive invalidates run list caches via a matcher", () => {
    useArchiveRun("run-1");

    lastMutationOptions?.onSuccess?.({
      intent: "archive",
      ok: true,
      run: {},
    });

    const matchers = mutateMock.mock.calls
      .map((call) => call[0])
      .filter((arg): arg is (key: unknown) => boolean => typeof arg === "function");
    expect(matchers.length).toBeGreaterThan(0);
    const matcher = matchers[0];
    expect(matcher!(["runs", "all", { includeArchived: false }])).toBe(true);
    expect(matcher!(["runs", "all", { includeArchived: true }])).toBe(true);
    expect(matcher!(["runs", "page", { sort: "created_at" }])).toBe(true);
    expect(matcher!(["runs", "detail", "run-1"])).toBe(false);
  });
});
