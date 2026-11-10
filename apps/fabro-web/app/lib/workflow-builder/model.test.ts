import { describe, expect, test } from "bun:test";

import { EDGE_FIELDS, GRAPH_FIELDS, NODE_FIELDS, NODE_KINDS } from "./catalog";
import { EXAMPLE_ASSETS, EXAMPLE_SOURCE, examplePromptParts } from "./example";
import {
  applyPromptParts,
  canonicalAssets,
  compilePromptParts,
  DraftExportError,
  draftFiles,
  effectiveEdgeAttributes,
  effectiveNodeAttributes,
  exportDraft,
  generatedPromptPath,
  getNodeKind,
  newDraft,
  nodeClasses,
  parseWorkflow,
  promptPartsFromAttribute,
  reconcilePromptParts,
  renameDraftAsset,
  renameDraftNode,
  serializeWorkflow,
  validateWorkflow,
  workflowFileReferences,
  workflowFileUse,
} from "./model";
import type { Diagnostic, PromptPart, WorkflowDraft, WorkflowGraph, WorkflowNode } from "./types";

function parsed(source: string): WorkflowGraph {
  const result = parseWorkflow(source);
  if (!result.graph) throw new Error(`expected source to parse: ${result.diagnostics[0]?.message}`);
  return result.graph;
}

function node(graph: WorkflowGraph, id: string): WorkflowNode {
  const found = graph.nodes.find((candidate) => candidate.id === id);
  if (!found) throw new Error(`no node ${id}`);
  return found;
}

function effective(graph: WorkflowGraph, id: string) {
  return effectiveNodeAttributes(graph, node(graph, id));
}

/** What a workflow means, independent of how it is laid out. */
function meaning(graph: WorkflowGraph) {
  return {
    name: graph.name,
    attrs: graph.attrs,
    nodeOrder: graph.nodes.map((item) => item.id),
    nodes: Object.fromEntries(
      graph.nodes.map((item) => [
        item.id,
        {
          attrs: effectiveNodeAttributes(graph, item),
          classes: nodeClasses(graph, item),
          group: item.groupId ?? null,
        },
      ]),
    ),
    edgesBySource: Object.fromEntries(
      [...new Set(graph.edges.map((edge) => edge.source))].map((source) => [
        source,
        graph.edges
          .filter((edge) => edge.source === source)
          .map((edge) => [edge.target, effectiveEdgeAttributes(graph, edge)]),
      ]),
    ),
    groups: Object.fromEntries(graph.groups.map((group) => [group.id, { parent: group.parentId ?? null, attrs: group.attrs }])),
  };
}

function expectRoundTrip(source: string): WorkflowGraph {
  const original = parsed(source);
  const output = serializeWorkflow(original);
  const reparsed = parsed(output);
  expect(meaning(reparsed)).toEqual(meaning(original));
  expect(serializeWorkflow(parsed(serializeWorkflow(reparsed)))).toBe(serializeWorkflow(reparsed));
  return reparsed;
}

function codes(diagnostics: Diagnostic[], severity?: Diagnostic["severity"]): string[] {
  return diagnostics
    .filter((item) => severity === undefined || item.severity === severity)
    .map((item) => item.code ?? "")
    .sort();
}

function deepFreeze<T>(value: T): T {
  if (typeof value === "object" && value !== null) {
    for (const key of Object.keys(value)) deepFreeze((value as Record<string, unknown>)[key]);
    Object.freeze(value);
  }
  return value;
}

const WELL_FORMED = (body: string, attrs = "") => `digraph G {
  ${attrs}
  start [shape=Mdiamond]
  exit [shape=Msquare]
  ${body}
}
`;

describe("workflowFileUse", () => {
  const plain = "digraph C { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }";
  const holding = (source: string) =>
    `digraph C { start [shape=Mdiamond] k [shape=house stack.child_dot_source=${JSON.stringify(source)}] exit [shape=Msquare] start -> k -> exit }`;
  const naming = 'digraph C { graph [goal="@ticket.md"] start [shape=Mdiamond] exit [shape=Msquare] start -> exit }';
  const broken = "digraph C { start [";

  test.each<[string, string, string]>([
    ["no files", plain, "none"],
    ["an inline child with no files", holding(plain), "none"],
    ["a file named directly", naming, "names_files"],
    ["a file named by an inline child", holding(naming), "names_files"],
    ["a file named two levels down", holding(holding(naming)), "names_files"],
    ["a child workflow two levels down", holding(holding(plain.replace("exit [", 'k [shape=house stack.child_workflow="x.fabro"] exit ['))), "names_files"],
    ["a template tag two levels down", holding(holding(plain.replace("exit [", `w [prompt="{% include 'x.md' %}"] exit [`))), "names_files"],
    ["a dynamic template tag", plain.replace("exit [", 'w [prompt="{% include name %}"] exit ['), "names_files"],
    ["template logic that loads nothing", plain.replace("exit [", 'w [prompt="{% if x %}y{% endif %}"] exit ['), "none"],
    ["a syntax error", broken, "unreadable"],
    ["a syntax error in an inline child", holding(broken), "unreadable"],
    ["a syntax error two levels down", holding(holding(broken)), "unreadable"],
  ])("%s", (_name, source, use) => {
    expect(workflowFileUse(source)).toBe(use);
  });
});

describe("a file only inline source names", () => {
  const CHILD = 'digraph C { graph [goal="@ticket.md"] start [shape=Mdiamond] exit [shape=Msquare] start -> exit }';
  const inlineNode = `child [shape=house stack.child_dot_source=${JSON.stringify(CHILD)}]\n start -> child -> exit`;
  const codes = (graph: WorkflowGraph, assets: Record<string, string>) =>
    validateWorkflow(graph, assets).map((item) => item.code);

  test("is flagged, since Fabro's bundler doesn't read inline source", () => {
    const diagnostics = validateWorkflow(parsed(WELL_FORMED(inlineNode)), { "ticket.md": "T" });
    expect(diagnostics).toMatchObject([{ severity: "error", code: "builder.inline_only_file", nodeId: "child" }]);
    expect(diagnostics[0].message).toContain("ticket.md");
    expect(diagnostics[0].message).toContain("file is preserved");
  });

  test("is fine when the workflow names it too", () => {
    expect(codes(parsed(WELL_FORMED(inlineNode, 'graph [goal="@ticket.md"]')), { "ticket.md": "T" })).toEqual([]);
  });

  test("is fine when a file a child workflow names is the same file", () => {
    const source = WELL_FORMED(`k [shape=house stack.child_workflow="other.fabro"]\n ${inlineNode.replace("start -> child", "start -> k -> child")}`);
    expect(codes(parsed(source), { "ticket.md": "T", "other.fabro": CHILD })).toEqual([]);
  });

  test("is fine when a template tag in a prompt file carries it", () => {
    const source = WELL_FORMED(`work [prompt="@main.md"]\n ${inlineNode.replace("start -> child", "start -> work -> child")}`);
    expect(codes(parsed(source), { "ticket.md": "T", "main.md": '{% include "ticket.md" %}' })).toEqual([]);
  });

  test("a missing file is reported as missing, once", () => {
    expect(codes(parsed(WELL_FORMED(inlineNode)), {})).toEqual(["builder.missing_file"]);
  });

  test("is flagged at depth, inside an attached workflow, from that workflow's folder", () => {
    const check = `digraph K { start [shape=Mdiamond] n [shape=house stack.child_dot_source=${JSON.stringify(CHILD)}] exit [shape=Msquare] start -> n -> exit }`;
    const source = WELL_FORMED('k [shape=house stack.child_workflow="shared/check.fabro"]\n start -> k -> exit');
    expect(codes(parsed(source), { "shared/check.fabro": check, "shared/ticket.md": "T" })).toEqual(["builder.inline_only_file"]);
    const named = WELL_FORMED('k [shape=house stack.child_workflow="shared/check.fabro"]\n start -> k -> exit', 'graph [goal="@shared/ticket.md"]');
    expect(codes(parsed(named), { "shared/check.fabro": check, "shared/ticket.md": "T" })).toEqual([]);
  });

  test("a child workflow only inline source runs is flagged the same way", () => {
    const inline = 'digraph C { start [shape=Mdiamond] k [shape=house stack.child_workflow="grand.fabro"] exit [shape=Msquare] start -> k -> exit }';
    const source = WELL_FORMED(`child [shape=house stack.child_dot_source=${JSON.stringify(inline)}]\n start -> child -> exit`);
    const grand = "digraph G { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }";
    expect(codes(parsed(source), { "grand.fabro": grand })).toEqual(["builder.inline_only_file"]);
  });

  test("what that child workflow names doesn't reach the run either", () => {
    const inline = 'digraph C { start [shape=Mdiamond] k [shape=house stack.child_workflow="grand.fabro"] exit [shape=Msquare] start -> k -> exit }';
    const source = WELL_FORMED(`child [shape=house stack.child_dot_source=${JSON.stringify(inline)}]\n start -> child -> exit`);
    const grand = 'digraph G { graph [goal="@notes.md"] start [shape=Mdiamond] exit [shape=Msquare] start -> exit }';
    expect(codes(parsed(source), { "grand.fabro": grand, "notes.md": "N" })).toEqual(["builder.inline_only_file", "builder.inline_only_file"]);
  });

  test("nothing changes for the sample, which uses no inline source", () => {
    expect(validateWorkflow(parsed(EXAMPLE_SOURCE), { ...EXAMPLE_ASSETS })).toEqual([]);
  });
});

describe("the sample workflow", () => {
  const graph = parsed(EXAMPLE_SOURCE);
  const assets = { ...EXAMPLE_ASSETS };

  test("keeps the five stages, ten nodes, twelve edges and remediation loop", () => {
    expect(graph.name).toBe("FrontendComponentChanges");
    expect(graph.nodes.map((item) => item.id).sort()).toEqual(
      ["analyze", "bug_fix", "exit", "implement", "jira", "pr", "review", "security", "start", "testing"],
    );
    expect(graph.edges).toHaveLength(12);
    expect(graph.groups.map((group) => [group.id, group.attrs.label])).toEqual([
      ["cluster_planning", "Stage 1: Planning"],
      ["cluster_development", "Stage 2: Development"],
      ["cluster_quality_assurance", "Stage 3: Quality Assurance"],
      ["cluster_security_review", "Stage 4: Security & Code Review"],
      ["cluster_delivery", "Stage 5: Delivery"],
    ]);
    expect(graph.attrs).toMatchObject({ default_max_retries: "0", max_node_visits: "4", rankdir: "TB" });
    expect(node(graph, "bug_fix").groupId).toBe("cluster_development");
    expect(node(graph, "start").groupId).toBeUndefined();
    expect(graph.edges.map((edge) => edge.id)).toEqual(expect.arrayContaining(["bug_fix->testing", "implement->testing"]));
  });

  test("routes a check's success onward, and everything else to remediation", () => {
    for (const [check, next] of [
      ["testing", "security"],
      ["security", "review"],
      ["review", "pr"],
    ]) {
      const edges = graph.edges.filter((edge) => edge.source === check);
      expect(edges.map((edge) => [edge.target, edge.attrs.condition])).toEqual([
        [next, "outcome=succeeded"],
        ["bug_fix", undefined],
      ]);
      expect(effective(graph, check).on_failure).toBe("route");
    }
  });

  test("an unhandled failure ends the run, rather than flowing on toward delivery", () => {
    expect(graph.attrs.on_failure).toBe("exit");
    for (const id of ["analyze", "implement", "bug_fix", "pr", "jira"]) {
      expect(effective(graph, id).on_failure).toBeUndefined();
    }
    expect(effective(graph, "bug_fix").output_schema).toBe("routing");
  });

  test("names no model, so it runs on the server's default", () => {
    expect(EXAMPLE_SOURCE).not.toContain("REPLACE_WITH");
    for (const item of graph.nodes) expect(effective(graph, item.id).model).toBeUndefined();
    expect(graph.attrs.default_model).toBeUndefined();
  });

  test("has nothing to complain about, once its files are in the draft", () => {
    expect(validateWorkflow(graph, assets)).toEqual([]);
    expect(parseWorkflow(EXAMPLE_SOURCE).diagnostics).toEqual([]);
  });

  test("without those files, says exactly which are needed", () => {
    const missing = validateWorkflow(graph).filter((item) => item.code === "builder.missing_file");
    expect(missing.map((item) => item.nodeId)).toEqual([undefined, "testing", "security", "review"]);
    expect(missing[1].message).toContain("shared/testing.fabro");
  });

  test("means the same after code -> visual -> code, comments included", () => {
    const reparsed = expectRoundTrip(EXAMPLE_SOURCE);
    expect(reparsed.comments).toEqual(graph.comments);
    expect(graph.comments).toContain("// Validation failures return to development and then repeat validation.");
    const output = serializeWorkflow(graph);
    expect(output.indexOf("// Validation failures")).toBeLessThan(output.indexOf("testing -> bug_fix"));
    expect(output.indexOf("// Validation failures")).toBeGreaterThan(output.indexOf("jira -> exit"));
  });

  test("keeps each child path and condition exactly as written", () => {
    const output = serializeWorkflow(graph);
    for (const child of ["shared/testing.fabro", "shared/security.fabro", "shared/code-review.fabro"]) {
      expect(output).toContain(`stack.child_workflow="${child}"`);
    }
    expect(graph.edges.map((edge) => edge.attrs.condition).filter(Boolean)).toEqual([
      "outcome=succeeded",
      "outcome=succeeded",
      "outcome=succeeded",
    ]);
  });

  test("exports every file, with the source untouched", () => {
    const draft: WorkflowDraft = {
      ...newDraft("Frontend"),
      source: EXAMPLE_SOURCE,
      assets,
      promptParts: examplePromptParts(),
    };
    const exported = exportDraft(draft);
    expect(exported).toMatchObject({ blocked: false, diagnostics: [] });
    expect(exported.files["workflow.fabro"]).toBe(EXAMPLE_SOURCE);
    expect(Object.keys(exported.files).sort()).toEqual([
      "README.md",
      "shared/code-review.fabro",
      "shared/security.fabro",
      "shared/testing.fabro",
      "shared/ticket.md",
      "workflow.builder.json",
      "workflow.fabro",
      "workflow.toml",
    ]);
  });

  test("its prompt blocks match its prompts, with fresh ids each time", () => {
    const parts = examplePromptParts();
    expect(Object.keys(parts).sort()).toEqual(["analyze", "bug_fix", "implement", "jira", "pr"]);
    for (const [id, blocks] of Object.entries(parts)) {
      expect(compilePromptParts(id, blocks, assets).prompt).toBe(effective(graph, id).prompt);
    }
    expect(examplePromptParts().analyze[0].id).not.toBe(parts.analyze[0].id);
  });
});

