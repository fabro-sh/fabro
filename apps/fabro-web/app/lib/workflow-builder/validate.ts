import { parseWorkflow } from "./dot-parse";
import {
  edgeOwner,
  effectiveEdgeAttributes,
  effectiveNodeAttributes,
  getAttr,
  groupsById,
  hasAttr,
  nodeClasses,
  nodeOwner,
  setAttr,
} from "./effective";
import {
  checkCondition,
  hasTemplateSyntax,
  parseStylesheet,
  selectorSpecificity,
  STYLESHEET_PROPERTIES,
} from "./expressions";
import type { StyleRule } from "./expressions";
import { getNodeKind, isExitNode, isKnownShape, isKnownType, isStartNode, shapeOf } from "./kinds";
import { bundledFiles, directoryOf, resolveFileReference, workflowFileReferences } from "./dependencies";
import { getFile, normalizeAssetPath } from "./prompts";
import type { Attributes, Diagnostic, WorkflowEdge, WorkflowGraph, WorkflowNode } from "./types";

/**
 * Structural checks a browser can honestly make, ported rule by rule from what
 * Petri's lowering enforces (`crates/attractor/frontend/src/lower/`; codes match).
 *
 * In scope: graph shape, node and edge attributes, conditions, stylesheet syntax,
 * and whether named files are present in this draft.
 *
 * Out of scope, because they need the server: the model catalog, template
 * rendering against run inputs, the contents of imports and child workflows that
 * are not in the draft, and `workflow.toml` settings. A clean result here does
 * not promise that `fabro validate` or run admission will pass.
 */

const LAYOUT_ATTRS = new Set(
  "arrowhead arrowsize arrowtail bgcolor center clusterrank color comment compound concentrate constraint decorate dir distortion dpi fillcolor fixedsize fontcolor fontname fontsize group headport height href id image imagescale labelangle labeldistance labelfloat labeljust labelloc layout lhead ltail margin mclimit minlen newrank nodesep nojustify nslimit ordering orientation outputorder overlap pad page pencolor penwidth peripheries pos rank rankdir ranksep ratio regular remincross rotate samehead sametail searchsize shapefile sides size skew splines style tailport target tooltip URL width xlabel".split(
    " ",
  ),
);
const GRAPH_ATTRS =
  "goal default_thread label backend model_stylesheet default_max_retries retry_target fallback_retry_target on_failure on_retries_exhausted default_fidelity default_model default_permissions default_provider max_node_visits selection acp.command acp.config stall_timeout loop_restart_signature_limit".split(
    " ",
  );
const NODE_ATTRS =
  "label shape type backend prompt script language for_each max_parallel stdin_source output_schema output_retries max_retries retry_policy max_visits goal_gate retry_target fallback_retry_target on_failure on_retries_exhausted allow_partial fidelity thread_id project_memory permissions speed max_tokens timeout model provider reasoning_effort acp.command acp.config selection class question_type sensitive review_target human.default_choice duration stack.child_workflow stack.child_dot_source manager.max_cycles manager.poll_interval manager.stop_condition".split(
    " ",
  );
const EDGE_ATTRS = "label condition weight fidelity thread_id loop_restart freeform human.description human.preview".split(
  " ",
);
const LEGACY_ATTRS = new Set(["llm_prompt", "is_codergen", "node_type"]);
const IMPORT_PLACEHOLDER_ATTRS = new Set([
  "model",
  "provider",
  "reasoning_effort",
  "speed",
  "backend",
  "permissions",
  "acp.command",
  "acp.config",
  "fidelity",
  "max_retries",
  "thread_id",
]);
const IMPORT_BOUNDARY_ATTRS = ["condition", "label", "weight", "fidelity", "thread_id", "loop_restart", "freeform"];
const FIDELITIES = ["full", "truncate", "compact", "summary:low", "summary:medium", "summary:high"];
const QUESTION_TYPES = ["yes_no", "confirmation", "multiple_choice", "multi_select", "freeform"];
const RETRY_POLICIES = ["none", "standard", "aggressive", "linear", "patient"];
const FAILURE_POLICIES = ["route", "exit", "succeed", "partially_succeed"];
const RANK_DIRECTIONS = ["TB", "LR", "BT", "RL"];
const RESERVED_KEYWORDS = new Set(["graph", "digraph", "subgraph", "node", "edge", "strict", "if"]);
const API_ONLY_ATTRS = ["model", "provider", "reasoning_effort", "max_tokens", "speed"];
const GOAL_CHECK_NODE = "goal_check";
const MAX_FIRINGS = 500;
const MAX_OUTPUT_RETRIES = 100;
const MAX_CHILD_DEPTH = 3;
const VALID_NODE_ID = /^[A-Za-z_][A-Za-z0-9_]*$/;
const PLACEHOLDER_VALUE =
  /^(replace[_-]?(with|me)|change[_-]?me|todo|fixme|tbd|placeholder|your[_-])|^<.+>$|^\[.+\]$/i;

/** Attributes each handler reads; on any other kind they do nothing. Petri's `HANDLER_SPECIFIC`. */
const HANDLER_SPECIFIC: [string, string[]][] = [
  ["script", ["command"]],
  ["language", ["command"]],
  ["stdin_source", ["command"]],
  ["duration", ["wait"]],
  ["max_parallel", ["parallel"]],
  ["output_retries", ["agent", "prompt", "parallel.fan_in"]],
  ["output_schema", ["agent", "prompt", "parallel.fan_in", "command"]],
  ["prompt", ["agent", "prompt", "parallel.fan_in"]],
  ["review_target", ["human"]],
];

function isValidDuration(text: string): boolean {
  return /^\d+(ms|s|m|h|d)$/.test(text.trim());
}

function readsAsInteger(text: string): boolean {
  return /^[+-]?\d+$/.test(text.trim());
}

function editDistance(a: string, b: string): number {
  let previous = Array.from({ length: b.length + 1 }, (_, index) => index);
  for (let i = 0; i < a.length; i++) {
    const current = [i + 1];
    for (let j = 0; j < b.length; j++) {
      current[j + 1] = Math.min(previous[j] + (a[i] === b[j] ? 0 : 1), previous[j + 1] + 1, current[j] + 1);
    }
    previous = current;
  }
  return previous[b.length];
}

function closest(key: string, known: string[]): string | undefined {
  return known
    .filter((candidate) => candidate !== key && editDistance(key, candidate) <= 2)
    .sort((a, b) => editDistance(key, a) - editDistance(key, b) || a.length - b.length)[0];
}

/** Does this split into a non-empty command line? Enough of shlex to catch what Petri refuses. */
function commandLineProblem(command: string): string | null {
  let quote: string | null = null;
  let hasWord = false;
  for (let index = 0; index < command.length; index++) {
    const char = command[index];
    if (quote) {
      if (char === "\\" && quote === '"') index++;
      else if (char === quote) quote = null;
    } else if (char === "'" || char === '"') {
      quote = char;
      hasWord = true;
    } else if (char === "\\") {
      index++;
      hasWord = true;
    } else if (!/\s/.test(char)) {
      hasWord = true;
    }
  }
  if (quote) return "has unbalanced quotes";
  return hasWord ? null : "is empty";
}

function acceleratorKey(label: string): string {
  const trimmed = label.trim();
  if (trimmed.startsWith("[")) {
    const end = trimmed.indexOf("]");
    if (end > 1) return trimmed.slice(1, end);
  }
  for (const separator of [")", " - "]) {
    const position = trimmed.indexOf(separator);
    if (position >= 1 && position <= 3 && /^[\p{L}\p{N}]+$/u.test(trimmed.slice(0, position))) {
      return trimmed.slice(0, position);
    }
  }
  return Array.from(trimmed)[0] ?? "";
}

