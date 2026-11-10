import { parseWorkflow } from "./dot-parse";
import { serializeWorkflow } from "./dot-serialize";
import { effectiveNodeAttributes, getAttr, nodeOwner, setAttr } from "./effective";
import { planAssetMove } from "./dependencies";
import {
  assetPathProblem,
  canonicalAssets,
  compilePromptParts,
  getFile,
  normalizeAssetPath,
  promptPartsFromAttribute,
  RESERVED_EXPORT_PATHS,
  setFile,
} from "./prompts";
import type { CommentAnchor, Diagnostic, Point, PromptPart, WorkflowDraft, WorkflowGraph } from "./types";
import { validateWorkflow } from "./validate";

/** Drafts: creating them, keeping prompt blocks and source in step, renaming safely, and exporting. */

/** The settings an export writes unless the draft holds its own. Compared whole, never by a marker in it. */
export const GENERATED_WORKFLOW_TOML = `_version = 1

[workflow]
graph = "workflow.fabro"

# Exported by the visual builder: a run of this workflow opens no pull request.
[run.pull_request]
enabled = false
`;

/** An attached file at this path is the workflow's settings, exported exactly as written. */
export const CUSTOM_TOML_PATH = "workflow.toml";

const TEMPLATE_PROMPT = "Complete this goal: {{ goal }}";

