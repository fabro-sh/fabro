import { assetIsReferenced } from "../../lib/workflow-builder/dependencies";
import { effectiveNodeAttributes } from "../../lib/workflow-builder/effective";
import { parseWorkflow, renameDraftAsset, renameDraftNode } from "../../lib/workflow-builder/model";
import {
  compilePromptParts,
  getFile,
  normalizeAssetPath,
  promptPartsFromAttribute,
  setFile,
} from "../../lib/workflow-builder/prompts";
import type { Diagnostic, Point, PromptPart, WorkflowGraph, WorkflowNode } from "../../lib/workflow-builder/types";
import { attachmentPathProblem } from "./attachments";
import type { BuilderDraft, EditorChange, EditorContext } from "./editor-state";
import {
  deleteEdges,
  deleteGroup,
  deleteNodes,
  duplicateNode,
  renameGroup,
  updateNodeAttributes,
} from "./graph-ops";
import { findFreeSpot, groupPositionKey, nodeSize } from "./layout";

/**
 * Edits that span the graph and the rest of the draft: positions, prompt blocks
 * and attached files all key off node IDs and prompt values.
 */

let partCounter = 0;

/** A block ID unique within this page's lifetime. Only ever compared for identity. */
export function newPromptPartId(): string {
  partCounter += 1;
  return `part-${Date.now().toString(36)}-${partCounter}`;
}

/**
 * The blocks to show for a node. The source is canonical, so stored blocks count
 * only while they still compile to the node's prompt. If the code changed the
 * prompt since, the blocks are read back from the code.
 */
export function promptPartsFor(
  graph: WorkflowGraph,
  node: WorkflowNode,
  draft: Pick<BuilderDraft, "promptParts" | "assets">,
): PromptPart[] {
  const prompt = effectiveNodeAttributes(graph, node).prompt;
  const stored = draft.promptParts[node.id];
  if (stored && stored.length > 0) {
    const compiled = compilePromptParts(node.id, stored, draft.assets);
    if (compiled.prompt === (prompt ?? "")) return stored;
    if (compiled.prompt === "" && stored.every((part) => !part.text.trim() && !part.path.trim())) return stored;
  }
  return promptPartsFromAttribute(node.id, prompt);
}

function withoutKey<T>(record: Record<string, T>, key: string): Record<string, T> {
  const { [key]: _removed, ...rest } = record;
  return rest;
}

/**
 * The file a node's blocks generated, if that can be proven: compiling them again
 * gives exactly what the file holds. So its content can always be rebuilt, and
 * nothing is lost if it goes. A name alone proves nothing, since anyone can upload
 * `prompts/agent.md`.
 */
function provenGeneratedFile(
  nodeId: string,
  parts: PromptPart[] | undefined,
  assets: Record<string, string>,
): string | null {
  if (!parts) return null;
  const { generatedPath, files } = compilePromptParts(nodeId, parts, assets);
  if (generatedPath === undefined) return null;
  return getFile(assets, generatedPath) === getFile(files, generatedPath) ? generatedPath : null;
}

/** Every attached file a node's blocks generated, for telling those apart from uploads. */
export function generatedAttachmentPaths(draft: Pick<BuilderDraft, "promptParts" | "assets">): Set<string> {
  const paths = new Set<string>();
  for (const nodeId of Object.keys(draft.promptParts)) {
    const path = provenGeneratedFile(nodeId, draft.promptParts[nodeId], draft.assets);
    if (path !== null) paths.add(path);
  }
  return paths;
}

/** Removes a node's generated file, unless something else in the draft still uses it. */
function releaseGeneratedFile(
  assets: Record<string, string>,
  path: string | null,
  check: { graph: WorkflowGraph; promptParts: Record<string, PromptPart[]>; exceptPromptOf?: string },
): void {
  if (path !== null && !assetIsReferenced(path, { ...check, assets })) delete assets[path];
}

function writeCompiledFiles(assets: Record<string, string>, files: Record<string, string>): void {
  for (const [path, content] of Object.entries(files)) setFile(assets, path, content);
}

