import { parseWorkflow } from "./dot-parse";
import { serializeWorkflow } from "./dot-serialize";
import { effectiveNodeAttributes, getAttr, setAttr } from "./effective";
import { getFile, normalizeAssetPath } from "./prompts";
import type { Attributes, PromptPart, WorkflowGraph } from "./types";

/**
 * Which workflow files name which other files. The workflow and every workflow
 * attached to it are read the same way, each from its own folder, so that a
 * rename can follow its references and a cleanup can tell a file is still in use.
 *
 * Inline source (`stack.child_dot_source`) is read too, at any depth. An inline
 * child reads files from the folder of the file that holds it.
 */

export type FileReferenceKind = "prompt" | "output_schema" | "goal" | "model_stylesheet" | "import" | "child_workflow";

export interface FileReference {
  kind: FileReferenceKind;
  /** As written, without the `@`. */
  path: string;
  nodeId?: string;
}

const AT_REFERENCES: [string, FileReferenceKind][] = [
  ["prompt", "prompt"],
  ["output_schema", "output_schema"],
];
const PLAIN_REFERENCES: [string, FileReferenceKind][] = [
  ["import", "import"],
  ["stack.child_workflow", "child_workflow"],
];
const GRAPH_AT_REFERENCES: [string, FileReferenceKind][] = [
  ["goal", "goal"],
  ["model_stylesheet", "model_stylesheet"],
];
const INLINE_CHILD = "stack.child_dot_source";

/** Every file this one graph names through Fabro's reference vocabulary, with defaults in scope applied. Not its inline children. */
export function workflowFileReferences(graph: WorkflowGraph): FileReference[] {
  const references: FileReference[] = [];
  for (const [key, kind] of GRAPH_AT_REFERENCES) {
    const value = getAttr(graph.attrs, key);
    if (value?.startsWith("@")) references.push({ kind, path: value.slice(1) });
  }
  for (const node of graph.nodes) {
    const attrs = effectiveNodeAttributes(graph, node);
    for (const [key, kind] of AT_REFERENCES) {
      const value = getAttr(attrs, key);
      if (value?.startsWith("@")) references.push({ kind, path: value.slice(1), nodeId: node.id });
    }
    for (const [key, kind] of PLAIN_REFERENCES) {
      const value = getAttr(attrs, key);
      if (value !== undefined) references.push({ kind, path: value, nodeId: node.id });
    }
  }
  return references;
}

export function directoryOf(path: string): string {
  return path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "";
}

function joinPath(directory: string, path: string): string {
  const relative = normalizeAssetPath(path);
  return directory.length > 0 ? `${directory}/${relative}` : relative;
}

/**
 * The attached file a reference reaches, or null when it isn't in the draft.
 * Fabro looks bundled files up by exact path, so a `..` never resolves, even one
 * that would land back inside the bundle.
 */
export function resolveFileReference(
  reference: Pick<FileReference, "kind" | "path">,
  assets: Record<string, string>,
  directory: string,
): string | null {
  const path = normalizeAssetPath(reference.path);
  if (path.split("/").includes("..")) return null;
  const candidates = [joinPath(directory, path)];
  if (reference.kind === "child_workflow" && path.startsWith("fabro/")) candidates.push(`.${path}`);
  return candidates.find((candidate) => getFile(assets, candidate) !== undefined) ?? null;
}

/** Where node attributes are written: defaults and the nodes themselves. */
function writtenNodeAttributes(graph: WorkflowGraph): Attributes[] {
  return [graph.nodeDefaults, ...graph.groups.map((group) => group.nodeDefaults), ...graph.nodes.map((node) => node.attrs)];
}

interface InlineFamily {
  /** The graph, then its inline children at any depth. They all read files from the same folder. */
  graphs: WorkflowGraph[];
  /** Inline source that doesn't parse, so what it names can't be known. */
  unreadable: string[];
}

function withInlineChildren(graph: WorkflowGraph): InlineFamily {
  const family: InlineFamily = { graphs: [graph], unreadable: [] };
  for (const current of family.graphs) {
    const sources = new Set(writtenNodeAttributes(current).map((attrs) => getAttr(attrs, INLINE_CHILD)));
    for (const source of sources) {
      if (source === undefined) continue;
      const child = parseWorkflow(source).graph;
      if (child) family.graphs.push(child);
      else family.unreadable.push(source);
    }
  }
  return family;
}

