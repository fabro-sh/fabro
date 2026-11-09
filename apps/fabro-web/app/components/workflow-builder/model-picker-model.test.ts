import { describe, expect, test } from "bun:test";

import { modelOfferingKey } from "../../lib/model-offerings";
import { newDraft, parseWorkflow } from "../../lib/workflow-builder/model";
import type { WorkflowGraph, WorkflowNode } from "../../lib/workflow-builder/types";
import { editorReducer, initEditor, type EditorState } from "./editor-state";
import { TEST_CATALOG } from "./model-picker-fixtures";
import {
  buildProviderGroups,
  choiceOf,
  inheritedNodeModel,
  modelChoiceChange,
  nextNavIndex,
  parseCustomChoice,
  pickerCatalogStatus,
  resolveModel,
  searchGroups,
  starRows,
  summarizeChoice,
} from "./model-picker-model";

const NOW = 1_000_000;
const CATALOG = TEST_CATALOG;
const GROUPS = buildProviderGroups(CATALOG);

function search(query: string): Record<string, string[]> {
  return Object.fromEntries(
    searchGroups(GROUPS, CATALOG.providers, query).map((match) => [
      match.group.id,
      match.models.map((model) => model.id),
    ]),
  );
}

function graphOf(source: string): WorkflowGraph {
  const { graph } = parseWorkflow(source);
  if (!graph) throw new Error("expected the source to parse");
  return graph;
}

function nodeOf(graph: WorkflowGraph, id: string): WorkflowNode {
  const node = graph.nodes.find((candidate) => candidate.id === id);
  if (!node) throw new Error(`no node ${id}`);
  return node;
}

function workflow(graphAttrs: string, plan: string, extra = ""): string {
  return `digraph T {
  graph [${graphAttrs}]
  ${extra}
  start [shape=Mdiamond]
  plan [shape=box, prompt="Plan"${plan ? `, ${plan}` : ""}]
  exit [shape=Msquare]
  start -> plan -> exit
}
`;
}

function editorWith(source: string): EditorState {
  return initEditor({ ...newDraft("Test"), source, positions: {}, promptParts: {}, assets: {} });
}

function liveGraph(state: EditorState): WorkflowGraph {
  if (!state.graph) throw new Error("expected a graph");
  return state.graph;
}

describe("provider tiers", () => {
  test("groups by provider, ready ones first, then by catalog priority, and empty ones last", () => {
    expect(GROUPS.map((group) => [group.id, group.ready, group.models.length])).toEqual([
      ["acme", true, 2],
      ["router", false, 2],
      ["unlisted", false, 1],
      ["local", false, 0],
    ]);
  });

  test("a provider the provider list doesn't name still shows, under its ID", () => {
    expect(GROUPS.find((group) => group.id === "unlisted")?.name).toBe("unlisted");
  });

  test("one model ID on two providers stays two offerings", () => {
    const acme = GROUPS.find((group) => group.id === "acme")?.models.find((model) => model.id === "acme-large");
    const router = GROUPS.find((group) => group.id === "router")?.models.find((model) => model.id === "acme-large");
    expect(acme?.display_name).toBe("Acme Large");
    expect(router?.display_name).toBe("Acme Large via Router");
    expect(modelOfferingKey(acme!)).not.toBe(modelOfferingKey(router!));
  });

  test("a group is ready only if one of its models is, even when the provider says it has credentials", () => {
    const [group] = buildProviderGroups({
      providers: [{ id: "p", display_name: "P", configured: true, priority: 1, aliases: [], default_model: null }],
      models: [{ ...CATALOG.models[0], provider: "p", configured: false }],
    });
    expect(group.ready).toBe(false);
  });
});

