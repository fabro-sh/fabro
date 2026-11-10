import { bareWordIsString, DotSyntaxError, parseDot } from "./dot-syntax";
import type { AstAttr, AstComment, AstIdent, AstStatement, DotAst, SourcePosition } from "./dot-syntax";
import {
  attributeSlot,
  classFromLabel,
  edgeOwner,
  GRAPH_ATTRS_OWNER,
  GRAPH_EDGE_DEFAULTS_OWNER,
  GRAPH_NODE_DEFAULTS_OWNER,
  groupAttrsOwner,
  groupClass,
  groupEdgeDefaultsOwner,
  groupNodeDefaultsOwner,
  groupsById,
  isOneChoiceKey,
  nodeClasses,
  nodeOwner,
  prefersBareWord,
  setAttr,
  splitClasses,
} from "./effective";
import type {
  Attributes,
  CommentAnchor,
  Diagnostic,
  ParseResult,
  WorkflowEdge,
  WorkflowGraph,
  WorkflowGroup,
  WorkflowNode,
} from "./types";

/**
 * Source text → `WorkflowGraph`.
 *
 * DOT defaults are sequential: a node takes the `node [...]` values in force
 * where it is first mentioned, and an edge the `edge [...]` values where it is
 * written. The model's defaults are scope-wide with no order, because a canvas
 * has no order. So this first simulates DOT exactly as Petri's `model::build`
 * does, then expresses the result in the model's terms: a default stays a
 * default only if it holds for everything in its scope, and any value that
 * would otherwise change is written onto the element. Effective meaning is
 * preserved; each such change is reported as a warning.
 */

interface SimAttr {
  value: string;
  bare: boolean;
  position: SourcePosition;
}

type SimAttrs = Map<string, SimAttr>;

interface SimNode {
  id: string;
  /** What the node's attributes really are, per DOT's order. */
  real: SimAttrs;
  /** What was written on node statements for it. */
  explicit: SimAttrs;
  classes: string[];
  declared: boolean;
  groupId?: string;
  edgeGroupId?: string;
  position?: SourcePosition;
}

interface SimEdge {
  id: string;
  source: string;
  target: string;
  real: SimAttrs;
  explicit: SimAttrs;
  groupId?: string;
  position: SourcePosition;
}

interface SimGroup {
  id: string;
  parentId?: string;
  anonymous: boolean;
  attrs: SimAttrs;
  nodeDefaults: SimAttrs;
  edgeDefaults: SimAttrs;
  /** The class this block gives its nodes, inherited when it has no label. */
  blockClass: string | null;
  position: SourcePosition;
}

function applyBlock(target: SimAttrs, block: AstAttr[]): void {
  if (block.some((attr) => isOneChoiceKey(attr.key))) {
    for (const key of target.keys()) {
      if (isOneChoiceKey(key)) target.delete(key);
    }
  }
  for (const attr of block) {
    target.set(attr.key, { value: attr.value, bare: attr.bare, position: { line: attr.line, column: attr.column } });
  }
}

function overlaySim(target: SimAttrs, layer: SimAttrs): void {
  if ([...layer.keys()].some(isOneChoiceKey)) {
    for (const key of target.keys()) {
      if (isOneChoiceKey(key)) target.delete(key);
    }
  }
  for (const [key, attr] of layer) target.set(key, attr);
}

function addClass(classes: string[], name: string): void {
  const trimmed = name.trim();
  if (trimmed.length > 0 && !classes.includes(trimmed)) classes.push(trimmed);
}

function isStringAttr(attr: SimAttr): boolean {
  return !attr.bare || bareWordIsString(attr.value);
}

function ownLabel(statements: AstStatement[]): AstAttr | undefined {
  for (const statement of statements) {
    if (statement.kind === "graphAttr" && statement.attr.key === "label") return statement.attr;
    if (statement.kind === "graphAttrs") {
      const found = statement.attrs.find((attr) => attr.key === "label");
      if (found) return found;
    }
  }
  return undefined;
}