describe("a workflow whose every route is conditional", () => {
  const source = WELL_FORMED(
    'check [shape=house, stack.child_workflow="c.fabro", on_failure="route"]\n fix [prompt="p"]\n start -> check\n check -> exit [condition="outcome=succeeded"]\n check -> fix [condition="outcome=failed"]\n fix -> check',
  );

  test("is refused, as `fabro validate` refuses it, and an unconditional fallback fixes it", () => {
    const errors = validateWorkflow(parsed(source)).filter((item) => item.severity === "error");
    expect(errors.map((item) => [item.code, item.nodeId])).toEqual([["attractor.all_conditional_edges", "check"]]);
    const fixed = source.replace(' [condition="outcome=failed"]', "");
    expect(validateWorkflow(parsed(fixed)).filter((item) => item.severity === "error")).toEqual([]);
  });
});

describe("attribute defaults", () => {
  test("stay inside their subgraph and nest, inner over outer", () => {
    const graph = expectRoundTrip(`digraph G {
      node [timeout="900s", model="outer"]
      edge [weight=1]
      top [prompt="p"]
      subgraph cluster_a {
        node [model="a", fidelity="full"]
        edge [weight=2]
        inside_a [prompt="p"]
        subgraph cluster_b {
          node [model="b"]
          inside_b [prompt="p", model="mine"]
          also_b [prompt="p"]
          inside_b -> also_b
        }
        inside_a -> inside_b
      }
      after [prompt="p"]
      top -> inside_a
    }`);
    expect(effective(graph, "top")).toEqual({ timeout: "900s", model: "outer", prompt: "p" });
    expect(effective(graph, "inside_a").model).toBe("a");
    expect(effective(graph, "also_b")).toMatchObject({ model: "b", fidelity: "full", timeout: "900s" });
    expect(effective(graph, "inside_b").model).toBe("mine");
    expect(effective(graph, "after").model).toBe("outer");
    expect(effective(graph, "after").fidelity).toBeUndefined();
    const weightOf = (source: string) =>
      effectiveEdgeAttributes(graph, graph.edges.find((edge) => edge.source === source) as WorkflowGraph["edges"][number])
        .weight;
    expect(weightOf("top")).toBe("1");
    expect(weightOf("inside_a")).toBe("2");
    expect(weightOf("inside_b")).toBe("2");
  });

  test("reach only what follows them, and that is written out rather than lost", () => {
    const result = parseWorkflow(`digraph G {
      early
      node [x=1]
      middle
      node [x=2]
      late
    }`);
    const graph = result.graph as WorkflowGraph;
    expect(effective(graph, "early").x).toBeUndefined();
    expect(effective(graph, "middle").x).toBe("1");
    expect(effective(graph, "late").x).toBe("2");
    expect(codes(result.diagnostics)).toContain("builder.default_materialized");
    expect(graph.nodeDefaults.x).toBeUndefined();
    const reparsed = parsed(serializeWorkflow(graph));
    expect(effective(reparsed, "early").x).toBeUndefined();
    expect(effective(reparsed, "middle").x).toBe("1");
    expect(effective(reparsed, "late").x).toBe("2");
  });

  test("are taken where a node is first mentioned, even by an edge", () => {
    const graph = expectRoundTrip(`digraph G {
      first -> second
      node [x=1]
      first [y=2]
      second
      third
    }`);
    expect(effective(graph, "first")).toEqual({ y: "2" });
    expect(effective(graph, "second")).toEqual({});
    expect(effective(graph, "third")).toEqual({ x: "1" });
  });

  test("a group's defaults don't reach a node first mentioned outside it", () => {
    const graph = expectRoundTrip(`digraph G {
      shared [p=1]
      subgraph cluster_x {
        node [q=2]
        shared
        own
      }
    }`);
    expect(node(graph, "shared").groupId).toBe("cluster_x");
    expect(effective(graph, "shared")).toEqual({ p: "1" });
    expect(effective(graph, "own")).toEqual({ q: "2" });
  });

  test("an edge inside a group gives its endpoints that group's class", () => {
    const graph = expectRoundTrip(`digraph G {
      a b
      subgraph cluster_x {
        label="Team X"
        a -> b
      }
      c
    }`);
    expect(nodeClasses(graph, node(graph, "a"))).toEqual(["team-x"]);
    expect(nodeClasses(graph, node(graph, "c"))).toEqual([]);
  });

  test("a nested group uses its own label's class, else its nearest labelled ancestor's", () => {
    const graph = expectRoundTrip(`digraph G {
      subgraph outer {
        label="Loop A"
        a
        subgraph middle {
          b
          subgraph inner {
            label="Inner Loop"
            c [class="x, y z"]
          }
        }
      }
    }`);
    expect(nodeClasses(graph, node(graph, "a"))).toEqual(["loop-a"]);
    expect(nodeClasses(graph, node(graph, "b"))).toEqual(["loop-a"]);
    expect(nodeClasses(graph, node(graph, "c"))).toEqual(["inner-loop", "x", "y", "z"]);
  });

  test("edge defaults follow the same scoping and order, and chains share their attributes", () => {
    const graph = expectRoundTrip(`digraph G {
      a b c d
      a -> b
      edge [weight=5]
      b -> c -> d [label="go"]
    }`);
    const attrs = graph.edges.map((edge) => effectiveEdgeAttributes(graph, edge));
    expect(attrs[0]).toEqual({});
    expect(attrs[1]).toEqual({ weight: "5", label: "go" });
    expect(attrs[2]).toEqual({ weight: "5", label: "go" });
  });

  test("acp.command and acp.config replace each other through every layer", () => {
    const graph = expectRoundTrip(`digraph G {
      node [acp.command="tool"]
      inherits
      overrides [acp.config="{}"]
      redeclared [acp.command="x"]
      redeclared [acp.config="y"]
    }`);
    expect(effective(graph, "inherits")).toEqual({ "acp.command": "tool" });
    expect(effective(graph, "overrides")).toEqual({ "acp.config": "{}" });
    expect(effective(graph, "redeclared")).toEqual({ "acp.config": "y" });
  });

  test("a node declared twice merges, and the later value wins", () => {
    const graph = expectRoundTrip('digraph G { a [x=1, y=2] a [y=3, z=4] }');
    expect(graph.nodes).toHaveLength(1);
    expect(effective(graph, "a")).toEqual({ x: "1", y: "3", z: "4" });
  });
});

describe("source syntax", () => {
  test("accepts the separators, bare values and dotted keys the language allows", () => {
    const graph = expectRoundTrip(`digraph Names {
      goal = "assign"; rankdir=LR
      a [x=1 y=2, z=3; timeout=900s; flag=true, ratio=.5, ver=gpt-5.2-codex, neg=-3, stack.child_workflow="c.fabro"];;
      b; a -> b;
    }`);
    expect(graph.attrs).toEqual({ goal: "assign", rankdir: "LR" });
    expect(node(graph, "a").attrs).toEqual({
      x: "1",
      y: "2",
      z: "3",
      timeout: "900s",
      flag: "true",
      ratio: ".5",
      ver: "gpt-5.2-codex",
      neg: "-3",
      "stack.child_workflow": "c.fabro",
    });
  });

  test("resolves escapes and writes back the same value", () => {
    const graph = expectRoundTrip(String.raw`digraph G {
      a [
        quote="say \"hi\"",
        back="C:\\tmp",
        unknown_escape="\d+ stays",
        newline="one\ntwo",
        tab="x\ty",
        joined="first \
second"
      ]
    }`);
    expect(node(graph, "a").attrs).toEqual({
      quote: 'say "hi"',
      back: "C:\\tmp",
      unknown_escape: "\\d+ stays",
      newline: "one\ntwo",
      tab: "x\ty",
      joined: "first second",
    });
  });

  test("keeps a literal multi-line string intact", () => {
    const script = "set -e\n  echo one\n\techo two\\\n";
    const source = `digraph G { a [script="${script.replace(/\\/g, "\\\\")}"] }`;
    expect(node(expectRoundTrip(source), "a").attrs.script).toBe(script);
  });

  test("preserves the bare/quoted distinction, because Fabro's typed reader sees it", () => {
    const output = serializeWorkflow(parsed('digraph G { a [i=5, s="5", e=1e5, q="1e5", b=true, t="true", goal_gate="true"] }'));
    expect(output).toContain("i=5");
    expect(output).toContain('s="5"');
    expect(output).toContain("e=1e5");
    expect(output).toContain('q="1e5"');
    expect(output).toContain("b=true");
    expect(output).toContain('t="true"');
    expect(output).toContain('goal_gate="true"');
  });

  test("keeps arbitrary attributes everywhere", () => {
    const graph = expectRoundTrip(`digraph G {
      graph [frobnicate="g", x.tool=1]
      node [custom.default="n"]
      edge [custom.edge="e"]
      "weird key"="graph-level"
      subgraph cluster_a { group_attr="kept"; "other key"=3; color=red; a [nonsense=1, "spaced key"="v"] }
      a -> a [wiggle="w"]
    }`);
    expect(graph.attrs).toEqual({ frobnicate: "g", "x.tool": "1", "weird key": "graph-level" });
    expect(graph.groups[0].attrs).toEqual({ group_attr: "kept", "other key": "3", color: "red" });
    expect(node(graph, "a").attrs).toEqual({ nonsense: "1", "spaced key": "v" });
    expect(effective(graph, "a")["custom.default"]).toBe("n");
    expect(effectiveEdgeAttributes(graph, graph.edges[0])).toEqual({ "custom.edge": "e", wiggle: "w" });
  });

  test("survives names that collide with JavaScript object internals", () => {
    const graph = expectRoundTrip(
      'digraph __proto__ { node [__proto__=1] constructor [toString=2] __proto__ [hasOwnProperty=3] subgraph __proto__ { inner } constructor -> __proto__ [__proto__=4] }',
    );
    expect(graph.name).toBe("__proto__");
    expect(Object.keys(effective(graph, "inner"))).toEqual(["__proto__"]);
    expect(Object.getPrototypeOf(effective(graph, "inner"))).toBe(Object.prototype);
    expect(({} as Record<string, unknown>).toString).toBe(Object.prototype.toString);
  });

  test("quotes ids that are keywords or not plain words, so they read back as ids", () => {
    const graph = expectRoundTrip('digraph "My Flow" { "node" [x=1] "has space" -> "1a" "subgraph" run-tests }');
    expect(graph.name).toBe("My Flow");
    expect(graph.nodes.map((item) => item.id)).toEqual(["node", "subgraph", "run-tests"]);
    expect(serializeWorkflow(graph)).toContain('digraph "My Flow" {');
  });

  test("treats a digraph with no name as Petri does, with a warning", () => {
    const graph = expectRoundTrip("digraph { a }");
    expect(graph.name).toBe("");
    expect(codes(validateWorkflow(graph))).toContain("builder.missing_graph_name");
  });

  test("keeps several edges between the same nodes, with distinct ids and in order", () => {
    const graph = expectRoundTrip('digraph G { a b a -> b [label="1"] a -> b [label="2"] b -> a a -> b [label="3"] }');
    expect(graph.edges.map((edge) => edge.id)).toEqual(["a->b", "a->b#2", "b->a", "a->b#3"]);
    expect(graph.edges.filter((edge) => edge.source === "a").map((edge) => edge.attrs.label)).toEqual(["1", "2", "3"]);
  });

  test("keeps each node's edge order when group edges and root edges interleave", () => {
    const graph = expectRoundTrip(`digraph G {
      a x y z u
      a -> x [label="first"]
      subgraph cluster_g { edge [w=1] a -> y [label="second"] }
      a -> z [label="third"]
      subgraph cluster_g { a -> u [label="fourth"] }
    }`);
    expect(graph.edges.filter((edge) => edge.source === "a").map((edge) => edge.attrs.label)).toEqual([
      "first",
      "second",
      "third",
      "fourth",
    ]);
    expect(graph.groups.map((group) => group.id)).toEqual(["cluster_g"]);
    expect(graph.edges.find((edge) => edge.target === "y")?.groupId).toBe("cluster_g");
  });

  test("keeps nodes in the order they were first mentioned, because Petri's preambles follow it", () => {
    const graph = expectRoundTrip(`digraph G {
      start -> first -> outside -> second -> exit
      subgraph cluster_g { label="G"; node [timeout="1s"]; second [x=1]; first [x=2] }
      start [shape=Mdiamond]
      exit [shape=Msquare]
      outside [x=3]
    }`);
    expect(graph.nodes.map((item) => item.id)).toEqual(["start", "first", "outside", "second", "exit"]);
    expect(graph.groups.map((group) => group.id)).toEqual(["cluster_g"]);
    expect(node(graph, "first").groupId).toBe("cluster_g");
    expect(node(graph, "second").groupId).toBe("cluster_g");
  });

  test("a group interleaved with other nodes reopens with its label and defaults, and means the same", () => {
    const graph = expectRoundTrip(`digraph G {
      subgraph { label="Unnamed"; node [t=1]; a }
      x
      subgraph cluster_g { label="G"; node [t=2]; b; subgraph cluster_inner { node [u=3]; c } }
      y
      subgraph cluster_g { label="G"; node [t=2]; d; subgraph cluster_inner { node [u=3]; e } }
    }`);
    expect(graph.nodes.map((item) => item.id)).toEqual(["a", "x", "b", "c", "y", "d", "e"]);
    expect(effective(graph, "d").t).toBe("2");
    expect(effective(graph, "e")).toEqual({ t: "2", u: "3" });
    expect(nodeClasses(graph, node(graph, "e"))).toEqual(["g"]);
  });

  test("a node added on the canvas joins its group rather than splitting it", () => {
    const graph = parsed('digraph G { subgraph cluster_a { label="A"; a } subgraph cluster_b { label="B"; b } z }');
    const added: WorkflowGraph = { ...graph, nodes: [...graph.nodes, { id: "new_one", attrs: {}, groupId: "cluster_a" }] };
    const output = serializeWorkflow(added);
    expect(output.match(/subgraph cluster_a/g)).toHaveLength(1);
    expect(parsed(output).nodes.map((item) => item.id)).toEqual(["a", "new_one", "b", "z"]);
    const fromScratch: WorkflowGraph = {
      name: "Built",
      attrs: {},
      nodeDefaults: {},
      edgeDefaults: {},
      nodes: [
        { id: "a", attrs: {}, groupId: "g" },
        { id: "b", attrs: {} },
        { id: "c", attrs: {}, groupId: "g" },
      ],
      edges: [],
      groups: [{ id: "g", attrs: { label: "G" }, nodeDefaults: {}, edgeDefaults: {} }],
    };
    expect(serializeWorkflow(fromScratch).match(/subgraph g/g)).toHaveLength(1);
  });

  test("keeps empty, nested, unnamed and cross-referencing groups", () => {
    const graph = expectRoundTrip(`digraph G {
      subgraph cluster_empty { label="Nothing yet" }
      subgraph { label="Unnamed"; a }
      subgraph cluster_outer { subgraph cluster_inner { b } }
      a -> b
    }`);
    expect(graph.groups.find((group) => group.id === "cluster_inner")?.parentId).toBe("cluster_outer");
    expect(graph.groups.find((group) => group.id === "cluster_empty")?.attrs).toEqual({ label: "Nothing yet" });
    expect(graph.groups.some((group) => group.anonymous)).toBe(true);
    expect(serializeWorkflow(graph)).toContain("subgraph {");
  });

  test("a subgraph reopened with a different label stays a separate group, with a warning", () => {
    const result = parseWorkflow('digraph G { subgraph c { label="A"; a } subgraph c { label="B"; b } }');
    expect(codes(result.diagnostics)).toContain("builder.reopened_group");
    const graph = result.graph as WorkflowGraph;
    expect(nodeClasses(graph, node(graph, "a"))).toEqual(["a"]);
    expect(nodeClasses(graph, node(graph, "b"))).toEqual(["b"]);
    expect(meaning(parsed(serializeWorkflow(graph)))).toEqual(meaning(graph));
  });
});

