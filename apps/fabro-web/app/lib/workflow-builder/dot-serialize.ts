import { asciiLower } from "./dot-syntax";
import {
  attributeSlot,
  edgeOwner,
  getAttr,
  GRAPH_ATTRS_OWNER,
  GRAPH_EDGE_DEFAULTS_OWNER,
  GRAPH_NODE_DEFAULTS_OWNER,
  groupAttrsOwner,
  groupChain,
  groupEdgeDefaultsOwner,
  groupNodeDefaultsOwner,
  groupsById,
  nodeOwner,
  prefersBareWord,
} from "./effective";
import type { Attributes, CommentAnchor, WorkflowEdge, WorkflowGraph, WorkflowGroup, WorkflowNode } from "./types";

/**
 * `WorkflowGraph` → source text.
 *
 * The order of the output carries meaning, because DOT reads it sequentially:
 *
 * - Defaults come first in their scope, so they reach everything in it.
 * - Nodes are written in `graph.nodes` order, because Petri lists a run's stages
 *   in that order when it builds a prompt's preamble, and `summary:low` keeps only
 *   the last ones. A group that isn't contiguous in that order is reopened.
 * - Every declared node is written before any edge names it. A node first seen
 *   in an edge would take the defaults in force at that edge, not its own group's.
 * - An edge inside a group stays there only if that keeps both endpoints already
 *   declared and keeps its place among its source's and target's edges, since
 *   edge order decides human-gate choice order and parallel branch indexes.
 *   Otherwise the group is reopened, with its label, after all node declarations.
 */

const LINE_WIDTH = 100;
const DOT_KEYWORDS = new Set(["graph", "digraph", "subgraph", "node", "edge", "strict"]);
interface Spelling {
  bare: Set<string>;
  quoted: Set<string>;
}

/** Quote for the Fabro lexer. Newlines stay literal so long prompts and scripts remain readable. */
function quote(value: string): string {
  return `"${value.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\t/g, "\\t")}"`;
}

function formatId(id: string): string {
  if (/^[A-Za-z_][A-Za-z0-9_.-]*$/.test(id) && !DOT_KEYWORDS.has(asciiLower(id))) return id;
  return quote(id);
}

function formatValue(key: string, value: string, slot: string, spelling: Spelling): string {
  if (spelling.bare.has(slot) && /^[A-Za-z0-9_.+-]+$/.test(value) && !value.startsWith("--")) return value;
  if (!spelling.quoted.has(slot) && prefersBareWord(key, value)) return value;
  return quote(value);
}

function formatComment(text: string): string[] {
  const singleLine = !text.includes("\n");
  if (singleLine && (text.startsWith("//") || text.startsWith("#"))) return [text];
  if (text.startsWith("/*") && text.endsWith("*/") && text.length >= 4 && !text.slice(2, -2).includes("*/")) {
    return [text];
  }
  return text.split(/\r?\n/).map((line) => (line.length > 0 ? `// ${line}` : "//"));
}

class CommentBook {
  private readonly buckets = new Map<string, string[]>();

  constructor(graph: WorkflowGraph) {
    const nodeIds = new Set(graph.nodes.map((node) => node.id));
    const edgeIds = new Set(graph.edges.map((edge) => edge.id));
    const groupIds = new Set(graph.groups.map((group) => group.id));
    const anchored = new Map<string, CommentAnchor[]>();
    for (const { comment, anchor } of graph.sourceMeta?.commentAnchors ?? []) {
      anchored.set(comment, [...(anchored.get(comment) ?? []), anchor]);
    }
    const isLive = (anchor: CommentAnchor): boolean => {
      if (anchor.kind === "node") return nodeIds.has(anchor.id);
      if (anchor.kind === "edge") return edgeIds.has(anchor.id);
      if (anchor.kind === "group") return groupIds.has(anchor.id);
      return true;
    };
    for (const comment of graph.comments ?? []) {
      const anchor = anchored.get(comment)?.shift();
      const bucket = this.keyOf(anchor && isLive(anchor) ? anchor : { kind: "top" });
      this.buckets.set(bucket, [...(this.buckets.get(bucket) ?? []), comment]);
    }
  }