function collectNames(statements: AstStatement[], declared: Set<string>, groupNames: Set<string>): void {
  for (const statement of statements) {
    if (statement.kind === "node") declared.add(statement.id.name);
    if (statement.kind === "subgraph") {
      if (statement.name) groupNames.add(statement.name.name);
      collectNames(statement.statements, declared, groupNames);
    }
  }
}

function uniqueName(base: string, taken: { has(name: string): boolean }): string {
  if (!taken.has(base)) return base;
  for (let suffix = 2; ; suffix++) {
    const candidate = `${base}_${suffix}`;
    if (!taken.has(candidate)) return candidate;
  }
}

class Simulation {
  readonly nodes = new Map<string, SimNode>();
  readonly edges: SimEdge[] = [];
  readonly groups = new Map<string, SimGroup>();
  readonly graphAttrs: SimAttrs = new Map();
  readonly rootNodeDefaults: SimAttrs = new Map();
  readonly rootEdgeDefaults: SimAttrs = new Map();
  readonly warnings: Diagnostic[] = [];
  /** Edge statements by their first expanded edge, and where each subgraph block landed, for comments. */
  readonly edgeIdByStatement = new Map<AstStatement, string>();
  readonly groupIdByStatement = new Map<AstStatement, string>();
  private nodeDefaults: SimAttrs = new Map();
  private edgeDefaults: SimAttrs = new Map();
  private readonly edgeCounts = new Map<string, number>();
  private readonly declared = new Set<string>();
  private readonly groupNames = new Set<string>();
  private anonymousCount = 0;

  run(ast: DotAst): void {
    collectNames(ast.statements, this.declared, this.groupNames);
    this.walk(ast.statements, null, null);
    for (const node of this.nodes.values()) {
      if (node.groupId === undefined) node.groupId = node.edgeGroupId;
    }
  }

  private ensureNode(id: AstIdent, declared: boolean): SimNode {
    let node = this.nodes.get(id.name);
    if (!node) {
      node = { id: id.name, real: new Map(this.nodeDefaults), explicit: new Map(), classes: [], declared };
      this.nodes.set(id.name, node);
    }
    node.declared ||= declared;
    return node;
  }

  private walk(statements: AstStatement[], blockClass: string | null, group: SimGroup | null): void {
    const savedNodeDefaults = new Map(this.nodeDefaults);
    const savedEdgeDefaults = new Map(this.edgeDefaults);
    for (const statement of statements) {
      switch (statement.kind) {
        case "graphAttrs":
          if (group) this.applyGroupAttrs(group, statement.attrs);
          else applyBlock(this.graphAttrs, statement.attrs);
          break;
        case "graphAttr":
          if (group) this.applyGroupAttrs(group, [statement.attr]);
          else applyBlock(this.graphAttrs, [statement.attr]);
          break;
        case "nodeDefaults":
          applyBlock(this.nodeDefaults, statement.attrs);
          applyBlock(group ? group.nodeDefaults : this.rootNodeDefaults, statement.attrs);
          break;
        case "edgeDefaults":
          applyBlock(this.edgeDefaults, statement.attrs);
          applyBlock(group ? group.edgeDefaults : this.rootEdgeDefaults, statement.attrs);
          break;
        case "node":
          this.nodeStatement(statement, blockClass, group);
          break;
        case "edge":
          this.edgeStatement(statement, blockClass, group);
          break;
        case "subgraph":
          this.subgraphStatement(statement, blockClass, group);
          break;
      }
    }
    this.nodeDefaults = savedNodeDefaults;
    this.edgeDefaults = savedEdgeDefaults;
  }

  private applyGroupAttrs(group: SimGroup, attrs: AstAttr[]): void {
    for (const attr of attrs) {
      const existing = group.attrs.get("label");
      if (attr.key === "label" && existing) {
        if (existing.value !== attr.value) {
          this.warnings.push({
            severity: "warning",
            code: "builder.repeated_group_label",
            message: `Subgraph \`${group.id}\` sets \`label\` more than once. Fabro derives its class from the first, "${existing.value}", so that one was kept and "${attr.value}" was dropped.`,
            line: attr.line,
            column: attr.column,
          });
        }
        continue;
      }
      applyBlock(group.attrs, [attr]);
    }
  }

