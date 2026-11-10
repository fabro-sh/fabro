import { importChunk } from "../../lib/import-chunk";
import { EXAMPLE_ASSETS, EXAMPLE_SOURCE, examplePromptParts } from "../../lib/workflow-builder/example";
import {
  canonicalAssets,
  CUSTOM_TOML_PATH,
  exportDraft,
  GENERATED_WORKFLOW_TOML,
  newDraft,
  reconcilePromptParts,
} from "../../lib/workflow-builder/model";
import { normalizeAssetPath, setFile } from "../../lib/workflow-builder/prompts";
import { draftSchemaProblem, saveDraft } from "../../lib/workflow-builder/storage";
import { findWorkflowGraph } from "../../lib/workflow-builder/workflow-toml";
import type { WorkflowDraft } from "../../lib/workflow-builder/types";
import { MAX_ATTACHMENT_BYTES, attachmentPathProblem, readTextFile } from "./attachments";
import { readZip, ZipFormatError } from "./zip-reader";

/**
 * Moving drafts in and out of the browser. Every import lands in a new draft, so
 * it can never overwrite one the author already has. Anything left out of an
 * import is named in `notes`.
 */

const WORKFLOW_FILE = "workflow.fabro";
const METADATA_FILE = "workflow.builder.json";
const BUILDER_FORMAT = "fabro-workflow-builder";
const MAX_BUNDLE_BYTES = 20 * 1024 * 1024;
const MAX_BUNDLE_ENTRIES = 1000;
/** What archiving tools add on their own, which isn't part of anyone's workflow. */
const ARCHIVE_NOISE = /(^|\/)(__MACOSX\/|\.DS_Store$|\._)/;
const EXAMPLE_NAME = "Tutorial Workflow";

export interface ImportedDraft {
  draft: WorkflowDraft;
  notes: string[];
}

function loadFflate() {
  return importChunk(() => import("fflate"));
}

export function fileStem(name: string): string {
  const stem = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return stem || "workflow";
}

function nameFromFile(fileName: string): string {
  return fileName.replace(/\.[^.]+$/, "") || "Imported workflow";
}

export function downloadBlob(fileName: string, blob: Blob): void {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = fileName;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  // Revoking on the next task lets the browser start the download first.
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

export function downloadSource(draft: WorkflowDraft): void {
  downloadBlob(`${fileStem(draft.name)}.fabro`, new Blob([draft.source], { type: "text/plain;charset=utf-8" }));
}

/** The draft as one JSON file, attached files included. */
export function builderJson(draft: WorkflowDraft): string {
  return `${JSON.stringify({ format: BUILDER_FORMAT, draft }, null, 2)}\n`;
}

export function downloadBuilderJson(draft: WorkflowDraft): void {
  downloadBlob(`${fileStem(draft.name)}.builder.json`, new Blob([builderJson(draft)], { type: "application/json" }));
}

/** Why the draft can't be bundled, or null. Invalid code still saves and downloads as `.fabro`. */
export function bundleProblem(draft: WorkflowDraft): string | null {
  const exported = exportDraft(draft);
  if (!exported.blocked) return null;
  const first = exported.diagnostics.find((diagnostic) => diagnostic.severity === "error");
  return first?.message ?? "This draft can't be bundled yet.";
}

/**
 * Downloads the ZIP the core lays out: workflow.fabro, workflow.toml,
 * workflow.builder.json and every attached and generated file. Throws with a
 * message to show when the draft can't be bundled.
 */
export async function downloadBundle(draft: WorkflowDraft): Promise<void> {
  const exported = exportDraft(draft);
  if (exported.blocked) throw new Error(bundleProblem(draft) ?? "This draft can't be bundled yet.");
  const { zipSync, strToU8 } = await loadFflate();
  const files: Record<string, Uint8Array> = {};
  for (const [path, content] of Object.entries(exported.files)) files[path] = strToU8(content);
  const zipped = zipSync(files, { level: 6 });
  downloadBlob(`${fileStem(draft.name)}.zip`, new Blob([zipped], { type: "application/zip" }));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseJsonObject(text: string, fileName: string): Record<string, unknown> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new Error(`${fileName} is not valid JSON.`);
  }
  if (!isRecord(parsed)) throw new Error(`${fileName} is not a builder file.`);
  return parsed;
}

