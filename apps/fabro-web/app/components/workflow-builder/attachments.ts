import {
  RESERVED_EXPORT_PATHS,
  assetPathProblem,
  normalizeAssetPath,
} from "../../lib/workflow-builder/prompts";

/** Reading files the author attaches, and keeping their paths safe to bundle. */

/** A draft lives in localStorage, which holds about 5 MB per site. */
export const MAX_ATTACHMENT_BYTES = 1024 * 1024;

export const TEXT_FILE_ACCEPT =
  ".md,.markdown,.mdx,.txt,.text,.prompt,.j2,.jinja,.json,.yaml,.yml,.toml,.xml,.html,.csv,.tsv,.fabro,.dot,.gv,text/*,application/json";
export const WORKFLOW_IMPORT_ACCEPT = ".fabro,.dot,.gv,.txt,.json,.zip,application/zip,application/json";

function formatSize(bytes: number): string {
  return bytes < 1024 * 1024 ? `${Math.ceil(bytes / 1024)} KB` : `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** Reads a text attachment. Throws an Error whose message can be shown as is. */
export async function readTextFile(file: File): Promise<string> {
  if (file.size > MAX_ATTACHMENT_BYTES) {
    throw new Error(
      `${file.name} is ${formatSize(file.size)}. Attachments are limited to ${formatSize(MAX_ATTACHMENT_BYTES)} because drafts live in this browser's storage.`,
    );
  }
  let content: string;
  try {
    content = await file.text();
  } catch {
    throw new Error(`${file.name} could not be read. Choose the file again.`);
  }
  if (content.includes("\0")) {
    throw new Error(`${file.name} looks like a binary file. Attach text files only.`);
  }
  return content;
}

/** A safe single path segment from an uploaded file name. */
export function sanitizeFileName(name: string): string {
  const base = name.split(/[\\/]/).pop() ?? "";
  const cleaned = base.replace(/[^A-Za-z0-9._-]+/g, "-").replace(/^[-.]+/, "");
  return cleaned || "file.txt";
}

/**
 * Why a path can't hold an attached file, or null when it can. Attached files go
 * into a bundle, so they must stay inside the workflow's folder. Fabro itself
 * accepts `~` and `..` references; those stay legal in the source and are simply
 * not attachable.
 */
export function attachmentPathProblem(path: string): string | null {
  const problem = assetPathProblem(path);
  if (problem) return `This path ${problem}.`;
  if (RESERVED_EXPORT_PATHS.includes(normalizeAssetPath(path))) {
    return `${path} is reserved for the workflow itself. Pick another path.`;
  }
  return null;
}

/** A path for new content that won't replace a different file already attached. */
export function freeAttachmentPath(assets: Record<string, string>, path: string, content: string): string {
  path = normalizeAssetPath(path);
  const conflicts = (candidate: string) => Object.entries(assets).some(
    ([existing, value]) => normalizeAssetPath(existing) === candidate && value !== content,
  );
  if (!conflicts(path)) return path;
  const slash = path.lastIndexOf("/");
  const directory = slash === -1 ? "" : path.slice(0, slash + 1);
  const file = slash === -1 ? path : path.slice(slash + 1);
  const dot = file.lastIndexOf(".");
  const stem = dot > 0 ? file.slice(0, dot) : file;
  const extension = dot > 0 ? file.slice(dot) : "";
  let suffix = 2;
  let candidate = `${directory}${stem}-${suffix}${extension}`;
  while (conflicts(candidate)) {
    suffix += 1;
    candidate = `${directory}${stem}-${suffix}${extension}`;
  }
  return candidate;
}

/** The folder part of a path, with a trailing slash, or "" at the root. */
export function directoryOf(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash === -1 ? "" : path.slice(0, slash + 1);
}

export function byteSize(content: string): number {
  return new TextEncoder().encode(content).length;
}