describe("search", () => {
  test("an empty query lists everything", () => {
    expect(Object.keys(search("   "))).toEqual(["acme", "router", "unlisted", "local"]);
  });

  test("a company name lists all of that company's models", () => {
    expect(search("acme ai")).toEqual({ acme: ["acme-large", "acme-small"] });
  });

  test("a provider alias finds the provider", () => {
    expect(search("rc")).toEqual({ router: ["acme-large", "router-fast"] });
  });

  test("model name, ID, alias and family each find the model", () => {
    expect(search("bolt 5.5")).toEqual({ router: ["router-fast"] });
    expect(search("router-fast")).toEqual({ router: ["router-fast"] });
    expect(search("bolt-latest")).toEqual({ router: ["router-fast"] });
    expect(search("atlas")).toEqual({ acme: ["acme-large", "acme-small"], router: ["acme-large"] });
  });

  test("words can mix company and model, in any order", () => {
    expect(search("large router")).toEqual({ router: ["acme-large"] });
    expect(search("routerco bolt")).toEqual({ router: ["router-fast"] });
  });

  test("punctuation and case don't matter", () => {
    expect(search("ROUTER_FAST")).toEqual({ router: ["router-fast"] });
    expect(search("bolt55")).toEqual({ router: ["router-fast"] });
  });

  test("a provider with no models is still found by name", () => {
    expect(search("local gateway")).toEqual({ local: [] });
  });

  test("a query that matches nothing returns nothing", () => {
    expect(search("nothing-like-this")).toEqual({});
  });

  test("punctuation alone only matches that punctuation", () => {
    expect(search("---")).toEqual({});
  });
});

describe("starred rows", () => {
  test("a star keeps its provider and is kept when the catalog no longer lists it", () => {
    const rows = starRows(
      [
        { provider: "router", id: "acme-large" },
        { provider: "gone", id: "retired" },
      ],
      CATALOG,
      "",
    );
    expect(rows.map((row) => [row.star.provider, row.model?.display_name ?? null, row.providerName])).toEqual([
      ["router", "Acme Large via Router", "RouterCo"],
      ["gone", null, "gone"],
    ]);
  });

  test("the query narrows them, including by company", () => {
    const stars = [
      { provider: "acme", id: "acme-small" },
      { provider: "router", id: "router-fast" },
    ];
    expect(starRows(stars, CATALOG, "routerco").map((row) => row.star.id)).toEqual(["router-fast"]);
    expect(starRows(stars, CATALOG, "small").map((row) => row.star.id)).toEqual(["acme-small"]);
  });

  test("they still list while the catalog is away", () => {
    expect(starRows([{ provider: "acme", id: "acme-small" }], null, "")[0]).toEqual({
      star: { provider: "acme", id: "acme-small" },
      model: null,
      providerName: "acme",
    });
  });
});

describe("reading a written value", () => {
  test("an explicit provider picks the right offering of a shared ID", () => {
    const router = resolveModel(CATALOG, "acme-large", "router");
    expect(router.kind === "resolved" && router.offering.display_name).toBe("Acme Large via Router");
  });

  test("an alias resolves within its provider, and so does a provider alias", () => {
    const hit = resolveModel(CATALOG, "large", "acme-ai");
    expect(hit.kind === "resolved" && modelOfferingKey(hit.offering)).toBe(
      modelOfferingKey({ provider: "acme", id: "acme-large" }),
    );
  });

  test("a bare ID on several providers is left for the run to choose", () => {
    expect(resolveModel(CATALOG, "acme-large", undefined).kind).toBe("ambiguous");
  });

  test("a bare ID on one provider resolves, and so does a provider/model selector", () => {
    expect(resolveModel(CATALOG, "acme-small", undefined).kind).toBe("resolved");
    const pinned = resolveModel(CATALOG, "router/acme-large", undefined);
    expect(pinned.kind === "resolved" && pinned.offering.provider).toBe("router");
  });

  test("a listed model under the wrong provider is a mismatch, and an unlisted one is unknown", () => {
    expect(resolveModel(CATALOG, "router-fast", "acme").kind).toBe("mismatch");
    expect(resolveModel(CATALOG, "vendor/experimental-9", undefined).kind).toBe("unknown");
  });
});