/** The draft fields in a downloaded builder JSON, which carries its own source. */
function builderJsonDraft(text: string, fileName: string): Record<string, unknown> {
  const parsed = parseJsonObject(text, fileName);
  const fields = parsed.format === BUILDER_FORMAT && isRecord(parsed.draft) ? parsed.draft : parsed;
  if (typeof fields.source !== "string") {
    throw new Error(`${fileName} has no workflow source. To open a bundle, import its ZIP file.`);
  }
  return fields;
}

/**
 * A fresh draft around imported fields: new ID and timestamp, nothing overwritten.
 * Two spellings of one file with different content are refused rather than one
 * quietly winning, and prompt blocks that no longer match their prompt give way to
 * the source, so a file edited outside the builder keeps its edit.
 */
function draftFrom(fields: Record<string, unknown>, fallbackName: string): WorkflowDraft {
  const name = typeof fields.name === "string" && fields.name.trim() ? fields.name : fallbackName;
  const base = newDraft(name);
  const draft = {
    ...base,
    ...fields,
    version: 1,
    id: base.id,
    name,
    source: typeof fields.source === "string" ? fields.source : base.source,
    positions: isRecord(fields.positions) ? fields.positions : {},
    assets: isRecord(fields.assets) ? fields.assets : {},
    promptParts: isRecord(fields.promptParts) ? fields.promptParts : {},
    updatedAt: new Date().toISOString(),
  };
  const problem = draftSchemaProblem(draft);
  if (problem) throw new Error(`The file can't be imported: ${problem}.`);
  const valid = draft as WorkflowDraft;
  const { assets, conflicts } = canonicalAssets(valid.assets);
  if (conflicts.length > 0) {
    const [first, second] = conflicts[0];
    throw new Error(`The file can't be imported: "${first}" and "${second}" name the same file with different content.`);
  }
  const canonical = { ...valid, assets };
  return { ...canonical, promptParts: reconcilePromptParts(canonical) };
}

function draftFromSource(source: string, name: string): WorkflowDraft {
  return draftFrom({ source, positions: {}, assets: {}, promptParts: {} }, name);
}

/**
 * Settings the builder didn't write are kept exactly, with one exception. Every
 * export calls the workflow `workflow.fabro`, so a `[workflow] graph` proven to name
 * the file being imported under another name is pointed at that, and only that value
 * changes. Only a byte-for-byte match with what the builder writes counts as its
 * own: anyone can edit a file and leave its comments in place.
 */
function importedToml(toml: string | undefined, workflowPath: string, assets: Record<string, string>, notes: string[]): void {
  if (toml === undefined || toml === GENERATED_WORKFLOW_TOML) return;
  const graph = findWorkflowGraph(toml);
  const named = graph && normalizeAssetPath(graph.value);
  const repoint = graph !== null && named === workflowPath && workflowPath !== WORKFLOW_FILE;
  setFile(assets, CUSTOM_TOML_PATH, repoint ? `${toml.slice(0, graph.start)}"${WORKFLOW_FILE}"${toml.slice(graph.end)}` : toml);
  notes.push(
    repoint
      ? `${CUSTOM_TOML_PATH} has custom settings, so it was kept. One change: its graph setting now says ${WORKFLOW_FILE}, the name the workflow is exported under. Review it before running: it can change environments, hooks and pull request behavior.`
      : `${CUSTOM_TOML_PATH} has custom settings, so it was kept and will be exported as written. Review it before running: it can change environments, hooks and pull request behavior.`,
  );
  if (named !== null && named !== workflowPath && named !== WORKFLOW_FILE) {
    notes.push(`${CUSTOM_TOML_PATH} runs ${named}, but this import opened ${workflowPath} and exports it as ${WORKFLOW_FILE}. Check which one you meant.`);
  } else if (named === null && workflowPath !== WORKFLOW_FILE && toml.includes(workflowPath)) {
    notes.push(`${CUSTOM_TOML_PATH} mentions ${workflowPath}, in a form the builder doesn't change, and this workflow exports as ${WORKFLOW_FILE}. If that's its graph setting, update it.`);
  }
}

function decodeText(bytes: Uint8Array): string | null {
  try {
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return text.includes("\0") ? null : text;
  } catch {
    return null;
  }
}