  private keyOf(anchor: CommentAnchor): string {
    switch (anchor.kind) {
      case "node":
      case "edge":
        return `${anchor.kind}:${anchor.trailing ? "trailing" : "before"}:${anchor.id}`;
      case "group":
        return `group:${anchor.position}:${anchor.id}`;
      default:
        return anchor.kind;
    }
  }

  take(anchor: CommentAnchor): string[] {
    const key = this.keyOf(anchor);
    const comments = this.buckets.get(key) ?? [];
    this.buckets.delete(key);
    return comments;
  }

  /** Anything never reached, so a comment can only move, not vanish. */
  takeRest(): string[] {
    const rest = [...this.buckets.values()].flat();
    this.buckets.clear();
    return rest;
  }
}

/**
 * The order to write nodes in. Nodes read from source keep their exact place. A
 * node added on the canvas has no place yet, so it joins the rest of its group
 * instead of forcing that group to be reopened.
 */
function nodeWriteOrder(graph: WorkflowGraph, groups: Map<string, WorkflowGroup>): WorkflowNode[] {
  const locations = graph.sourceMeta?.locations ?? {};
  const ordered: WorkflowNode[] = [];
  for (const node of graph.nodes) {
    if (node.groupId === undefined || Object.hasOwn(locations, nodeOwner(node.id))) {
      ordered.push(node);
      continue;
    }
    const inSameGroup = (other: WorkflowNode) =>
      groupChain(groups, other.groupId).some((group) => group.id === node.groupId);
    let last = ordered.length - 1;
    while (last >= 0 && !inSameGroup(ordered[last])) last--;
    if (last < 0) ordered.push(node);
    else ordered.splice(last + 1, 0, node);
  }
  return ordered;
}

class Writer {
  private readonly lines: string[] = [];
  private readonly spelling: Spelling;

  constructor(graph: WorkflowGraph) {
    this.spelling = { bare: new Set(graph.sourceMeta?.bare), quoted: new Set(graph.sourceMeta?.quoted) };
  }

  line(depth: number, text: string): void {
    this.lines.push(`${"  ".repeat(depth)}${text}`);
  }

  comments(depth: number, comments: string[]): void {
    for (const comment of comments) {
      for (const line of formatComment(comment)) this.line(depth, line);
    }
  }

  private attrParts(attrs: Attributes, owner: string): string[] {
    return Object.entries(attrs).map(
      ([key, value]) => `${formatId(key)}=${formatValue(key, value, attributeSlot(owner, key), this.spelling)}`,
    );
  }

  /** `head [attrs]` on one line when short, one attribute per line otherwise. */
  statement(depth: number, head: string, attrs: Attributes, owner: string, trailing: string[] = []): void {
    const parts = this.attrParts(attrs, owner);
    const suffix = this.trailingText(trailing);
    if (parts.length === 0) {
      this.line(depth, `${head}${suffix}`);
      return;
    }
    const inline = `${head} [${parts.join(", ")}]`;
    if (!inline.includes("\n") && inline.length + depth * 2 <= LINE_WIDTH) {
      this.line(depth, `${inline}${suffix}`);
      return;
    }
    this.line(depth, `${head} [`);
    parts.forEach((part, index) => this.line(depth + 1, `${part}${index < parts.length - 1 ? "," : ""}`));
    this.line(depth, `]${suffix}`);
  }