describe("comments", () => {
  const source = `// file header
/* block
   header */
digraph G { // beside the brace
  // about the settings
  graph [goal="g"] // beside the settings
  // about n
  n [x=1] // beside n
  subgraph cluster_c {
    // about the label
    label="C"
    // about m
    m /* beside m */
    // closing c
  }
  // about the edge
  n -> m // beside the edge
  // closing the graph
}
// file footer
`;

  test("all come back, next to what they describe", () => {
    const graph = parsed(source);
    const output = serializeWorkflow(graph);
    for (const comment of graph.comments as string[]) expect(output).toContain(comment);
    expect(graph.comments).toHaveLength(15);
    expect(output.startsWith("// file header\n/* block\n   header */\ndigraph G {")).toBe(true);
    expect(output.trimEnd().endsWith("}\n// file footer")).toBe(true);
    expect(output).toContain("n [x=1] // beside n");
    expect(output).toContain("m /* beside m */");
    expect(output).toContain("n -> m // beside the edge");
    expect(output.indexOf("// about the edge")).toBeLessThan(output.indexOf("n -> m"));
    expect(output.indexOf("// closing c")).toBeLessThan(output.indexOf("  }\n"));
    expect(serializeWorkflow(parsed(output))).toBe(output);
  });

  test("move to the top rather than vanish when what they describe is deleted", () => {
    const graph = parsed(source);
    const edited: WorkflowGraph = {
      ...graph,
      nodes: graph.nodes.filter((item) => item.id !== "n"),
      edges: [],
    };
    const output = serializeWorkflow(edited);
    expect(output).toContain("// about n");
    expect(output).toContain("// beside the edge");
    expect(parseWorkflow(output).graph).not.toBeNull();
  });

  test("written as plain text in the graph still serialize as comments", () => {
    const graph: WorkflowGraph = { ...parsed("digraph G { a }"), comments: ["a note", "two\nlines", "/* not */ closed */"] };
    const output = serializeWorkflow(graph);
    expect(output).toContain("// a note");
    expect(output).toContain("// two\n  // lines");
    expect(parsed(output).nodes.map((item) => item.id)).toEqual(["a"]);
  });

  test("inside strings are text, not comments", () => {
    const graph = parsed('digraph G { a [p="// keep /* me */ # too"] }');
    expect(node(graph, "a").attrs.p).toBe("// keep /* me */ # too");
    expect(graph.comments).toEqual([]);
  });
});

describe("source Fabro refuses", () => {
  const cases: [string, string, string, number, number][] = [
    ["strict graphs", "strict digraph G {}", "unsupported.dot.strict", 1, 1],
    ["undirected graphs", "graph G { a }", "unsupported.dot.undirected", 1, 1],
    ["undirected edges", "digraph G {\n  a -- b\n}", "unsupported.dot.undirected", 2, 5],
    ["ports", "digraph G { a:p -> b }", "unsupported.dot.port", 1, 14],
    ["HTML labels", 'digraph G {\n  a [label=<b>hi</b>]\n}', "unsupported.dot.html_string", 2, 12],
    ["bare braces", "digraph G { { a } }", "unsupported.dot.anonymous_subgraph", 1, 13],
    ["subgraph endpoints", "digraph G { a -> { b } }", "unsupported.dot.subgraph_edge", 1, 18],
    ["a dangling arrow", "digraph G { a -> }", "dot.syntax", 1, 18],
    ["a missing closing brace", "digraph G { a", "dot.syntax", 1, 13],
    ["an unterminated string", 'digraph G {\n  a [x="oops]\n}', "dot.syntax", 2, 8],
    ["an unterminated comment", "digraph G { /* oops }", "dot.syntax", 1, 13],
    ["text after the graph", "digraph A {} digraph B {}", "dot.syntax", 1, 14],
    ["commas between statements", "digraph G { a, b }", "dot.syntax", 1, 14],
    ["a keyword as an assignment", "digraph G { node=5 }", "dot.syntax", 1, 17],
    ["no digraph at all", "just words", "dot.syntax", 1, 1],
  ];

  for (const [name, source, code, line, column] of cases) {
    test(`${name}: no graph, and the problem's position`, () => {
      const result = parseWorkflow(source);
      expect(result.graph).toBeNull();
      expect(result.diagnostics).toHaveLength(1);
      expect(result.diagnostics[0]).toMatchObject({ severity: "error", code, line, column });
    });
  }

  test("an otherwise fine graph with one bad statement yields nothing partial", () => {
    const result = parseWorkflow(`digraph G {
      start [shape=Mdiamond]
      exit [shape=Msquare]
      work [prompt="fine"]
      start -> work -> exit
      broken [label=<nope>]
    }`);
    expect(result.graph).toBeNull();
    expect(result.diagnostics[0].line).toBe(6);
  });

  test("absurd nesting is a diagnostic, not a crash", () => {
    const source = `digraph G { ${"subgraph s { ".repeat(5000)}${"} ".repeat(5000)}}`;
    const result = parseWorkflow(source);
    expect(result.graph).toBeNull();
    expect(result.diagnostics[0].severity).toBe("error");
  });
});

describe("node kinds", () => {
  const kindOf = (attrs: Record<string, string>, id = "n") => getNodeKind({ id, attrs }).type;

  test("the catalog covers all 11 documented shapes, each resolving to its own handler", () => {
    expect(NODE_KINDS.map((kind) => [kind.shape, kind.type])).toEqual([
      ["Mdiamond", "start"],
      ["Msquare", "exit"],
      ["box", "agent"],
      ["tab", "prompt"],
      ["parallelogram", "command"],
      ["hexagon", "human"],
      ["diamond", "conditional"],
      ["component", "parallel"],
      ["tripleoctagon", "parallel.fan_in"],
      ["insulator", "wait"],
      ["house", "stack.manager_loop"],
    ]);
    for (const kind of NODE_KINDS) {
      expect(kindOf({ shape: kind.shape })).toBe(kind.type);
      expect(kindOf(kind.defaults)).toBe(kind.type);
    }
    expect(NODE_KINDS.filter((kind) => kind.supportsPrompt).map((kind) => kind.type)).toEqual([
      "agent",
      "prompt",
      "parallel.fan_in",
    ]);
  });

  test("an explicit type beats the shape", () => {
    expect(kindOf({ shape: "box", type: "command" })).toBe("command");
    expect(kindOf({ shape: "parallelogram", type: "agent" })).toBe("agent");
    expect(kindOf({ shape: "hexagon", type: "wait" })).toBe("wait");
  });

  test("an explicit shape beats script inference, which beats the box default", () => {
    expect(kindOf({ script: "make" })).toBe("command");
    expect(kindOf({ shape: "box", script: "make" })).toBe("agent");
    expect(kindOf({ type: "agent", script: "make" })).toBe("agent");
    expect(kindOf({ prompt: "p" })).toBe("agent");
    expect(kindOf({})).toBe("agent");
  });

  test("start and exit are recognised by id, shape or type, ahead of everything else", () => {
    expect(kindOf({}, "start")).toBe("start");
    expect(kindOf({}, "End")).toBe("exit");
    expect(kindOf({ type: "start" })).toBe("start");
    expect(kindOf({ shape: "Msquare", type: "agent" })).toBe("exit");
  });

  test("the legacy `tool` type is a command; unknown types and shapes read as agents", () => {
    expect(kindOf({ type: "tool" })).toBe("command");
    expect(kindOf({ type: "nope" })).toBe("agent");
    expect(kindOf({ shape: "circle" })).toBe("agent");
  });

  test("with the graph, defaults in scope decide; without it, only the node's own attributes", () => {
    const graph = parsed('digraph G { node [shape=tab] a [prompt="p"] }');
    expect(getNodeKind(node(graph, "a"), graph).type).toBe("prompt");
    expect(getNodeKind(node(graph, "a")).type).toBe("agent");
  });

  test("the catalog offers the settings the engine reads", () => {
    const fieldsOf = (type: string) => NODE_KINDS.find((kind) => kind.type === type)?.fields.map((field) => field.key);
    expect(fieldsOf("agent")).toEqual(expect.arrayContaining(["prompt", "model", "fidelity", "backend", "acp.command", "output_schema"]));
    expect(fieldsOf("command")).toEqual(expect.arrayContaining(["script", "language", "stdin_source"]));
    expect(fieldsOf("wait")).toEqual(["duration"]);
    expect(fieldsOf("parallel")).toEqual(["max_parallel", "for_each"]);
    expect(fieldsOf("human")).toEqual(expect.arrayContaining(["question_type", "human.default_choice", "review_target"]));
    expect(fieldsOf("stack.manager_loop")).toEqual(expect.arrayContaining(["stack.child_workflow", "manager.max_cycles"]));
    expect(GRAPH_FIELDS.map((field) => field.key)).toEqual(expect.arrayContaining(["goal", "model_stylesheet", "on_failure", "max_node_visits"]));
    expect(NODE_FIELDS.map((field) => field.key)).toEqual(expect.arrayContaining(["label", "shape", "type", "retry_target", "goal_gate"]));
    expect(EDGE_FIELDS.map((field) => field.key)).toEqual(expect.arrayContaining(["label", "condition", "weight", "loop_restart", "freeform"]));
  });
});

