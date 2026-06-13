/**
 * Parse a constrained subset of Graphviz DOT into a `WorkflowDraft`.
 *
 * The playground's chat endpoint asks the model to emit the full
 * `workflow.fabro` each turn via the `write_workflow_file` tool. This
 * parser turns that DOT string back into the same draft schema the
 * reducer operates on, so the new state can be diffed against the
 * previous state and animated into the canvas.
 *
 * The grammar is intentionally limited to what `render-fabro.ts` emits:
 * a single top-level `digraph` block, plain identifiers, quoted-string
 * attribute values, `graph [goal=...]` for the workflow goal, simple
 * `<id> [<attrs>]` node declarations, and `<from> -> <to>` edges with
 * optional `[<attrs>]` lists. Edge chains (`a -> b -> c`) are
 * supported because the model is encouraged to write them.
 */
import {
  ALL_SHAPES,
  DEFAULT_NAME,
  type AttrValue,
  type Edge,
  type Node,
  type Shape,
  type WorkflowDraft,
} from "./draft";

export type ParseResult =
  | { ok: true; draft: WorkflowDraft }
  | { ok: false; error: string };

interface State {
  src: string;
  pos: number;
}

const SHAPE_SET = new Set(ALL_SHAPES as readonly string[]);

export function parseFabro(src: string): ParseResult {
  const state: State = { src, pos: 0 };
  skipWs(state);
  if (!tryConsume(state, "digraph")) {
    return fail("expected `digraph` keyword at start of file", state);
  }

  skipWs(state);
  // The digraph name is optional but our renderer always writes one.
  let digraphName: string | null = null;
  if (state.src[state.pos] !== "{") {
    digraphName = parseIdent(state) ?? parseString(state);
  }
  skipWs(state);
  if (!consumeChar(state, "{")) {
    return fail("expected `{` after digraph header", state);
  }

  const draft: WorkflowDraft = {
    name:  digraphName ? toSnakeCase(digraphName) : DEFAULT_NAME,
    goal:  "",
    nodes: [],
    edges: [],
  };
  const seenNodeIds = new Set<string>();

  while (true) {
    skipWs(state);
    if (state.pos >= state.src.length) {
      return fail("unexpected end of input before closing `}`", state);
    }
    if (consumeChar(state, "}")) {
      // Trailing junk after `}` is tolerated — model may add prose
      // after the closing brace and we don't care for parsing.
      return { ok: true, draft };
    }

    // `graph [goal=...]` carries the workflow goal.
    if (tryConsume(state, "graph") && isAttrOrEnd(state)) {
      const attrs = parseAttrList(state);
      if (attrs && typeof attrs.goal === "string") {
        draft.goal = attrs.goal;
      }
      consumeStatementEnd(state);
      continue;
    }
    // `node [...]` / `edge [...]` set defaults that we ignore.
    // `rankdir=LR` and similar bare attribute assignments are layout
    // hints — also ignored.
    if (
      (tryConsume(state, "node") && isAttrOrEnd(state)) ||
      (tryConsume(state, "edge") && isAttrOrEnd(state))
    ) {
      parseAttrList(state);
      consumeStatementEnd(state);
      continue;
    }
    if (tryConsumeBareAssignment(state)) {
      continue;
    }

    const id = parseIdent(state) ?? parseString(state);
    if (!id) {
      return fail(`unexpected token`, state);
    }
    skipWs(state);

    // Edge (possibly chained).
    if (peek(state, "->")) {
      let from = id;
      while (peek(state, "->")) {
        state.pos += 2;
        skipWs(state);
        const to = parseIdent(state) ?? parseString(state);
        if (!to) {
          return fail("expected target node after `->`", state);
        }
        const edge: Edge = { from, to };
        skipWs(state);
        // The attribute list (if present) binds to the terminal edge
        // in the chain. While there's another `->`, we have more
        // edges to emit before any attrs apply.
        if (!peek(state, "->") && state.src[state.pos] === "[") {
          const attrs = parseAttrList(state) ?? {};
          applyEdgeAttrs(edge, attrs);
        }
        draft.edges.push(edge);
        from = to;
        skipWs(state);
      }
      consumeStatementEnd(state);
      continue;
    }

    // Node declaration.
    if (seenNodeIds.has(id)) {
      // Duplicate node decl — last write wins. Drop the previous.
      const idx = draft.nodes.findIndex((n) => n.id === id);
      if (idx >= 0) draft.nodes.splice(idx, 1);
    }
    let attrs: Record<string, AttrValue> = {};
    if (state.src[state.pos] === "[") {
      attrs = parseAttrList(state) ?? {};
    }
    const node = buildNode(id, attrs);
    draft.nodes.push(node);
    seenNodeIds.add(id);
    consumeStatementEnd(state);
  }
}

