import { describe, expect, mock, test, beforeEach } from "bun:test";

import { queryKeys } from "./query-keys";

const mutateMock = mock((..._args: unknown[]) => Promise.resolve(undefined));
let lastMutationOptions: { onSuccess?: (result: unknown) => void } | null = null;

const useSWRMutationMock = mock((_key: unknown, _fetcher: unknown, options: unknown) => {
  lastMutationOptions = options as { onSuccess?: (result: unknown) => void };
  return {};
});

mock.module("swr", () => ({
  useSWRConfig: () => ({ mutate: mutateMock }),
}));

mock.module("swr/mutation", () => ({
  default: useSWRMutationMock,
}));

mock.module("./api-client", () => ({
  apiData: mock(),
  authApi: {},
  humanInTheLoopApi: {},
  runsApi: {},
}));

mock.module("./run-actions", () => ({
  archiveRun: mock(),
  cancelRun: mock(),
  isLifecycleActionError: () => false,
  retryRun: mock(),
  unarchiveRun: mock(),
}));

const { useArchiveRun } = await import("./mutations");

beforeEach(() => {
  mutateMock.mockClear();
  useSWRMutationMock.mockClear();
  lastMutationOptions = null;
});

describe("lifecycle mutations", () => {
  test("successful archive invalidates both board run caches", () => {
    useArchiveRun("run-1");

    lastMutationOptions?.onSuccess?.({
      intent: "archive",
      ok: true,
      run: {},
    });

    const keys = mutateMock.mock.calls.map((call) => call[0]);
    expect(keys).toContainEqual(queryKeys.boards.runs(false));
    expect(keys).toContainEqual(queryKeys.boards.runs(true));
  });
});