describe("structural validation", () => {
  const errorsOf = (source: string, assets?: Record<string, string>) =>
    codes(validateWorkflow(parsed(source), assets), "error");
  const warningsOf = (source: string, assets?: Record<string, string>) =>
    codes(validateWorkflow(parsed(source), assets), "warning");

  test("a well-formed workflow is clean", () => {
    expect(validateWorkflow(parsed(WELL_FORMED('work [prompt="p"]\n start -> work -> exit')))).toEqual([]);
  });

  test("requires exactly one start and one exit", () => {
    expect(errorsOf('digraph G { exit [shape=Msquare] a [prompt="p"] a -> exit }')).toContain("attractor.no_start");
    expect(errorsOf('digraph G { start [shape=Mdiamond] a [prompt="p"] start -> a }')).toContain("attractor.no_exit");
    expect(errorsOf(WELL_FORMED('other [shape=Mdiamond]\n start -> exit\n other -> exit'))).toContain("attractor.multiple_starts");
    expect(errorsOf(WELL_FORMED("end\n start -> exit\n start -> end"))).toContain("attractor.multiple_exits");
  });

  test("an edge naming an undeclared node is flagged, and the node is not invented", () => {
    const graph = parsed(WELL_FORMED("start -> ghost -> exit"));
    expect(graph.nodes.map((item) => item.id)).toEqual(["start", "exit"]);
    const diagnostics = validateWorkflow(graph).filter((item) => item.code === "attractor.undeclared_node");
    expect(diagnostics.map((item) => item.edgeId)).toEqual(["start->ghost", "ghost->exit"]);
    expect(diagnostics[0].message).toContain("ghost");
    expect(serializeWorkflow(graph)).not.toMatch(/^\s*ghost\s*(\[.*)?$/m);
  });

  test("the start takes no incoming edges and the exit no outgoing ones", () => {
    const errors = errorsOf(WELL_FORMED('work [prompt="p"]\n start -> work -> exit\n work -> start\n exit -> work'));
    expect(errors).toContain("attractor.start_has_incoming");
    expect(errors).toContain("attractor.exit_has_outgoing");
  });

  test("every node must be reachable from the start, and so must the exit", () => {
    expect(errorsOf(WELL_FORMED('a [prompt="p"]\n island [prompt="q"]\n start -> a -> exit'))).toEqual(["attractor.unreachable_node"]);
    expect(errorsOf(WELL_FORMED('a [prompt="p"]\n start -> a'))).toEqual(
      expect.arrayContaining(["attractor.unreachable_node", "attractor.exit_unreachable"]),
    );
  });

  test("a node with no way forward is called out", () => {
    expect(warningsOf(WELL_FORMED('a [prompt="p"]\n b [prompt="q"]\n start -> a\n a -> b\n a -> exit'))).toContain("builder.dead_end");
  });

  test("ids in a graph built by hand are checked: duplicates, invalid names, reserved names", () => {
    const base = parsed(WELL_FORMED('a [prompt="p"]\n start -> a -> exit'));
    const withNodes = (...ids: string[]): WorkflowGraph => ({
      ...base,
      nodes: [...base.nodes, ...ids.map((id) => ({ id, attrs: { prompt: "p" } }))],
    });
    expect(codes(validateWorkflow(withNodes("a")), "error")).toContain("builder.duplicate_node_id");
    expect(codes(validateWorkflow(withNodes("has space")), "error")).toContain("builder.invalid_node_id");
    expect(codes(validateWorkflow(withNodes("1abc")), "error")).toContain("builder.invalid_node_id");
    expect(codes(validateWorkflow(withNodes("")), "error")).toContain("builder.invalid_node_id");
    expect(codes(validateWorkflow(withNodes("goal_check")), "error")).toContain("attractor.reserved_node_id");
    expect(codes(validateWorkflow(withNodes("strict")), "warning")).toContain("attractor.reserved_keyword_node_id");
    expect(
      codes(validateWorkflow({ ...base, edges: [...base.edges, { ...base.edges[0] }] }), "error"),
    ).toContain("builder.duplicate_edge_id");
    expect(
      codes(validateWorkflow({ ...base, nodes: [...base.nodes, { id: "b", attrs: {}, groupId: "nope" }] }), "error"),
    ).toContain("builder.unknown_group");
    const cyclic: WorkflowGraph = {
      ...base,
      groups: [
        { id: "x", parentId: "y", attrs: {}, nodeDefaults: {}, edgeDefaults: {} },
        { id: "y", parentId: "x", attrs: {}, nodeDefaults: {}, edgeDefaults: {} },
      ],
    };
    expect(codes(validateWorkflow(cyclic), "error")).toContain("builder.group_cycle");
    expect(parseWorkflow(serializeWorkflow(cyclic)).graph).not.toBeNull();
    const orphanGroup: WorkflowGraph = {
      ...base,
      groups: [{ id: "x", parentId: "gone", attrs: {}, nodeDefaults: {}, edgeDefaults: {} }],
    };
    expect(codes(validateWorkflow(orphanGroup), "error")).toEqual(["builder.unknown_group"]);
    const twinGroups: WorkflowGraph = {
      ...base,
      groups: [
        { id: "x", attrs: { label: "One" }, nodeDefaults: {}, edgeDefaults: {} },
        { id: "x", attrs: { color: "red" }, nodeDefaults: { timeout: "1s" }, edgeDefaults: {} },
      ],
    };
    expect(codes(validateWorkflow(twinGroups), "error")).toEqual(["builder.duplicate_group_id"]);
    const written = serializeWorkflow(twinGroups);
    expect(written).toContain('label="One"');
    expect(written).toContain('color="red"');
    expect(written).toContain('timeout="1s"');
    expect(parseWorkflow(written).graph).not.toBeNull();
  });

  test("handler-specific requirements", () => {
    const check = (body: string) => errorsOf(WELL_FORMED(`${body}\n start -> n -> exit`));
    expect(check("n [shape=parallelogram]")).toEqual(["attractor.command_requires_script"]);
    expect(check('n [shape=parallelogram, script="  "]')).toEqual(["attractor.command_requires_script"]);
    expect(check('n [script="x", language="ruby"]')).toEqual(["attractor.bad_language"]);
    expect(check("n [shape=insulator]")).toEqual(["attractor.wait_requires_duration"]);
    expect(check('n [shape=insulator, duration="soon"]')).toEqual(["attractor.bad_duration"]);
    expect(check("n [shape=house]")).toEqual(["attractor.manager_loop_without_child"]);
    expect(check('n [shape=hexagon, question_type="essay"]')).toEqual(["attractor.bad_question_type"]);
    expect(check('n [shape=hexagon, human.default_choice="nope"]')).toEqual(["attractor.bad_default_choice"]);
    expect(check('n [prompt="p", timeout=900]')).toEqual(["unsupported.legacy_dialect"]);
    expect(check('n [prompt="p", retry_policy="often"]')).toEqual(["attractor.bad_retry_policy"]);
    expect(check('n [prompt="p", on_failure="panic"]')).toEqual(["attractor.bad_on_failure"]);
    expect(check('n [prompt="p", fidelity="most"]')).toEqual(["attractor.bad_fidelity"]);
    expect(check('n [prompt="p", backend="acp"]')).toEqual(["attractor.acp_requires_command"]);
    expect(check('n [prompt="p", acp.command="tool"]')).toEqual(["attractor.backend_options"]);
    expect(check('n [shape=tab, prompt="p", backend="acp"]')).toEqual(["attractor.prompt_backend"]);
    expect(check('n [type="nope"]')).toEqual(["attractor.unknown_type"]);
    expect(check('n [prompt="p", for_each="context.x"]')).toEqual(["attractor.for_each.not_parallel"]);
  });

  test("a node can't set both script and prompt, and a kind that ignores an attribute says so", () => {
    expect(errorsOf(WELL_FORMED('n [script="x", prompt="y"]\n start -> n -> exit'))).toEqual(["attractor.script_prompt_conflict"]);
    expect(warningsOf(WELL_FORMED('n [shape=box, script="x", label="L"]\n start -> n -> exit'))).toContain("attractor.inert_attribute");
  });

  test("explicit type decides which requirements apply", () => {
    expect(errorsOf(WELL_FORMED('n [shape=box, type="command"]\n start -> n -> exit'))).toEqual(["attractor.command_requires_script"]);
    expect(errorsOf(WELL_FORMED('n [shape=box, type="command", script="x"]\n start -> n -> exit'))).toEqual([]);
    expect(warningsOf(WELL_FORMED('n [shape=parallelogram, type="agent", label="L"]\n start -> n -> exit'))).toContain("attractor.prompt_missing");
  });

  test("an unknown attribute is reported honestly and still kept", () => {
    const source = WELL_FORMED('n [prompt="p", max_retrys=3, x.tool="fine", color=red]\n start -> n -> exit');
    const diagnostics = validateWorkflow(parsed(source)).filter((item) => item.code === "attractor.unknown_attribute");
    expect(diagnostics).toHaveLength(1);
    expect(diagnostics[0].message).toContain("max_retries");
    expect(serializeWorkflow(parsed(source))).toContain("max_retrys=3");
  });

  test("placeholder models are warnings with a next step, wherever they are set", () => {
    const source = WELL_FORMED(
      'a [prompt="p", model="REPLACE_WITH_REAL_MODEL"]\n b [prompt="p", provider="<provider>"]\n c [prompt="p", model="claude-opus-5-5"]\n start -> a -> b -> c -> exit',
      'graph [default_model="TODO", model_stylesheet="* { model: CHANGE_ME; }"]',
    );
    const placeholders = validateWorkflow(parsed(source)).filter((item) => item.code === "builder.placeholder_model");
    expect(placeholders.every((item) => item.severity === "warning")).toBe(true);
    expect(placeholders.map((item) => item.nodeId).filter(Boolean)).toEqual(["a", "b"]);
    expect(placeholders).toHaveLength(4);
    expect(placeholders[0].message).toContain("fabro model");
  });

  describe("retry loops", () => {
    const LOOP = (extra = "") =>
      WELL_FORMED(`plan [prompt="p", max_visits=3 ${extra}]
        check [script="make test"]
        fix [prompt="fix it"]
        start -> plan -> check
        check -> exit [condition="outcome=succeeded"]
        check -> fix
        fix -> check`);

    test("a loop is a valid shape", () => {
      expect(validateWorkflow(parsed(LOOP()))).toEqual([]);
    });

    test("a retry target must name a node, as a warning, as Petri has it", () => {
      expect(validateWorkflow(parsed(LOOP(', retry_target="check", fallback_retry_target="plan"')))).toEqual([]);
      const diagnostics = validateWorkflow(parsed(LOOP(', retry_target="nowhere"')));
      expect(diagnostics.map((item) => [item.severity, item.code, item.nodeId])).toEqual([
        ["warning", "attractor.retry_target_not_found", "plan"],
      ]);
    });

    test("graph and defaulted retry targets count too", () => {
      expect(codes(validateWorkflow(parsed(WELL_FORMED("start -> exit", 'graph [retry_target="nowhere"]'))))).toEqual([
        "attractor.retry_target_not_found",
      ]);
      const inherited = WELL_FORMED('subgraph c { node [retry_target="nowhere"]\n a [prompt="p"] }\n start -> a -> exit');
      expect(codes(validateWorkflow(parsed(inherited)))).toEqual(["attractor.retry_target_not_found"]);
    });

    test("a target inside an import can't be checked here, so it isn't flagged", () => {
      const source = WELL_FORMED('lib [import="lib.fabro"]\n a [prompt="p", retry_target="lib.step"]\n start -> lib -> a -> exit');
      expect(codes(validateWorkflow(parsed(source)))).not.toContain("attractor.retry_target_not_found");
    });

    test("visit caps the engine would refuse", () => {
      expect(errorsOf(LOOP().replace("max_visits=3", "max_visits=501"))).toEqual(["attractor.max_visits_too_large"]);
      expect(errorsOf(LOOP().replace("max_visits=3", "max_visits=500"))).toEqual([]);
      expect(errorsOf(LOOP().replace("max_visits=3", 'max_visits="often"'))).toEqual(["attractor.bad_attribute_type"]);
    });

    test("a goal gate needs a target that exists", () => {
      expect(warningsOf(WELL_FORMED('g [prompt="p", goal_gate=true]\n start -> g -> exit'))).toEqual(["attractor.goal_gate_without_target"]);
      expect(warningsOf(WELL_FORMED('g [prompt="p", goal_gate=true, retry_target="g"]\n start -> g -> exit'))).toEqual([]);
    });
  });

  describe("conditions and routing", () => {
    const WITH_CONDITION = (condition: string) =>
      WELL_FORMED(`a [prompt="p"]\n start -> a\n a -> exit [condition="${condition.replace(/"/g, '\\"')}"]\n a -> exit`);

    test("every documented form is accepted", () => {
      for (const condition of [
        "outcome=succeeded",
        "outcome!=failed",
        "outcome=succeeded && context.tests_passed=true",
        "outcome=succeeded || outcome=partially_succeeded",
        "!outcome=succeeded",
        "context.score > 80",
        "context.count <= 10",
        "context.log contains error",
        "context.version matches ^v2\\.",
        "my_flag",
        'context.text contains "two words"',
        "preferred_label=fix",
      ]) {
        expect(codes(validateWorkflow(parsed(WITH_CONDITION(condition))))).toEqual([]);
      }
    });

    test("expressions are preserved verbatim, even oddly spaced or invalid ones", () => {
      for (const condition of ["outcome=succeeded   &&  x", "a &&", 'x contains "y\\\\z"']) {
        const graph = parsed(WITH_CONDITION(condition));
        const value = graph.edges[1].attrs.condition;
        expect(parsed(serializeWorkflow(graph)).edges[1].attrs.condition).toBe(value);
      }
      expect(parsed(WITH_CONDITION("outcome=succeeded   &&  x")).edges[1].attrs.condition).toBe("outcome=succeeded   &&  x");
    });

    test("broken expressions are errors with the edge named", () => {
      const diagnostics = validateWorkflow(parsed(WITH_CONDITION("a &&")));
      expect(diagnostics).toHaveLength(1);
      expect(diagnostics[0]).toMatchObject({ severity: "error", code: "attractor.condition.syntax", edgeId: "a->exit" });
      expect(codes(validateWorkflow(parsed(WITH_CONDITION("a & b"))))).toEqual(["attractor.condition.syntax"]);
      expect(codes(validateWorkflow(parsed(WITH_CONDITION("outcome=done"))))).toEqual(["unsupported.outcome_value"]);
      expect(codes(validateWorkflow(parsed(WITH_CONDITION("outcome > 3"))))).toEqual(["attractor.condition.outcome_op"]);
      expect(codes(validateWorkflow(parsed(WITH_CONDITION("outcome=success"))))).toEqual(["deprecated.outcome_alias"]);
      expect(codes(validateWorkflow(parsed(WITH_CONDITION("x > big"))))).toEqual(["attractor.condition.non_numeric"]);
    });

    test("a node needs a fallback when all its edges are conditional, except parallel and exit", () => {
      const allConditional = WELL_FORMED('a [prompt="p"]\n start -> a\n a -> exit [condition="outcome=succeeded"]');
      expect(codes(validateWorkflow(parsed(allConditional)))).toEqual(["attractor.all_conditional_edges"]);
      const blank = WELL_FORMED('a [prompt="p"]\n start -> a\n a -> exit [condition="  "]');
      expect(codes(validateWorkflow(parsed(blank)))).toEqual([]);
    });

    test("random selection can't mix with conditions", () => {
      const source = WELL_FORMED('a [prompt="p", selection="random"]\n start -> a\n a -> exit [condition="x"]\n a -> exit');
      expect(errorsOf(source)).toEqual(["attractor.random_with_conditions"]);
    });

    test("a human gate offers its edges, and defaults match by target or accelerator", () => {
      const gate = (attrs: string) => WELL_FORMED(`h [shape=hexagon ${attrs}]\n start -> h\n h -> exit [label="[Y] Yes"]`);
      expect(errorsOf(gate(', human.default_choice="exit"'))).toEqual([]);
      expect(errorsOf(gate(', human.default_choice="Y"'))).toEqual([]);
      expect(errorsOf(gate(', human.default_choice="N"'))).toEqual(["attractor.bad_default_choice"]);
      expect(errorsOf(WELL_FORMED("h [shape=hexagon]\n start -> h"))).toEqual(
        expect.arrayContaining(["attractor.human_without_edges"]),
      );
      const twoFreeform = WELL_FORMED('h [shape=hexagon]\n a [prompt="p"]\n start -> h\n h -> a [freeform=true]\n h -> exit [freeform=true]\n a -> exit');
      expect(errorsOf(twoFreeform)).toEqual(["attractor.freeform_edge_count"]);
    });

    test("parallel forks: unconditional branches that converge", () => {
      const FORK = (body: string) => WELL_FORMED(`f [shape=component]\n a [prompt="p"]\n b [prompt="q"]\n m [shape=tripleoctagon]\n start -> f\n ${body}\n m -> exit`);
      expect(errorsOf(FORK("f -> a\n f -> b\n a -> m\n b -> m"))).toEqual([]);
      expect(errorsOf(FORK('f -> a [condition="x"]\n f -> b\n a -> m\n b -> m'))).toEqual(["attractor.parallel.conditional_branch"]);
      expect(errorsOf(FORK("f -> a\n f -> b\n a -> m\n b -> exit"))).toEqual(["attractor.parallel.no_join"]);
      expect(errorsOf(FORK("f -> m\n a -> m\n b -> m"))).toEqual(expect.arrayContaining(["attractor.parallel.bad_branch_target"]));
    });

    test("for_each: one edge, to an agent or prompt", () => {
      const EACH = (body: string, attrs = 'for_each="context.items"') =>
        WELL_FORMED(`f [shape=component, ${attrs}]\n a [prompt="p"]\n c [script="x"]\n m [shape=tripleoctagon]\n start -> f\n ${body}\n a -> m\n c -> m\n m -> exit`);
      expect(errorsOf(EACH("f -> a"))).toEqual(["attractor.unreachable_node"]);
      expect(errorsOf(WELL_FORMED('f [shape=component, for_each="context.items"]\n a [prompt="p"]\n m [shape=tripleoctagon]\n start -> f -> a -> m -> exit'))).toEqual([]);
      expect(errorsOf(EACH("f -> a\n f -> c"))).toEqual(["attractor.for_each.template_edges"]);
      expect(errorsOf(WELL_FORMED('f [shape=component, for_each="context.items"]\n c [script="x"]\n m [shape=tripleoctagon]\n start -> f -> c -> m -> exit'))).toEqual(["attractor.for_each.target"]);
    });
  });

  describe("file references", () => {
    test("finds prompts, schemas, goals, children and imports, including through defaults", () => {
      const graph = parsed(`digraph G {
        graph [goal="@goal.md"]
        subgraph c { node [output_schema="@schemas/out.json"] a [prompt="@prompts/a.md"] }
        b [prompt="inline text"]
        k [shape=house, stack.child_workflow="child.fabro"]
        i [import="./lib.fabro"]
      }`);
      expect(workflowFileReferences(graph)).toEqual([
        { kind: "goal", path: "goal.md" },
        { kind: "prompt", path: "prompts/a.md", nodeId: "a" },
        { kind: "output_schema", path: "schemas/out.json", nodeId: "a" },
        { kind: "child_workflow", path: "child.fabro", nodeId: "k" },
        { kind: "import", path: "./lib.fabro", nodeId: "i" },
      ]);
    });

    const CHILD = 'digraph C { start [shape=Mdiamond] exit [shape=Msquare] w [prompt="p"] start -> w -> exit }';
    const USES = (attrs: string) => WELL_FORMED(`n [${attrs}]\n start -> n -> exit`);

    test("a missing file is a warning saying what to do, never a blocker", () => {
      const diagnostics = validateWorkflow(parsed(USES('prompt="@prompts/a.md"')));
      expect(diagnostics.map((item) => [item.severity, item.code, item.nodeId])).toEqual([["warning", "builder.missing_file", "n"]]);
      expect(diagnostics[0].message).toContain("prompts/a.md");
      expect(validateWorkflow(parsed(USES('prompt="@./prompts/a.md"')), { "prompts/a.md": "text" })).toEqual([]);
    });

    test("a reference outside the workflow folder is flagged as not bundleable, not as unsafe", () => {
      expect(codes(validateWorkflow(parsed(USES('prompt="@../common/plan.md"'))))).toEqual(["builder.external_file"]);
      expect(codes(validateWorkflow(parsed(USES('prompt="@~/shared/plan.md"'))))).toEqual(["builder.external_file"]);
    });

    test("a child can't climb back to a file beside it with .., because Fabro refuses that", () => {
      const child = 'digraph C { graph [goal="@../ticket.md"] start [shape=Mdiamond] exit [shape=Msquare] start -> exit }';
      const house = USES('shape=house, stack.child_workflow="shared/c.fabro"');
      const diagnostics = validateWorkflow(parsed(house), { "shared/c.fabro": child, "ticket.md": "text" });
      expect(codes(diagnostics)).toEqual(["builder.external_file"]);
      expect(diagnostics[0].message).toContain("exact path");
      const beside = child.replace("@../ticket.md", "@ticket.md");
      expect(validateWorkflow(parsed(house), { "shared/c.fabro": beside, "shared/ticket.md": "text" })).toEqual([]);
    });

    test("template syntax in a file reference is an error", () => {
      expect(errorsOf(USES('prompt="@prompts/{{ inputs.x }}.md"'))).toEqual(["builder.templated_reference"]);
    });

    test("an uploaded schema must be a JSON object", () => {
      expect(errorsOf(USES('prompt="p", output_schema="@s.json"'), { "s.json": "[1]" })).toEqual(["attractor.bad_output_schema"]);
      expect(errorsOf(USES('prompt="p", output_schema="@s.json"'), { "s.json": '{"type":"object"}' })).toEqual([]);
    });

    test("child workflows in the draft are validated too, and cycles refused", () => {
      const house = USES('shape=house, stack.child_workflow="c.fabro"');
      expect(validateWorkflow(parsed(house), { "c.fabro": CHILD })).toEqual([]);
      const syntax = validateWorkflow(parsed(house), { "c.fabro": "digraph oops {" });
      expect(syntax[0]).toMatchObject({ severity: "error", code: "dot.syntax", nodeId: "n" });
      expect(syntax[0].message).toContain("does not parse");
      const invalid = validateWorkflow(parsed(house), { "c.fabro": "digraph C { start [shape=Mdiamond] }" });
      expect(invalid.map((item) => item.code)).toContain("attractor.no_exit");
      const looping = USES('shape=house, stack.child_workflow="c.fabro"');
      expect(errorsOf(house, { "c.fabro": looping })).toContain("attractor.workflow_cycle");
      expect(errorsOf(USES('shape=house, stack.child_dot_source="digraph C { oops"'))).toEqual(["dot.syntax"]);
    });

    test("a child's own files resolve beside the child", () => {
      const child = 'digraph C { start [shape=Mdiamond] exit [shape=Msquare] w [prompt="@w.md"] start -> w -> exit }';
      const house = USES('shape=house, stack.child_workflow="shared/c.fabro"');
      expect(codes(validateWorkflow(parsed(house), { "shared/c.fabro": child }))).toEqual(["builder.missing_file"]);
      expect(validateWorkflow(parsed(house), { "shared/c.fabro": child, "shared/w.md": "text" })).toEqual([]);
    });

    test("imports must keep Fabro's contract, and a placeholder only inheritable settings", () => {
      const placeholder = WELL_FORMED('lib [import="lib.fabro"]\n start -> lib -> exit');
      expect(codes(validateWorkflow(parsed(placeholder)))).toEqual(["builder.missing_file"]);
      expect(validateWorkflow(parsed(placeholder), { "lib.fabro": CHILD })).toEqual([]);
      const twoSteps = 'digraph C { start [shape=Mdiamond] exit [shape=Msquare] a [prompt="p"] b [prompt="q"] start -> a\n start -> b\n a -> exit\n b -> exit }';
      expect(errorsOf(placeholder, { "lib.fabro": twoSteps })).toEqual(["attractor.import"]);
      const labelled = WELL_FORMED('lib [import="lib.fabro", label="Nope"]\n start -> lib -> exit');
      expect(errorsOf(labelled, { "lib.fabro": CHILD })).toEqual(["attractor.import"]);
      const allowed = WELL_FORMED('lib [import="lib.fabro", model="m", class="shared"]\n start -> lib -> exit');
      expect(errorsOf(allowed, { "lib.fabro": CHILD })).toEqual([]);
    });
  });

  test("validation doesn't change the graph", () => {
    const graph = deepFreeze(parsed(EXAMPLE_SOURCE));
    expect(() => validateWorkflow(graph)).not.toThrow();
    expect(() => serializeWorkflow(graph)).not.toThrow();
    expect(() => effectiveNodeAttributes(graph, graph.nodes[0])).not.toThrow();
  });
});

describe("prompt blocks", () => {
  const text = (value: string, id = "t"): PromptPart => ({ id, mode: "text", text: value, path: "" });
  const file = (path: string, id = "f"): PromptPart => ({ id, mode: "file", text: "", path });

  test("one text block stays inline, verbatim", () => {
    expect(compilePromptParts("n", [text("  Do the {{ goal }}.\n")], {})).toEqual({
      prompt: "  Do the {{ goal }}.\n",
      files: {},
      diagnostics: [],
    });
  });

  test("one uploaded file becomes an @ reference with its content exported", () => {
    expect(compilePromptParts("n", [file("prompts/plan.md")], { "prompts/plan.md": "the plan" })).toEqual({
      prompt: "@prompts/plan.md",
      files: { "prompts/plan.md": "the plan" },
      diagnostics: [],
    });
  });

  test("several blocks join with blank lines into one generated file, in order, with uploads exported too", () => {
    const compiled = compilePromptParts("review", [text("first"), file("a.md"), text("last\n")], { "a.md": "middle\n" });
    expect(compiled.prompt).toBe("@prompts/review.md");
    expect(compiled.files).toEqual({ "prompts/review.md": "first\n\nmiddle\n\nlast", "a.md": "middle\n" });
    expect(compiled.diagnostics).toEqual([]);
  });

  test("blank blocks add nothing, and nothing at all leaves no reference", () => {
    expect(compilePromptParts("n", [text("   \n")], {})).toEqual({ prompt: "", files: {}, diagnostics: [] });
    expect(compilePromptParts("n", [], {})).toEqual({ prompt: "", files: {}, diagnostics: [] });
    expect(compilePromptParts("n", [text("only"), text(" ")], {}).prompt).toBe("only");
    const fileless = compilePromptParts("n", [file(""), text("kept")], {});
    expect(fileless.prompt).toBe("kept");
    expect(fileless.diagnostics[0].code).toBe("builder.file_block_without_path");
  });

  test("text in a file block with no path is kept", () => {
    const compiled = compilePromptParts("n", [{ id: "x", mode: "file", text: "typed text", path: "" }], {});
    expect(compiled.prompt).toBe("typed text");
  });

  test("text starting with @ is moved to a file, so Fabro doesn't read it as a reference", () => {
    const compiled = compilePromptParts("n", [text("@team please review")], {});
    expect(compiled.prompt).toBe("@prompts/n.md");
    expect(compiled.files).toEqual({ "prompts/n.md": "@team please review" });
  });

  test("an unresolved file keeps its reference and is flagged; content is never invented", () => {
    const single = compilePromptParts("n", [file("external.md")], {});
    expect(single.prompt).toBe("@external.md");
    expect(single.files).toEqual({});
    expect(single.diagnostics).toMatchObject([{ severity: "warning", code: "builder.missing_file", nodeId: "n" }]);
    const joined = compilePromptParts("n", [text("intro"), file("external.md")], {});
    expect(joined.files).toEqual({ "prompts/n.md": "intro\n\n@external.md" });
    expect(joined.diagnostics).toMatchObject([{ severity: "error", code: "builder.missing_file" }]);
  });

  test("unsafe paths are refused, and never make it into a reference or the files", () => {
    for (const path of [
      "/etc/passwd",
      "../secret.md",
      "a/../../b.md",
      "~/.ssh/id",
      "C:\\temp\\x.md",
      "C:/temp/x.md",
      "a\\b.md",
      ".git/hooks/pre-commit",
      "sub/.GIT/config",
      "a//b.md",
      "a/./b.md",
      "trailing/",
      "x\u0000y.md",
      "https://example.com/x.md",
      "{{ inputs.p }}.md",
    ]) {
      const compiled = compilePromptParts("n", [file(path)], { [path]: "secret" });
      expect(compiled.prompt).toBe("");
      expect(compiled.files).toEqual({});
      expect(compiled.diagnostics).toMatchObject([{ severity: "error", code: "builder.unsafe_path", nodeId: "n" }]);
    }
  });

  test("a refused file doesn't take the remaining text down with it", () => {
    const compiled = compilePromptParts("n", [text("kept"), file("../x.md")], {});
    expect(compiled.prompt).toBe("kept");
    expect(codes(compiled.diagnostics)).toEqual(["builder.unsafe_path"]);
  });

  test("the generated name can't collide with an upload that is part of the prompt", () => {
    const compiled = compilePromptParts("n", [text("a"), file("prompts/n.md")], { "prompts/n.md": "upload" });
    expect(compiled.prompt).toBe("@prompts/n.generated.md");
    expect(compiled.files).toEqual({ "prompts/n.md": "upload", "prompts/n.generated.md": "a\n\nupload" });
    expect(codes(compiled.diagnostics)).toEqual(["builder.path_collision"]);
  });

  test("node ids that aren't path-safe still get a safe, distinct file", () => {
    expect(generatedPromptPath("plan_2")).toBe("prompts/plan_2.md");
    const dotted = generatedPromptPath("../../evil");
    expect(dotted).toMatch(/^prompts\/evil-[0-9a-f]{8}\.md$/);
    expect(generatedPromptPath("a b")).not.toBe(generatedPromptPath("a_b"));
    expect(generatedPromptPath("a b")).not.toBe(generatedPromptPath("a-b"));
  });

  test("an upload named like an object internal is exported, not lost", () => {
    const assets: Record<string, string> = {};
    Object.defineProperty(assets, "__proto__", { value: "body", enumerable: true });
    const compiled = compilePromptParts("n", [file("__proto__")], assets);
    expect(compiled.prompt).toBe("@__proto__");
    expect(Object.keys(compiled.files)).toEqual(["__proto__"]);
  });

  test("doesn't touch what it is given", () => {
    const parts = deepFreeze([text("a"), file("b.md")]);
    const assets = deepFreeze({ "b.md": "b" });
    expect(compilePromptParts("n", parts, assets).prompt).toBe("@prompts/n.md");
  });

  test("blocks can be derived from a prompt attribute written in code", () => {
    expect(promptPartsFromAttribute("n", "@x.md")).toMatchObject([{ mode: "file", path: "x.md" }]);
    expect(promptPartsFromAttribute("n", "inline")).toMatchObject([{ mode: "text", text: "inline" }]);
    expect(promptPartsFromAttribute("n", undefined)).toEqual([]);
    expect(promptPartsFromAttribute("n", "  ")).toEqual([]);
  });
});

describe("drafts", () => {
  const withParts = (parts: PromptPart[], assets: Record<string, string> = {}): WorkflowDraft => ({
    ...newDraft("Flow"),
    assets,
    promptParts: { agent: parts },
  });
  const text = (value: string): PromptPart => ({ id: value, mode: "text", text: value, path: "" });
  const file = (path: string): PromptPart => ({ id: path, mode: "file", text: "", path });

  test("a new draft is a valid Start -> Agent -> Exit, with everything it needs", () => {
    const draft = newDraft("My Cool Flow!");
    expect(draft.version).toBe(1);
    expect(draft.id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(draft.name).toBe("My Cool Flow!");
    expect(Number.isNaN(Date.parse(draft.updatedAt))).toBe(false);
    expect(draft.assets).toEqual({});
    const graph = parsed(draft.source);
    expect(graph.name).toBe("MyCoolFlow");
    expect(graph.nodes.map((item) => getNodeKind(item, graph).type)).toEqual(["start", "agent", "exit"]);
    expect(graph.edges.map((edge) => edge.id)).toEqual(["start->agent", "agent->exit"]);
    expect(validateWorkflow(graph)).toEqual([]);
    expect(Object.keys(draft.positions).sort()).toEqual(["agent", "exit", "start"]);
    expect(applyPromptParts(draft).source).toBe(draft.source);
  });

  test("new drafts have their own ids and sensible names", () => {
    expect(newDraft().id).not.toBe(newDraft().id);
    expect(parsed(newDraft().source).name).toBe("UntitledWorkflow");
    expect(parsed(newDraft("123 go").source).name).toBe("Workflow123Go");
    expect(parsed(newDraft('"quotes" -> {braces}').source).name).toBe("QuotesBraces");
    expect(parsed(newDraft("日本語").source).name).toBe("Workflow");
  });

  test("export has the workflow, a toml pointing at it with PR off, and nothing invented", () => {
    const draft = newDraft("Flow");
    const files = draftFiles(draft);
    expect(Object.keys(files).sort()).toEqual(["workflow.builder.json", "workflow.fabro", "workflow.toml"]);
    expect(files["workflow.fabro"]).toBe(draft.source);
    expect(files["workflow.toml"]).toContain('graph = "workflow.fabro"');
    expect(files["workflow.toml"]).toMatch(/\[run\.pull_request\]\nenabled = false/);
    expect(files["workflow.toml"]).toContain("_version = 1");
    const metadata = JSON.parse(files["workflow.builder.json"]);
    expect(metadata).toMatchObject({ version: 1, id: draft.id, name: "Flow", positions: draft.positions });
    expect(files["workflow.fabro"]).not.toContain("positions");
  });

  test("an untouched draft's source is exported byte for byte, odd formatting and all", () => {
    const source = '// keep\ndigraph  G{start[shape=Mdiamond];exit[shape=Msquare]; start->exit\n\n\n}';
    expect(draftFiles({ ...newDraft(), source, promptParts: {} })["workflow.fabro"]).toBe(source);
  });

  test("prompt blocks reach the workflow as plain Fabro syntax, with their files", () => {
    const draft = withParts([text("first"), file("notes/a.md")], { "notes/a.md": "second", "extra.txt": "unused" });
    const files = draftFiles(draft);
    const graph = parsed(files["workflow.fabro"]);
    expect(effective(graph, "agent").prompt).toBe("@prompts/agent.md");
    expect(files["prompts/agent.md"]).toBe("first\n\nsecond");
    expect(files["notes/a.md"]).toBe("second");
    expect(files["extra.txt"]).toBe("unused");
    expect(graph.comments).toEqual([]);
  });

  test("a prompt edited through blocks keeps the rest of the workflow's meaning and comments", () => {
    const source = `// note
digraph G {
  node [timeout="900s"]
  start [shape=Mdiamond]
  exit [shape=Msquare]
  agent [prompt="old", wiggle="kept"] // beside
  start -> agent -> exit
}`;
    const draft: WorkflowDraft = { ...newDraft(), source, promptParts: { agent: [text("new")] } };
    const output = draftFiles(draft)["workflow.fabro"];
    expect(output).toContain("// note");
    expect(output).toContain("// beside");
    const graph = parsed(output);
    expect(effective(graph, "agent")).toEqual({ timeout: "900s", prompt: "new", wiggle: "kept" });
  });

  test("blocks that compile to nothing can't erase a prompt", () => {
    const source = WELL_FORMED('agent [prompt="precious"]\n start -> agent -> exit');
    const draft: WorkflowDraft = { ...newDraft(), source, promptParts: { agent: [text("  ")] } };
    expect(applyPromptParts(draft).source).toBe(source);
  });

  test("source that won't parse is never exported, and says why", () => {
    const draft: WorkflowDraft = { ...newDraft(), source: "digraph {" };
    expect(exportDraft(draft)).toMatchObject({ files: {}, blocked: true, diagnostics: [{ severity: "error", line: 1 }] });
    expect(() => draftFiles(draft)).toThrow(DraftExportError);
  });

  test("structural errors and missing files don't stop an export; they are reported", () => {
    const source = WELL_FORMED('agent [prompt="@gone.md", model="REPLACE_ME"]\n start -> agent');
    const exported = exportDraft({ ...newDraft(), source, promptParts: {} });
    expect(exported.blocked).toBe(false);
    expect(exported.files["workflow.fabro"]).toBe(source);
    expect(codes(exported.diagnostics)).toEqual(
      expect.arrayContaining(["attractor.exit_unreachable", "builder.missing_file", "builder.placeholder_model"]),
    );
  });

  test("an unsafe upload name blocks the export instead of landing in a zip", () => {
    for (const path of ["../../evil.sh", "/etc/cron.d/x", ".git/hooks/post-checkout"]) {
      const draft = withParts([], { [path]: "payload" });
      const exported = exportDraft(draft);
      expect(exported.blocked).toBe(true);
      expect(Object.keys(exported.files)).not.toContain(path);
      expect(() => draftFiles(draft)).toThrow(/cannot be included/);
    }
  });

  test("uploads can't overwrite the workflow's own files or each other", () => {
    expect(exportDraft(withParts([], { "workflow.fabro": "x" })).blocked).toBe(true);
    expect(exportDraft(withParts([], { "A.md": "1", "a.md": "2" })).blocked).toBe(true);
    expect(exportDraft(withParts([], { a: "1", "a/b.md": "2" })).blocked).toBe(true);
  });

  describe("two spellings of one path", () => {
    test("with different content, the export is blocked instead of losing one", () => {
      const exported = exportDraft(withParts([], { "./prompts/a.md": "one", "prompts/a.md": "two" }));
      expect(exported.blocked).toBe(true);
      expect(exported.diagnostics).toMatchObject([
        { severity: "error", code: "builder.path_collision", message: expect.stringContaining('"./prompts/a.md" and "prompts/a.md"') },
      ]);
      expect(() => draftFiles(withParts([], { "./prompts/a.md": "one", "prompts/a.md": "two" }))).toThrow(DraftExportError);
    });

    test("in either order", () => {
      expect(exportDraft(withParts([], { "prompts/a.md": "one", "./prompts/a.md": "two" })).blocked).toBe(true);
      expect(exportDraft(withParts([], { "prompts/a.md": "one", "././prompts/a.md": "two" })).blocked).toBe(true);
    });

    test("with the same content, they export as one file", () => {
      const exported = exportDraft(withParts([], { "./prompts/a.md": "same", "prompts/a.md": "same" }));
      expect(exported.blocked).toBe(false);
      expect(exported.files["prompts/a.md"]).toBe("same");
      expect(Object.keys(exported.files)).not.toContain("./prompts/a.md");
    });

    test("a lone alias still exports, and a prompt block can read it", () => {
      const exported = exportDraft(withParts([text("intro"), file("a.md")], { "./a.md": "body" }));
      expect(exported.blocked).toBe(false);
      expect(exported.files["a.md"]).toBe("body");
      expect(exported.files["prompts/agent.md"]).toBe("intro\n\nbody");
    });

    test("canonicalAssets names each conflict without touching what it's given", () => {
      const input = deepFreeze({ "./a.md": "one", "a.md": "two", "b.md": "three" });
      expect(canonicalAssets(input)).toEqual({
        assets: { "a.md": "one", "b.md": "three" },
        conflicts: [["./a.md", "a.md"]],
      });
    });
  });

  test("a generated prompt takes a free path rather than overwrite an upload another node relies on", () => {
    const source = WELL_FORMED('agent [prompt="x"]\n other [prompt="@prompts/agent.md"]\n start -> agent -> other -> exit');
    const draft: WorkflowDraft = {
      ...newDraft(),
      source,
      assets: { "prompts/agent.md": "other's prompt" },
      promptParts: { agent: [text("one"), text("two")] },
    };
    const exported = exportDraft(draft);
    expect(exported.blocked).toBe(false);
    expect(exported.files["prompts/agent.md"]).toBe("other's prompt");
    expect(exported.files["prompts/agent.generated.md"]).toBe("one\n\ntwo");
    const graph = parsed(exported.files["workflow.fabro"]);
    expect(effective(graph, "agent").prompt).toBe("@prompts/agent.generated.md");
    expect(effective(graph, "other").prompt).toBe("@prompts/agent.md");
    expect(exported.diagnostics.map((item) => item.code)).toContain("builder.path_collision");
  });

  test("blocks for a node that no longer exists are reported and survive in the metadata", () => {
    const draft: WorkflowDraft = { ...newDraft(), promptParts: { gone: [text("my work")] } };
    const exported = exportDraft(draft);
    expect(exported.blocked).toBe(false);
    expect(codes(exported.diagnostics)).toContain("builder.orphan_prompt_parts");
    expect(JSON.parse(exported.files["workflow.builder.json"]).promptParts.gone[0].text).toBe("my work");
  });

  test("export doesn't touch the draft", () => {
    const draft = deepFreeze(withParts([text("a"), file("b.md")], { "b.md": "b" }));
    expect(() => draftFiles(draft)).not.toThrow();
    expect(() => applyPromptParts(draft)).not.toThrow();
    expect(() => renameDraftNode(draft, "agent", "worker")).not.toThrow();
    expect(() => renameDraftAsset(draft, "b.md", "c.md")).not.toThrow();
    expect(() => reconcilePromptParts(draft)).not.toThrow();
  });

  describe("editing prompts in code", () => {
    test("with the previous source, only prompts the edit changed are rebuilt", () => {
      const source = WELL_FORMED('agent [prompt="old"]\n other [prompt="same"]\n start -> agent -> other -> exit');
      const unsynced: WorkflowDraft = {
        ...newDraft(),
        source,
        promptParts: { agent: [text("old")], other: [text("edited on the canvas, not yet applied")] },
      };
      const edited: WorkflowDraft = { ...unsynced, source: source.replace('prompt="old"', 'prompt="typed in code"') };
      const next = reconcilePromptParts(edited, source);
      expect(next.agent).toMatchObject([{ mode: "text", text: "typed in code" }]);
      expect(next.other).toBe(unsynced.promptParts.other);
    });

    test("using blocks over a prompt that differs in the source says so", () => {
      const source = WELL_FORMED('agent [prompt="typed in code"]\n start -> agent -> exit');
      const draft: WorkflowDraft = { ...newDraft(), source, promptParts: { agent: [text("from blocks")] } };
      expect(codes(applyPromptParts(draft).diagnostics)).toEqual(["builder.prompt_replaced"]);
      expect(codes(applyPromptParts({ ...draft, promptParts: { agent: [text("typed in code")] } }).diagnostics)).toEqual([]);
    });

    test("blocks that still match are kept; ones the code changed are rebuilt from the code", () => {
      const base = withParts([text("one"), file("a.md")], { "a.md": "two" });
      const synced: WorkflowDraft = { ...base, source: applyPromptParts(base).source };
      expect(reconcilePromptParts(synced).agent).toBe(base.promptParts.agent);
      const edited: WorkflowDraft = { ...synced, source: synced.source.replace('prompt="@prompts/agent.md"', 'prompt="typed in code"') };
      expect(reconcilePromptParts(edited).agent).toMatchObject([{ mode: "text", text: "typed in code" }]);
      const orphan: WorkflowDraft = { ...synced, promptParts: { ...synced.promptParts, gone: [text("x")] } };
      expect(reconcilePromptParts(orphan).gone).toEqual([text("x")]);
    });
  });

  describe("renaming a node", () => {
    const source = `digraph G {
      // about review
      start [shape=Mdiamond]
      exit [shape=Msquare]
      review [prompt="p", retry_target="review"] // beside review
      gate [shape=hexagon, human.default_choice="review"]
      helper [prompt="q", fallback_retry_target="review"]
      start -> gate
      gate -> review [label="[R] Review"]
      gate -> helper
      review -> exit [weight=2]
      helper -> exit
    }`;
    const draft: WorkflowDraft = {
      ...newDraft(),
      source,
      positions: { review: { x: 1, y: 2 } },
      assets: { "a.md": "A" },
      promptParts: { review: [text("one"), file("a.md")] },
    };

    test("reaches everything that names it", () => {
      const { draft: renamed, diagnostics } = renameDraftNode(draft, "review", "audit");
      expect(diagnostics).toEqual([]);
      const graph = parsed(renamed.source);
      expect(graph.nodes.map((item) => item.id)).toEqual(["start", "exit", "audit", "gate", "helper"]);
      expect(graph.edges.map((edge) => edge.id)).toEqual(["start->gate", "gate->audit", "gate->helper", "audit->exit", "helper->exit"]);
      expect(effective(graph, "audit").retry_target).toBe("audit");
      expect(effective(graph, "helper").fallback_retry_target).toBe("audit");
      expect(effective(graph, "gate")["human.default_choice"]).toBe("audit");
      expect(renamed.positions).toEqual({ audit: { x: 1, y: 2 } });
      expect(renamed.promptParts.audit).toEqual(draft.promptParts.review);
      expect(renamed.promptParts.review).toBeUndefined();
      expect(renamed.source).toContain("audit [");
      expect(renamed.source).toContain("// beside review");
      expect(renamed.source).toContain("audit -> exit [weight=2]");
    });

    test("moves its generated prompt to the new name", () => {
      const { draft: renamed } = renameDraftNode(draft, "review", "audit");
      expect(effective(parsed(renamed.source), "audit").prompt).toBe("@prompts/audit.md");
      const files = draftFiles(renamed);
      expect(files["prompts/audit.md"]).toBe("one\n\nA");
      expect(files["prompts/review.md"]).toBeUndefined();
    });

    test("refuses a name that would merge two nodes, and invalid names, leaving the draft alone", () => {
      for (const [target, code] of [
        ["helper", "builder.duplicate_node_id"],
        ["has space", "builder.invalid_node_id"],
        ["1a", "builder.invalid_node_id"],
      ]) {
        const { draft: unchanged, diagnostics } = renameDraftNode(draft, "review", target);
        expect(unchanged).toBe(draft);
        expect(diagnostics).toMatchObject([{ severity: "error", code }]);
      }
      expect(renameDraftNode(draft, "nope", "x").diagnostics[0].code).toBe("builder.unknown_node");
    });

    test("flags a stylesheet selector it can't safely rewrite", () => {
      const styled: WorkflowDraft = {
        ...newDraft(),
        source: 'digraph G { graph [model_stylesheet="#agent { model: m; }"] start [shape=Mdiamond] exit [shape=Msquare] agent [prompt="p"] start -> agent -> exit }',
        promptParts: {},
      };
      expect(codes(renameDraftNode(styled, "agent", "worker").diagnostics)).toEqual(["builder.stylesheet_selector"]);
    });
  });

  describe("renaming an uploaded file", () => {
    const source = WELL_FORMED(
      'a [prompt="@old.md"]\n b [prompt="q", output_schema="@old.md"]\n c [shape=house, stack.child_workflow="old.md"]\n d [import="./old.md"]\n start -> a -> b -> c -> d -> exit',
      'graph [goal="@old.md"]',
    );
    const draft: WorkflowDraft = {
      ...newDraft(),
      source,
      assets: { "old.md": "content", "other.md": "x" },
      promptParts: { a: [file("old.md")] },
    };

    test("moves the content and every reference to it", () => {
      const { draft: renamed, diagnostics } = renameDraftAsset(draft, "old.md", "prompts/new.md");
      expect(diagnostics).toEqual([]);
      expect(renamed.assets).toEqual({ "prompts/new.md": "content", "other.md": "x" });
      expect(renamed.promptParts.a[0].path).toBe("prompts/new.md");
      const graph = parsed(renamed.source);
      expect(graph.attrs.goal).toBe("@prompts/new.md");
      expect(effective(graph, "a").prompt).toBe("@prompts/new.md");
      expect(effective(graph, "b").output_schema).toBe("@prompts/new.md");
      expect(effective(graph, "c")["stack.child_workflow"]).toBe("prompts/new.md");
      expect(effective(graph, "d").import).toBe("prompts/new.md");
    });

    describe("in attached workflows", () => {
      const sample = (): WorkflowDraft => ({
        ...newDraft("Sample"),
        source: EXAMPLE_SOURCE,
        assets: { ...EXAMPLE_ASSETS },
        promptParts: examplePromptParts(),
      });
      const CHILDREN = ["shared/testing.fabro", "shared/security.fabro", "shared/code-review.fabro"];

      test("a shared ticket renamed in its folder is followed by the workflow and every child", () => {
        const { draft: renamed, diagnostics } = renameDraftAsset(sample(), "shared/ticket.md", "shared/renamed-ticket.md");
        expect(diagnostics).toEqual([]);
        expect(parsed(renamed.source).attrs.goal).toBe("@shared/renamed-ticket.md");
        for (const child of CHILDREN) expect(parsed(renamed.assets[child]).attrs.goal).toBe("@renamed-ticket.md");
        expect(renamed.assets["shared/ticket.md"]).toBeUndefined();
        expect(renamed.assets["shared/renamed-ticket.md"]).toBe(EXAMPLE_ASSETS["shared/ticket.md"]);
        expect(validateWorkflow(parsed(renamed.source), renamed.assets)).toEqual([]);
        expect(exportDraft(renamed)).toMatchObject({ blocked: false, diagnostics: [] });
      });

      test("what a child means, and its comments, survive the rewrite", () => {
        const { draft: renamed } = renameDraftAsset(sample(), "shared/ticket.md", "shared/renamed-ticket.md");
        for (const child of CHILDREN) {
          const before = parsed(EXAMPLE_ASSETS[child]);
          const after = parsed(renamed.assets[child]);
          expect(after.comments).toEqual(before.comments);
          expect(after.nodes.map((node) => [node.id, node.attrs])).toEqual(before.nodes.map((node) => [node.id, node.attrs]));
          expect(after.edges.map((edge) => edge.id)).toEqual(before.edges.map((edge) => edge.id));
        }
      });

      test("files that don't name it aren't touched, byte for byte", () => {
        const { draft: renamed } = renameDraftAsset(sample(), "README.md", "docs/README.md");
        expect(renamed.source).toBe(EXAMPLE_SOURCE);
        for (const child of CHILDREN) expect(renamed.assets[child]).toBe(EXAMPLE_ASSETS[child]);
        expect(renamed.assets["docs/README.md"]).toBe(EXAMPLE_ASSETS["README.md"]);
      });

      test("moving it where a child could only reach it through .. is refused, with nothing changed", () => {
        const draft = sample();
        const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "shared/ticket.md", "docs/ticket.md");
        expect(unchanged).toBe(draft);
        expect(diagnostics).toMatchObject([{ severity: "error", code: "builder.rename_refused" }]);
        expect(diagnostics[0].message).toContain("shared/testing.fabro");
        expect(diagnostics[0].message).toContain('".."');
      });

      test("moving it deeper is fine: each reader gets its own relative path", () => {
        const { draft: renamed, diagnostics } = renameDraftAsset(sample(), "shared/ticket.md", "shared/tickets/now.md");
        expect(diagnostics).toEqual([]);
        expect(parsed(renamed.source).attrs.goal).toBe("@shared/tickets/now.md");
        for (const child of CHILDREN) expect(parsed(renamed.assets[child]).attrs.goal).toBe("@tickets/now.md");
        expect(validateWorkflow(parsed(renamed.source), renamed.assets)).toEqual([]);
      });

      test("a child workflow renamed in its folder is followed by its parent, and keeps its own references", () => {
        const { draft: renamed, diagnostics } = renameDraftAsset(sample(), "shared/testing.fabro", "shared/qa.fabro");
        expect(diagnostics).toEqual([]);
        expect(effective(parsed(renamed.source), "testing")["stack.child_workflow"]).toBe("shared/qa.fabro");
        expect(parsed(renamed.assets["shared/qa.fabro"]).attrs.goal).toBe("@ticket.md");
        expect(renamed.assets["shared/qa.fabro"]).toBe(EXAMPLE_ASSETS["shared/testing.fabro"]);
        expect(validateWorkflow(parsed(renamed.source), renamed.assets)).toEqual([]);
      });

      test("a child workflow can't move away from a file only .. could then reach", () => {
        const draft = sample();
        const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "shared/testing.fabro", "shared/qa/testing.fabro");
        expect(unchanged).toBe(draft);
        expect(diagnostics[0].code).toBe("builder.rename_refused");
      });

      const NESTED = {
        "a/outer.fabro": 'digraph O { start [shape=Mdiamond] exit [shape=Msquare] k [shape=house, stack.child_workflow="b/inner.fabro"] start -> k -> exit }\n',
        "a/b/inner.fabro": 'digraph I { start [shape=Mdiamond] exit [shape=Msquare] w [prompt="@notes.md"] start -> w -> exit }\n',
        "a/b/notes.md": "Notes",
      };
      const nestedDraft = (): WorkflowDraft => ({
        ...newDraft(),
        source: WELL_FORMED('k [shape=house, stack.child_workflow="a/outer.fabro"]\n start -> k -> exit'),
        assets: { ...NESTED },
        promptParts: {},
      });

      test("transitive dependencies are followed, each from its own folder", () => {
        const { draft: renamed, diagnostics } = renameDraftAsset(nestedDraft(), "a/b/notes.md", "a/b/renamed.md");
        expect(diagnostics).toEqual([]);
        expect(effective(parsed(renamed.assets["a/b/inner.fabro"]), "w").prompt).toBe("@renamed.md");
        expect(renamed.assets["a/outer.fabro"]).toBe(NESTED["a/outer.fabro"]);
        expect(validateWorkflow(parsed(renamed.source), renamed.assets)).toEqual([]);
      });

      test("a middle child moved within reach keeps the chain working", () => {
        const { draft: renamed, diagnostics } = renameDraftAsset(nestedDraft(), "a/b/inner.fabro", "a/b/deeper.fabro");
        expect(diagnostics).toEqual([]);
        expect(effective(parsed(renamed.assets["a/outer.fabro"]), "k")["stack.child_workflow"]).toBe("b/deeper.fabro");
        expect(validateWorkflow(parsed(renamed.source), renamed.assets)).toEqual([]);
      });

      test("a reference through a default is followed too", () => {
        const child = 'digraph C { node [prompt="@x.md"] start [shape=Mdiamond] exit [shape=Msquare] w start -> w -> exit }\n';
        const draft: WorkflowDraft = {
          ...newDraft(),
          source: WELL_FORMED('k [shape=house, stack.child_workflow="c/child.fabro"]\n start -> k -> exit'),
          assets: { "c/child.fabro": child, "c/x.md": "X" },
          promptParts: {},
        };
        const { draft: renamed } = renameDraftAsset(draft, "c/x.md", "c/y.md");
        expect(parsed(renamed.assets["c/child.fabro"]).nodeDefaults.prompt).toBe("@y.md");
      });

      test("an attached workflow with a syntax error that might name it blocks the rename", () => {
        const draft: WorkflowDraft = { ...newDraft(), assets: { "x.md": "X", "broken.fabro": 'digraph { a [prompt="@x.md"' } };
        const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "x.md", "y.md");
        expect(unchanged).toBe(draft);
        expect(diagnostics[0].message).toContain("broken.fabro");
      });

      test("an attached workflow with a syntax error that doesn't name it doesn't", () => {
        const draft: WorkflowDraft = { ...newDraft(), assets: { "x.md": "X", "broken.fabro": "digraph {" } };
        expect(renameDraftAsset(draft, "x.md", "y.md").diagnostics).toEqual([]);
      });

      test("an {% include %} naming it blocks the rename, since it stays as written", () => {
        const draft: WorkflowDraft = {
          ...newDraft(),
          assets: { "partials/x.md": "X", "main.md": '{% include "partials/x.md" %}' },
        };
        const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "partials/x.md", "partials/y.md");
        expect(unchanged).toBe(draft);
        expect(diagnostics[0].message).toContain("main.md");
      });

      test("an {% include %} in the workflow itself blocks the rename too", () => {
        const draft: WorkflowDraft = {
          ...newDraft(),
          source: WELL_FORMED('work [prompt="{% include \\"partials/x.md\\" %}"]\n start -> work -> exit'),
          assets: { "partials/x.md": "X" },
          promptParts: {},
        };
        const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "partials/x.md", "partials/y.md");
        expect(unchanged).toBe(draft);
        expect(diagnostics[0].message).toContain("{% include %}");
      });

      test("a custom workflow.toml naming it blocks the rename, since it stays as written", () => {
        const draft: WorkflowDraft = {
          ...newDraft(),
          assets: { "docker/Dockerfile": "FROM x", "workflow.toml": '[environments.a.image]\ndockerfile = { path = "docker/Dockerfile" }\n' },
        };
        const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "docker/Dockerfile", "Dockerfile");
        expect(unchanged).toBe(draft);
        expect(diagnostics[0].message).toContain("workflow.toml");
      });

      test("two spellings of the file with different content block the rename", () => {
        const draft: WorkflowDraft = { ...newDraft(), assets: { "./x.md": "one", "x.md": "two" } };
        expect(renameDraftAsset(draft, "x.md", "y.md").diagnostics[0].code).toBe("builder.path_collision");
      });
    });

    describe("in inline children", () => {
      /** A complete child workflow whose graph goal names `goal`, or holds `body` as extra statements. */
      const childNaming = (goal: string, body = "start -> exit") =>
        `digraph C { graph [goal="${goal}"] start [shape=Mdiamond] exit [shape=Msquare] ${body} }`;
      const holding = (source: string) =>
        `digraph C { start [shape=Mdiamond] nested [shape=house stack.child_dot_source=${JSON.stringify(source)}] exit [shape=Msquare] start -> nested -> exit }`;
      const rootHolding = (source: string, goal = "@ticket.md") =>
        WELL_FORMED(`child [shape=house stack.child_dot_source=${JSON.stringify(source)}]\n start -> child -> exit`, `graph [goal="${goal}"]`);
      const draftOf = (source: string, assets: Record<string, string>): WorkflowDraft => ({ ...newDraft(), source, assets, promptParts: {} });
      const inlineOf = (graph: WorkflowGraph, id = "child") => parsed(effective(graph, id)["stack.child_dot_source"]);

      test("the workflow and its inline child both follow the rename", () => {
        const draft = draftOf(rootHolding(childNaming("@ticket.md")), { "ticket.md": "T" });
        const { draft: renamed, diagnostics } = renameDraftAsset(draft, "ticket.md", "renamed.md");
        expect(diagnostics).toEqual([]);
        const graph = parsed(renamed.source);
        expect(graph.attrs.goal).toBe("@renamed.md");
        expect(inlineOf(graph).attrs.goal).toBe("@renamed.md");
        expect(renamed.assets).toEqual({ "renamed.md": "T" });
        expect(exportDraft(renamed)).toMatchObject({ blocked: false, diagnostics: [] });
      });

      test("a grandchild follows it too", () => {
        const draft = draftOf(rootHolding(holding(holding(childNaming("@ticket.md")))), { "ticket.md": "T" });
        const { draft: renamed, diagnostics } = renameDraftAsset(draft, "ticket.md", "renamed.md");
        expect(diagnostics).toEqual([]);
        const depth3 = inlineOf(inlineOf(inlineOf(parsed(renamed.source)), "nested"), "nested");
        expect(depth3.attrs.goal).toBe("@renamed.md");
        expect(renamed.source).not.toContain("@ticket.md");
      });

      test("an inline child inside an attached workflow reads from that workflow's folder", () => {
        const draft = draftOf(
          WELL_FORMED('k [shape=house stack.child_workflow="shared/check.fabro"]\n start -> k -> exit', 'graph [goal="@shared/ticket.md"]'),
          { "shared/check.fabro": holding(childNaming("@ticket.md")), "shared/ticket.md": "T", "ticket.md": "A different file" },
        );
        const { draft: renamed, diagnostics } = renameDraftAsset(draft, "shared/ticket.md", "shared/tickets/now.md");
        expect(diagnostics).toEqual([]);
        expect(inlineOf(parsed(renamed.assets["shared/check.fabro"]), "nested").attrs.goal).toBe("@tickets/now.md");
        expect(renamed.assets["ticket.md"]).toBe("A different file");
        expect(exportDraft(renamed).diagnostics).toEqual([]);
      });

      test("a file with the same name in another folder isn't what the inline child means", () => {
        const draft = draftOf(
          WELL_FORMED('k [shape=house stack.child_workflow="shared/check.fabro"]\n start -> k -> exit'),
          { "shared/check.fabro": holding(childNaming("@ticket.md")), "shared/ticket.md": "Theirs", "ticket.md": "Mine" },
        );
        const { draft: renamed } = renameDraftAsset(draft, "ticket.md", "mine.md");
        expect(renamed.assets["shared/check.fabro"]).toBe(draft.assets["shared/check.fabro"]);
        expect(renamed.assets["mine.md"]).toBe("Mine");
      });

      test("a node default holding inline source is followed", () => {
        const source = `digraph P { start [shape=Mdiamond] exit [shape=Msquare] subgraph cluster_a { node [stack.child_dot_source=${JSON.stringify(childNaming("@ticket.md"))}] k [shape=house] } start -> k -> exit }`;
        const { draft: renamed } = renameDraftAsset(draftOf(source, { "ticket.md": "T" }), "ticket.md", "renamed.md");
        expect(renamed.source).toContain("renamed.md");
        expect(renamed.source).not.toContain("@ticket.md");
      });

      test("a child workflow an inline child runs is followed, and then counted as a workflow", () => {
        const inner = 'digraph G { graph [goal="@notes.md"] start [shape=Mdiamond] exit [shape=Msquare] start -> exit }';
        const draft = draftOf(
          rootHolding(childNaming("@ticket.md", 'k [shape=house stack.child_workflow="grand.dat"] start -> k -> exit'), "@ticket.md"),
          { "ticket.md": "T", "grand.dat": inner, "notes.md": "N" },
        );
        const afterChild = renameDraftAsset(draft, "grand.dat", "grand2.dat");
        expect(afterChild.diagnostics).toEqual([]);
        expect(effective(inlineOf(parsed(afterChild.draft.source)), "k")["stack.child_workflow"]).toBe("grand2.dat");
        // grand.dat has no workflow extension, so only the inline child shows it's one.
        const afterNotes = renameDraftAsset(draft, "notes.md", "n2.md");
        expect(afterNotes.diagnostics).toEqual([]);
        expect(parsed(afterNotes.draft.assets["grand.dat"]).attrs.goal).toBe("@n2.md");
      });

      test("what an inline child means, and its comments, survive the rewrite", () => {
        const child = `digraph C {
  // why this child exists
  graph [goal="@ticket.md", label="Keep \\"me\\""]
  start [shape=Mdiamond]
  work [prompt="Line one\\nLine two"] // and this node
  exit [shape=Msquare]
  start -> work -> exit
}`;
        const draft = draftOf(rootHolding(child), { "ticket.md": "T" });
        const { draft: renamed } = renameDraftAsset(draft, "ticket.md", "renamed.md");
        const before = inlineOf(parsed(draft.source));
        const after = inlineOf(parsed(renamed.source));
        expect(after.comments).toEqual(before.comments);
        expect(after.comments.length).toBeGreaterThan(0);
        expect(after.nodes.map((node) => [node.id, node.attrs])).toEqual(before.nodes.map((node) => [node.id, node.attrs]));
        expect({ ...after.attrs, goal: "" }).toEqual({ ...before.attrs, goal: "" });
      });

      test("inline source that doesn't name the file isn't touched, byte for byte", () => {
        const child = `digraph   Spaced   {  start [shape=Mdiamond]   exit [shape=Msquare]  start->exit  }`;
        const draft = draftOf(rootHolding(child, "@ticket.md"), { "ticket.md": "T" });
        const { draft: renamed } = renameDraftAsset(draft, "ticket.md", "renamed.md");
        expect(effective(parsed(renamed.source), "child")["stack.child_dot_source"]).toBe(child);
      });

      test("moving a file where a grandchild could only reach it through .. is refused, with nothing changed", () => {
        const draft = draftOf(
          WELL_FORMED('k [shape=house stack.child_workflow="shared/check.fabro"]\n start -> k -> exit'),
          { "shared/check.fabro": holding(childNaming("@ticket.md")), "shared/ticket.md": "T" },
        );
        const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "shared/ticket.md", "docs/ticket.md");
        expect(unchanged).toBe(draft);
        expect(diagnostics).toMatchObject([{ severity: "error", code: "builder.rename_refused" }]);
        expect(diagnostics[0].message).toContain("shared/check.fabro");
      });

      test("moving an attached workflow takes its inline child's folder with it, or is refused", () => {
        const draft = draftOf(
          WELL_FORMED('k [shape=house stack.child_workflow="a/check.fabro"]\n start -> k -> exit'),
          { "a/check.fabro": holding(childNaming("@ticket.md")), "a/ticket.md": "T" },
        );
        const sameFolder = renameDraftAsset(draft, "a/check.fabro", "a/renamed.fabro");
        expect(sameFolder.diagnostics).toEqual([]);
        expect(sameFolder.draft.assets["a/renamed.fabro"]).toBe(draft.assets["a/check.fabro"]);
        const deeper = renameDraftAsset(draft, "a/check.fabro", "a/b/check.fabro");
        expect(deeper.draft).toBe(draft);
        expect(deeper.diagnostics[0].code).toBe("builder.rename_refused");
      });

      test("inline source with a syntax error that might name the file blocks the rename", () => {
        for (const broken of ['digraph C { graph [goal="@ticket.md"] start [', holding('digraph C { graph [goal="@ticket.md"] start [')]) {
          const draft = draftOf(rootHolding(broken), { "ticket.md": "T" });
          const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "ticket.md", "renamed.md");
          expect(unchanged).toBe(draft);
          expect(diagnostics).toMatchObject([{ code: "builder.rename_refused" }]);
          expect(diagnostics[0].message).toContain("syntax error");
        }
      });

      test("inside an attached workflow, the same error names that file", () => {
        const draft = draftOf(
          WELL_FORMED('k [shape=house stack.child_workflow="shared/check.fabro"]\n start -> k -> exit'),
          { "shared/check.fabro": holding('digraph C { graph [goal="@ticket.md"] start ['), "shared/ticket.md": "T" },
        );
        const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "shared/ticket.md", "shared/renamed.md");
        expect(unchanged).toBe(draft);
        expect(diagnostics[0].message).toContain("shared/check.fabro");
      });

      test("inline source with a syntax error that can't name the file doesn't block it", () => {
        const draft = draftOf(rootHolding("digraph C { start ["), { "ticket.md": "T" });
        const { draft: renamed, diagnostics } = renameDraftAsset(draft, "ticket.md", "renamed.md");
        expect(diagnostics).toEqual([]);
        expect(effective(parsed(renamed.source), "child")["stack.child_dot_source"]).toBe("digraph C { start [");
      });

      test.each<[string, string]>([
        ["include", `{% include "x.md" %}`],
        ["a trimmed include", `{%- include "x.md" -%}`],
        ["ignore missing", `{% include ignore missing "x.md" %}`],
        ["import", `{% import "x.md" as m %}`],
        ["extends", `{% extends "x.md" %}`],
      ])("a template tag at any depth blocks the rename, since it stays as written: %s", (_name, tag) => {
        const inner = `digraph C { start [shape=Mdiamond] work [prompt=${JSON.stringify(tag)}] exit [shape=Msquare] start -> work -> exit }`;
        for (const source of [inner, holding(inner), holding(holding(inner))]) {
          const draft = draftOf(rootHolding(source), { "ticket.md": "T", "x.md": "X" });
          const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "x.md", "y.md");
          expect(unchanged).toBe(draft);
          expect(diagnostics[0].message).toContain("{% include %}");
        }
      });

      test("a template tag stepping out with .. lands where Petri lands it", () => {
        const draft = draftOf(WELL_FORMED("start -> exit"), { "shared/main.md": '{% include "../partials/x.md" %}', "partials/x.md": "X" });
        const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "partials/x.md", "partials/y.md");
        expect(unchanged).toBe(draft);
        expect(diagnostics[0].message).toContain("shared/main.md");
      });
    });

    describe("with two spellings of a file already in the draft", () => {
      test("renaming an unrelated file is refused rather than losing one of them", () => {
        for (const assets of [
          { "./prompts/a.md": "first", "prompts/a.md": "second", "other.md": "third" },
          { "prompts/a.md": "second", "./prompts/a.md": "first", "other.md": "third" },
          { "other.md": "third", "././prompts/a.md": "first", "prompts/a.md": "second" },
        ] as Record<string, string>[]) {
          const draft: WorkflowDraft = { ...newDraft(), assets };
          const before = structuredClone(assets);
          const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "other.md", "moved.md");
          expect(unchanged).toBe(draft);
          expect(unchanged.assets).toEqual(before);
          expect(diagnostics).toMatchObject([{ severity: "error", code: "builder.path_collision" }]);
          expect(diagnostics[0].message).toContain("prompts/a.md");
          expect(diagnostics[0].message).toContain('"other.md" was not renamed');
        }
      });

      test("renaming either spelling is refused the same way", () => {
        const draft: WorkflowDraft = { ...newDraft(), assets: { "./a.md": "one", "a.md": "two" } };
        for (const from of ["./a.md", "a.md"]) {
          const result = renameDraftAsset(draft, from, "b.md");
          expect(result.draft).toBe(draft);
          expect(result.diagnostics[0].code).toBe("builder.path_collision");
        }
      });

      test("spellings with the same content lose nothing, so the rename goes ahead", () => {
        const draft: WorkflowDraft = { ...newDraft(), assets: { "./a.md": "same", "a.md": "same", "other.md": "third" } };
        const { draft: renamed, diagnostics } = renameDraftAsset(draft, "other.md", "moved.md");
        expect(diagnostics).toEqual([]);
        expect(renamed.assets).toEqual({ "a.md": "same", "moved.md": "third" });
      });

      test("renaming onto another spelling of a taken path is refused", () => {
        const draft: WorkflowDraft = { ...newDraft(), assets: { "./taken.md": "keep me", "other.md": "third" } };
        const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "other.md", "taken.md");
        expect(unchanged).toBe(draft);
        expect(diagnostics[0].code).toBe("builder.path_collision");
      });
    });

    test("refuses to half-rename when the source doesn't parse", () => {
      const broken: WorkflowDraft = { ...draft, source: `${source}\n oops` };
      const { draft: unchanged, diagnostics } = renameDraftAsset(broken, "old.md", "new.md");
      expect(unchanged).toBe(broken);
      expect(diagnostics).toMatchObject([{ severity: "error", code: "builder.unparseable_source" }]);
    });

    test("refuses unsafe, taken or reserved names, and files that aren't there, losing nothing", () => {
      for (const [target, code] of [
        ["../out.md", "builder.unsafe_path"],
        ["/abs.md", "builder.unsafe_path"],
        ["other.md", "builder.path_collision"],
        ["workflow.fabro", "builder.path_collision"],
      ]) {
        const { draft: unchanged, diagnostics } = renameDraftAsset(draft, "old.md", target);
        expect(unchanged).toBe(draft);
        expect(diagnostics).toMatchObject([{ severity: "error", code }]);
      }
      expect(renameDraftAsset(draft, "nope.md", "x.md").diagnostics[0].code).toBe("builder.missing_file");
    });
  });
});