const WORKFLOW_FILE = /\.(fabro|dot|gv)$/;

interface AttachedWorkflow {
  path: string;
  text: string;
  /** Null when it doesn't parse, so what it names can't be known. */
  graph: WorkflowGraph | null;
}

/** Every attached file that is a workflow: by its extension, or because a workflow runs or imports it. */
function attachedWorkflows(root: WorkflowGraph, assets: Record<string, string>): AttachedWorkflow[] {
  const found = new Map<string, AttachedWorkflow>();
  const visit = (path: string) => {
    if (found.has(path)) return;
    const text = getFile(assets, path);
    if (text === undefined) return;
    const graph = parseWorkflow(text).graph;
    found.set(path, { path, text, graph });
    if (graph) follow(graph, directoryOf(path));
  };
  const follow = (graph: WorkflowGraph, directory: string) => {
    for (const inner of withInlineChildren(graph).graphs) {
      for (const reference of workflowFileReferences(inner)) {
        if (reference.kind !== "child_workflow" && reference.kind !== "import") continue;
        const target = resolveFileReference(reference, assets, directory);
        if (target !== null) visit(target);
      }
    }
  };
  follow(root, "");
  for (const path of Object.keys(assets)) {
    if (WORKFLOW_FILE.test(path)) visit(path);
  }
  return [...found.values()];
}