  /** A trailing `#` line is not a comment to the lexer, so only `//` and block comments can trail. */
  private trailingText(comments: string[]): string {
    return comments
      .flatMap(formatComment)
      .map((text) => ` ${text.startsWith("#") ? `// ${text}` : text}`)
      .join("");
  }

  /** Bare `key=value` lines: how a group's own attributes are written. */
  assignments(depth: number, attrs: Attributes, owner: string): void {
    for (const part of this.attrParts(attrs, owner)) this.line(depth, part);
  }

  toString(): string {
    return `${this.lines.join("\n")}\n`;
  }
}

function edgeHead(edge: WorkflowEdge): string {
  return `${formatId(edge.source)} -> ${formatId(edge.target)}`;
}

export function serializeWorkflow(graph: WorkflowGraph): string {
  const first = writeWorkflow(graph, new Set());
  // Reopening a group means writing its name, and an anonymous group has none. If that
  // happens, write the whole thing again with those groups named, so both blocks agree.
  const needNames = new Set([...first.reopened].filter((id) => graph.groups.find((group) => group.id === id)?.anonymous));
  return needNames.size > 0 ? writeWorkflow(graph, needNames).text : first.text;
}

function writeWorkflow(graph: WorkflowGraph, forceNamed: Set<string>): { text: string; reopened: Set<string> } {
  const reopened = new Set<string>();
  const writer = new Writer(graph);
  const book = new CommentBook(graph);
  const groups = groupsById(graph);
  const order = nodeWriteOrder(graph, groups);
  const declaredNodes = new Set(graph.nodes.map((node) => node.id));
  const writtenNodes = new Set<string>();

  const edgesBySource = new Map<string, number[]>();
  const edgesByTarget = new Map<string, number[]>();
  graph.edges.forEach((edge, index) => {
    edgesBySource.set(edge.source, [...(edgesBySource.get(edge.source) ?? []), index]);
    edgesByTarget.set(edge.target, [...(edgesByTarget.get(edge.target) ?? []), index]);
  });
  const writtenEdges = new Set<number>();
  const edgesInGroup = new Map<string, number[]>();
  graph.edges.forEach((edge, index) => {
    if (edge.groupId !== undefined && groups.has(edge.groupId)) {
      edgesInGroup.set(edge.groupId, [...(edgesInGroup.get(edge.groupId) ?? []), index]);
    }
  });

  const isNext = (queue: number[] | undefined, index: number): boolean =>
    (queue ?? []).find((candidate) => !writtenEdges.has(candidate)) === index;

  const canWriteInPlace = (index: number): boolean => {
    const edge = graph.edges[index];
    const declaredReady = (id: string) => !declaredNodes.has(id) || writtenNodes.has(id);
    return (
      declaredReady(edge.source) &&
      declaredReady(edge.target) &&
      isNext(edgesBySource.get(edge.source), index) &&
      isNext(edgesByTarget.get(edge.target), index)
    );
  };

  const writeEdge = (depth: number, index: number) => {
    const edge = graph.edges[index];
    writtenEdges.add(index);
    writer.comments(depth, book.take({ kind: "edge", id: edge.id }));
    writer.statement(
      depth,
      edgeHead(edge),
      edge.attrs,
      edgeOwner(edge.id),
      book.take({ kind: "edge", id: edge.id, trailing: true }),
    );
  };

  const writeNode = (depth: number, node: WorkflowNode) => {
    writtenNodes.add(node.id);
    writer.comments(depth, book.take({ kind: "node", id: node.id }));
    writer.statement(
      depth,
      formatId(node.id),
      node.attrs,
      nodeOwner(node.id),
      book.take({ kind: "node", id: node.id, trailing: true }),
    );
  };

  const writeDefaults = (depth: number, nodeDefaults: Attributes, nodeSlot: string, edgeDefaults: Attributes, edgeSlot: string) => {
    if (Object.keys(nodeDefaults).length > 0) writer.statement(depth, "node", nodeDefaults, nodeSlot);
    if (Object.keys(edgeDefaults).length > 0) writer.statement(depth, "edge", edgeDefaults, edgeSlot);
  };

  // The groups currently open, outermost first, and which have been opened or finished so far.
  const open: WorkflowGroup[] = [];
  const opened = new Set<string>();
  const finished = new Set<string>();
  const lastNodeInGroup = new Map<string, number>();
  order.forEach((node, index) => {
    for (const group of groupChain(groups, node.groupId)) lastNodeInGroup.set(group.id, index);
  });

  const enterGroup = (group: WorkflowGroup, writingNodes: boolean) => {
    const depth = open.length + 1;
    const inner = depth + 1;
    if (opened.has(group.id)) {
      // The same label, or the parser would read this block as a different group.
      reopened.add(group.id);
      writer.line(depth, `subgraph ${formatId(group.id)} {`);
      const label = getAttr(group.attrs, "label");
      if (label !== undefined) writer.assignments(inner, { label }, groupAttrsOwner(group.id));
      writeDefaults(
        inner,
        writingNodes ? group.nodeDefaults : {},
        groupNodeDefaultsOwner(group.id),
        group.edgeDefaults,
        groupEdgeDefaultsOwner(group.id),
      );
    } else {
      opened.add(group.id);
      writer.comments(depth, book.take({ kind: "group", id: group.id, position: "before" }));
      writer.line(depth, group.anonymous && !forceNamed.has(group.id) ? "subgraph {" : `subgraph ${formatId(group.id)} {`);
      writer.comments(inner, book.take({ kind: "group", id: group.id, position: "inside" }));
      writer.assignments(inner, group.attrs, groupAttrsOwner(group.id));
      writeDefaults(
        inner,
        group.nodeDefaults,
        groupNodeDefaultsOwner(group.id),
        group.edgeDefaults,
        groupEdgeDefaultsOwner(group.id),
      );
    }
    open.push(group);
  };

  const closeDownTo = (keep: number, nextNode: number) => {
    while (open.length > keep) {
      const group = open[open.length - 1];
      const inner = open.length + 1;
      if (!finished.has(group.id) && (lastNodeInGroup.get(group.id) ?? -1) < nextNode) {
        finished.add(group.id);
        for (const index of edgesInGroup.get(group.id) ?? []) {
          if (canWriteInPlace(index)) writeEdge(inner, index);
        }
        writer.comments(inner, book.take({ kind: "group", id: group.id, position: "end" }));
      }
      open.pop();
      writer.line(open.length + 1, "}");
    }
  };

  const moveToGroups = (chain: WorkflowGroup[], nextNode: number, writingNodes: boolean) => {
    let shared = 0;
    while (shared < chain.length && shared < open.length && chain[shared] === open[shared]) shared++;
    closeDownTo(shared, nextNode);
    for (const group of chain.slice(shared)) enterGroup(group, writingNodes);
  };

  const writeNodes = () => {
    order.forEach((node, index) => {
      moveToGroups(groupChain(groups, node.groupId), index, true);
      writeNode(open.length + 1, node);
    });
    // Groups with no nodes, and the settings of a duplicate id, which is `validateWorkflow`'s error.
    for (const group of graph.groups) {
      if (groups.get(group.id) !== group) {
        moveToGroups([], Number.POSITIVE_INFINITY, true);
        writer.line(1, `subgraph ${formatId(group.id)} {`);
        writer.assignments(2, group.attrs, groupAttrsOwner(group.id));
        writeDefaults(2, group.nodeDefaults, groupNodeDefaultsOwner(group.id), group.edgeDefaults, groupEdgeDefaultsOwner(group.id));
        writer.line(1, "}");
      } else if (!opened.has(group.id)) {
        moveToGroups(groupChain(groups, group.id), Number.POSITIVE_INFINITY, true);
      }
    }
    closeDownTo(0, Number.POSITIVE_INFINITY);
  };

  /** What's left, in order: root edges as they are, group edges inside a reopened group with its edge defaults. */
  const writeRemainingEdges = () => {
    graph.edges.forEach((edge, index) => {
      if (writtenEdges.has(index)) return;
      moveToGroups(groupChain(groups, edge.groupId), Number.POSITIVE_INFINITY, false);
      writeEdge(open.length + 1, index);
    });
    closeDownTo(0, Number.POSITIVE_INFINITY);
  };

  writer.comments(0, book.take({ kind: "header" }));
  writer.line(0, graph.name.length > 0 ? `digraph ${formatId(graph.name)} {` : "digraph {");
  writer.comments(1, book.take({ kind: "top" }));
  if (Object.keys(graph.attrs).length > 0) writer.statement(1, "graph", graph.attrs, GRAPH_ATTRS_OWNER);
  writeDefaults(1, graph.nodeDefaults, GRAPH_NODE_DEFAULTS_OWNER, graph.edgeDefaults, GRAPH_EDGE_DEFAULTS_OWNER);
  writeNodes();
  writeRemainingEdges();
  writer.comments(1, book.take({ kind: "end" }));
  const footer = book.take({ kind: "footer" });
  writer.comments(1, book.takeRest());
  writer.line(0, "}");
  writer.comments(0, footer);
  return { text: writer.toString(), reopened };
}
