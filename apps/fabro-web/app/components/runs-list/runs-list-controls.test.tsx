import { afterAll, beforeAll, describe, expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter } from "react-router";
import { SWRConfig } from "swr";
import { BoardColumn } from "@qltysh/fabro-api-client";

import type { RunWithStatus } from "../../data/runs";
import { setupReactTestEnv, textContent } from "../../lib/test-utils";
import { FilterButton } from "./filter-button";
import { ListPager } from "./list-pager";
import { RunTableRow } from "./run-table-row";
import { SelectionCheckbox } from "./selection-checkbox";
import { SortHeader } from "./sort-header";
import { statusChipClass } from "./status-chip";
import { StatusFilterButton } from "./status-filter-button";

let restore: () => void;
beforeAll(() => {
  restore = setupReactTestEnv();
});
afterAll(() => restore());

function create(
  element: Parameters<typeof TestRenderer.create>[0],
  options?: Parameters<typeof TestRenderer.create>[1],
) {
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(element, options);
  });
  return renderer;
}

const run = (overrides: Partial<RunWithStatus> = {}): RunWithStatus =>
  ({
    id: "run-1",
    repo: "fabro",
    title: "Fix the thing",
    workflow: "implement",
    status: BoardColumn.FAILED,
    statusLabel: "Failed",
    elapsed: "2m",
    lifecycleStatus: "failed",
    createdBy: { kind: "user", login: "octocat" },
    ...overrides,
  }) as unknown as RunWithStatus;

function rowMarkup(props: Partial<Parameters<typeof RunTableRow>[0]> = {}) {
  return renderToStaticMarkup(
    <SWRConfig value={{ provider: () => new Map() }}>
      <MemoryRouter>
        <table>
          <tbody>
            <RunTableRow
              run={run()}
              hiddenColumns={new Set()}
              selected={false}
              onToggleSelected={() => {}}
              {...props}
            />
          </tbody>
        </table>
      </MemoryRouter>
    </SWRConfig>,
  );
}

describe("run status", () => {
  test.each([
    [BoardColumn.RUNNING, "chip-live"],
    [BoardColumn.SUCCEEDED, "chip-ok"],
    [BoardColumn.FAILED, "chip-bad"],
    [BoardColumn.BLOCKED, "chip-warn"],
    [BoardColumn.INITIALIZING, "chip-info"],
    [BoardColumn.PENDING, "chip-outline"],
    [BoardColumn.RUNNABLE, "chip-outline"],
    [BoardColumn.REMOVING, "chip-outline"],
  ])("%s uses Factory's %s", (status, chip) => {
    expect(statusChipClass(status)).toBe(`chip ${chip}`);
  });

  test("an archived run is the plain chip", () => {
    expect(statusChipClass(BoardColumn.ARCHIVED)).toBe("chip");
  });

  test("the row writes the status as a word inside the chip, not as a color alone", () => {
    const markup = rowMarkup();
    expect(markup).toContain('data-run-status="failed"');
    expect(markup).toMatch(/class="chip chip-bad"[^>]*>Failed</);
  });
});

describe("selection", () => {
  test("a selected row carries Factory's fill and left marker", () => {
    expect(rowMarkup({ selected: true })).toContain("bg-peach");
    expect(rowMarkup({ selected: true })).toContain("bg-signal");
    expect(rowMarkup({ selected: false })).not.toContain("bg-signal");
  });

  test("the checkbox states what it will do", () => {
    expect(rowMarkup({ selected: true })).toContain('aria-label="Deselect run Fix the thing"');
    expect(rowMarkup({ selected: false })).toContain('aria-label="Select run Fix the thing"');
  });

  test("the header checkbox shows a partial selection through the DOM property", () => {
    const input = { indeterminate: false };
    create(
      <SelectionCheckbox checked={false} indeterminate onChange={() => {}} ariaLabel="All" />,
      { createNodeMock: () => input },
    );
    expect(input.indeterminate).toBe(true);
  });
});

