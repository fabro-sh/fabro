import { describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import {
  RunSummaryPanelView,
  type RunSummaryPanelViewProps,
} from "./run-summary-panel";

function instanceText(instance: TestRenderer.ReactTestInstance): string {
  const parts: string[] = [];
  for (const child of instance.children) {
    if (typeof child === "string") parts.push(child);
    else parts.push(instanceText(child));
  }
  return parts.join("");
}

function render(props: Partial<RunSummaryPanelViewProps> = {}) {
  const full: RunSummaryPanelViewProps = {
    run:              null,
    runLoading:       false,
    sandboxResources: null,
    sandboxLoading:   false,
    artifactsCount:   null,
    artifactsLoading: false,
    ...props,
  };
  let tree: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    tree = TestRenderer.create(<RunSummaryPanelView {...full} />);
  });
  return tree!;
}

function cellAfterLabel(
  tree: TestRenderer.ReactTestRenderer,
  label: string,
): TestRenderer.ReactTestInstance {
  const labelNode = tree.root.find(
    (node) =>
      node.type === "div" &&
      node.children.length === 1 &&
      typeof node.children[0] === "string" &&
      node.children[0] === label,
  );
  const parent = labelNode.parent;
  if (!parent) throw new Error(`Could not find parent of label "${label}"`);
  return parent.children[1] as TestRenderer.ReactTestInstance;
}

function makeRun(overrides: Record<string, any> = {}) {
  return {
    id:         "run_1",
    created_by: null,
    diff:       null,
    billing:    null,
    ...overrides,
  } as any;
}

const EM_DASH = "—";

describe("RunSummaryPanelView", () => {
  test("renders all five column labels", () => {
    const tree = render();
    const rendered = JSON.stringify(tree.toJSON());
    for (const label of ["Created by", "Changes", "Sandbox", "Cost", "Artifacts"]) {
      expect(rendered).toContain(label);
    }
  });

  test("shows em dash for missing run fields after load", () => {
    const tree = render({ run: makeRun() });
    expect(instanceText(cellAfterLabel(tree, "Created by"))).toBe(EM_DASH);
    expect(instanceText(cellAfterLabel(tree, "Changes"))).toBe(EM_DASH);
    expect(instanceText(cellAfterLabel(tree, "Cost"))).toBe(EM_DASH);
  });

  test("shows em dash when sandbox is absent", () => {
    const tree = render({ run: makeRun(), sandboxResources: null });
    expect(instanceText(cellAfterLabel(tree, "Sandbox"))).toBe(EM_DASH);
  });

  test("shows em dash when artifacts count is zero", () => {
    const tree = render({ run: makeRun(), artifactsCount: 0 });
    expect(instanceText(cellAfterLabel(tree, "Artifacts"))).toBe(EM_DASH);
  });

  test("renders diff additions/deletions/files with correct formatting", () => {
    const tree = render({
      run: makeRun({
        diff: { additions: 124, deletions: 37, files_changed: 7 },
      }),
    });
    expect(instanceText(cellAfterLabel(tree, "Changes"))).toBe(
      "+124 −37in 7 files",
    );
  });

  test("singular 'file' when files_changed is 1", () => {
    const tree = render({
      run: makeRun({
        diff: { additions: 3, deletions: 0, files_changed: 1 },
      }),
    });
    expect(instanceText(cellAfterLabel(tree, "Changes"))).toBe(
      "+3 −0in 1 file",
    );
  });

  test("renders cost from total_usd_micros", () => {
    const tree = render({
      run: makeRun({ billing: { total_usd_micros: 840_000 } }),
    });
    expect(instanceText(cellAfterLabel(tree, "Cost"))).toBe("$0.84");
  });

  test("renders sandbox CPU and memory", () => {
    const tree = render({
      run:              makeRun(),
      sandboxResources: { cpu_cores: 4, memory_bytes: 8 * 1024 * 1024 * 1024 } as any,
    });
    expect(instanceText(cellAfterLabel(tree, "Sandbox"))).toBe("4 CPU · 8 GiB");
  });

  test("renders artifacts count when positive", () => {
    const tree = render({ run: makeRun(), artifactsCount: 3 });
    expect(instanceText(cellAfterLabel(tree, "Artifacts"))).toBe("3");
  });

  test("renders user actor with login initial", () => {
    const tree = render({
      run: makeRun({
        created_by: {
          kind:        "user",
          identity:    { issuer: "github", subject: "1" },
          login:       "brynary",
          auth_method: "oauth",
        },
      }),
    });
    expect(instanceText(cellAfterLabel(tree, "Created by"))).toBe("Bbrynary");
  });

  test("renders user actor avatar when avatar_url is set", () => {
    const tree = render({
      run: makeRun({
        created_by: {
          kind:        "user",
          identity:    { issuer: "github", subject: "1" },
          login:       "brynary",
          auth_method: "oauth",
          avatar_url:  "https://example.com/brynary.png",
        },
      }),
    });
    const cell = cellAfterLabel(tree, "Created by");
    const img = cell.find((node) => node.type === "img");
    expect(img.props.src).toBe("https://example.com/brynary.png");
    expect(instanceText(cell)).toBe("brynary");
  });

  test("renders non-user actor with kind label", () => {
    for (const kind of ["agent", "system", "slack", "webhook", "worker", "anonymous"]) {
      const tree = render({ run: makeRun({ created_by: { kind } as any }) });
      expect(instanceText(cellAfterLabel(tree, "Created by"))).toContain(kind);
    }
  });

  test("shows skeleton placeholders while queries are loading", () => {
    const tree = render({
      runLoading:       true,
      sandboxLoading:   true,
      artifactsLoading: true,
    });
    const rendered = JSON.stringify(tree.toJSON());
    expect(rendered).toContain("animate-pulse");
    expect(instanceText(cellAfterLabel(tree, "Created by"))).not.toContain(EM_DASH);
    expect(instanceText(cellAfterLabel(tree, "Sandbox"))).not.toContain(EM_DASH);
    expect(instanceText(cellAfterLabel(tree, "Artifacts"))).not.toContain(EM_DASH);
  });
});