describe("what the field says", () => {
  test("a catalog model shows its friendly name, provider and exact ID", () => {
    expect(summarizeChoice(CATALOG, { model: "acme-small", provider: "acme" }, {})).toEqual({
      title: "Acme Small",
      detail: "Acme AI · acme-small",
      source: null,
      currentKey: modelOfferingKey({ provider: "acme", id: "acme-small" }),
      setupRequired: false,
      notInCatalog: false,
      warning: null,
    });
  });

  test("an unconfigured offering says setup is required", () => {
    expect(summarizeChoice(CATALOG, { model: "router-fast", provider: "router" }, {}).setupRequired).toBe(true);
  });

  test("an imported model the catalog doesn't list is reported as written", () => {
    expect(summarizeChoice(CATALOG, { model: "vendor/experimental-9", provider: "vendor" }, {})).toMatchObject({
      title: "vendor/experimental-9",
      detail: "vendor · vendor/experimental-9",
      notInCatalog: true,
      currentKey: null,
    });
  });

  test("nothing is claimed about the catalog before it loads", () => {
    expect(summarizeChoice(null, { model: "whatever", provider: "acme" }, {})).toMatchObject({
      title: "whatever",
      notInCatalog: false,
      setupRequired: false,
      currentKey: null,
    });
  });

  test("a template value waits for the run", () => {
    expect(summarizeChoice(CATALOG, { model: "{{ vars.model }}" }, {})).toMatchObject({
      title: "{{ vars.model }}",
      notInCatalog: false,
    });
  });

  test("inheriting shows where it comes from", () => {
    expect(
      summarizeChoice(CATALOG, {}, {
        model: { value: "acme-large", source: "Workflow default" },
        provider: { value: "acme", source: "Workflow default" },
      }),
    ).toMatchObject({ title: "Acme Large", source: "Workflow default" });
  });

  test("nothing set anywhere is the server default", () => {
    expect(summarizeChoice(CATALOG, {}, {})).toMatchObject({ title: "Server default", source: null, currentKey: null });
  });

  test("a stylesheet that can't be read here is admitted", () => {
    expect(summarizeChoice(CATALOG, {}, { stylesheetOpaque: true }).title).toBe("Stylesheet or server default");
  });

  test("a provider alone runs that provider's default model", () => {
    expect(summarizeChoice(CATALOG, { provider: "router" }, {})).toMatchObject({
      title: "RouterCo default",
      detail: "router-fast",
      setupRequired: true,
      source: null,
    });
  });

  test("a model paired with an inherited provider that doesn't serve it warns and names the source", () => {
    const summary = summarizeChoice(
      CATALOG,
      { model: "router-fast" },
      { provider: { value: "acme", source: "Workflow default" } },
    );
    expect(summary.warning).toBe(
      "router-fast is not listed under acme (workflow default). Choose it from the list to set the model and provider together.",
    );
    expect(summary.currentKey).toBeNull();
  });
});

describe("what a node inherits", () => {
  test("each layer wins over the next: stage, node defaults, stylesheet, workflow", () => {
    const stylesheet = `model_stylesheet="* { model: sheet-model; provider: sheet-provider; }"`;
    const defaults = `default_model="graph-model", default_provider="graph-provider"`;

    const fromGraph = graphOf(workflow(defaults, ""));
    expect(inheritedNodeModel(fromGraph, nodeOf(fromGraph, "plan"))).toEqual({
      model: { value: "graph-model", source: "Workflow default" },
      provider: { value: "graph-provider", source: "Workflow default" },
    });

    const fromSheet = graphOf(workflow(`${defaults}, ${stylesheet}`, ""));
    expect(inheritedNodeModel(fromSheet, nodeOf(fromSheet, "plan")).model).toEqual({
      value: "sheet-model",
      source: "Model stylesheet",
    });

    const fromNodeDefaults = graphOf(workflow(`${defaults}, ${stylesheet}`, "", `node [model="default-model"]`));
    expect(inheritedNodeModel(fromNodeDefaults, nodeOf(fromNodeDefaults, "plan"))).toMatchObject({
      model: { value: "default-model", source: "Node default" },
      provider: { value: "sheet-provider", source: "Model stylesheet" },
    });
  });

  test("a stage's defaults beat the workflow's, and the model and provider fall back separately", () => {
    const graph = graphOf(`digraph T {
  graph [default_model="graph-model", default_provider="graph-provider"]
  start [shape=Mdiamond]
  subgraph cluster_a {
    node [model="stage-model"]
    plan [shape=box, prompt="Plan"]
  }
  exit [shape=Msquare]
  start -> plan -> exit
}
`);
    expect(inheritedNodeModel(graph, nodeOf(graph, "plan"))).toEqual({
      model: { value: "stage-model", source: "Stage default" },
      provider: { value: "graph-provider", source: "Workflow default" },
    });
  });

  test("the most specific stylesheet rule wins", () => {
    const graph = graphOf(
      workflow(`model_stylesheet="* { model: everyone; } #plan { model: just-plan; } box { model: boxes; }"`, ""),
    );
    expect(inheritedNodeModel(graph, nodeOf(graph, "plan")).model?.value).toBe("just-plan");
  });

  test("a stylesheet kept in a file can't be read here, and that is said out loud", () => {
    const graph = graphOf(workflow(`model_stylesheet="@styles.css"`, ""));
    expect(inheritedNodeModel(graph, nodeOf(graph, "plan"))).toEqual({ stylesheetOpaque: true });
  });

  test("nothing set means nothing inherited", () => {
    const graph = graphOf(workflow(`goal="Ship"`, ""));
    expect(inheritedNodeModel(graph, nodeOf(graph, "plan"))).toEqual({});
  });
});