/** Writes blocks onto a node and returns the graph and files that change. */
function compileInto(
  graph: WorkflowGraph,
  nodeId: string,
  parts: PromptPart[],
  assets: Record<string, string>,
): { graph: WorkflowGraph; assets: Record<string, string> } {
  const compiled = compilePromptParts(nodeId, parts, assets);
  const nextAssets = { ...assets };
  writeCompiledFiles(nextAssets, compiled.files);
  // A block with an unsafe path compiles to nothing. That must not erase what the source says.
  const refused = compiled.diagnostics.some((diagnostic) => diagnostic.code === "builder.unsafe_path");
  if (!compiled.prompt && refused) return { graph, assets: nextAssets };
  return {
    graph: updateNodeAttributes(graph, nodeId, { prompt: compiled.prompt || undefined }),
    assets: nextAssets,
  };
}

/**
 * Editing a node's blocks. Its old generated file goes first if nothing else uses
 * it, so the new one can take its place. If something does use it, or it isn't
 * proven to be generated, it stays, and the new prompt takes a free path instead.
 * Other nodes joining a file that was just edited get theirs rebuilt the same way.
 */
export function setPromptPartsChange(
  { draft, graph }: EditorContext,
  nodeId: string,
  parts: PromptPart[],
  assetEdits: Record<string, string> = {},
): EditorChange | null {
  if (!graph) return null;
  const promptParts = { ...draft.promptParts, [nodeId]: parts };
  const assets = { ...draft.assets };
  const edited = new Set(Object.keys(assetEdits).map(normalizeAssetPath));
  const previous = provenGeneratedFile(nodeId, draft.promptParts[nodeId], draft.assets);
  const stale = Object.keys(draft.promptParts)
    .filter(
      (id) =>
        id !== nodeId &&
        draft.promptParts[id].some((part) => part.mode === "file" && edited.has(normalizeAssetPath(part.path))),
    )
    .map((id) => ({ id, path: provenGeneratedFile(id, draft.promptParts[id], draft.assets) }));
  for (const [path, content] of Object.entries(assetEdits)) setFile(assets, normalizeAssetPath(path), content);

  const namedByBlocks = parts.some((part) => part.mode === "file" && normalizeAssetPath(part.path) === previous);
  if (previous !== null && !edited.has(previous) && !namedByBlocks) {
    releaseGeneratedFile(assets, previous, { graph, promptParts, exceptPromptOf: nodeId });
  }
  for (const { id, path } of stale) {
    if (path === null || assetIsReferenced(path, { graph, assets, promptParts, exceptPromptOf: id })) continue;
    const { [path]: _replaced, ...without } = assets;
    const rebuilt = compilePromptParts(id, promptParts[id], without);
    // Only if it lands on the same path: that node's prompt still names that one.
    if (rebuilt.generatedPath === path) writeCompiledFiles(assets, rebuilt.files);
  }
  const compiled = compileInto(graph, nodeId, parts, assets);
  return { graph: compiled.graph, assets: compiled.assets, promptParts };
}

function firstError(diagnostics: Diagnostic[]): string | null {
  return diagnostics.find((diagnostic) => diagnostic.severity === "error")?.message ?? null;
}

/**
 * Moves a file block to a new path. An attached file moves with it, and everything
 * else naming that file follows; a path that would replace another file is refused.
 * A path with nothing attached is just a reference, so only the block changes.
 */
export function renamePromptFile(
  context: EditorContext,
  nodeId: string,
  parts: PromptPart[],
  partId: string,
  nextPath: string,
): { change: EditorChange | null; error: string | null } {
  const { draft, graph } = context;
  const part = parts.find((candidate) => candidate.id === partId);
  if (!graph || !part || part.path === nextPath) return { change: null, error: null };
  // Say so now: the core refuses these at compile time, which would look like a silent revert.
  const problem = nextPath ? attachmentPathProblem(nextPath) : null;
  if (problem) return { change: null, error: problem };
  const attached = part.path !== "" && getFile(draft.assets, normalizeAssetPath(part.path)) !== undefined;
  if (attached && nextPath) {
    const result = renameDraftAsset(
      { ...draft, promptParts: { ...draft.promptParts, [nodeId]: parts } },
      part.path,
      nextPath,
    );
    const error = firstError(result.diagnostics);
    if (error) return { change: null, error };
    return {
      change: {
        source: result.draft.source,
        assets: result.draft.assets,
        promptParts: result.draft.promptParts,
      },
      error: null,
    };
  }
  const nextParts = parts.map((candidate) =>
    candidate.id === partId ? { ...candidate, path: nextPath } : candidate,
  );
  return { change: setPromptPartsChange(context, nodeId, nextParts), error: null };
}

