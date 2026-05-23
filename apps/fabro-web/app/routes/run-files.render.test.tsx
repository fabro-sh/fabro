import { afterEach, describe, expect, mock, test } from "bun:test";
import { useRef } from "react";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter, Route, Routes } from "react-router";

import { ToastProvider } from "../components/toast";

let currentFilesPayload: any = null;
let currentCommitsPayload: any = null;
let currentRunStatus = "succeeded";
const useRunFilesCalls: any[] = [];

const multiFileDiffCalls: any[] = [];
const patchDiffCalls: any[] = [];
let patchDiffMountSeq = 0;
const virtualizerCalls: any[] = [];
const providerCalls: any[] = [];
const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

mock.module("@pierre/diffs/react", () => ({
  MultiFileDiff: (props: any) => {
    multiFileDiffCalls.push(props);
    return <div data-pierre-multi="true">{props.newFile.name}</div>;
  },
  PatchDiff: (props: any) => {
    const mountId = useRef(++patchDiffMountSeq);
    patchDiffCalls.push({ ...props, mountId: mountId.current });
    return (
      <div data-pierre-patch="true" data-mount-id={mountId.current}>
        {props.patch}
      </div>
    );
  },
  Virtualizer: (props: any) => {
    virtualizerCalls.push(props);
    return <div data-pierre-virtualizer="true">{props.children}</div>;
  },
  WorkerPoolContextProvider: (props: any) => {
    providerCalls.push(props);
    return <div data-pierre-worker-pool="true">{props.children}</div>;
  },
}));

mock.module("../lib/queries", () => ({
  useRun: () => ({
    data: {
      id:               "run_1",
      goal:             "Run 1",
      title:            "Run 1",
      workflow:         { slug: "default", name: "Default" },
      automation:       null,
      repository:       { name: "fabro", origin_url: null, provider: "unknown" },
      created_by:       null,
      origin:           { kind: "api" },
      labels:           {},
      lifecycle:        {
        status:          { kind: currentRunStatus },
        pending_control: null,
        queue_position:  null,
        error:           null,
        archived:        false,
        archived_at:     null,
      },
      sandbox:          null,
      models:           [],
      source_directory: null,
      timestamps:       {
        created_at:    "2026-05-05T12:00:00Z",
        started_at:    null,
        last_event_at: null,
        completed_at:  null,
      },
      billing:          null,
      size:             "XS",
      diff:             null,
      pull_request:     null,
      current_question: null,
      superseded_by:    null,
      retried_from:     null,
      links:            { web: null },
    },
  }),
  useRunCommits: () => ({
    data:         currentCommitsPayload,
    error:        null,
    isLoading:    false,
    isValidating: false,
    mutate:       mock(() => Promise.resolve(currentCommitsPayload)),
  }),
  useRunFiles: (id: string | undefined, selection: any) => {
    useRunFilesCalls.push({ id, selection });
    return {
      data:         currentFilesPayload,
      error:        null,
      isLoading:    false,
      isValidating: false,
      mutate:       mock(() => Promise.resolve(currentFilesPayload)),
    };
  },
  useRunQuestions: () => ({ data: [] }),
}));

const { default: RunFiles } = await import("./run-files");

function makeFiles(count: number) {
  return Array.from({ length: count }, (_, index) => {
    const name = `src/file-${index}.ts`;
    return {
      change_kind: "modified",
      old_file:    { name, contents: `old ${index}\n` },
      new_file:    { name, contents: `new ${index}\n` },
    };
  });
}

function makePayload(count: number, source = "sandbox") {
  return {
    data: makeFiles(count),
    meta: {
      source,
      scope:               "committed",
      degraded:            false,
      degraded_reason:     null,
      total_changed:       count,
      stats:               { additions: count, deletions: count },
      truncated:           false,
      to_sha:              "abc1234",
      to_sha_committed_at: "2026-05-05T12:00:00Z",
    },
  };
}

function makePatchPayload(patch: string) {
  return {
    data: [
      {
        change_kind: "modified",
        old_file:    { name: "docs/live.md", contents: null },
        new_file:    { name: "docs/live.md", contents: null },
        unified_patch: patch,
      },
    ],
    meta:   {
      source:              "sandbox",
      scope:               "committed",
      degraded:            false,
      degraded_reason:     null,
      total_changed:       1,
      stats:               { additions: 1, deletions: 0 },
      truncated:           false,
      to_sha:              "abc1234",
      to_sha_committed_at: "2026-05-05T12:00:00Z",
    },
  };
}

function runFilesTree(initialEntry = "/runs/run_1/files") {
  return (
    <ToastProvider>
      <MemoryRouter initialEntries={[initialEntry]}>
        <Routes>
          <Route path="/runs/:id/files" element={<RunFiles />} />
        </Routes>
      </MemoryRouter>
    </ToastProvider>
  );
}