describe("writing a choice", () => {
  const NODE = { scope: "node", nodeId: "plan" } as const;
  const GRAPH = { scope: "graph" } as const;

  function choose(state: EditorState, change: ReturnType<typeof modelChoiceChange>): EditorState {
    return editorReducer(state, { type: "apply", change, now: NOW });
  }

  test("choosing sets the model and provider together, and one undo restores both", () => {
    const before = editorWith(workflow(`goal="Ship"`, `model="acme-large", provider="acme"`));
    const after = choose(before, modelChoiceChange(NODE, choiceOf({ id: "acme-large", provider: "router" })));

    expect(nodeOf(liveGraph(after), "plan").attrs).toMatchObject({ model: "acme-large", provider: "router" });
    expect(after.past.length).toBe(before.past.length + 1);

    const undone = editorReducer(after, { type: "undo" });
    expect(undone.draft.source).toBe(before.draft.source);
    expect(nodeOf(liveGraph(undone), "plan").attrs).toMatchObject({ model: "acme-large", provider: "acme" });
  });

  test("the workflow default writes both of its keys in one step, and undo restores them", () => {
    const before = editorWith(workflow(`goal="Ship"`, ""));
    const after = choose(before, modelChoiceChange(GRAPH, choiceOf({ id: "router-fast", provider: "router" })));

    expect(liveGraph(after).attrs).toMatchObject({ default_model: "router-fast", default_provider: "router" });
    expect(after.past.length).toBe(before.past.length + 1);
    expect(editorReducer(after, { type: "undo" }).draft.source).toBe(before.draft.source);
  });

  test("two quick choices are two undo steps", () => {
    const start = editorWith(workflow(`goal="Ship"`, ""));
    const first = choose(start, modelChoiceChange(NODE, { model: "a", provider: "p" }));
    const second = choose(first, modelChoiceChange(NODE, { model: "b", provider: "q" }));
    expect(second.past.length).toBe(start.past.length + 2);
    expect(nodeOf(liveGraph(editorReducer(second, { type: "undo" })), "plan").attrs).toMatchObject({
      model: "a",
      provider: "p",
    });
  });

  test("clearing removes both, which returns the node to what it inherits", () => {
    const source = workflow(`default_model="graph-model", default_provider="graph-provider"`, `model="own", provider="mine"`);
    const after = choose(editorWith(source), modelChoiceChange(NODE, null));
    const node = nodeOf(liveGraph(after), "plan");

    expect(node.attrs.model).toBeUndefined();
    expect(node.attrs.provider).toBeUndefined();
    expect(liveGraph(after).attrs).toMatchObject({ default_model: "graph-model", default_provider: "graph-provider" });
    expect(inheritedNodeModel(liveGraph(after), node).model?.value).toBe("graph-model");
  });

  test("clearing the workflow default removes both keys", () => {
    const source = workflow(`goal="Ship", default_model="m", default_provider="p"`, "");
    const attrs = liveGraph(choose(editorWith(source), modelChoiceChange(GRAPH, null))).attrs;
    expect(attrs.default_model).toBeUndefined();
    expect(attrs.default_provider).toBeUndefined();
    expect(attrs.goal).toBe("Ship");
  });

  test("a custom ID with no provider leaves no stale provider behind", () => {
    const before = editorWith(workflow(`goal="Ship"`, `model="acme-large", provider="acme"`));
    const after = choose(before, modelChoiceChange(NODE, { model: "vendor/new-model" }));
    const { attrs } = nodeOf(liveGraph(after), "plan");
    expect(attrs.model).toBe("vendor/new-model");
    expect(attrs.provider).toBeUndefined();
  });

  test("choosing what is already written adds no undo step and leaves the source alone", () => {
    const before = editorWith(workflow(`goal="Ship"`, `model="acme-large", provider="acme"`));
    const same = choose(before, modelChoiceChange(NODE, { model: "acme-large", provider: "acme" }));
    expect(same).toBe(before);

    const clearNothing = editorWith(workflow(`goal="Ship"`, ""));
    expect(choose(clearNothing, modelChoiceChange(NODE, null))).toBe(clearNothing);
  });

  test("a node that is gone changes nothing", () => {
    const before = editorWith(workflow(`goal="Ship"`, ""));
    expect(choose(before, modelChoiceChange({ scope: "node", nodeId: "nope" }, { model: "m" }))).toBe(before);
  });

  test("other attributes on the node survive", () => {
    const before = editorWith(workflow(`goal="Ship"`, `reasoning_effort="high"`));
    const { attrs } = nodeOf(liveGraph(choose(before, modelChoiceChange(NODE, { model: "m", provider: "p" }))), "plan");
    expect(attrs).toMatchObject({ reasoning_effort: "high", prompt: "Plan", model: "m", provider: "p" });
  });

  test("an imported model with an unusual ID survives a round trip through the source", () => {
    const before = editorWith(workflow(`goal="Ship"`, `model="vendor/exp-9.1:beta"`));
    expect(nodeOf(liveGraph(before), "plan").attrs.model).toBe("vendor/exp-9.1:beta");

    const after = choose(before, modelChoiceChange(NODE, { model: "other/exp 2", provider: "other" }));
    expect(nodeOf(liveGraph(after), "plan").attrs).toMatchObject({ model: "other/exp 2", provider: "other" });
    expect(graphOf(after.draft.source).nodes.find((node) => node.id === "plan")?.attrs.model).toBe("other/exp 2");
  });
});