function buildNode(id: string, attrs: Record<string, AttrValue>): Node {
  const shape = coerceShape(attrs.shape, id);
  const label = typeof attrs.label === "string" ? attrs.label : id;
  const node: Node = { id, label, shape };
  if (typeof attrs.prompt === "string") node.prompt = attrs.prompt;
  const rest = { ...attrs };
  delete rest.shape;
  delete rest.label;
  delete rest.prompt;
  if (Object.keys(rest).length > 0) node.attrs = rest;
  return node;
}

function coerceShape(raw: AttrValue | undefined, nodeId: string): Shape {
  if (typeof raw !== "string") {
    // Shape omitted: default to start/exit terminals if id matches,
    // otherwise `box` (Fabro's agent default).
    if (nodeId === "start") return "mdiamond";
    if (nodeId === "exit") return "msquare";
    return "box";
  }
  const lower = raw.toLowerCase();
  if (SHAPE_SET.has(lower)) return lower as Shape;
  return "box";
}

function applyEdgeAttrs(edge: Edge, attrs: Record<string, AttrValue>): void {
  if (typeof attrs.condition === "string") edge.condition = attrs.condition;
  if (typeof attrs.label === "string") edge.label = attrs.label;
  const rest = { ...attrs };
  delete rest.condition;
  delete rest.label;
  if (Object.keys(rest).length > 0) edge.attrs = rest;
}

function skipWs(state: State): void {
  while (state.pos < state.src.length) {
    const c = state.src[state.pos]!;
    if (c === " " || c === "\t" || c === "\n" || c === "\r") {
      state.pos++;
      continue;
    }
    if (c === "/" && state.src[state.pos + 1] === "/") {
      while (state.pos < state.src.length && state.src[state.pos] !== "\n") {
        state.pos++;
      }
      continue;
    }
    if (c === "/" && state.src[state.pos + 1] === "*") {
      state.pos += 2;
      while (
        state.pos + 1 < state.src.length &&
        !(state.src[state.pos] === "*" && state.src[state.pos + 1] === "/")
      ) {
        state.pos++;
      }
      state.pos += 2;
      continue;
    }
    if (c === "#") {
      // Some DOT writers use `#` for line comments.
      while (state.pos < state.src.length && state.src[state.pos] !== "\n") {
        state.pos++;
      }
      continue;
    }
    break;
  }
}

function parseIdent(state: State): string | null {
  skipWs(state);
  const start = state.pos;
  // DOT identifiers: [a-zA-Z_-￿][\w-￿]*
  const first = state.src[state.pos];
  if (!first || !/[a-zA-Z_]/.test(first)) return null;
  state.pos++;
  while (
    state.pos < state.src.length &&
    /[a-zA-Z0-9_]/.test(state.src[state.pos]!)
  ) {
    state.pos++;
  }
  return state.src.slice(start, state.pos);
}

