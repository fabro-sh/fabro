import type { Diagnostic, PromptPart } from "./types";

/**
 * Fabro nodes have one `prompt` attribute, whose only file syntax is a leading
 * `@path`. The builder's prompt blocks compile onto that and nothing else:
 * no new prompt syntax reaches the workflow.
 */

export interface CompiledPrompt {
  prompt: string;
  files: Record<string, string>;
  /** Additive: unsafe paths and unresolved files. The result is still safe to use. */
  diagnostics: Diagnostic[];
  /** Additive: the file holding the joined prompt, when the blocks need one. */
  generatedPath?: string;
}

/** Files every export writes; an asset cannot take these names. */
export const RESERVED_EXPORT_PATHS = ["workflow.fabro", "workflow.toml", "workflow.builder.json"];

/** A record safe for keys like `__proto__`, which an uploaded file can be named. */
export function setFile(files: Record<string, string>, path: string, content: string): void {
  Object.defineProperty(files, path, { value: content, enumerable: true, writable: true, configurable: true });
}

export function getFile(files: Record<string, string>, path: string): string | undefined {
  return Object.hasOwn(files, path) ? files[path] : undefined;
}

/** `./a/b.md` and `a/b.md` name the same file. */
export function normalizeAssetPath(path: string): string {
  return path.replace(/^(\.\/)+/, "");
}

/**
 * The same files under their canonical names. Two spellings of one path with the
 * same content are one file. With different content they are `conflicts`, and the
 * caller has to refuse: picking one would silently lose the other.
 */
export function canonicalAssets(assets: Record<string, string>): {
  assets: Record<string, string>;
  conflicts: [string, string][];
} {
  const canonical: Record<string, string> = {};
  const spelledAs = new Map<string, string>();
  const conflicts: [string, string][] = [];
  for (const path of Object.keys(assets)) {
    const normalized = normalizeAssetPath(path);
    const earlier = spelledAs.get(normalized);
    if (earlier !== undefined && assets[earlier] !== assets[path]) conflicts.push([earlier, path]);
    if (earlier === undefined) {
      spelledAs.set(normalized, path);
      setFile(canonical, normalized, assets[path]);
    }
  }
  return { assets: canonical, conflicts };
}

/** Why `path` cannot be written into a workflow folder, or null when it is safe. */
export function assetPathProblem(path: string): string | null {
  const normalized = normalizeAssetPath(path);
  if (normalized.length === 0) return "is empty";
  // biome-ignore lint/suspicious/noControlCharactersInRegex: rejecting control characters is the point.
  if (/[\u0000-\u001f\u007f]/.test(normalized)) return "contains a control character";
  if (normalized.includes("\\")) return "uses a backslash; separate folders with /";
  if (normalized.startsWith("/")) return "is absolute; use a path relative to the workflow file";
  if (normalized.startsWith("~")) return "points into a home directory; use a path relative to the workflow file";
  if (normalized.includes(":")) return "contains a colon, which is not portable and can name a drive or URL";
  if (normalized.includes("{{") || normalized.includes("{%")) {
    return "contains template syntax, and Fabro resolves file references before templates render";
  }
  for (const segment of normalized.split("/")) {
    if (segment === "..") return "climbs out of the workflow folder with ..";
    if (segment === "." || segment === "") return "has an empty path segment";
    if (segment.endsWith(" ") || segment.endsWith(".")) return `has a segment ending in a space or dot ("${segment}")`;
    if (segment.toLowerCase() === ".git") return "writes into .git";
  }
  return null;
}