describe("custom IDs", () => {
  test("trims and keeps what was typed", () => {
    expect(parseCustomChoice("  vendor/m-1 ", " vendor ")).toEqual({ choice: { model: "vendor/m-1", provider: "vendor" } });
  });

  test("a blank provider is left out, not written empty", () => {
    expect(parseCustomChoice("m", "  ")).toEqual({ choice: { model: "m" } });
  });

  test("refuses what can't be one ID", () => {
    expect(parseCustomChoice("   ", "p")).toEqual({ error: "Enter a model ID." });
    expect(parseCustomChoice("two\nlines", "")).toHaveProperty("error");
    expect(parseCustomChoice("x".repeat(257), "")).toHaveProperty("error");
    expect(parseCustomChoice("m", "two words")).toHaveProperty("error");
  });
});

describe("the catalog query", () => {
  const status = (hasModels: boolean, modelsFailed: boolean, hasProviders: boolean, providersFailed: boolean) =>
    pickerCatalogStatus({ hasModels, modelsFailed, hasProviders, providersFailed });

  test("waits until both have answered", () => {
    expect(status(false, false, false, false).status).toBe("loading");
    expect(status(true, false, false, false).status).toBe("loading");
    expect(status(false, false, true, false).status).toBe("loading");
    expect(status(true, false, true, false)).toEqual({ status: "ready", providersFailed: false });
  });

  test("failing models is an error, and failing provider names only loses the names", () => {
    expect(status(false, true, true, false).status).toBe("error");
    expect(status(true, false, false, true)).toEqual({ status: "ready", providersFailed: true });
  });

  test("a failed refresh keeps what is already on screen", () => {
    expect(status(true, true, true, true)).toEqual({ status: "ready", providersFailed: false });
  });
});

describe("arrow keys", () => {
  test("move one stop and stop at the ends", () => {
    expect(nextNavIndex("ArrowDown", 0, 3)).toBe(1);
    expect(nextNavIndex("ArrowDown", 2, 3)).toBe(2);
    expect(nextNavIndex("ArrowUp", 1, 3)).toBe(0);
    expect(nextNavIndex("ArrowUp", 0, 3)).toBe(0);
    expect(nextNavIndex("Home", 2, 3)).toBe(0);
    expect(nextNavIndex("End", 0, 3)).toBe(2);
  });

  test("other keys aren't moves, and an empty list has nowhere to go", () => {
    expect(nextNavIndex("a", 0, 3)).toBeNull();
    expect(nextNavIndex("ArrowDown", 0, 0)).toBeNull();
  });
});