/** Fabro's boundary rules for an imported file (`validate_imported`), or null when it satisfies them. */
function importContractProblem(graph: WorkflowGraph): string | null {
  const isStart = (id: string, attrs: Attributes) => getAttr(attrs, "shape") === "Mdiamond" || id === "start" || id === "Start";
  const isExit = (id: string, attrs: Attributes) =>
    getAttr(attrs, "shape") === "Msquare" || id === "exit" || id === "Exit" || id === "end" || id === "End";
  const nodes = graph.nodes.map((node) => ({ id: node.id, attrs: effectiveNodeAttributes(graph, node) }));
  const starts = nodes.filter((node) => isStart(node.id, node.attrs));
  const exits = nodes.filter((node) => isExit(node.id, node.attrs));
  if (starts.length !== 1) return `it must have exactly one start node, found ${starts.length}`;
  if (exits.length !== 1) return `it must have exactly one exit node, found ${exits.length}`;
  const start = starts[0].id;
  const exit = exits[0].id;
  const hasBody = nodes.some((node) => !isStart(node.id, node.attrs) && !isExit(node.id, node.attrs));
  const carriesMeaning = (edge: WorkflowEdge) => {
    const attrs = effectiveEdgeAttributes(graph, edge);
    return IMPORT_BOUNDARY_ATTRS.some((key) => hasAttr(attrs, key));
  };
  if (graph.edges.some((edge) => edge.target === start)) return `its start node \`${start}\` must not have incoming edges`;
  if (graph.edges.some((edge) => edge.source === exit)) return `its exit node \`${exit}\` must not have outgoing edges`;
  const fromStart = graph.edges.filter((edge) => edge.source === start);
  if (fromStart.length !== 1) return `its start node \`${start}\` must have exactly one successor`;
  if (carriesMeaning(fromStart[0])) return `its edge \`${fromStart[0].source} -> ${fromStart[0].target}\` must not carry semantic attributes`;
  if (hasBody && fromStart[0].target === exit) return "its start node cannot route directly to exit when it has other nodes";
  const intoExit = graph.edges.filter((edge) => edge.target === exit);
  if (intoExit.length !== 1) return `its exit node \`${exit}\` must have exactly one predecessor`;
  if (carriesMeaning(intoExit[0])) return `its edge \`${intoExit[0].source} -> ${intoExit[0].target}\` must not carry semantic attributes`;
  if (hasBody && intoExit[0].source === start) return "its exit node cannot be reached directly from start when it has other nodes";
  return null;
}

class Validator {
  readonly out: Diagnostic[] = [];
  private readonly locations: Record<string, { line: number; column: number }>;
  private readonly nodeAttrs = new Map<string, Attributes>();
  private readonly styled = new Map<string, Set<string>>();
  private readonly nodes = new Map<string, WorkflowNode>();
  private readonly edgeAttrs = new Map<WorkflowEdge, Attributes>();
  private readonly outgoing = new Map<string, WorkflowEdge[]>();
  private readonly incoming = new Map<string, WorkflowEdge[]>();
  private readonly importIds = new Set<string>();

  constructor(
    private readonly graph: WorkflowGraph,
    private readonly assets: Record<string, string>,
    private readonly baseDir: string,
    /** Workflow files being validated, root first, to refuse a cycle and cap nesting. */
    private readonly stack: string[],
    /** What Fabro's bundler carries to the run from the root workflow. */
    private readonly bundled: Set<string>,
  ) {
    this.locations = graph.sourceMeta?.locations ?? {};
    for (const node of graph.nodes) {
      if (this.nodes.has(node.id)) continue;
      this.nodes.set(node.id, node);
      const attrs = effectiveNodeAttributes(graph, node);
      this.nodeAttrs.set(node.id, attrs);
      if (hasAttr(attrs, "import")) this.importIds.add(node.id);
    }
    for (const edge of graph.edges) {
      this.edgeAttrs.set(edge, effectiveEdgeAttributes(graph, edge));
      this.outgoing.set(edge.source, [...(this.outgoing.get(edge.source) ?? []), edge]);
      this.incoming.set(edge.target, [...(this.incoming.get(edge.target) ?? []), edge]);
    }
  }

  private push(severity: Diagnostic["severity"], code: string, message: string, where: { nodeId?: string; edge?: WorkflowEdge } = {}): void {
    const diagnostic: Diagnostic = { severity, code, message };
    let location: { line: number; column: number } | undefined;
    if (where.nodeId !== undefined) {
      diagnostic.nodeId = where.nodeId;
      const key = nodeOwner(where.nodeId);
      location = Object.hasOwn(this.locations, key) ? this.locations[key] : undefined;
    }
    if (where.edge) {
      diagnostic.edgeId = where.edge.id;
      const key = edgeOwner(where.edge.id);
      location = Object.hasOwn(this.locations, key) ? this.locations[key] : undefined;
    }
    if (location) {
      diagnostic.line = location.line;
      diagnostic.column = location.column;
    }
    this.out.push(diagnostic);
  }

  private error(code: string, message: string, where: { nodeId?: string; edge?: WorkflowEdge } = {}): void {
    this.push("error", code, message, where);
  }

  private warning(code: string, message: string, where: { nodeId?: string; edge?: WorkflowEdge } = {}): void {
    this.push("warning", code, message, where);
  }

  private kindOf(node: WorkflowNode): string {
    return getNodeKind(node, this.graph).type;
  }

  private typeOfId(id: string): string | undefined {
    const node = this.nodes.get(id);
    return node ? this.kindOf(node) : undefined;
  }

  run(): void {
    this.applyStylesheet();
    this.checkIntegrity();
    this.checkStructure();
    this.checkGraphAttrs();
    for (const node of this.nodes.values()) this.checkNode(node);
    this.checkEdges();
    this.checkFiles();
  }

  /** What a static stylesheet writes onto nodes, so backend-dependent rules see what Petri sees. */
  private applyStylesheet(): void {
    const text = getAttr(this.graph.attrs, "model_stylesheet");
    if (text === undefined) return;
    if (text.startsWith("@") || hasTemplateSyntax(text)) return;
    const parsed = parseStylesheet(text);
    if ("error" in parsed) {
      this.error("attractor.stylesheet.syntax", `The model stylesheet does not parse: ${parsed.error}.`);
      return;
    }
    const unknown = new Set(
      parsed.rules.flatMap((rule) =>
        rule.declarations.map(([property]) => property).filter((property) => !STYLESHEET_PROPERTIES.includes(property)),
      ),
    );
    for (const property of unknown) {
      this.warning(
        "attractor.stylesheet.unknown_property",
        `The stylesheet sets \`${property}\`, which is not a model property (${STYLESHEET_PROPERTIES.join(", ")}). It is ignored.`,
      );
    }
    const rules: StyleRule[] = [...parsed.rules].sort(
      (a, b) => selectorSpecificity(a.selector) - selectorSpecificity(b.selector),
    );
    for (const rule of rules) {
      for (const [property, value] of rule.declarations) {
        if ((property === "model" || property === "provider") && PLACEHOLDER_VALUE.test(value)) {
          this.warning(
            "builder.placeholder_model",
            `The stylesheet sets ${property} to the placeholder "${value}". Replace it with a real value (\`fabro model\` lists models) before running.`,
          );
        }
      }
    }
    for (const node of this.nodes.values()) {
      const attrs = this.nodeAttrs.get(node.id) as Attributes;
      const classes = nodeClasses(this.graph, node);
      const shape = shapeOf(attrs);
      const applied = new Map<string, string>();
      for (const rule of rules) {
        const { selector } = rule;
        const matches =
          selector.kind === "universal" ||
          (selector.kind === "shape" && selector.name === shape) ||
          (selector.kind === "class" && classes.includes(selector.name)) ||
          (selector.kind === "id" && selector.name === node.id);
        if (!matches) continue;
        for (const [property, value] of rule.declarations) {
          if (STYLESHEET_PROPERTIES.includes(property)) applied.set(property, value);
        }
      }
      const written = new Set<string>();
      for (const [property, value] of applied) {
        if (!hasAttr(attrs, property)) {
          setAttr(attrs, property, value);
          written.add(property);
        }
      }
      this.styled.set(node.id, written);
    }
  }