/**
 * Renames a node through the core, which updates edges, retry targets, positions
 * and prompt blocks, and refuses a taken name. A generated prompt file follows the
 * new name. `warnings` carries what the author still has to do by hand.
 */
export function renameNodeChange(
  { draft }: EditorContext,
  from: string,
  to: string,
): { change: EditorChange | null; error: string | null; warnings: string[] } {
  if (from === to) return { change: null, error: null, warnings: [] };
  const result = renameDraftNode(draft, from, to);
  const error = firstError(result.diagnostics);
  if (error) return { change: null, error, warnings: [] };
  const assets = { ...draft.assets };
  const renamed = parseWorkflow(result.draft.source).graph;
  if (renamed) {
    releaseGeneratedFile(assets, provenGeneratedFile(from, draft.promptParts[from], draft.assets), {
      graph: renamed,
      promptParts: result.draft.promptParts,
      exceptPromptOf: to,
    });
  }
  const parts = result.draft.promptParts[to];
  if (parts) writeCompiledFiles(assets, compilePromptParts(to, parts, assets).files);
  return {
    change: {
      source: result.draft.source,
      positions: result.draft.positions,
      promptParts: result.draft.promptParts,
      assets,
    },
    error: null,
    warnings: result.diagnostics
      .filter((diagnostic) => diagnostic.severity === "warning")
      .map((diagnostic) => diagnostic.message),
  };
}

export function renameGroupChange(
  { draft, graph }: EditorContext,
  from: string,
  to: string,
): EditorChange | null {
  if (!graph || from === to) return null;
  const positions: Record<string, Point> = withoutKey(draft.positions, groupPositionKey(from));
  const stored = draft.positions[groupPositionKey(from)];
  if (stored) positions[groupPositionKey(to)] = stored;
  return { graph: renameGroup(graph, from, to), positions };
}

export interface DeleteTargets {
  nodes: string[];
  edges: string[];
  groups: string[];
}

/** Removes nodes with their edges, edges, and groups. A group's members stay. */
export function deleteSelectionChange(
  { draft, graph }: EditorContext,
  targets: DeleteTargets,
): EditorChange | null {
  if (!graph) return null;
  if (targets.nodes.length + targets.edges.length + targets.groups.length === 0) return null;
  let next = deleteEdges(graph, targets.edges);
  next = deleteNodes(next, targets.nodes);
  for (const group of targets.groups) next = deleteGroup(next, group);

  const positions = { ...draft.positions };
  const promptParts = { ...draft.promptParts };
  const assets = { ...draft.assets };
  for (const id of targets.nodes) {
    delete positions[id];
    delete promptParts[id];
  }
  // A joined prompt goes with its node, unless something else uses it. Files the author attached stay.
  for (const id of targets.nodes) {
    releaseGeneratedFile(assets, provenGeneratedFile(id, draft.promptParts[id], draft.assets), {
      graph: next,
      promptParts,
    });
  }
  for (const group of targets.groups) delete positions[groupPositionKey(group)];
  return { graph: next, positions, promptParts, assets };
}

/** Copies nodes without their edges. Returns the copies' IDs for selecting them. */
export function duplicateNodesChange(
  { draft, graph }: EditorContext,
  ids: string[],
): { change: EditorChange; ids: string[] } | null {
  if (!graph) return null;
  let next = graph;
  const positions = { ...draft.positions };
  let promptParts = draft.promptParts;
  let assets = draft.assets;
  const created: string[] = [];
  for (const id of ids) {
    const copy = duplicateNode(next, id);
    if (!copy) continue;
    next = copy.graph;
    created.push(copy.id);
    const origin = positions[id];
    const copied = next.nodes.find((node) => node.id === copy.id);
    if (origin && copied) {
      // Clear of everything, so the original and its handles stay reachable.
      positions[copy.id] = findFreeSpot(next, positions, {
        x: origin.x + 32,
        y: origin.y + 32,
        ...nodeSize(next, copied),
      });
    }
    const stored = draft.promptParts[id];
    if (stored) {
      const parts = stored.map((part) => ({ ...part, id: newPromptPartId() }));
      const compiled = compileInto(next, copy.id, parts, assets);
      next = compiled.graph;
      assets = compiled.assets;
      promptParts = { ...promptParts, [copy.id]: parts };
    }
  }
  if (created.length === 0) return null;
  return { change: { graph: next, positions, promptParts, assets }, ids: created };
}