describe("filter buttons", () => {
  const options = [
    { value: "all", label: "All repos" },
    { value: "fabro", label: "fabro" },
  ];

  test("a filter narrowing the list says so in words and marks itself", () => {
    const markup = renderToStaticMarkup(
      <FilterButton label="Repo" value="fabro" allValue="all" options={options} onChange={() => {}} />,
    );
    expect(markup).toContain("Repo: fabro");
    expect(markup).toContain('data-filtering="true"');
    expect(markup).toContain("btn btn-sm h-9");
  });

  test("an idle filter is plain", () => {
    const markup = renderToStaticMarkup(
      <FilterButton label="Repo" value="all" allValue="all" options={options} onChange={() => {}} />,
    );
    expect(markup).toContain(">Repo<");
    expect(markup).not.toContain("data-filtering");
  });

  test("status counts several choices, and an all-or-nothing choice narrows nothing", () => {
    const two = renderToStaticMarkup(
      <StatusFilterButton
        value={new Set([BoardColumn.RUNNING, BoardColumn.FAILED])}
        onChange={() => {}}
      />,
    );
    expect(two).toContain("Status: 2");
    expect(two).toContain('data-filtering="true"');
    const none = renderToStaticMarkup(
      <StatusFilterButton value={new Set()} onChange={() => {}} />,
    );
    expect(none).toContain(">Status<");
    expect(none).not.toContain("data-filtering");
  });
});

describe("sort header", () => {
  const mountHeader = (activeSort: string, direction: "asc" | "desc") => {
    const onClick = mock((_key: string) => {});
    const renderer = create(
      <table>
        <thead>
          <tr>
            <SortHeader
              label="Title"
              sortKey="title"
              activeSort={activeSort}
              direction={direction}
              onClick={onClick}
            />
          </tr>
        </thead>
      </table>,
    );
    return { th: renderer.root.findByType("th"), renderer, onClick };
  };

  test("tells assistive tech the sort state and reports clicks", () => {
    expect(mountHeader("title", "asc").th.props["aria-sort"]).toBe("ascending");
    expect(mountHeader("title", "desc").th.props["aria-sort"]).toBe("descending");
    expect(mountHeader("repo", "asc").th.props["aria-sort"]).toBe("none");
    const { renderer, onClick } = mountHeader("repo", "asc");
    act(() => renderer.root.findByType("button").props.onClick());
    expect(onClick).toHaveBeenCalledWith("title");
  });
});

describe("pager", () => {
  const mountPager = (page: number, pageCount: number | null, hasMore = false) => {
    const onPageChange = mock((_page: number) => {});
    const onPageSizeChange = mock((_size: number) => {});
    const renderer = create(
      <ListPager
        page={page}
        pageSize={25}
        pageCount={pageCount}
        hasMore={hasMore}
        disabled={false}
        onPageChange={onPageChange}
        onPageSizeChange={onPageSizeChange}
      />,
    );
    const button = (label: string) => renderer.root.findByProps({ "aria-label": label, type: "button" });
    return { renderer, button, onPageChange, onPageSizeChange };
  };

  test("the ends disable themselves, and the middle moves", () => {
    const first = mountPager(1, 3);
    expect(first.button("First page").props.disabled).toBe(true);
    expect(first.button("Previous page").props.disabled).toBe(true);
    expect(first.button("Next page").props.disabled).toBe(false);

    const middle = mountPager(2, 3);
    act(() => middle.button("Next page").props.onClick());
    expect(middle.onPageChange).toHaveBeenCalledWith(3);
    expect(textContent(middle.renderer.root)).toContain("Page 2 of 3");

    const last = mountPager(3, 3);
    expect(last.button("Next page").props.disabled).toBe(true);
    expect(last.button("Last page").props.disabled).toBe(true);
  });

  test("with an unknown total it trusts has-more and offers no last page", () => {
    const pager = mountPager(4, null, true);
    expect(pager.button("Next page").props.disabled).toBe(false);
    expect(pager.button("Last page").props.disabled).toBe(true);
    expect(textContent(pager.renderer.root)).not.toContain(" of ");
  });

  test("the page-size select is a labelled field that reports a number", () => {
    const pager = mountPager(1, 3);
    const select = pager.renderer.root.findByType("select");
    expect(pager.renderer.root.findByType("label").props.htmlFor).toBe(select.props.id);
    act(() => select.props.onChange({ target: { value: "50" } }));
    expect(pager.onPageSizeChange).toHaveBeenCalledWith(50);
  });
});