function hashOf(text: string): string {
  let hash = 0x811c9dc5;
  for (const char of text) {
    hash ^= char.codePointAt(0) ?? 0;
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}

/** Where a node's joined prompt goes. A node id that isn't path-safe still gets a stable, unique name. */
export function generatedPromptPath(nodeId: string): string {
  if (/^[A-Za-z0-9_-]+$/.test(nodeId)) return `prompts/${nodeId}.md`;
  const safe = nodeId.replace(/[^A-Za-z0-9_-]+/g, "_").replace(/^_+|_+$/g, "") || "node";
  return `prompts/${safe}-${hashOf(nodeId)}.md`;
}

/** Where a node's joined prompt may go, first choice first. */
function* generatedPathCandidates(nodeId: string): Generator<string> {
  const path = generatedPromptPath(nodeId);
  yield path;
  const stem = path.replace(/\.md$/, "");
  yield `${stem}.generated.md`;
  for (let suffix = 2; ; suffix++) yield `${stem}.generated-${suffix}.md`;
}

/** Blocks for a `prompt` attribute that was written directly, so nothing typed in code is lost on the canvas. */
export function promptPartsFromAttribute(nodeId: string, prompt: string | undefined): PromptPart[] {
  if (prompt === undefined || prompt.trim().length === 0) return [];
  const id = `${nodeId}-1`;
  if (prompt.startsWith("@")) return [{ id, mode: "file", text: "", path: prompt.slice(1) }];
  return [{ id, mode: "text", text: prompt, path: "" }];
}

type Block = { kind: "text"; text: string } | { kind: "file"; path: string; content: string | undefined };

function stripBlankEdges(text: string): string {
  return text.replace(/^(\r?\n)+/, "").replace(/(\r?\n)+$/, "");
}

/**
 * - One text block stays inline.
 * - One file block becomes `@path`.
 * - Several blocks join with blank lines into a generated `prompts/<node>.md`,
 *   and each uploaded file is exported too.
 *
 * Blank blocks are skipped, so no empty file or empty reference is generated.
 * A file with no content in `assets` keeps its reference and is flagged; its
 * content is never made up. A path that isn't safe is refused and flagged.
 *
 * A joined prompt never lands on a file with other content. If its usual path is
 * taken, it takes the next free one and says so.
 */
export function compilePromptParts(
  nodeId: string,
  parts: PromptPart[],
  assets: Record<string, string>,
): CompiledPrompt {
  const diagnostics: Diagnostic[] = [];
  const blocks: Block[] = [];
  for (const part of parts) {
    if (part.mode === "text") {
      if (part.text.trim().length > 0) blocks.push({ kind: "text", text: part.text });
      continue;
    }
    if (part.path.trim().length === 0) {
      if (part.text.trim().length > 0) {
        blocks.push({ kind: "text", text: part.text });
        diagnostics.push({
          severity: "warning",
          code: "builder.file_block_without_path",
          nodeId,
          message: `A file block on \`${nodeId}\` has text but no path. Its text was used as a text block.`,
        });
      } else {
        diagnostics.push({
          severity: "warning",
          code: "builder.file_block_without_path",
          nodeId,
          message: `A file block on \`${nodeId}\` has no file yet, so it adds nothing to the prompt. Upload a file or remove the block.`,
        });
      }
      continue;
    }
    const problem = assetPathProblem(part.path);
    if (problem) {
      diagnostics.push({
        severity: "error",
        code: "builder.unsafe_path",
        nodeId,
        message: `A file block on \`${nodeId}\` names "${part.path}", which ${problem}. It was left out of the prompt.`,
      });
      continue;
    }
    const path = normalizeAssetPath(part.path);
    blocks.push({ kind: "file", path, content: getFile(assets, path) });
  }

  const files: Record<string, string> = {};
  const fileBlocks = blocks.filter((block): block is Extract<Block, { kind: "file" }> => block.kind === "file");
  for (const block of fileBlocks) {
    if (block.content !== undefined) setFile(files, block.path, block.content);
  }

  if (blocks.length === 0) return { prompt: "", files, diagnostics };

  if (blocks.length === 1) {
    const only = blocks[0];
    if (only.kind === "file") {
      if (only.content === undefined) {
        diagnostics.push({
          severity: "warning",
          code: "builder.missing_file",
          nodeId,
          message: `The prompt of \`${nodeId}\` loads "${only.path}", which is not in this draft. Upload it, or make sure it exists next to workflow.fabro in the repo.`,
        });
      }
      return { prompt: `@${only.path}`, files, diagnostics };
    }
    // An inline value starting with @ would be read as a file reference.
    if (!only.text.startsWith("@")) return { prompt: only.text, files, diagnostics };
  }

  const includers: string[] = [];
  const pieces = blocks.map((block) => {
    if (block.kind === "text") return stripBlankEdges(block.text);
    if (block.content === undefined) {
      diagnostics.push({
        severity: "error",
        code: "builder.missing_file",
        nodeId,
        message: `The prompt of \`${nodeId}\` joins "${block.path}", which is not in this draft, so it appears as the literal text @${block.path}. Upload the file to include its content.`,
      });
      return `@${block.path}`;
    }
    const directory = block.path.includes("/") ? block.path.slice(0, block.path.lastIndexOf("/")) : "";
    if (/\{%-?\s*include\b/.test(block.content) && directory !== "prompts") includers.push(block.path);
    return stripBlankEdges(block.content);
  });
  const content = pieces.join("\n\n");

  const blockPaths = new Set(fileBlocks.map((block) => block.path));
  const isFree = (path: string) => {
    const existing = getFile(assets, path);
    return !blockPaths.has(path) && (existing === undefined || existing === content);
  };
  const preferred = generatedPromptPath(nodeId);
  let target = preferred;
  for (const candidate of generatedPathCandidates(nodeId)) {
    target = candidate;
    if (isFree(candidate)) break;
  }
  if (target !== preferred) {
    diagnostics.push({
      severity: "warning",
      code: "builder.path_collision",
      nodeId,
      message: `A file already uses "${preferred}", so the joined prompt of \`${nodeId}\` was written to "${target}".`,
    });
  }
  for (const path of includers) {
    diagnostics.push({
      severity: "warning",
      code: "builder.include_moved",
      nodeId,
      message: `"${path}" uses {% include %}, which resolves beside the file being rendered. Joined into "${target}", those includes now resolve from prompts/.`,
    });
  }
  setFile(files, target, content);
  return { prompt: `@${target}`, files, diagnostics, generatedPath: target };
}