  private checkIntegrity(): void {
    const { graph } = this;
    if (graph.name.trim().length === 0) {
      this.warning(
        "builder.missing_graph_name",
        "The workflow has no name. The Fabro language reference requires one, though the parser tolerates its absence.",
      );
    }
    const seen = new Set<string>();
    for (const node of graph.nodes) {
      if (seen.has(node.id)) {
        this.error("builder.duplicate_node_id", `Two nodes share the id \`${node.id}\`. Give each a unique id.`, {
          nodeId: node.id,
        });
      }
      seen.add(node.id);
      if (node.id === GOAL_CHECK_NODE) {
        this.error("attractor.reserved_node_id", `\`${GOAL_CHECK_NODE}\` is reserved for goal-gate lowering.`, {
          nodeId: node.id,
        });
      }
      if (!VALID_NODE_ID.test(node.id)) {
        this.error(
          "builder.invalid_node_id",
          `Node id "${node.id}" is not valid. Use letters, digits and underscores, starting with a letter or underscore. Today's parser tolerates quoted ids, but the language reference does not, and such ids do not work in stylesheets and context keys.`,
          { nodeId: node.id },
        );
      }
      if (RESERVED_KEYWORDS.has(node.id.toLowerCase())) {
        this.warning(
          "attractor.reserved_keyword_node_id",
          `Node id \`${node.id}\` is a DOT reserved word, so other DOT tools may fail to parse it. Rename it, for example to \`${node.id.toLowerCase()}_step\`.`,
          { nodeId: node.id },
        );
      }
    }
    const edgeIds = new Set<string>();
    for (const edge of graph.edges) {
      if (edgeIds.has(edge.id)) {
        this.error("builder.duplicate_edge_id", `Two edges share the id \`${edge.id}\`.`, { edge });
      }
      edgeIds.add(edge.id);
    }
    const groups = groupsById(graph);
    const groupIds = new Set<string>();
    for (const group of graph.groups) {
      if (groupIds.has(group.id)) this.error("builder.duplicate_group_id", `Two groups share the id \`${group.id}\`.`);
      groupIds.add(group.id);
      if (group.parentId !== undefined && !groups.has(group.parentId)) {
        this.error("builder.unknown_group", `Group \`${group.id}\` has a parent that does not exist: \`${group.parentId}\`.`);
      }
      const seenParents = new Set([group.id]);
      for (let parent = group.parentId; parent !== undefined; parent = groups.get(parent)?.parentId) {
        if (seenParents.has(parent)) {
          this.error("builder.group_cycle", `Group \`${group.id}\` is nested inside itself.`);
          break;
        }
        seenParents.add(parent);
      }
    }
    for (const node of graph.nodes) {
      if (node.groupId !== undefined && !groups.has(node.groupId)) {
        this.error("builder.unknown_group", `Node \`${node.id}\` is in a group that does not exist: \`${node.groupId}\`.`, {
          nodeId: node.id,
        });
      }
    }
    for (const edge of graph.edges) {
      if (edge.groupId !== undefined && !groups.has(edge.groupId)) {
        this.error("builder.unknown_group", `Edge \`${edge.id}\` is in a group that does not exist: \`${edge.groupId}\`.`, {
          edge,
        });
      }
    }
  }

  private checkStructure(): void {
    const ids = [...this.nodes.keys()];
    for (const edge of this.graph.edges) {
      for (const endpoint of [edge.source, edge.target]) {
        if (!this.nodes.has(endpoint)) {
          this.error(
            "attractor.undeclared_node",
            `\`${endpoint}\` is named by the edge \`${edge.source} -> ${edge.target}\` but never declared. Declare it or fix the edge.`,
            { edge },
          );
        }
      }
    }
    const starts = ids.filter((id) => isStartNode(id, this.nodeAttrs.get(id) as Attributes));
    const exits = ids.filter((id) => isExitNode(id, this.nodeAttrs.get(id) as Attributes));
    if (starts.length === 0) {
      this.error("attractor.no_start", "The workflow has no start node. Add one with shape=Mdiamond.");
    } else if (starts.length > 1) {
      this.error("attractor.multiple_starts", `The workflow has multiple start nodes: ${starts.join(", ")}. Keep exactly one.`);
    }
    if (exits.length === 0) {
      this.error("attractor.no_exit", "The workflow has no exit node. Add one with shape=Msquare.");
    } else if (exits.length > 1) {
      this.error("attractor.multiple_exits", `The workflow has multiple exit nodes: ${exits.join(", ")}. Keep exactly one.`);
    }
    if (starts.length === 0 || exits.length === 0) return;
    const start = starts[0];
    const exit = exits[0];
    for (const edge of this.graph.edges) {
      if (edge.target === start) {
        this.error("attractor.start_has_incoming", `\`${edge.source}\` points at the start node, which takes no incoming edges.`, {
          edge,
        });
      }
      if (edge.source === exit) {
        this.error("attractor.exit_has_outgoing", "The exit node has an outgoing edge. Nothing runs after exit.", { edge });
      }
    }
    const reached = new Set([start]);
    const queue = [start];
    while (queue.length > 0) {
      for (const edge of this.outgoing.get(queue.shift() as string) ?? []) {
        if (!reached.has(edge.target)) {
          reached.add(edge.target);
          queue.push(edge.target);
        }
      }
    }
    for (const id of ids) {
      if (!reached.has(id)) {
        this.error("attractor.unreachable_node", `\`${id}\` is not reachable from the start node. Connect it or remove it.`, {
          nodeId: id,
        });
      }
    }
    if (!reached.has(exit)) {
      this.error("attractor.exit_unreachable", "The exit node is not reachable from the start node.", { nodeId: exit });
    }
    for (const id of ids) {
      if (id !== exit && reached.has(id) && (this.outgoing.get(id) ?? []).length === 0) {
        this.warning(
          "builder.dead_end",
          `\`${id}\` has no outgoing edges, so a run that reaches it stops there without reaching exit.`,
          { nodeId: id },
        );
      }
    }
  }

  private checkUnknownAttrs(
    attrs: Attributes,
    known: string[],
    what: string,
    where: { nodeId?: string; edge?: WorkflowEdge } = {},
  ): void {
    for (const key of Object.keys(attrs)) {
      if (key === "import" && where.nodeId !== undefined) continue;
      if (LEGACY_ATTRS.has(key)) {
        this.error(
          "unsupported.legacy_dialect",
          `\`${key}\` on ${what} is a legacy attribute Fabro no longer reads. Use \`prompt\` on a box node, or \`shape\` for the kind.`,
          where,
        );
      } else if (key === "acp_command") {
        this.error("unsupported.acp_command", `\`acp_command\` on ${what} is the legacy spelling. Use \`acp.command\`.`, where);
      } else if (key === "auto_status") {
        this.warning(
          "deprecated.auto_status",
          `\`auto_status\` on ${what} is deprecated. Use on_failure="succeed".`,
          where,
        );
      } else if (!known.includes(key) && !LAYOUT_ATTRS.has(key) && !key.startsWith("x.")) {
        const suggestion = closest(key, known);
        this.error(
          "attractor.unknown_attribute",
          `\`${key}\` on ${what} is not a Fabro attribute, and Petri refuses it. ${suggestion ? `Did you mean \`${suggestion}\`?` : "Tooling metadata belongs under the `x.` namespace."} The builder keeps the value so you can fix it.`,
          where,
        );
      }
    }
  }

  private checkInteger(attrs: Attributes, key: string, what: string, where: { nodeId?: string; edge?: WorkflowEdge } = {}): void {
    const value = getAttr(attrs, key);
    if (value !== undefined && !readsAsInteger(value)) {
      this.error("attractor.bad_attribute_type", `\`${key}\` on ${what} must be an integer, not "${value}".`, where);
    }
  }