function renderRunFiles(initialEntry = "/runs/run_1/files") {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(runFilesTree(initialEntry));
  });
  mountedRenderers.push(renderer!);
  return renderer!;
}

describe("RunFiles rendering", () => {
  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    currentFilesPayload = null;
    currentCommitsPayload = null;
    currentRunStatus = "succeeded";
    multiFileDiffCalls.length = 0;
    patchDiffCalls.length = 0;
    patchDiffMountSeq = 0;
    virtualizerCalls.length = 0;
    providerCalls.length = 0;
    useRunFilesCalls.length = 0;
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  test("renders a one-file payload through Pierre Virtualizer", () => {
    currentFilesPayload = makePayload(1);

    const renderer = renderRunFiles();

    expect(virtualizerCalls).toHaveLength(1);
    expect(renderer.root.findAllByProps({ "data-run-file-row": "true" })).toHaveLength(1);
    expect(multiFileDiffCalls[0].options.diffStyle).toBe("split");
  });

  test("passes the selected URL scope to useRunFiles", () => {
    currentFilesPayload = makePayload(1);

    renderRunFiles("/runs/run_1/files?scope=all#file=src/file-0.ts");

    expect(useRunFilesCalls[0]).toEqual({
      id:        "run_1",
      selection: { kind: "scope", scope: "all" },
    });
  });

  test("passes a selected commit range to useRunFiles", () => {
    currentFilesPayload = makePayload(1);
    currentCommitsPayload = {
      data: [
        {
          sha:       "b".repeat(40),
          short_sha: "bbbbbbb",
          subject:   "fabro(run_1): implement (succeeded)",
          parents:   [{ sha: "a".repeat(40), short_sha: "aaaaaaa" }],
        },
      ],
    };

    renderRunFiles(`/runs/run_1/files?commit=${"b".repeat(40)}`);

    expect(useRunFilesCalls[0]).toEqual({
      id:        "run_1",
      selection: {
        kind:    "commit",
        fromSha: "a".repeat(40),
        toSha:   "b".repeat(40),
      },
    });
  });

  test("shows the scope picker only for sandbox responses", () => {
    currentFilesPayload = makePayload(1, "sandbox");
    const sandboxRenderer = renderRunFiles();
    expect(
      sandboxRenderer.root.findAllByProps({ "aria-label": "Diff selection" }),
    ).not.toHaveLength(0);

    act(() => sandboxRenderer.unmount());
    mountedRenderers.pop();
    currentFilesPayload = makePayload(1, "final_patch");
    const fallbackRenderer = renderRunFiles("/runs/run_1/files?scope=all");

    expect(
      fallbackRenderer.root.findAllByProps({ "aria-label": "Diff selection" }),
    ).toHaveLength(0);
  });

  test("renders a 27-file payload through one Pierre Virtualizer", () => {
    currentFilesPayload = makePayload(27);

    const renderer = renderRunFiles();

    expect(virtualizerCalls).toHaveLength(1);
    expect(renderer.root.findAllByProps({ "data-run-file-row": "true" })).toHaveLength(27);
  });

  test("passes stable Pierre cache keys across unrelated re-renders", () => {
    currentFilesPayload = makePayload(1);

    const renderer = renderRunFiles();
    const firstOldKey = multiFileDiffCalls[0].oldFile.cacheKey;
    const firstNewKey = multiFileDiffCalls[0].newFile.cacheKey;

    act(() => {
      renderer.update(runFilesTree());
    });

    const lastCall = multiFileDiffCalls[multiFileDiffCalls.length - 1];
    expect(firstOldKey).toBe(lastCall.oldFile.cacheKey);
    expect(firstNewKey).toBe(lastCall.newFile.cacheKey);
    expect(firstOldKey).toContain("fabro-run-file:run_1:abc1234:old:src/file-0.ts:");
    expect(firstNewKey).toContain("fabro-run-file:run_1:abc1234:new:src/file-0.ts:");
    expect(lastCall.options).not.toHaveProperty("theme");
  });

  test("remounts patch diffs when the patch changes for the same file", () => {
    currentFilesPayload = makePatchPayload(
      "diff --git a/docs/live.md b/docs/live.md\n@@ -1 +1 @@\n+committed\n",
    );

    const renderer = renderRunFiles("/runs/run_1/files?scope=all");
    const firstMountId = patchDiffCalls[0].mountId;

    currentFilesPayload = makePatchPayload(
      "diff --git a/docs/live.md b/docs/live.md\n@@ -1,0 +1,2 @@\n+committed\n+uncommitted\n",
    );
    act(() => {
      renderer.update(runFilesTree("/runs/run_1/files?scope=all"));
    });

    const lastCall = patchDiffCalls[patchDiffCalls.length - 1];
    expect(lastCall.patch).toContain("+uncommitted");
    expect(lastCall.mountId).not.toBe(firstMountId);
  });
});