/** The tags that load another file. Dynamic names count: what they load can't be known. */
const LOADS_FILE = /\{%[-+]?\s*(?:include|import|extends)\b/;

/** Where a template tag's name lands, as Petri joins it: `..` steps out of the folder, unlike `@file`. */
function templatePath(directory: string, name: string): string {
  const parts = directory.length > 0 ? directory.split("/") : [];
  for (const part of name.split("/")) {
    if (part === "..") parts.pop();
    else if (part !== "" && part !== ".") parts.push(part);
  }
  return parts.join("/");
}

/**
 * What template tags name in a text, resolved from the file it appears in. Quotes
 * may be escaped to any depth, since the text can be a workflow holding inline
 * source, and an inline child resolves from the same folder as that workflow.
 */
function includeTargets(text: string, directory: string): string[] {
  const tags = /\{%[-+]?\s*(?:include|import|extends)\s+(?:ignore missing\s+)?\\*["']([^"'\\]+)\\*["']/g;
  return [...text.matchAll(tags)].map((match) => templatePath(directory, match[1]));
}

function basename(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

export interface ReferenceCheck {
  graph: WorkflowGraph;
  assets: Record<string, string>;
  promptParts: Record<string, PromptPart[]>;
  /** A node whose own prompt is being replaced, so its reference doesn't count. */
  exceptPromptOf?: string;
}

/** Whether anything in the draft still needs `path`. An attached workflow that can't be read might, so it counts. */
export function assetIsReferenced(path: string, check: ReferenceCheck): boolean {
  const { graph, assets, promptParts, exceptPromptOf } = check;
  const mightNameIt = (text: string) => text.includes(basename(path));
  const familyNeedsIt = (workflow: WorkflowGraph, directory: string) => {
    const { graphs, unreadable } = withInlineChildren(workflow);
    return (
      unreadable.some(mightNameIt) ||
      graphs.some((inner) =>
        workflowFileReferences(inner).some(
          (reference) =>
            // The prompt being replaced is the top-level node's. An inline child can reuse that node's id.
            !(inner === graph && reference.kind === "prompt" && reference.nodeId === exceptPromptOf) &&
            resolveFileReference(reference, assets, directory) === path,
        ),
      )
    );
  };
  if (familyNeedsIt(graph, "")) return true;
  for (const nodeId of Object.keys(promptParts)) {
    if (nodeId === exceptPromptOf) continue;
    if (promptParts[nodeId].some((part) => part.mode === "file" && normalizeAssetPath(part.path) === path)) return true;
  }
  for (const workflow of attachedWorkflows(graph, assets)) {
    if (workflow.path === path) continue;
    if (workflow.graph ? familyNeedsIt(workflow.graph, directoryOf(workflow.path)) : mightNameIt(workflow.text)) return true;
  }
  if (includeTargets(serializeWorkflow(graph), "").includes(path)) return true;
  return Object.keys(assets).some((file) => file !== path && includeTargets(assets[file], directoryOf(file)).includes(path));
}

/**
 * Calls `rewrite` for every reference written in the graph and its inline children,
 * on the attribute that holds it. Inline source is only rewritten if something in
 * it changed, and source that doesn't parse goes to `onUnreadable`. True if any changed.
 */
function rewriteReferences(
  graph: WorkflowGraph,
  rewrite: (reference: Pick<FileReference, "kind" | "path">) => string | null,
  onUnreadable: (source: string) => void,
): boolean {
  let changed = false;
  const apply = (attrs: Attributes, references: [string, FileReferenceKind][], at: boolean) => {
    for (const [key, kind] of references) {
      const value = getAttr(attrs, key);
      if (value === undefined || (at && !value.startsWith("@"))) continue;
      const next = rewrite({ kind, path: at ? value.slice(1) : value });
      if (next === null) continue;
      setAttr(attrs, key, at ? `@${next}` : next);
      changed = true;
    }
  };
  apply(graph.attrs, GRAPH_AT_REFERENCES, true);
  for (const attrs of writtenNodeAttributes(graph)) {
    apply(attrs, AT_REFERENCES, true);
    apply(attrs, PLAIN_REFERENCES, false);
    const source = getAttr(attrs, INLINE_CHILD);
    if (source === undefined) continue;
    const child = parseWorkflow(source).graph;
    if (!child) onUnreadable(source);
    else if (rewriteReferences(child, rewrite, onUnreadable)) {
      setAttr(attrs, INLINE_CHILD, serializeWorkflow(child));
      changed = true;
    }
  }
  return changed;
}

/** How to name `target` from a folder without `..`, which Fabro refuses. Null when that's impossible. */
function pathFrom(directory: string, target: string): string | null {
  if (directory.length === 0) return target;
  return target.startsWith(`${directory}/`) ? target.slice(directory.length + 1) : null;
}

export interface AssetMove {
  /** The rewritten root workflow, when it changed. */
  rootGraph: WorkflowGraph | null;
  /** New text for attached workflows that changed, by their current path. */
  attachedText: Record<string, string>;
  /** Why the move can't be done. Nothing should change if this is set. */
  problem: string | null;
}

/**
 * Works out everything renaming `from` to `to` has to change, reading each
 * workflow from its own folder. A workflow that moves also has its own relative
 * references recomputed. When any of it can't be expressed, nothing changes.
 */
export function planAssetMove(
  root: WorkflowGraph,
  assets: Record<string, string>,
  from: string,
  to: string,
): AssetMove {
  const refuse = (problem: string): AssetMove => ({ rootGraph: null, attachedText: {}, problem });

  /** Source we can't parse only matters if it could name this file, or moves away from what it names. */
  const couldBeAffected = (text: string, moves: boolean) =>
    text.includes(basename(from)) || (moves && /@|stack\.child_workflow|import\s*=/.test(text));

  const rewriteGraph = (graph: WorkflowGraph, name: string, directory: string, nextDirectory: string) => {
    let problem: string | null = null;
    const changed = rewriteReferences(
      graph,
      (reference) => {
        const target = resolveFileReference(reference, assets, directory);
        if (target === null) return null;
        const nextTarget = target === from ? to : target;
        if (nextTarget === target && nextDirectory === directory) return null;
        const next = pathFrom(nextDirectory, nextTarget);
        if (next === null) {
          problem = `${name} needs "${nextTarget}", which it could only reach through "..", and Fabro refuses that. Keep the file inside "${nextDirectory}/".`;
          return null;
        }
        return next === normalizeAssetPath(reference.path) ? null : next;
      },
      (source) => {
        if (couldBeAffected(source, nextDirectory !== directory)) {
          problem = `${name} has inline source with a syntax error, so its references can't be updated. Fix it first.`;
        }
      },
    );
    return { changed, problem: problem as string | null };
  };

  const rootGraph = structuredClone(root);
  const rootResult = rewriteGraph(rootGraph, "The workflow", "", "");
  if (rootResult.problem) return refuse(rootResult.problem);

  const attachedText: Record<string, string> = {};
  for (const workflow of attachedWorkflows(root, assets)) {
    const moving = workflow.path === from;
    const directory = directoryOf(workflow.path);
    const nextDirectory = moving ? directoryOf(to) : directory;
    if (!workflow.graph) {
      if (couldBeAffected(workflow.text, nextDirectory !== directory)) {
        return refuse(`"${workflow.path}" has a syntax error, so its references can't be updated. Fix it first.`);
      }
      continue;
    }
    const graph = structuredClone(workflow.graph);
    const result = rewriteGraph(graph, `"${workflow.path}"`, directory, nextDirectory);
    if (result.problem) return refuse(result.problem);
    if (result.changed) attachedText[workflow.path] = serializeWorkflow(graph);
  }

  // These stay as written, so a change that would break one is refused instead.
  if (includeTargets(serializeWorkflow(root), "").includes(from)) {
    return refuse("The workflow pulls it in with {% include %}, which can't be updated automatically. Change that first.");
  }
  for (const file of Object.keys(assets)) {
    if (includeTargets(assets[file], directoryOf(file)).includes(from)) {
      return refuse(`"${file}" pulls it in with {% include %}, which can't be updated automatically. Change that first.`);
    }
    if (file === from && directoryOf(from) !== directoryOf(to) && includeTargets(assets[file], directoryOf(file)).length > 0) {
      return refuse("It uses {% include %}, which resolves beside the file, so it can't move to another folder automatically.");
    }
  }
  const toml = getFile(assets, "workflow.toml");
  if (toml?.includes(from)) return refuse("workflow.toml mentions it, and that file is kept exactly as written. Change it there first.");

  return { rootGraph: rootResult.changed ? rootGraph : null, attachedText, problem: null };
}

export type WorkflowFileUse = "none" | "names_files" | "unreadable";

/**
 * What a workflow's source does with other files, inline children included.
 * `unreadable` means some of it doesn't parse, so it could name anything.
 */
export function workflowFileUse(source: string): WorkflowFileUse {
  const graph = parseWorkflow(source).graph;
  if (!graph) return "unreadable";
  const { graphs, unreadable } = withInlineChildren(graph);
  if (unreadable.length > 0) return "unreadable";
  const names = graphs.some((inner) => workflowFileReferences(inner).length > 0) || LOADS_FILE.test(source);
  return names ? "names_files" : "none";
}

/**
 * The attached files Fabro's bundler carries to a run: what the workflow names,
 * followed through imports, child workflows and template tags. It doesn't read
 * inline source, so a file named only there never reaches the run, and Fabro
 * refuses the workflow even with the file sitting beside it.
 */
export function bundledFiles(root: WorkflowGraph, assets: Record<string, string>): Set<string> {
  const bundled = new Set<string>();
  const followTags = (text: string, directory: string) => {
    for (const target of includeTargets(text, directory)) {
      const content = getFile(assets, target);
      if (content === undefined || bundled.has(target)) continue;
      bundled.add(target);
      followTags(content, directoryOf(target));
    }
  };
  const visit = (graph: WorkflowGraph, directory: string) => {
    const templates = [
      getAttr(graph.attrs, "goal"),
      getAttr(graph.attrs, "model_stylesheet"),
      ...graph.nodes.map((node) => getAttr(effectiveNodeAttributes(graph, node), "prompt")),
    ];
    for (const text of templates) {
      if (text !== undefined && !text.startsWith("@")) followTags(text, directory);
    }
    for (const reference of workflowFileReferences(graph)) {
      const target = resolveFileReference(reference, assets, directory);
      if (target === null || bundled.has(target)) continue;
      bundled.add(target);
      const content = getFile(assets, target) as string;
      if (reference.kind === "child_workflow" || reference.kind === "import") {
        const child = parseWorkflow(content).graph;
        if (child) visit(child, directoryOf(target));
      } else if (reference.kind === "prompt" || reference.kind === "goal") {
        followTags(content, directoryOf(target));
      }
    }
  };
  visit(root, "");
  return bundled;
}