  private checkBoolean(attrs: Attributes, key: string, what: string, where: { nodeId?: string; edge?: WorkflowEdge } = {}): void {
    const value = getAttr(attrs, key);
    if (value !== undefined && value !== "true" && value !== "false") {
      this.error("attractor.bad_attribute_type", `\`${key}\` on ${what} must be true or false, not "${value}".`, where);
    }
  }

  private checkPolicy(attrs: Attributes, key: string, what: string, where: { nodeId?: string } = {}): void {
    const value = getAttr(attrs, key);
    if (value === undefined) return;
    if (!FAILURE_POLICIES.includes(value)) {
      this.error("attractor.bad_on_failure", `\`${key}\` on ${what} must be route, exit or succeed, not "${value}".`, where);
    } else if (value === "partially_succeed") {
      this.warning(
        "attractor.petri_extension",
        `\`${key}="partially_succeed"\` on ${what} is a Petri extension. Fabro's validator accepts only route, exit and succeed.`,
        where,
      );
    }
  }

  private checkRetryTargets(attrs: Attributes, what: string, where: { nodeId?: string } = {}): void {
    for (const key of ["retry_target", "fallback_retry_target"]) {
      const target = getAttr(attrs, key);
      if (target === undefined || this.nodes.has(target)) continue;
      // An imported node's id only exists after the import expands.
      if ([...this.importIds].some((id) => target.startsWith(`${id}.`))) continue;
      this.warning("attractor.retry_target_not_found", `\`${key}="${target}"\` on ${what} names no node. Declare it or fix \`${key}\`.`, where);
    }
  }

  private checkPermissions(attrs: Attributes, key: string, what: string, where: { nodeId?: string } = {}): void {
    const value = getAttr(attrs, key);
    if (value === undefined) return;
    if (!["read-only", "read-write", "full"].includes(value)) {
      this.error("attractor.permissions", `\`${key}\` on ${what} must be read-only, read-write or full, not "${value}".`, where);
    } else {
      this.warning("attractor.petri_extension", `\`${key}\` on ${what} is a Petri extension. Fabro does not read it, so under Fabro these agents keep full access.`, where);
    }
  }

  private checkAcpPair(attrs: Attributes, owner: string, where: { nodeId?: string } = {}): void {
    const command = getAttr(attrs, "acp.command");
    const config = getAttr(attrs, "acp.config");
    if (command !== undefined && config !== undefined) {
      this.error("attractor.acp_both", `${owner} sets both \`acp.command\` and \`acp.config\`. Keep one.`, where);
      return;
    }
    if (command !== undefined) {
      const problem = commandLineProblem(command.trim());
      if (problem) this.error("attractor.bad_acp_command", `\`acp.command\` on ${owner} ${problem}.`, where);
    }
    if (config !== undefined) {
      let parsed: unknown;
      try {
        parsed = JSON.parse(config);
      } catch (error) {
        this.error("attractor.bad_acp_config", `\`acp.config\` on ${owner} is not JSON: ${String(error)}.`, where);
        return;
      }
      const name = (parsed as { command?: unknown } | null)?.command;
      if (typeof name !== "string" || name.trim().length === 0) {
        this.error("attractor.bad_acp_config", `\`acp.config\` on ${owner} must be a JSON object with a non-empty \`command\`.`, where);
      }
    }
  }

  private checkGraphAttrs(): void {
    const { attrs } = this.graph;
    this.checkUnknownAttrs(attrs, GRAPH_ATTRS, "the graph");
    this.checkPolicy(attrs, "on_failure", "the graph");
    this.checkPolicy(attrs, "on_retries_exhausted", "the graph");
    this.checkRetryTargets(attrs, "the graph");
    this.checkPermissions(attrs, "default_permissions", "the graph");
    this.checkAcpPair(attrs, "the graph");
    this.checkInteger(attrs, "default_max_retries", "the graph");
    this.checkInteger(attrs, "max_node_visits", "the graph");
    this.checkInteger(attrs, "loop_restart_signature_limit", "the graph");
    const limit = getAttr(attrs, "loop_restart_signature_limit");
    if (limit !== undefined && readsAsInteger(limit) && Number(limit) < 1) {
      this.error("attractor.bad_signature_limit", `\`loop_restart_signature_limit=${limit}\` must be at least 1.`);
    }
    const visits = getAttr(attrs, "max_node_visits");
    if (visits !== undefined && readsAsInteger(visits) && Number(visits) > MAX_FIRINGS) {
      this.error("attractor.max_visits_too_large", `\`max_node_visits=${visits}\` exceeds the hard maximum of ${MAX_FIRINGS}.`);
    }
    const stall = getAttr(attrs, "stall_timeout");
    if (stall !== undefined && !isValidDuration(stall)) {
      this.error("attractor.bad_duration", `\`stall_timeout\` must be a duration such as 900s or 15m, not "${stall}".`);
    }
    const direction = getAttr(attrs, "rankdir");
    if (direction !== undefined && !RANK_DIRECTIONS.includes(direction)) {
      this.warning("attractor.bad_rankdir", `\`rankdir=${direction}\` is not a Graphviz rank direction. Use ${RANK_DIRECTIONS.join(", ")}.`);
    }
    const selection = getAttr(attrs, "selection");
    if (selection !== undefined && selection !== "deterministic" && selection !== "random") {
      this.error("attractor.bad_selection", `\`selection\` must be deterministic or random, not "${selection}".`);
    }
    const fidelity = getAttr(attrs, "default_fidelity");
    if (fidelity !== undefined && !FIDELITIES.includes(fidelity)) {
      this.error("attractor.bad_fidelity", `"${fidelity}" on the graph is not a fidelity mode (${FIDELITIES.join(", ")}).`);
    }
    const thread = getAttr(attrs, "default_thread");
    if (thread && fidelity !== "full") {
      this.warning("attractor.thread_id_requires_fidelity_full", 'The graph has default_thread but default_fidelity is not "full". Add default_fidelity="full" or remove default_thread.');
    }
    for (const key of ["default_model", "default_provider"]) {
      const value = getAttr(attrs, key);
      if (value !== undefined && PLACEHOLDER_VALUE.test(value)) {
        this.warning("builder.placeholder_model", `The graph's \`${key}\` is the placeholder "${value}". Replace it with a real value (\`fabro model\` lists models) before running.`);
      }
    }
  }