  private nodeStatement(
    statement: Extract<AstStatement, { kind: "node" }>,
    blockClass: string | null,
    group: SimGroup | null,
  ): void {
    const node = this.ensureNode(statement.id, true);
    node.position ??= { line: statement.id.line, column: statement.id.column };
    if (statement.attrs) {
      applyBlock(node.real, statement.attrs);
      applyBlock(node.explicit, statement.attrs);
    }
    if (blockClass) addClass(node.classes, blockClass);
    const written = node.real.get("class");
    if (written && isStringAttr(written)) {
      for (const name of splitClasses(written.value)) addClass(node.classes, name);
    }
    if (group && node.groupId === undefined) node.groupId = group.id;
  }

  private edgeStatement(
    statement: Extract<AstStatement, { kind: "edge" }>,
    blockClass: string | null,
    group: SimGroup | null,
  ): void {
    for (const id of statement.nodes) {
      const known = this.declared.has(id.name);
      const node = this.ensureNode(id, false);
      if (known && blockClass) addClass(node.classes, blockClass);
      if (known && group && node.edgeGroupId === undefined) node.edgeGroupId = group.id;
    }
    const real: SimAttrs = new Map(this.edgeDefaults);
    const explicit: SimAttrs = new Map();
    if (statement.attrs) {
      applyBlock(real, statement.attrs);
      applyBlock(explicit, statement.attrs);
    }
    for (let index = 0; index + 1 < statement.nodes.length; index++) {
      const source = statement.nodes[index];
      const target = statement.nodes[index + 1];
      const base = `${source.name}->${target.name}`;
      const count = (this.edgeCounts.get(base) ?? 0) + 1;
      this.edgeCounts.set(base, count);
      const id = count === 1 ? base : `${base}#${count}`;
      if (index === 0) this.edgeIdByStatement.set(statement, id);
      this.edges.push({
        id,
        source: source.name,
        target: target.name,
        real: new Map(real),
        explicit: new Map(explicit),
        groupId: group?.id,
        position: { line: source.line, column: source.column },
      });
    }
  }

  private subgraphStatement(
    statement: Extract<AstStatement, { kind: "subgraph" }>,
    inheritedClass: string | null,
    parent: SimGroup | null,
  ): void {
    const label = ownLabel(statement.statements);
    const blockClass = label ? classFromLabel(label.value) : inheritedClass;
    const group = this.resolveGroup(statement, blockClass, parent);
    this.groupIdByStatement.set(statement, group.id);
    this.walk(statement.statements, blockClass, group);
  }

  private resolveGroup(
    statement: Extract<AstStatement, { kind: "subgraph" }>,
    blockClass: string | null,
    parent: SimGroup | null,
  ): SimGroup {
    const position = statement.name
      ? { line: statement.name.line, column: statement.name.column }
      : { line: 1, column: 1 };
    const create = (id: string, anonymous: boolean): SimGroup => {
      const group: SimGroup = {
        id,
        parentId: parent?.id,
        anonymous,
        attrs: new Map(),
        nodeDefaults: new Map(),
        edgeDefaults: new Map(),
        blockClass,
        position,
      };
      this.groups.set(id, group);
      return group;
    };
    if (!statement.name) {
      this.anonymousCount++;
      const taken = { has: (name: string) => this.groups.has(name) || this.groupNames.has(name) };
      return create(uniqueName(`subgraph_${this.anonymousCount}`, taken), true);
    }
    const existing = this.groups.get(statement.name.name);
    if (!existing) return create(statement.name.name, false);
    // Blocks of one name are one group only if they give their contents the same class.
    // Merging others would change which stylesheet rules reach those nodes.
    if (existing.blockClass === blockClass) return existing;
    const taken = { has: (name: string) => this.groups.has(name) || this.groupNames.has(name) };
    const id = uniqueName(statement.name.name, taken);
    this.warnings.push({
      severity: "warning",
      code: "builder.reopened_group",
      message: `Subgraph \`${statement.name.name}\` is opened again with a different label, so its contents get a different class. It was kept as a separate group, \`${id}\`.`,
      ...position,
    });
    return create(id, false);
  }
}