export function randomId(): string {
  const crypto = globalThis.crypto;
  if (typeof crypto?.randomUUID === "function") return crypto.randomUUID();
  // randomUUID needs a secure context; a self-hosted server on plain http has only getRandomValues.
  const bytes = new Uint8Array(16);
  if (typeof crypto?.getRandomValues === "function") {
    crypto.getRandomValues(bytes);
  } else {
    for (let index = 0; index < bytes.length; index++) bytes[index] = Math.floor(Math.random() * 256);
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function graphNameFrom(name: string): string {
  const words = name.match(/[A-Za-z0-9]+/g) ?? [];
  const joined = words.map((word) => word[0].toUpperCase() + word.slice(1)).join("");
  return /^[A-Za-z_]/.test(joined) ? joined : `Workflow${joined}`;
}

/** A new draft holding a valid Start → Agent → Exit workflow. */
export function newDraft(name?: string): WorkflowDraft {
  const displayName = name?.trim() || "Untitled workflow";
  const source = `digraph ${graphNameFrom(displayName)} {
  graph [goal="Describe the outcome this workflow should produce."]
  rankdir=LR
  start [shape=Mdiamond, label="Start"]
  agent [shape=box, label="Agent", prompt="${TEMPLATE_PROMPT}"]
  exit [shape=Msquare, label="Exit"]
  start -> agent
  agent -> exit
}
`;
  return {
    version: 1,
    id: randomId(),
    name: displayName,
    source,
    positions: { start: { x: 80, y: 160 }, agent: { x: 340, y: 160 }, exit: { x: 600, y: 160 } },
    assets: {},
    promptParts: { agent: [{ id: randomId(), mode: "text", text: TEMPLATE_PROMPT, path: "" }] },
    updatedAt: new Date().toISOString(),
  };
}

function cloneGraph(graph: WorkflowGraph): WorkflowGraph {
  return structuredClone(graph);
}

export interface PromptApplication {
  /** Null when `source` does not parse. */
  graph: WorkflowGraph | null;
  source: string;
  files: Record<string, string>;
  diagnostics: Diagnostic[];
}

/**
 * Write each node's prompt blocks onto its `prompt` attribute. The source text
 * is only rewritten if a value actually changes, so an untouched draft keeps
 * its formatting and comments byte for byte. Blocks that compile to nothing
 * leave the attribute alone, so an empty block can't erase a prompt.
 */
export function applyPromptParts(draft: WorkflowDraft): PromptApplication {
  const parsed = parseWorkflow(draft.source);
  if (!parsed.graph) return { graph: null, source: draft.source, files: {}, diagnostics: parsed.diagnostics };
  const graph = cloneGraph(parsed.graph);
  const nodeIds = new Set(graph.nodes.map((node) => node.id));
  const files: Record<string, string> = {};
  const diagnostics: Diagnostic[] = [...parsed.diagnostics];
  let changed = false;
  for (const nodeId of Object.keys(draft.promptParts)) {
    const parts = draft.promptParts[nodeId];
    if (!nodeIds.has(nodeId)) {
      if (parts.some((part) => part.text.trim().length > 0 || part.path.trim().length > 0)) {
        diagnostics.push({
          severity: "warning",
          code: "builder.orphan_prompt_parts",
          nodeId,
          message: `Prompt blocks are saved for \`${nodeId}\`, but no such node exists, so they are not in the workflow. They stay in the draft and in workflow.builder.json. Rename them onto a node or delete them.`,
        });
      }
      continue;
    }
    const compiled = compilePromptParts(nodeId, parts, draft.assets);
    diagnostics.push(...compiled.diagnostics);
    for (const [path, content] of Object.entries(compiled.files)) setFile(files, path, content);
    if (compiled.prompt.length === 0) continue;
    const node = graph.nodes.find((candidate) => candidate.id === nodeId);
    if (!node) continue;
    const written = getAttr(effectiveNodeAttributes(graph, node), "prompt");
    if (written !== compiled.prompt) {
      if (written?.trim()) {
        diagnostics.push({
          severity: "warning",
          code: "builder.prompt_replaced",
          nodeId,
          message: `The prompt written in the source for \`${nodeId}\` differs from its prompt blocks, and the blocks were used. If the source is right, rebuild the blocks from it.`,
        });
      }
      setAttr(node.attrs, "prompt", compiled.prompt);
      changed = true;
    }
  }
  return { graph, source: changed ? serializeWorkflow(graph) : draft.source, files, diagnostics };
}

/**
 * After the source was edited as code: keep blocks that still compile to the
 * node's `prompt`, and rebuild the others from it, because the code is what the
 * person just changed. Pass the source from before the edit to rebuild only the
 * prompts the edit really changed. Blocks for missing nodes are kept;
 * `applyPromptParts` reports them.
 */
export function reconcilePromptParts(draft: WorkflowDraft, previousSource?: string): Record<string, PromptPart[]> {
  const parsed = parseWorkflow(draft.source);
  if (!parsed.graph) return draft.promptParts;
  const previous = previousSource === undefined ? null : parseWorkflow(previousSource).graph;
  const previousPrompt = (id: string): string | undefined => {
    const node = previous?.nodes.find((candidate) => candidate.id === id);
    return previous && node ? getAttr(effectiveNodeAttributes(previous, node), "prompt") : undefined;
  };
  const next: Record<string, PromptPart[]> = {};
  for (const nodeId of Object.keys(draft.promptParts)) {
    Object.defineProperty(next, nodeId, {
      value: draft.promptParts[nodeId],
      enumerable: true,
      writable: true,
      configurable: true,
    });
  }
  for (const node of parsed.graph.nodes) {
    const prompt = getAttr(effectiveNodeAttributes(parsed.graph, node), "prompt");
    const existing = Object.hasOwn(next, node.id) ? next[node.id] : undefined;
    if (!existing) continue;
    if (previous?.nodes.some((candidate) => candidate.id === node.id) && previousPrompt(node.id) === prompt) continue;
    const compiled = compilePromptParts(node.id, existing, draft.assets);
    if (compiled.prompt === (prompt ?? "") || compiled.prompt.length === 0) continue;
    next[node.id] = promptPartsFromAttribute(node.id, prompt);
  }
  return next;
}

export interface DraftEdit {
  draft: WorkflowDraft;
  diagnostics: Diagnostic[];
}

function refused(draft: WorkflowDraft, code: string, message: string, nodeId?: string): DraftEdit {
  const diagnostic: Diagnostic = { severity: "error", code, message };
  if (nodeId !== undefined) diagnostic.nodeId = nodeId;
  return { draft, diagnostics: [diagnostic] };
}

function rewriteSpellingSlots(graph: WorkflowGraph, rewrite: (owner: string) => string): void {
  const meta = graph.sourceMeta;
  if (!meta) return;
  const remap = (slots: string[]) =>
    slots.map((slot) => {
      const separator = slot.indexOf("\u0000");
      return `${rewrite(slot.slice(0, separator))}${slot.slice(separator)}`;
    });
  if (meta.bare) meta.bare = remap(meta.bare);
  if (meta.quoted) meta.quoted = remap(meta.quoted);
}

function rekeyAnchors(graph: WorkflowGraph, rewrite: (anchor: CommentAnchor) => CommentAnchor): void {
  if (graph.sourceMeta?.commentAnchors) {
    graph.sourceMeta.commentAnchors = graph.sourceMeta.commentAnchors.map(({ comment, anchor }) => ({
      comment,
      anchor: rewrite(anchor),
    }));
  }
}

/**
 * Rename a node everywhere it is named: edges, retry targets, human default
 * choices, its position and its prompt blocks. Refuses a name that is taken, so
 * a rename cannot merge two nodes. A stylesheet `#id` selector is flagged rather
 * than rewritten, since it may sit inside a template.
 */
export function renameDraftNode(draft: WorkflowDraft, from: string, to: string): DraftEdit {
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(to)) {
    return refused(draft, "builder.invalid_node_id", `"${to}" is not a valid node id. Use letters, digits and underscores, starting with a letter or underscore.`, from);
  }
  const parsed = parseWorkflow(draft.source);
  if (!parsed.graph) return { draft, diagnostics: parsed.diagnostics };
  const graph = cloneGraph(parsed.graph);
  const node = graph.nodes.find((candidate) => candidate.id === from);
  if (!node) return refused(draft, "builder.unknown_node", `There is no node \`${from}\` to rename.`, from);
  if (from === to) return { draft, diagnostics: [] };
  if (graph.nodes.some((candidate) => candidate.id === to)) {
    return refused(draft, "builder.duplicate_node_id", `Node \`${to}\` already exists, so \`${from}\` was not renamed.`, from);
  }

  const renameReference = (value: string): string => {
    if (value === from) return to;
    return value.startsWith(`${from}.`) ? `${to}${value.slice(from.length)}` : value;
  };
  const rewriteTargets = (attrs: Record<string, string>) => {
    for (const key of ["retry_target", "fallback_retry_target"]) {
      const value = getAttr(attrs, key);
      if (value !== undefined) setAttr(attrs, key, renameReference(value));
    }
    if (getAttr(attrs, "human.default_choice") === from) setAttr(attrs, "human.default_choice", to);
  };
  rewriteTargets(graph.attrs);
  rewriteTargets(graph.nodeDefaults);
  for (const group of graph.groups) rewriteTargets(group.nodeDefaults);
  for (const candidate of graph.nodes) rewriteTargets(candidate.attrs);

  const oldOwner = nodeOwner(from);
  const newOwner = nodeOwner(to);
  rewriteSpellingSlots(graph, (owner) => (owner === oldOwner ? newOwner : owner));
  rekeyAnchors(graph, (anchor) => (anchor.kind === "node" && anchor.id === from ? { ...anchor, id: to } : anchor));
  node.id = to;
  const edgeIdChanges = new Map<string, string>();
  const counts = new Map<string, number>();
  for (const edge of graph.edges) {
    if (edge.source === from) edge.source = to;
    if (edge.target === from) edge.target = to;
    const base = `${edge.source}->${edge.target}`;
    const count = (counts.get(base) ?? 0) + 1;
    counts.set(base, count);
    const id = count === 1 ? base : `${base}#${count}`;
    if (id !== edge.id) edgeIdChanges.set(edge.id, id);
    edge.id = id;
  }
  rewriteSpellingSlots(graph, (owner) => {
    const changed = owner.startsWith("edge:") ? edgeIdChanges.get(owner.slice("edge:".length)) : undefined;
    return changed ? `edge:${changed}` : owner;
  });
  rekeyAnchors(graph, (anchor) =>
    anchor.kind === "edge" && edgeIdChanges.has(anchor.id)
      ? { ...anchor, id: edgeIdChanges.get(anchor.id) as string }
      : anchor,
  );
  // A location also marks a node as read from source, which keeps its place in the output.
  const location = graph.sourceMeta?.locations?.[oldOwner];
  if (graph.sourceMeta?.locations && location) {
    delete graph.sourceMeta.locations[oldOwner];
    graph.sourceMeta.locations[newOwner] = location;
  }

  const positions: Record<string, Point> = {};
  for (const key of Object.keys(draft.positions)) {
    Object.defineProperty(positions, key === from ? to : key, {
      value: draft.positions[key],
      enumerable: true,
      writable: true,
      configurable: true,
    });
  }
  const promptParts: Record<string, PromptPart[]> = {};
  for (const key of Object.keys(draft.promptParts)) {
    Object.defineProperty(promptParts, key === from ? to : key, {
      value: draft.promptParts[key],
      enumerable: true,
      writable: true,
      configurable: true,
    });
  }

  const diagnostics: Diagnostic[] = [];
  const stylesheet = getAttr(graph.attrs, "model_stylesheet");
  if (stylesheet?.includes(`#${from}`)) {
    diagnostics.push({
      severity: "warning",
      code: "builder.stylesheet_selector",
      nodeId: to,
      message: `The model stylesheet still selects \`#${from}\`. Update it to \`#${to}\`.`,
    });
  }
  const renamed: WorkflowDraft = { ...draft, source: serializeWorkflow(graph), positions, promptParts };
  // A generated prompt path follows the node's new name.
  const applied = applyPromptParts(renamed);
  return {
    draft: { ...renamed, source: applied.source },
    diagnostics: [...diagnostics, ...applied.diagnostics.filter((item) => item.severity === "error")],
  };
}

/**
 * Rename an uploaded file everywhere it is named: prompt blocks, `@file`
 * attributes, imports and child workflows, in the workflow and in every attached
 * workflow, each read from its own folder. It's all or nothing: an unsafe or taken
 * path, or a reference that couldn't be updated, leaves the draft as it was.
 */
export function renameDraftAsset(draft: WorkflowDraft, from: string, to: string): DraftEdit {
  const oldPath = normalizeAssetPath(from);
  const newPath = normalizeAssetPath(to);
  const { assets: current, conflicts } = canonicalAssets(draft.assets);
  // The new assets are rebuilt from canonical names, which would drop one side of any conflict, related or not.
  if (conflicts.length > 0) {
    const [first, second] = conflicts[0];
    return refused(draft, "builder.path_collision", `"${first}" and "${second}" name the same file with different content, so "${from}" was not renamed. Remove one first.`);
  }
  const content = getFile(current, oldPath);
  if (content === undefined) return refused(draft, "builder.missing_file", `There is no file "${from}" in this draft.`);
  const problem = assetPathProblem(newPath);
  if (problem) return refused(draft, "builder.unsafe_path", `"${to}" ${problem}, so "${from}" was not renamed.`);
  if (RESERVED_EXPORT_PATHS.includes(newPath)) {
    return refused(draft, "builder.path_collision", `"${newPath}" is reserved for the workflow's own files, so "${from}" was not renamed.`);
  }
  if (oldPath === newPath) return { draft, diagnostics: [] };
  if (getFile(current, newPath) !== undefined) {
    return refused(draft, "builder.path_collision", `A file "${newPath}" already exists, so "${from}" was not renamed.`);
  }
  const parsed = parseWorkflow(draft.source);
  if (!parsed.graph) {
    // The references can't be updated in source that doesn't parse, and a half rename would break them.
    return refused(draft, "builder.unparseable_source", `The workflow source has a syntax error, so "${from}" was not renamed. Fix the source first.`);
  }
  const move = planAssetMove(parsed.graph, current, oldPath, newPath);
  if (move.problem) {
    return refused(draft, "builder.rename_refused", `"${from}" was not renamed to "${to}". ${move.problem}`);
  }

  const assets: Record<string, string> = {};
  for (const path of Object.keys(current)) {
    setFile(assets, path === oldPath ? newPath : path, getFile(move.attachedText, path) ?? current[path]);
  }
  const promptParts: Record<string, PromptPart[]> = {};
  for (const nodeId of Object.keys(draft.promptParts)) {
    Object.defineProperty(promptParts, nodeId, {
      value: draft.promptParts[nodeId].map((part) =>
        part.mode === "file" && normalizeAssetPath(part.path) === oldPath ? { ...part, path: newPath } : part,
      ),
      enumerable: true,
      writable: true,
      configurable: true,
    });
  }
  const source = move.rootGraph ? serializeWorkflow(move.rootGraph) : draft.source;
  return { draft: { ...draft, assets, promptParts, source }, diagnostics: [] };
}

export interface DraftExport {
  files: Record<string, string>;
  diagnostics: Diagnostic[];
  /** True when the files are not safe to hand over: unparseable source, an unsafe path, or a collision. */
  blocked: boolean;
}

const BLOCKING_CODES = new Set(["builder.unsafe_path", "builder.path_collision"]);

/**
 * The workflow folder as files, with everything worth knowing about it. Warnings
 * and structural errors don't stop an export, so work in progress can be
 * downloaded. Only output that would be unusable or unsafe to unpack does.
 */
export function exportDraft(input: WorkflowDraft): DraftExport {
  // Normalizing first means a file can't be lost by two spellings landing on one path.
  const { assets, conflicts } = canonicalAssets(input.assets);
  const draft: WorkflowDraft = { ...input, assets };
  const applied = applyPromptParts(draft);
  if (!applied.graph) return { files: {}, diagnostics: applied.diagnostics, blocked: true };
  // A missing single-file prompt is `@path` in the graph by now, so validation reports it below.
  const diagnostics: Diagnostic[] = applied.diagnostics.filter(
    (diagnostic) => !(diagnostic.code === "builder.missing_file" && diagnostic.severity === "warning"),
  );
  const reject = (message: string) =>
    diagnostics.push({ severity: "error", code: "builder.path_collision", message });
  for (const [first, second] of conflicts) {
    reject(`"${first}" and "${second}" name the same file with different content. Remove one.`);
  }

  const customToml = getFile(assets, CUSTOM_TOML_PATH);
  if (customToml !== undefined) {
    diagnostics.push({
      severity: "warning",
      code: "builder.custom_workflow_toml",
      message: `This workflow has its own ${CUSTOM_TOML_PATH}, exported exactly as written. It applies when the workflow runs, and can change environments, hooks and pull request behavior.`,
    });
  }

  const files: Record<string, string> = {};
  setFile(files, "workflow.fabro", applied.source);
  setFile(files, CUSTOM_TOML_PATH, customToml ?? GENERATED_WORKFLOW_TOML);
  setFile(
    files,
    "workflow.builder.json",
    `${JSON.stringify(
      {
        version: draft.version,
        id: draft.id,
        name: draft.name,
        positions: draft.positions,
        promptParts: draft.promptParts,
        updatedAt: draft.updatedAt,
      },
      null,
      2,
    )}\n`,
  );

  for (const path of Object.keys(assets)) {
    const problem = assetPathProblem(path);
    if (problem) {
      diagnostics.push({
        severity: "error",
        code: "builder.unsafe_path",
        message: `Uploaded file "${path}" ${problem}. It cannot be included in a download. Rename or remove it.`,
      });
      continue;
    }
    if (path === CUSTOM_TOML_PATH) continue;
    if (RESERVED_EXPORT_PATHS.includes(path)) {
      reject(`Uploaded file "${path}" would overwrite the workflow's own file. Rename it.`);
      continue;
    }
    setFile(files, path, assets[path]);
  }
  for (const [path, content] of Object.entries(applied.files)) {
    if (path === CUSTOM_TOML_PATH) continue;
    if (RESERVED_EXPORT_PATHS.includes(path)) {
      reject(`A prompt file would overwrite the workflow's own "${path}". Rename the upload.`);
      continue;
    }
    const existing = getFile(files, path);
    if (existing !== undefined && existing !== content) {
      reject(`"${path}" is both an uploaded file and a generated prompt, with different content. Rename the upload.`);
      continue;
    }
    setFile(files, path, content);
  }

  // These don't coexist in one folder on case-insensitive filesystems, or at all when one is a folder.
  const paths = Object.keys(files);
  const byFoldedPath = new Map<string, string>();
  for (const path of paths) {
    const folded = path.toLowerCase();
    const other = byFoldedPath.get(folded);
    if (other !== undefined) reject(`"${other}" and "${path}" differ only by case, so one would overwrite the other on many machines. Rename one.`);
    byFoldedPath.set(folded, path);
  }
  for (const path of paths) {
    const segments = path.split("/");
    for (let depth = 1; depth < segments.length; depth++) {
      const folder = segments.slice(0, depth).join("/");
      if (getFile(files, folder) !== undefined) reject(`"${folder}" is a file, but "${path}" needs it to be a folder. Rename one.`);
    }
  }

  diagnostics.push(...validateWorkflow(applied.graph, files));
  return {
    files,
    diagnostics,
    blocked: diagnostics.some(
      (diagnostic) =>
        diagnostic.severity === "error" && diagnostic.code !== undefined && BLOCKING_CODES.has(diagnostic.code),
    ),
  };
}

export class DraftExportError extends Error {
  readonly diagnostics: Diagnostic[];

  constructor(diagnostics: Diagnostic[]) {
    const errors = diagnostics.filter((diagnostic) => diagnostic.severity === "error");
    const first = errors[0]?.message ?? "The draft cannot be exported.";
    super(errors.length > 1 ? `${first} (and ${errors.length - 1} more)` : first);
    this.name = "DraftExportError";
    this.diagnostics = diagnostics;
  }
}

/**
 * The repo-ready files of a draft, keyed by path relative to the workflow folder.
 * Throws `DraftExportError` rather than return unparseable source or an unsafe
 * path. Use `exportDraft` to see the problems without a throw.
 */
export function draftFiles(draft: WorkflowDraft): Record<string, string> {
  const exported = exportDraft(draft);
  if (exported.blocked) throw new DraftExportError(exported.diagnostics);
  return exported.files;
}