  private checkNode(node: WorkflowNode): void {
    const attrs = this.nodeAttrs.get(node.id) as Attributes;
    const where = { nodeId: node.id };
    const what = `node \`${node.id}\``;
    this.checkUnknownAttrs(attrs, NODE_ATTRS, what, where);

    if (hasAttr(attrs, "import")) {
      this.checkImportPlaceholder(node, attrs);
      return;
    }

    const type = getAttr(attrs, "type");
    if (type !== undefined && !isKnownType(type)) {
      this.error("attractor.unknown_type", `\`type="${type}"\` on ${what} is not a Fabro handler type.`, where);
    }
    const shape = getAttr(attrs, "shape");
    if (type === undefined && shape !== undefined && !isKnownShape(shape)) {
      this.warning("attractor.unknown_shape", `\`shape=${shape}\` on ${what} is not a Fabro shape, so it runs as an agent.`, where);
    }
    const kind = this.kindOf(node);
    this.checkPolicy(attrs, "on_failure", what, where);
    this.checkPolicy(attrs, "on_retries_exhausted", what, where);
    this.checkRetryTargets(attrs, what, where);
    this.checkPermissions(attrs, "permissions", what, where);

    const hasScript = hasAttr(attrs, "script");
    const hasPrompt = hasAttr(attrs, "prompt");
    if (hasScript && hasPrompt) {
      this.error("attractor.script_prompt_conflict", `${what} sets both \`script\` and \`prompt\`. A command runs the script and an agent reads the prompt; no node does both. Remove one or split the node.`, where);
    }
    const knownKind = type !== undefined ? isKnownType(type) : isKnownShape(shapeOf(attrs));
    if (knownKind) {
      for (const [key, readers] of HANDLER_SPECIFIC) {
        if (!hasAttr(attrs, key) || readers.includes(kind)) continue;
        if (hasScript && hasPrompt && (key === "script" || key === "prompt")) continue;
        this.warning("attractor.inert_attribute", `\`${key}\` on ${what} (${kind}) has no effect. Only ${readers.join(", ")} nodes read it.`, where);
      }
    }
    if (kind !== "parallel" && hasAttr(attrs, "for_each")) {
      this.error("attractor.for_each.not_parallel", `\`for_each\` on ${what} (${kind}) never fans out. Remove it or use shape=component.`, where);
    }

    this.checkInteger(attrs, "max_retries", what, where);
    this.checkInteger(attrs, "max_visits", what, where);
    const maxVisits = getAttr(attrs, "max_visits");
    if (maxVisits !== undefined && readsAsInteger(maxVisits) && Number(maxVisits) > MAX_FIRINGS) {
      this.error("attractor.max_visits_too_large", `\`max_visits=${maxVisits}\` on ${what} exceeds the hard maximum of ${MAX_FIRINGS}.`, where);
    }
    for (const key of ["goal_gate", "allow_partial", "auto_status"]) this.checkBoolean(attrs, key, what, where);

    const retryPolicy = getAttr(attrs, "retry_policy");
    if (retryPolicy !== undefined && !RETRY_POLICIES.includes(retryPolicy)) {
      this.error("attractor.bad_retry_policy", `\`retry_policy\` on ${what} must be ${RETRY_POLICIES.join(", ")}, not "${retryPolicy}".`, where);
    }
    const selection = getAttr(attrs, "selection");
    if (selection !== undefined && selection !== "deterministic" && selection !== "random") {
      this.error("attractor.bad_selection", `\`selection\` on ${what} must be deterministic or random, not "${selection}".`, where);
    }
    const timeout = getAttr(attrs, "timeout");
    if (timeout !== undefined) {
      if (timeout.trim().length > 0 && !Number.isNaN(Number(timeout.trim()))) {
        this.error("unsupported.legacy_dialect", `\`timeout=${timeout}\` on ${what} is a bare number. Write the unit, such as timeout="${timeout}s".`, where);
      } else if (!isValidDuration(timeout)) {
        this.error("attractor.bad_duration", `\`timeout\` on ${what} must be a duration such as 900s or 15m, not "${timeout}".`, where);
      }
    }
    const exhausted = getAttr(attrs, "on_retries_exhausted") ?? getAttr(this.graph.attrs, "on_retries_exhausted");
    if (getAttr(attrs, "allow_partial") === "true" && exhausted !== undefined && exhausted !== "partially_succeed") {
      this.error("attractor.allow_partial_conflict", `\`allow_partial=true\` on ${what} means on_retries_exhausted="partially_succeed", but the node says "${exhausted}".`, where);
    }
    if (getAttr(attrs, "goal_gate") === "true") {
      const targets = [
        getAttr(attrs, "retry_target"),
        getAttr(attrs, "fallback_retry_target"),
        getAttr(this.graph.attrs, "retry_target"),
        getAttr(this.graph.attrs, "fallback_retry_target"),
      ];
      if (!targets.some((target) => target !== undefined && this.nodes.has(target))) {
        this.warning("attractor.goal_gate_without_target", `Goal gate \`${node.id}\` has no retry target that exists, so when it fails the run ends failed.`, where);
      }
    }
    const ownAttrs = this.styled.get(node.id) ?? new Set<string>();
    for (const key of ["model", "provider"]) {
      const value = getAttr(attrs, key);
      if (value !== undefined && !ownAttrs.has(key) && PLACEHOLDER_VALUE.test(value)) {
        this.warning("builder.placeholder_model", `${what} has the placeholder ${key} "${value}". Replace it with a real value (\`fabro model\` lists models) before running.`, where);
      }
    }

    switch (kind) {
      case "agent":
      case "prompt":
        this.checkLlmNode(node, attrs, kind);
        break;
      case "parallel.fan_in":
        if ((getAttr(attrs, "prompt") ?? "").trim().length > 0) this.checkLlmNode(node, attrs, kind);
        break;
      case "command":
        this.checkCommandNode(node, attrs);
        break;
      case "human":
        this.checkHumanNode(node, attrs);
        break;
      case "wait":
        this.checkWaitNode(node, attrs);
        break;
      case "stack.manager_loop":
        this.checkManagerNode(node, attrs);
        break;
      case "parallel":
        this.checkParallelNode(node, attrs);
        break;
      case "conditional":
        this.checkConditionalNode(node);
        break;
    }
  }

  private checkImportPlaceholder(node: WorkflowNode, attrs: Attributes): void {
    const where = { nodeId: node.id };
    for (const key of Object.keys(attrs)) {
      if (key === "import" || key === "class" || IMPORT_PLACEHOLDER_ATTRS.has(key)) continue;
      this.error("attractor.import", `Import placeholder \`${node.id}\` has unsupported attribute \`${key}\`. It may carry only import, class and the settings its nodes inherit.`, where);
    }
  }