/** Scope-wide defaults, split into what can stay default and what must move onto elements. */
function reconcileDefaults(
  items: { label: string; groupId?: string; real: SimAttrs; explicit: SimAttrs }[],
  root: SimAttrs,
  scopes: Map<string, SimGroup>,
  pick: (group: SimGroup) => SimAttrs,
  what: "node" | "edge",
  warnings: Diagnostic[],
): SimAttrs[] {
  const chainOf = (groupId?: string): SimAttrs[] => {
    const layers: SimAttrs[] = [];
    const seen = new Set<string>();
    let current = groupId;
    while (current !== undefined && !seen.has(current)) {
      seen.add(current);
      const group = scopes.get(current);
      if (!group) break;
      layers.unshift(pick(group));
      current = group.parentId;
    }
    layers.unshift(root);
    return layers;
  };
  const scopeName = (layer: SimAttrs): string => {
    if (layer === root) return "the graph";
    for (const group of scopes.values()) {
      if (pick(group) === layer) return `subgraph \`${group.id}\``;
    }
    return "its scope";
  };
  const effective = (item: (typeof items)[number]): SimAttrs => {
    const merged: SimAttrs = new Map();
    for (const layer of chainOf(item.groupId)) overlaySim(merged, layer);
    overlaySim(merged, item.explicit);
    return merged;
  };

  // A default some element in scope really lacks cannot stay: nothing can unset it.
  const demoted = new Set<string>();
  for (let changed = true; changed; ) {
    changed = false;
    for (const item of items) {
      for (const key of effective(item).keys()) {
        if (item.real.has(key)) continue;
        const chain = chainOf(item.groupId);
        for (let index = chain.length - 1; index >= 0; index--) {
          const attr = chain[index].get(key);
          if (!attr) continue;
          chain[index].delete(key);
          demoted.add(key);
          warnings.push({
            severity: "warning",
            code: "builder.default_materialized",
            message: `\`${what} [${key}=...]\` in ${scopeName(chain[index])} does not reach every ${what} in that scope because of statement order. The builder's defaults are scope-wide, so the value was moved onto the ${what}s that actually have it. Meaning is unchanged.`,
            ...attr.position,
          });
          changed = true;
          break;
        }
      }
    }
  }

  // Whatever still differs goes onto the element.
  const moved = new Map<string, string[]>();
  const results: SimAttrs[] = items.map((item) => {
    const attrs: SimAttrs = new Map(item.explicit);
    const inherited = effective(item);
    for (const [key, attr] of item.real) {
      const current = inherited.get(key);
      if (current && current.value === attr.value && current.bare === attr.bare) continue;
      attrs.set(key, attr);
      if (!item.explicit.has(key) && !demoted.has(key)) {
        moved.set(key, [...(moved.get(key) ?? []), item.label]);
      }
    }
    // Writing one half of a one-choice pair drops the inherited other half, so write both.
    if ([...attrs.keys()].some(isOneChoiceKey)) {
      for (const [key, attr] of item.real) {
        if (isOneChoiceKey(key)) attrs.set(key, attr);
      }
    }
    return attrs;
  });
  for (const [key, labels] of moved) {
    warnings.push({
      severity: "warning",
      code: "builder.default_materialized",
      message: `\`${key}\` differs from the default in scope on ${labels.map((label) => `\`${label}\``).join(", ")} because of statement order. The value it really has was written onto it. Meaning is unchanged.`,
    });
  }
  return results;
}

interface SpellingRecord {
  bare: string[];
  quoted: string[];
}

function toAttributes(sim: SimAttrs, owner: string, spelling: SpellingRecord): Attributes {
  const attrs: Attributes = {};
  for (const [key, attr] of sim) {
    setAttr(attrs, key, attr.value);
    if (attr.bare) spelling.bare.push(attributeSlot(owner, key));
    else if (prefersBareWord(key, attr.value)) spelling.quoted.push(attributeSlot(owner, key));
  }
  return attrs;
}

function anchorComments(
  ast: DotAst,
  simulation: Simulation,
): { comment: string; anchor: CommentAnchor }[] {
  const beforeStatement = (statement: AstStatement, owner: string | null): CommentAnchor => {
    switch (statement.kind) {
      case "node":
        return { kind: "node", id: statement.id.name };
      case "edge":
        return { kind: "edge", id: simulation.edgeIdByStatement.get(statement) ?? "" };
      case "subgraph":
        return { kind: "group", id: simulation.groupIdByStatement.get(statement) ?? "", position: "before" };
      default:
        return owner === null ? { kind: "top" } : { kind: "group", id: owner, position: "inside" };
    }
  };
  const trailingOf = (statement: AstStatement): CommentAnchor | null => {
    if (statement.kind === "node") return { kind: "node", id: statement.id.name, trailing: true };
    if (statement.kind === "edge") {
      return { kind: "edge", id: simulation.edgeIdByStatement.get(statement) ?? "", trailing: true };
    }
    return null;
  };
  const locate = (comment: AstComment, statements: AstStatement[], owner: string | null): CommentAnchor => {
    let previous: AstStatement | null = null;
    for (const statement of statements) {
      const index = comment.tokenIndex;
      if (index > statement.start && index < statement.end) {
        if (statement.kind !== "subgraph") return beforeStatement(statement, owner);
        const id = simulation.groupIdByStatement.get(statement) ?? "";
        return locate(comment, statement.statements, id);
      }
      if (index <= statement.start) {
        const trailing = previous && comment.sameLineAsPrevious ? trailingOf(previous) : null;
        return trailing ?? beforeStatement(statement, owner);
      }
      previous = statement;
    }
    const trailing = previous && comment.sameLineAsPrevious ? trailingOf(previous) : null;
    if (trailing) return trailing;
    return owner === null ? { kind: "end" } : { kind: "group", id: owner, position: "end" };
  };
  return ast.comments.map((comment) => {
    if (comment.tokenIndex === 0) return { comment: comment.text, anchor: { kind: "header" } };
    if (comment.tokenIndex >= ast.tokenCount) return { comment: comment.text, anchor: { kind: "footer" } };
    return { comment: comment.text, anchor: locate(comment, ast.statements, null) };
  });
}

function buildGraph(ast: DotAst): { graph: WorkflowGraph; warnings: Diagnostic[] } {
  const simulation = new Simulation();
  simulation.run(ast);
  const warnings = simulation.warnings;
  const declaredNodes = [...simulation.nodes.values()].filter((node) => node.declared);

  const nodeAttrs = reconcileDefaults(
    declaredNodes.map((node) => ({ label: node.id, groupId: node.groupId, real: node.real, explicit: node.explicit })),
    simulation.rootNodeDefaults,
    simulation.groups,
    (group) => group.nodeDefaults,
    "node",
    warnings,
  );
  const edgeAttrs = reconcileDefaults(
    simulation.edges.map((edge) => ({
      label: `${edge.source} -> ${edge.target}`,
      groupId: edge.groupId,
      real: edge.real,
      explicit: edge.explicit,
    })),
    simulation.rootEdgeDefaults,
    simulation.groups,
    (group) => group.edgeDefaults,
    "edge",
    warnings,
  );

  const spelling: SpellingRecord = { bare: [], quoted: [] };
  const groups: WorkflowGroup[] = [...simulation.groups.values()].map((group) => {
    const out: WorkflowGroup = {
      id: group.id,
      attrs: toAttributes(group.attrs, groupAttrsOwner(group.id), spelling),
      nodeDefaults: toAttributes(group.nodeDefaults, groupNodeDefaultsOwner(group.id), spelling),
      edgeDefaults: toAttributes(group.edgeDefaults, groupEdgeDefaultsOwner(group.id), spelling),
    };
    if (group.parentId !== undefined) out.parentId = group.parentId;
    if (group.anonymous) out.anonymous = true;
    return out;
  });
  const nodes: WorkflowNode[] = declaredNodes.map((node, index) => {
    const out: WorkflowNode = { id: node.id, attrs: toAttributes(nodeAttrs[index], nodeOwner(node.id), spelling) };
    if (node.groupId !== undefined) out.groupId = node.groupId;
    return out;
  });
  const edges: WorkflowEdge[] = simulation.edges.map((edge, index) => {
    const out: WorkflowEdge = {
      id: edge.id,
      source: edge.source,
      target: edge.target,
      attrs: toAttributes(edgeAttrs[index], edgeOwner(edge.id), spelling),
    };
    if (edge.groupId !== undefined) out.groupId = edge.groupId;
    return out;
  });

  const locations: Record<string, { line: number; column: number }> = {};
  for (const node of declaredNodes) {
    if (node.position) setAttrLocation(locations, nodeOwner(node.id), node.position);
  }
  for (const edge of simulation.edges) setAttrLocation(locations, edgeOwner(edge.id), edge.position);

  const graph: WorkflowGraph = {
    name: ast.name.name,
    attrs: toAttributes(simulation.graphAttrs, GRAPH_ATTRS_OWNER, spelling),
    nodeDefaults: toAttributes(simulation.rootNodeDefaults, GRAPH_NODE_DEFAULTS_OWNER, spelling),
    edgeDefaults: toAttributes(simulation.rootEdgeDefaults, GRAPH_EDGE_DEFAULTS_OWNER, spelling),
    nodes,
    edges,
    groups,
    comments: ast.comments.map((comment) => comment.text),
    sourceMeta: { bare: spelling.bare, quoted: spelling.quoted, commentAnchors: anchorComments(ast, simulation), locations },
  };

  // Fabro keeps a running list of classes per node: the group's on each mention, plus
  // every value `class` ever had. The model derives them from the group and the final
  // `class` value, so any it would lose, or put in another order, go onto `class`.
  const groupsMap = groupsById(graph);
  declaredNodes.forEach((node, index) => {
    const modelled = nodeClasses(graph, nodes[index]);
    const missing = node.classes.filter((name) => !modelled.includes(name));
    if (missing.length > 0 || modelled[0] !== node.classes[0]) {
      const fromGroup = groupClass(groupsMap, nodes[index].groupId);
      const rest = node.classes.filter((name) => name !== fromGroup);
      const written = node.real.get("class");
      if (rest.length > 0 || (written && !isStringAttr(written))) {
        // A bare `class=5` was never a class, so it doesn't survive into the new value.
        if (rest.length > 0) setAttr(nodes[index].attrs, "class", rest.join(" "));
      }
    }
    const final = nodeClasses(graph, nodes[index]);
    if (node.classes.length > 0 && final[0] !== node.classes[0]) {
      warnings.push({
        severity: "warning",
        code: "builder.class_order_changed",
        message: `Node \`${node.id}\` was first given class \`${node.classes[0]}\`, but its group now puts \`${final[0]}\` first. A node's first class names its default thread, so check any thread_id that relies on it.`,
        nodeId: node.id,
        ...(node.position ?? {}),
      });
    }
  });

  return { graph, warnings };
}

function setAttrLocation(
  locations: Record<string, { line: number; column: number }>,
  key: string,
  position: SourcePosition,
): void {
  Object.defineProperty(locations, key, {
    value: { line: position.line, column: position.column },
    enumerable: true,
    writable: true,
    configurable: true,
  });
}

/**
 * Parse Fabro source. On a syntax error `graph` is null and the one diagnostic
 * carries the position; there is never a partial graph. A graph that parses
 * can still be structurally wrong: that is `validateWorkflow`'s job.
 */
export function parseWorkflow(source: string): ParseResult {
  let ast: DotAst;
  try {
    ast = parseDot(source);
  } catch (error) {
    if (error instanceof DotSyntaxError) {
      return {
        graph: null,
        diagnostics: [
          { severity: "error", code: error.code, message: error.message, line: error.line, column: error.column },
        ],
      };
    }
    if (error instanceof RangeError) {
      return {
        graph: null,
        diagnostics: [{ severity: "error", code: "dot.syntax", message: "The source is too deeply nested to parse." }],
      };
    }
    throw error;
  }
  const { graph, warnings } = buildGraph(ast);
  return { graph, diagnostics: warnings };
}