function parseString(state: State): string | null {
  skipWs(state);
  if (state.src[state.pos] !== '"') return null;
  state.pos++;
  let result = "";
  while (state.pos < state.src.length) {
    const c = state.src[state.pos]!;
    if (c === "\\") {
      const next = state.src[state.pos + 1];
      if (next === '"') {
        result += '"';
        state.pos += 2;
      } else if (next === "\\") {
        result += "\\";
        state.pos += 2;
      } else if (next === "n") {
        result += "\n";
        state.pos += 2;
      } else if (next === "t") {
        result += "\t";
        state.pos += 2;
      } else if (next === "r") {
        result += "\r";
        state.pos += 2;
      } else {
        // Unknown escape: pass through verbatim.
        result += c;
        state.pos += 1;
      }
    } else if (c === '"') {
      state.pos++;
      // DOT supports string concatenation with `+`. Splice if present.
      const save = state.pos;
      skipWs(state);
      if (state.src[state.pos] === "+") {
        state.pos++;
        const more = parseString(state);
        if (more === null) {
          state.pos = save;
          return result;
        }
        return result + more;
      }
      state.pos = save;
      return result;
    } else {
      result += c;
      state.pos++;
    }
  }
  return null;
}

function parseAttrValue(state: State): AttrValue | null {
  skipWs(state);
  if (state.src[state.pos] === '"') {
    return parseString(state);
  }
  const start = state.pos;
  while (state.pos < state.src.length && /[a-zA-Z0-9_\-.]/.test(state.src[state.pos]!)) {
    state.pos++;
  }
  if (state.pos === start) return null;
  const raw = state.src.slice(start, state.pos);
  if (/^-?\d+$/.test(raw)) return Number.parseInt(raw, 10);
  if (/^-?\d+\.\d+$/.test(raw)) return Number.parseFloat(raw);
  if (raw === "true") return true;
  if (raw === "false") return false;
  return raw;
}

function parseAttrList(state: State): Record<string, AttrValue> | null {
  skipWs(state);
  if (state.src[state.pos] !== "[") return null;
  state.pos++;
  const out: Record<string, AttrValue> = {};
  while (true) {
    skipWs(state);
    if (state.pos >= state.src.length) return null;
    if (state.src[state.pos] === "]") {
      state.pos++;
      return out;
    }
    const key = parseIdent(state);
    if (!key) return null;
    skipWs(state);
    if (state.src[state.pos] !== "=") return null;
    state.pos++;
    const value = parseAttrValue(state);
    if (value === null) return null;
    out[key] = value;
    skipWs(state);
    if (state.src[state.pos] === "," || state.src[state.pos] === ";") {
      state.pos++;
    }
  }
}

function tryConsume(state: State, word: string): boolean {
  skipWs(state);
  if (state.src.slice(state.pos, state.pos + word.length) !== word) return false;
  // Word-boundary check so `nodename` doesn't match `node`.
  const after = state.src[state.pos + word.length];
  if (after && /[a-zA-Z0-9_]/.test(after)) return false;
  state.pos += word.length;
  return true;
}

function consumeChar(state: State, ch: string): boolean {
  skipWs(state);
  if (state.src[state.pos] !== ch) return false;
  state.pos++;
  return true;
}

function consumeStatementEnd(state: State): void {
  skipWs(state);
  if (state.src[state.pos] === ";") state.pos++;
}

function peek(state: State, str: string): boolean {
  skipWs(state);
  return state.src.slice(state.pos, state.pos + str.length) === str;
}

function isAttrOrEnd(state: State): boolean {
  skipWs(state);
  const c = state.src[state.pos];
  return c === "[" || c === ";" || c === "}" || c === undefined;
}

/**
 * Handle bare `rankdir=LR` style assignments at digraph scope. Returns
 * true if one was consumed.
 */
function tryConsumeBareAssignment(state: State): boolean {
  const save = state.pos;
  skipWs(state);
  const ident = parseIdent(state);
  if (!ident) {
    state.pos = save;
    return false;
  }
  skipWs(state);
  if (state.src[state.pos] !== "=") {
    state.pos = save;
    return false;
  }
  state.pos++;
  parseAttrValue(state);
  consumeStatementEnd(state);
  return true;
}

function fail(message: string, state: State): ParseResult {
  const lines = state.src.slice(0, state.pos).split("\n");
  const line = lines.length;
  const col = lines[lines.length - 1]!.length + 1;
  return { ok: false, error: `${message} (line ${line}, col ${col})` };
}

/**
 * Convert PascalCase or camelCase back to snake_case so `digraph
 * ReleaseNotes` round-trips with `release_notes`.
 */
function toSnakeCase(name: string): string {
  return name
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1_$2")
    .toLowerCase();
}