  private checkOutputSchema(node: WorkflowNode, attrs: Attributes): void {
    const schema = getAttr(attrs, "output_schema");
    if (schema === undefined || schema === "routing" || schema.startsWith("@") || hasTemplateSyntax(schema)) return;
    let parsed: unknown;
    try {
      parsed = JSON.parse(schema);
    } catch {
      parsed = undefined;
    }
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      this.error("attractor.bad_output_schema", `\`output_schema\` on \`${node.id}\` must be routing, @path/to/schema.json or a JSON Schema object.`, { nodeId: node.id });
    }
  }

  private checkLlmNode(node: WorkflowNode, attrs: Attributes, kind: string): void {
    const where = { nodeId: node.id };
    const what = `node \`${node.id}\``;
    if ((getAttr(attrs, "prompt") ?? "").trim().length === 0) {
      this.warning("attractor.prompt_missing", `${kind} node \`${node.id}\` has no prompt, so its label is used as the prompt. Write one.`, where);
    }
    const speed = getAttr(attrs, "speed");
    if (speed && speed !== "standard" && speed !== "fast") {
      this.error("attractor.bad_speed", `Invalid speed "${speed}" on ${what}. Expected standard or fast.`, where);
    }
    this.checkBoolean(attrs, "project_memory", what, where);
    this.checkInteger(attrs, "output_retries", what, where);
    this.checkInteger(attrs, "max_tokens", what, where);
    const repairs = getAttr(attrs, "output_retries");
    if (repairs !== undefined && readsAsInteger(repairs) && Number(repairs) > MAX_OUTPUT_RETRIES) {
      this.error("attractor.output_retries_too_large", `\`output_retries=${repairs}\` on ${what} exceeds the hard maximum of ${MAX_OUTPUT_RETRIES}.`, where);
    }
    this.checkOutputSchema(node, attrs);

    const isAgent = kind === "agent";
    const ownBackend = getAttr(attrs, "backend");
    const backend = ownBackend ?? getAttr(this.graph.attrs, "backend");
    if (backend !== undefined && backend !== "acp" && backend !== "api") {
      this.error("attractor.bad_backend", `Agent backend on ${what} must be acp or api, not "${backend}".`, where);
    }
    if (!isAgent && ownBackend === "acp") {
      this.error("attractor.prompt_backend", `backend="acp" on ${what} is only valid on agent nodes. Prompt nodes are API-only.`, where);
    }
    const setsAcp = hasAttr(attrs, "acp.command") || hasAttr(attrs, "acp.config");
    if (isAgent && backend === "acp") {
      const styled = this.styled.get(node.id) ?? new Set<string>();
      const present = API_ONLY_ATTRS.filter((key) => hasAttr(attrs, key) && !styled.has(key));
      if (present.length > 0) {
        this.error("attractor.acp_api_only_attributes", `${what} runs on backend="acp", which does not read ${present.join(", ")}. The ACP agent chooses its own model. Remove them or use backend="api".`, where);
      }
      if (setsAcp) {
        this.checkAcpPair(attrs, what, where);
      } else if (!hasAttr(this.graph.attrs, "acp.command") && !hasAttr(this.graph.attrs, "acp.config")) {
        this.error("attractor.acp_requires_command", `${what} runs on backend="acp" but names no ACP agent. Set acp.command or acp.config on the node or the graph.`, where);
      }
    } else if (setsAcp && !isAgent) {
      this.error("attractor.backend_options", `${what} cannot set acp.command or acp.config. Prompt nodes are API-only.`, where);
    } else if (setsAcp && (backend === undefined || backend === "api")) {
      this.error("attractor.backend_options", `${what} runs on the API, which cannot set acp.command or acp.config. Set backend="acp" to use an ACP agent.`, where);
    }

    const fidelity = getAttr(attrs, "fidelity");
    if (fidelity !== undefined && !FIDELITIES.includes(fidelity)) {
      this.error("attractor.bad_fidelity", `"${fidelity}" on ${what} is not a fidelity mode (${FIDELITIES.join(", ")}).`, where);
    }
    const branchFirst = (this.incoming.get(node.id) ?? []).some((edge) => this.typeOfId(edge.source) === "parallel");
    const thread = getAttr(attrs, "thread_id");
    if (thread) {
      if (branchFirst) {
        this.warning("attractor.parallel_branch_inert_attribute", `\`thread_id\` on \`${node.id}\` is inert: it is entered directly from a parallel fork, and concurrent branches cannot share a conversation.`, where);
      } else if (fidelity !== "full" && getAttr(this.graph.attrs, "default_fidelity") !== "full") {
        this.warning("attractor.thread_id_requires_fidelity_full", `${what} has thread_id but fidelity is not "full". Add fidelity="full" to enable session reuse, or remove thread_id.`, where);
      }
    }
    if (branchFirst && fidelity === "full") {
      this.warning("attractor.parallel_branch_inert_attribute", `fidelity="full" on \`${node.id}\` is degraded: a parallel branch runs at most at summary:high.`, where);
    }
  }

  private checkCommandNode(node: WorkflowNode, attrs: Attributes): void {
    const where = { nodeId: node.id };
    const script = getAttr(attrs, "script");
    if (script === undefined || script.trim().length === 0) {
      this.error("attractor.command_requires_script", `Command node \`${node.id}\` needs a script.`, where);
    } else if (/\bcd\s+\//.test(script)) {
      this.warning("attractor.script_absolute_cd", `The script of \`${node.id}\` changes to an absolute directory (cd /...). Use a relative path; commands already run in the workspace.`, where);
    }
    const language = getAttr(attrs, "language");
    if (language !== undefined && language !== "shell" && language !== "python") {
      this.error("attractor.bad_language", `\`language\` on \`${node.id}\` must be shell or python, not "${language}".`, where);
    }
    const stdin = getAttr(attrs, "stdin_source");
    if (stdin !== undefined) {
      const key = stdin.replace(/^context\./, "").trim();
      if (key.length === 0) {
        this.error("attractor.bad_stdin_source", `\`stdin_source\` on \`${node.id}\` must name a context key such as context.output, not "${stdin}".`, where);
      } else if (key.startsWith("internal.")) {
        this.warning("attractor.internal_context", `\`stdin_source\` on \`${node.id}\` reads \`${key}\`, which is Fabro-internal run state and may read as null.`, where);
      } else if (key === "parallel.results" && !this.hasUpstreamFanIn(node.id)) {
        this.warning("attractor.parallel_results_without_fan_in", `\`${node.id}\` reads context.parallel.results, but no fan-in node precedes it.`, where);
      }
    }
    this.checkOutputSchema(node, attrs);
  }

  private hasUpstreamFanIn(id: string): boolean {
    const seen = new Set([id]);
    const queue = [id];
    while (queue.length > 0) {
      for (const edge of this.incoming.get(queue.shift() as string) ?? []) {
        const kind = this.typeOfId(edge.source);
        if (kind === "parallel.fan_in" || kind === "parallel") return true;
        if (!seen.has(edge.source)) {
          seen.add(edge.source);
          queue.push(edge.source);
        }
      }
    }
    return false;
  }

  private checkHumanNode(node: WorkflowNode, attrs: Attributes): void {
    const where = { nodeId: node.id };
    const edges = this.outgoing.get(node.id) ?? [];
    const choices: { key: string; target: string }[] = [];
    let freeform = 0;
    for (const edge of edges) {
      const edgeAttrs = this.edgeAttrs.get(edge) as Attributes;
      this.checkBoolean(edgeAttrs, "freeform", `edge \`${edge.source} -> ${edge.target}\``, { edge });
      if (getAttr(edgeAttrs, "freeform") === "true") {
        freeform++;
        if (freeform === 2) {
          this.error("attractor.freeform_edge_count", `Human gate \`${node.id}\` has more than one freeform=true edge.`, { edge });
        }
        continue;
      }
      const label = getAttr(edgeAttrs, "label") || edge.target;
      choices.push({ key: acceleratorKey(label), target: edge.target });
    }
    if (edges.length === 0) {
      this.error("attractor.human_without_edges", `Human gate \`${node.id}\` has no outgoing edges to offer as choices.`, where);
    }
    const question = getAttr(attrs, "question_type");
    if (question !== undefined && !QUESTION_TYPES.includes(question)) {
      this.error("attractor.bad_question_type", `\`question_type\` on \`${node.id}\` must be one of ${QUESTION_TYPES.join(", ")}.`, where);
    }
    this.checkBoolean(attrs, "sensitive", `node \`${node.id}\``, where);
    this.checkBoolean(attrs, "review_target", `node \`${node.id}\``, where);
    const fallback = getAttr(attrs, "human.default_choice");
    if (fallback !== undefined && !choices.some((choice) => choice.key === fallback || choice.target === fallback)) {
      this.error("attractor.bad_default_choice", `\`human.default_choice="${fallback}"\` on \`${node.id}\` names none of the gate's choices or targets.`, where);
    }
  }

  private checkWaitNode(node: WorkflowNode, attrs: Attributes): void {
    const where = { nodeId: node.id };
    const duration = getAttr(attrs, "duration");
    if (duration === undefined) {
      this.error("attractor.wait_requires_duration", `Wait node \`${node.id}\` needs a duration.`, where);
    } else if (!isValidDuration(duration)) {
      this.error("attractor.bad_duration", `\`duration\` on \`${node.id}\` must be a duration such as 30s or 2m, not "${duration}".`, where);
    }
  }

  private checkManagerNode(node: WorkflowNode, attrs: Attributes): void {
    const where = { nodeId: node.id };
    const child = getAttr(attrs, "stack.child_workflow");
    const inline = getAttr(attrs, "stack.child_dot_source");
    if (child === undefined && inline === undefined) {
      this.error("attractor.manager_loop_without_child", `Sub-workflow \`${node.id}\` needs stack.child_workflow or stack.child_dot_source.`, where);
    }
    if (child === undefined && inline !== undefined) {
      this.checkNestedWorkflow(node.id, inline, "inline child workflow", this.baseDir, `${this.stack[this.stack.length - 1]}#${node.id}`);
    }
    const cycles = getAttr(attrs, "manager.max_cycles");
    if (cycles !== undefined && !(readsAsInteger(cycles) && Number(cycles) >= 0)) {
      this.warning("attractor.manager.max_cycles", `\`manager.max_cycles=${cycles}\` on \`${node.id}\` is not a non-negative integer, so Fabro reads it as 1000.`, where);
    }
    const interval = getAttr(attrs, "manager.poll_interval");
    if (interval !== undefined && !isValidDuration(interval)) {
      this.error("attractor.bad_duration", `\`manager.poll_interval\` on \`${node.id}\` must be a duration such as 45s, not "${interval}".`, where);
    }
    const stop = getAttr(attrs, "manager.stop_condition");
    if (stop !== undefined) {
      for (const issue of checkCondition(stop)) this.push(issue.severity, issue.code, `\`${node.id}\` stop condition: ${issue.message}`, where);
    }
  }

  /** Parse and validate a workflow this one runs or splices in, reporting on the node that names it. */
  private checkNestedWorkflow(nodeId: string, source: string, what: string, baseDir: string, name: string): WorkflowGraph | null {
    const where = { nodeId };
    if (this.stack.includes(name)) {
      this.error("attractor.workflow_cycle", `\`${nodeId}\` names ${what}, which is already being run: ${[...this.stack, name].join(" -> ")}.`, where);
      return null;
    }
    const parsed = parseWorkflow(source);
    if (!parsed.graph) {
      const problem = parsed.diagnostics[0];
      this.error(problem?.code ?? "dot.syntax", `\`${nodeId}\` names ${what}, which does not parse${problem?.line ? ` at line ${problem.line}, column ${problem.column}` : ""}: ${problem?.message}`, where);
      return null;
    }
    if (this.stack.length > MAX_CHILD_DEPTH) {
      this.error("attractor.workflow_depth", `\`${nodeId}\` nests workflows more than ${MAX_CHILD_DEPTH} deep.`, where);
      return null;
    }
    const nested = new Validator(parsed.graph, this.assets, baseDir, [...this.stack, name], this.bundled);
    nested.run();
    for (const diagnostic of nested.out) {
      this.push(diagnostic.severity, diagnostic.code ?? "builder.nested_workflow", `In ${what} of \`${nodeId}\`: ${diagnostic.message}`, where);
    }
    return parsed.graph;
  }

  /** Where this fork's branches converge: the first direct successor they all share. */
  private forkJoin(id: string, visiting: Set<string>): string | null {
    if (visiting.has(id)) return null;
    visiting.add(id);
    const exitOf = (target: string): string =>
      this.typeOfId(target) === "parallel" ? (this.forkJoin(target, visiting) ?? target) : target;
    let shared: Set<string> | null = null;
    for (const branch of this.outgoing.get(id) ?? []) {
      const successors = new Set((this.outgoing.get(exitOf(branch.target)) ?? []).map((edge) => edge.target));
      if (shared) {
        const previous: Set<string> = shared;
        shared = new Set([...previous].filter((target) => successors.has(target)));
      } else {
        shared = successors;
      }
    }
    return shared ? ([...shared].sort()[0] ?? null) : null;
  }

  private checkParallelNode(node: WorkflowNode, attrs: Attributes): void {
    const where = { nodeId: node.id };
    const edges = this.outgoing.get(node.id) ?? [];
    for (const edge of edges) {
      const edgeAttrs = this.edgeAttrs.get(edge) as Attributes;
      if ((getAttr(edgeAttrs, "condition") ?? "").trim().length > 0) {
        this.error("attractor.parallel.conditional_branch", `A parallel node's branches are unconditional, but \`${edge.source} -> ${edge.target}\` has a condition.`, { edge });
      }
    }
    if (edges.length === 0) {
      this.error("attractor.parallel.no_branches", `Parallel node \`${node.id}\` has no branches.`, where);
      return;
    }
    const maxParallel = getAttr(attrs, "max_parallel");
    if (maxParallel !== undefined) {
      if (!readsAsInteger(maxParallel) || Number(maxParallel) < 0) {
        this.warning("attractor.max_parallel.normalized", `\`max_parallel=${maxParallel}\` on \`${node.id}\` is not a non-negative integer, so Fabro reads it as 4.`, where);
      } else if (Number(maxParallel) === 0) {
        this.warning("attractor.max_parallel.normalized", `\`max_parallel=0\` on \`${node.id}\` runs one branch at a time.`, where);
      }
    }
    const forEach = getAttr(attrs, "for_each");
    if (forEach !== undefined) {
      if (forEach.replace(/^context\./, "").trim().length === 0) {
        this.error("attractor.for_each.source", `\`for_each\` on \`${node.id}\` must name a context key.`, where);
        return;
      }
      if (edges.length !== 1) {
        this.error("attractor.for_each.template_edges", `for_each node \`${node.id}\` needs exactly one template edge.`, where);
        return;
      }
      const template = edges[0].target;
      const kind = this.typeOfId(template);
      if (kind !== "agent" && kind !== "prompt") {
        this.error("attractor.for_each.target", `The for_each template \`${template}\` must be an agent or prompt node.`, { edge: edges[0] });
        return;
      }
      if (hasAttr(this.nodeAttrs.get(template) ?? {}, "for_each")) {
        this.error("attractor.for_each.nested", "Nested for_each is not supported.", { edge: edges[0] });
        return;
      }
    } else {
      let targetsOk = true;
      for (const edge of edges) {
        const kind = this.typeOfId(edge.target);
        if (kind === undefined || kind === "start" || kind === "exit" || kind === "parallel.fan_in") {
          this.error("attractor.parallel.bad_branch_target", `\`${edge.target}\` cannot be a branch of parallel node \`${node.id}\`. A branch target is a stage that does work and then returns to the join.`, { edge });
          targetsOk = false;
        }
      }
      if (!targetsOk) return;
    }
    const join = this.forkJoin(node.id, new Set());
    if (join === null) {
      this.error("attractor.parallel.no_join", `The branches of parallel node \`${node.id}\` share no direct successor to join at. Every branch target must have an edge to the same node.`, where);
      return;
    }
    for (const branch of edges) {
      if (this.typeOfId(branch.target) === "parallel") continue;
      for (const edge of this.outgoing.get(branch.target) ?? []) {
        if (edge.target !== join) {
          this.warning("attractor.parallel.branch_edge_ignored", `\`${edge.source} -> ${edge.target}\` is never taken: \`${branch.target}\` runs as a branch of \`${node.id}\` and returns to the join \`${join}\`.`, { edge });
        }
      }
    }
  }

  private checkConditionalNode(node: WorkflowNode): void {
    const edges = this.outgoing.get(node.id) ?? [];
    const conditional = edges.filter((edge) => (getAttr(this.edgeAttrs.get(edge) as Attributes, "condition") ?? "").trim().length > 0);
    if (edges.length > 0 && (edges.length < 2 || conditional.length === 0)) {
      this.warning("builder.conditional_without_routes", `Conditional node \`${node.id}\` routes nowhere useful. Give it several outgoing edges, at least one with a condition.`, { nodeId: node.id });
    }
  }

  private checkEdges(): void {
    for (const edge of this.graph.edges) {
      if (!this.nodes.has(edge.target)) continue;
      const attrs = this.edgeAttrs.get(edge) as Attributes;
      const what = `edge \`${edge.source} -> ${edge.target}\``;
      const where = { edge };
      this.checkUnknownAttrs(attrs, EDGE_ATTRS, what, where);
      const sourceKind = this.typeOfId(edge.source);
      const targetKind = this.typeOfId(edge.target);
      const fromFork = sourceKind === "parallel";
      if (sourceKind !== "exit" && !fromFork) {
        this.checkInteger(attrs, "weight", what, where);
        this.checkBoolean(attrs, "loop_restart", what, where);
        const condition = getAttr(attrs, "condition");
        if (condition !== undefined && condition.trim().length > 0) {
          for (const issue of checkCondition(condition)) this.push(issue.severity, issue.code, `${what}: ${issue.message}`, where);
        }
      }
      const fidelity = getAttr(attrs, "fidelity");
      const thread = getAttr(attrs, "thread_id");
      if (fromFork) {
        if (fidelity !== undefined && !FIDELITIES.includes(fidelity)) {
          this.error("attractor.bad_fidelity", `"${fidelity}" on ${what} is not a fidelity mode (${FIDELITIES.join(", ")}).`, where);
        }
        if (thread) {
          this.warning("attractor.parallel_branch_inert_attribute", `\`thread_id\` on ${what} is inert: concurrent branches cannot share a conversation.`, where);
        }
        if (fidelity === "full") {
          this.warning("attractor.parallel_branch_inert_attribute", `fidelity="full" on ${what} is degraded: a parallel branch runs at most at summary:high.`, where);
        }
      } else if (targetKind === "agent" || targetKind === "prompt") {
        if (fidelity !== undefined && !FIDELITIES.includes(fidelity)) {
          this.error("attractor.bad_fidelity", `"${fidelity}" on ${what} is not a fidelity mode (${FIDELITIES.join(", ")}).`, where);
        }
        if (thread && fidelity !== "full" && getAttr(this.graph.attrs, "default_fidelity") !== "full") {
          this.warning("attractor.thread_id_requires_fidelity_full", `${what} has thread_id but fidelity is not "full". Add fidelity="full" to enable session reuse, or remove thread_id.`, where);
        }
      }
    }
    for (const node of this.nodes.values()) {
      const kind = this.kindOf(node);
      if (kind === "exit" || kind === "parallel") continue;
      const edges = this.outgoing.get(node.id) ?? [];
      if (edges.length === 0) continue;
      const conditional = edges.filter((edge) => (getAttr(this.edgeAttrs.get(edge) as Attributes, "condition") ?? "").trim().length > 0);
      if (conditional.length === edges.length) {
        this.error("attractor.all_conditional_edges", `Every outgoing edge of \`${node.id}\` has a condition, so nothing is taken when none holds. Add one unconditional edge as the fallback.`, { nodeId: node.id });
      }
      const selection = getAttr(this.nodeAttrs.get(node.id) as Attributes, "selection") ?? getAttr(this.graph.attrs, "selection");
      if (selection === "random" && conditional.length > 0) {
        this.error("attractor.random_with_conditions", `\`${node.id}\` uses selection="random" and has conditional edges. Fabro forbids the combination.`, { nodeId: node.id });
      }
    }
  }

  private checkFiles(): void {
    for (const reference of workflowFileReferences(this.graph)) {
      const where = reference.nodeId === undefined ? {} : { nodeId: reference.nodeId };
      const owner = reference.nodeId === undefined ? "the graph" : `\`${reference.nodeId}\``;
      const label = {
        prompt: "prompt file",
        output_schema: "output schema file",
        goal: "goal file",
        model_stylesheet: "stylesheet file",
        import: "imported workflow",
        child_workflow: "child workflow",
      }[reference.kind];
      if (hasTemplateSyntax(reference.path)) {
        this.error("builder.templated_reference", `${owner} names a ${label} with template syntax, "${reference.path}". File references resolve before templates render, so the path must be literal.`, where);
        continue;
      }
      const resolved = resolveFileReference(reference, this.assets, this.baseDir);
      if (resolved !== null) {
        const content = getFile(this.assets, resolved) as string;
        if (!this.bundled.has(resolved)) {
          this.error("builder.inline_only_file", `${owner} needs the ${label} "${reference.path}", and only inline source names it. Fabro doesn't collect files referenced only inside inline source, so it refuses this workflow. The file is preserved in this draft. Use a child-workflow file for this source, or reference the file directly from its containing workflow.`, where);
        }
        if (reference.nodeId !== undefined && reference.kind === "child_workflow") {
          this.checkNestedWorkflow(reference.nodeId, content, `child workflow "${reference.path}"`, directoryOf(resolved), resolved);
        } else if (reference.nodeId !== undefined && reference.kind === "import") {
          const imported = this.checkNestedWorkflow(reference.nodeId, content, `imported workflow "${reference.path}"`, directoryOf(resolved), resolved);
          const problem = imported && importContractProblem(imported);
          if (problem) {
            this.error("attractor.import", `Import \`${reference.nodeId}\` names "${reference.path}", which Fabro cannot splice in: ${problem}.`, where);
          }
        } else if (reference.kind === "output_schema" && !hasTemplateSyntax(content)) {
          let parsed: unknown;
          try {
            parsed = JSON.parse(content);
          } catch {
            parsed = undefined;
          }
          if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
            this.error("attractor.bad_output_schema", `${owner} loads output schema "${reference.path}", which is not a JSON Schema object.`, where);
          }
        }
        continue;
      }
      const path = normalizeAssetPath(reference.path);
      if (path.split("/").includes("..")) {
        this.warning("builder.external_file", `${owner} names a ${label} through "..": "${reference.path}". Fabro looks up bundled files by exact path, so this is refused when the workflow loads. Keep the file inside the workflow folder, and name it without "..".`, where);
      } else if (path.startsWith("/") || path.startsWith("~")) {
        this.warning("builder.external_file", `${owner} names a ${label} outside the workflow folder, "${reference.path}". The builder cannot include it in a download, so it must exist where Fabro runs.`, where);
      } else {
        this.warning("builder.missing_file", `${owner} needs the ${label} "${reference.path}", which is not in this draft. Upload it here, or add it beside workflow.fabro in the repo. Fabro refuses the workflow until it exists.`, where);
      }
    }
  }

}

/**
 * Structural diagnostics for a graph. Pure, and never a reason to refuse saving:
 * missing files and placeholder models are warnings with a next step.
 */
export function validateWorkflow(graph: WorkflowGraph, assets: Record<string, string> = {}): Diagnostic[] {
  const validator = new Validator(graph, assets, "", ["workflow.fabro"], bundledFiles(graph, assets));
  validator.run();
  return validator.out;
}