async function importZip(file: File): Promise<ImportedDraft> {
  if (file.size > MAX_BUNDLE_BYTES) throw new Error(`${file.name} is too large to import.`);
  const { Inflate } = await loadFflate();
  const notes: string[] = [];
  let unpacked: Awaited<ReturnType<typeof readZip>>;
  try {
    unpacked = readZip(new Uint8Array(await file.arrayBuffer()), Inflate, {
      maxEntries: MAX_BUNDLE_ENTRIES,
      maxFileBytes: MAX_ATTACHMENT_BYTES,
      maxTotalBytes: MAX_BUNDLE_BYTES,
    });
  } catch (error) {
    if (error instanceof ZipFormatError) throw new Error(`${file.name} is not a readable ZIP file: ${error.message}.`);
    throw new Error(`${file.name} is not a readable ZIP file.`);
  }
  if (unpacked.tooLarge.length > 0) notes.push(`Left out, too large: ${unpacked.tooLarge.join(", ")}.`);
  if (unpacked.unsupported.length > 0) {
    notes.push(`Left out, encrypted or compressed in an unsupported way: ${unpacked.unsupported.join(", ")}.`);
  }

  const paths = [...unpacked.files.keys()].filter((path) => !ARCHIVE_NOISE.test(path));
  // A bundle made on the command line may hold everything in one top folder.
  const roots = new Set(paths.map((path) => path.split("/")[0]));
  const prefix = roots.size === 1 && paths.every((path) => path.includes("/")) ? `${[...roots][0]}/` : "";
  const texts = new Map<string, string>();
  const skippedBinary: string[] = [];
  for (const path of paths) {
    const text = decodeText(unpacked.files.get(path) as Uint8Array);
    if (text === null) skippedBinary.push(path);
    else texts.set(path.slice(prefix.length), text);
  }
  if (skippedBinary.length > 0) notes.push(`Left out, not text: ${skippedBinary.join(", ")}.`);

  const workflowPath = texts.has(WORKFLOW_FILE)
    ? WORKFLOW_FILE
    : [...texts.keys()].find((path) => !path.includes("/") && path.endsWith(".fabro"));
  if (!workflowPath) {
    throw new Error(`${file.name} has no ${WORKFLOW_FILE} at its top level, so there is nothing to open.`);
  }

  const metadataText = texts.get(METADATA_FILE);
  const fields: Record<string, unknown> = metadataText ? parseJsonObject(metadataText, METADATA_FILE) : {};
  if (!metadataText) notes.push(`No ${METADATA_FILE} in the bundle, so the canvas was laid out automatically.`);

  const assets: Record<string, string> = {};
  const unsafe: string[] = [];
  for (const [path, text] of texts) {
    if (path === workflowPath || path === METADATA_FILE || path === CUSTOM_TOML_PATH) continue;
    if (attachmentPathProblem(path)) unsafe.push(path);
    else setFile(assets, path, text);
  }
  if (unsafe.length > 0) notes.push(`Left out, unsafe path: ${unsafe.join(", ")}.`);
  importedToml(texts.get(CUSTOM_TOML_PATH), workflowPath, assets, notes);

  return {
    draft: draftFrom({ ...fields, source: texts.get(workflowPath), assets }, nameFromFile(file.name)),
    notes,
  };
}

/** Reads a `.fabro`, builder JSON or ZIP into a new draft. Throws with a message to show. */
export async function importWorkflowFile(file: File): Promise<ImportedDraft> {
  const lower = file.name.toLowerCase();
  if (lower.endsWith(".zip")) return importZip(file);
  const text = await readTextFile(file);
  if (lower.endsWith(".json")) {
    const draft = draftFrom(builderJsonDraft(text, file.name), nameFromFile(file.name));
    const custom = Object.hasOwn(draft.assets, CUSTOM_TOML_PATH);
    return {
      draft,
      notes: custom ? [`This workflow has its own ${CUSTOM_TOML_PATH}, kept as written. Review it before running.`] : [],
    };
  }
  return { draft: draftFromSource(text, nameFromFile(file.name)), notes: [] };
}

/** The sample workflow and all its files as a brand-new draft. It never replaces the one open. */
export function createExampleDraft(): WorkflowDraft {
  const draft = draftFrom(
    {
      source: EXAMPLE_SOURCE,
      positions: {},
      assets: { ...EXAMPLE_ASSETS },
      promptParts: examplePromptParts(),
    },
    EXAMPLE_NAME,
  );
  saveDraft(draft);
  return draft;
}
