import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import type { ReactNode } from "react";
import type { SandboxFileEntry, SandboxFileListResponse } from "@qltysh/fabro-api-client";

interface CapturedTreeOptions {
  onSelectionChange?: (selected: readonly string[]) => void;
}

interface FilesQueryState {
  data?: SandboxFileListResponse;
  error?: Error;
  isValidating: boolean;
  mutate:       ReturnType<typeof mock>;
}

interface FileQueryState {
  data?: ArrayBuffer;
  error?: Error;
  mutate:       ReturnType<typeof mock>;
}

let lastFilesArgs: { id: string | undefined; path: string | undefined } | null = null;
let filesState: FilesQueryState = makeEmptyFilesState();
let lastFileArgs: { id: string | undefined; path: string | null | undefined } | null = null;
let fileState: FileQueryState = makeEmptyFileState();
let lastTreeOptions: CapturedTreeOptions | null = null;
const pierreFileCalls: Array<{ file: { name: string; contents: string; cacheKey?: string } }> = [];
const providerCalls: any[] = [];
const virtualizerCalls: any[] = [];

function makeEmptyFilesState(): FilesQueryState {
  return {
    isValidating: false,
    mutate:       mock(() => Promise.resolve()),
  };
}

function makeEmptyFileState(): FileQueryState {
  return {
    mutate: mock(() => Promise.resolve()),
  };
}

mock.module("../../lib/queries", () => ({
  useSandboxFiles: (id: string | undefined, path: string | undefined) => {
    lastFilesArgs = { id, path };
    return filesState;
  },
  useSandboxFile: (id: string | undefined, path: string | null | undefined) => {
    lastFileArgs = { id, path };
    return fileState;
  },
}));

// Stub the heavy tree/diff renderers so we can assert structure without
// pulling shiki/highlighter modules into the test runtime.
mock.module("@pierre/trees/react", () => ({
  FileTree:             (props: { className?: string }) => (
    <div data-test-id="file-tree" className={props.className} />
  ),
  useFileTree:          (options: {
    paths?: readonly string[];
    onSelectionChange?: (selected: readonly string[]) => void;
  }) => {
    lastTreeOptions = { onSelectionChange: options.onSelectionChange };
    return {
      model: {
        paths:             options.paths ?? [],
        onSelectionChange: options.onSelectionChange,
        resetPaths:        () => {},
      },
    };
  },
  useFileTreeSelection: () => [],
}));

mock.module("@pierre/trees", () => ({
  themeToTreeStyles: () => ({}),
}));

mock.module("@pierre/theme/pierre-dark", () => ({
  default: {},
}));

mock.module("@pierre/diffs/react", () => ({
  File: (props: { file: { name: string; contents: string; cacheKey?: string } }) => {
    pierreFileCalls.push(props);
    return (
      <div data-test-id="pierre-file" data-file-name={props.file.name}>
        {props.file.contents}
      </div>
    );
  },
  Virtualizer: (props: any) => {
    virtualizerCalls.push(props);
    return <div data-test-id="pierre-virtualizer">{props.children}</div>;
  },
  WorkerPoolContextProvider: (props: any) => {
    providerCalls.push(props);
    return <div data-test-id="pierre-worker-pool">{props.children}</div>;
  },
}));

const filesystemPanelModule = await import("./filesystem-panel");
const {
  default: FilesystemPanel,
  buildBreadcrumbs,
  buildTreeInputs,
  basename,
  classifySelection,
  decodeUtf8Strict,
  downloadUrl,
  formatFileSize,
  joinPath,
  looksLikeBinary,
  parentPath,
} = filesystemPanelModule;
mock.restore();

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function renderPanel(props: Partial<React.ComponentProps<typeof FilesystemPanel>> = {}): TestRenderer.ReactTestRenderer {
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(<FilesystemPanel runId="run_1" {...props} />);
  });
  mountedRenderers.push(renderer);
  return renderer;
}

beforeEach(() => {
  lastFilesArgs = null;
  lastFileArgs = null;
  lastTreeOptions = null;
  pierreFileCalls.length = 0;
  providerCalls.length = 0;
  virtualizerCalls.length = 0;
  filesState = makeEmptyFilesState();
  fileState = makeEmptyFileState();
});

afterEach(() => {
  for (const renderer of mountedRenderers.splice(0)) {
    act(() => renderer.unmount());
  }
});

describe("path helpers", () => {
  test("joinPath joins absolute parent and relative child", () => {
    expect(joinPath("/", "foo")).toBe("/foo");
    expect(joinPath("/workspace", "src")).toBe("/workspace/src");
    expect(joinPath("/workspace/src", "main.ts")).toBe("/workspace/src/main.ts");
    expect(joinPath("/workspace", "")).toBe("/workspace");
  });

  test("parentPath returns the parent directory", () => {
    expect(parentPath("/")).toBe("/");
    expect(parentPath("/workspace")).toBe("/");
    expect(parentPath("/workspace/src")).toBe("/workspace");
    expect(parentPath("/workspace/src/main.ts")).toBe("/workspace/src");
  });

  test("basename returns the last segment", () => {
    expect(basename("/")).toBe("/");
    expect(basename("/workspace")).toBe("workspace");
    expect(basename("/workspace/src/main.ts")).toBe("main.ts");
  });

  test("buildBreadcrumbs splits path into clickable segments", () => {
    expect(buildBreadcrumbs("/")).toEqual([{ name: "/", path: "/" }]);
    expect(buildBreadcrumbs("/workspace/src")).toEqual([
      { name: "/", path: "/" },
      { name: "workspace", path: "/workspace" },
      { name: "src", path: "/workspace/src" },
    ]);
  });

  test("downloadUrl encodes the path query string", () => {
    expect(downloadUrl("run_1", "/workspace/main.ts")).toBe(
      "/api/v1/runs/run_1/sandbox/file?path=%2Fworkspace%2Fmain.ts",
    );
  });

  test("formatFileSize uses binary units", () => {
    expect(formatFileSize(undefined)).toBeNull();
    expect(formatFileSize(512)).toBe("512 B");
    expect(formatFileSize(2048)).toBe("2.0 KiB");
    expect(formatFileSize(2 * 1024 * 1024)).toBe("2.0 MiB");
  });
});

describe("binary and decoding helpers", () => {
  test("looksLikeBinary detects null bytes", () => {
    expect(looksLikeBinary(new Uint8Array([1, 2, 3, 4]))).toBe(false);
    expect(looksLikeBinary(new Uint8Array([1, 0, 3, 4]))).toBe(true);
  });

  test("decodeUtf8Strict returns null on invalid bytes", () => {
    const valid = new TextEncoder().encode("hello");
    expect(decodeUtf8Strict(valid)).toBe("hello");
    const invalid = new Uint8Array([0xff, 0xfe, 0xfd]);
    expect(decodeUtf8Strict(invalid)).toBeNull();
  });
});

describe("buildTreeInputs", () => {
  test("emits native directory paths without user-visible placeholders", () => {
    const entries: SandboxFileEntry[] = [
      { name: "src", is_dir: true },
      { name: "package.json", is_dir: false, size: 200 },
    ];
    const inputs = buildTreeInputs(entries);
    expect(inputs.paths).toContain("src/");
    expect(inputs.paths).toContain("package.json");
    expect(inputs.paths).not.toContain("src/__fabro_dir__");
    expect(inputs.directories.has("src")).toBe(true);
    expect(inputs.fileEntries.get("package.json")?.size).toBe(200);
  });
});

describe("classifySelection", () => {
  const entries: SandboxFileEntry[] = [
    { name: "src", is_dir: true },
    { name: "package.json", is_dir: false, size: 100 },
  ];
  const inputs = buildTreeInputs(entries);

  test("recognizes a native directory path selection as a directory", () => {
    expect(classifySelection("src/", inputs.fileEntries, inputs.directories)).toEqual({
      kind:         "dir",
      relativePath: "src",
    });
  });

  test("recognizes a known file entry", () => {
    const result = classifySelection("package.json", inputs.fileEntries, inputs.directories);
    expect(result).toEqual({
      kind:  "file",
      entry: { name: "package.json", is_dir: false, size: 100 },
    });
  });

  test("treats unknown nested paths as directory navigations", () => {
    const result = classifySelection("src/sub", inputs.fileEntries, inputs.directories);
    expect(result).toEqual({ kind: "dir", relativePath: "src/sub" });
  });
});

function findByTestId(renderer: TestRenderer.ReactTestRenderer, id: string) {
  return renderer.root.findAll(
    (node) =>
      typeof node.type !== "string"
        ? false
        : node.props["data-test-id"] === id,
  );
}

function textContent(node: TestRenderer.ReactTestInstance): string {
  const flatten = (children: ReactNode): string => {
    if (children == null || children === false) return "";
    // react-doctor-disable-next-line react-doctor/no-polymorphic-children -- Test helper recursively flattens renderer output; this is not component API branching.
    if (typeof children === "string") return children;
    if (typeof children === "number") return String(children);
    if (Array.isArray(children)) return children.map(flatten).join("");
    if (typeof children === "object" && "children" in (children as { children?: ReactNode })) {
      return flatten((children as { children?: ReactNode }).children);
    }
    return "";
  };
  return flatten(node.children as ReactNode);
}

describe("FilesystemPanel render", () => {
  test("requests the root directory on mount", () => {
    filesState = {
      ...makeEmptyFilesState(),
      data: {
        data: [
          { name: "src", is_dir: true },
          { name: "README.md", is_dir: false, size: 12 },
        ],
      },
    };
    renderPanel();
    expect(lastFilesArgs).toEqual({ id: "run_1", path: "/" });
  });

  test("requests the sandbox working directory when provided", () => {
    filesState = {
      ...makeEmptyFilesState(),
      data: { data: [] },
    };
    renderPanel({ rootDirectory: "/workspace" });
    expect(lastFilesArgs).toEqual({ id: "run_1", path: "/workspace" });
  });

  test("renders breadcrumbs and tree when entries arrive", () => {
    filesState = {
      ...makeEmptyFilesState(),
      data: {
        data: [
          { name: "src", is_dir: true },
          { name: "README.md", is_dir: false, size: 12 },
        ],
      },
    };
    const renderer = renderPanel();
    const trees = findByTestId(renderer, "file-tree");
    expect(trees).toHaveLength(1);

    const breadcrumbsNav = renderer.root.find(
      (node) => node.props["aria-label"] === "Sandbox path",
    );
    expect(textContent(breadcrumbsNav)).toContain("/");
  });

  test("shows an error state when listing fails", () => {
    filesState = {
      ...makeEmptyFilesState(),
      error: new Error("boom"),
    };
    const renderer = renderPanel();
    const titles = renderer.root.findAll(
      (node) =>
        node.type === "p" &&
        Array.isArray(node.children) &&
        node.children.includes("Listing unavailable"),
    );
    expect(titles).toHaveLength(1);
  });

  test("shows the empty-directory state when the listing has no entries", () => {
    filesState = {
      ...makeEmptyFilesState(),
      data: { data: [] },
    };
    const renderer = renderPanel();
    const empties = renderer.root.findAll(
      (node) =>
        node.type === "output" &&
        Array.isArray(node.children) &&
        node.children.includes("Empty directory"),
    );
    expect(empties).toHaveLength(1);
  });

  test("starts with the no-file-selected empty state", () => {
    filesState = {
      ...makeEmptyFilesState(),
      data: { data: [{ name: "README.md", is_dir: false, size: 12 }] },
    };
    const renderer = renderPanel();
    const titles = renderer.root.findAll(
      (node) =>
        node.type === "p" &&
        Array.isArray(node.children) &&
        node.children.includes("No file selected"),
    );
    expect(titles).toHaveLength(1);
    expect(lastFileArgs).toEqual({ id: "run_1", path: null });
  });

  test("selecting a folder requests the new directory listing", () => {
    filesState = {
      ...makeEmptyFilesState(),
      data: {
        data: [
          { name: "src", is_dir: true },
          { name: "README.md", is_dir: false, size: 12 },
        ],
      },
    };
    renderPanel();
    expect(lastFilesArgs?.path).toBe("/");
    const directoryPath = filesystemPanelModule.buildTreeInputs(
      filesState.data!.data,
    ).paths.find((path) => path === "src/")!;
    act(() => {
      lastTreeOptions?.onSelectionChange?.([directoryPath]);
    });
    expect(lastFilesArgs?.path).toBe("/src");
  });

  test("selecting a file fetches its contents and renders the preview", () => {
    filesState = {
      ...makeEmptyFilesState(),
      data: { data: [{ name: "README.md", is_dir: false, size: 12 }] },
    };
    fileState = {
      ...makeEmptyFileState(),
      data: new TextEncoder().encode("hello world").buffer as ArrayBuffer,
    };
    const renderer = renderPanel();
    act(() => {
      lastTreeOptions?.onSelectionChange?.(["README.md"]);
    });
    expect(lastFileArgs).toEqual({
      id:   "run_1",
      path: "/README.md",
    });
    const previews = renderer.root.findAll(
      (node) =>
        typeof node.type !== "string"
          ? false
          : node.props["data-test-id"] === "pierre-file"
            && node.props["data-file-name"] === "README.md",
    );
    expect(previews).toHaveLength(1);
    expect(providerCalls).toHaveLength(1);
    expect(virtualizerCalls).toHaveLength(1);
    expect(pierreFileCalls[0].file.cacheKey).toContain(
      "fabro-sandbox-file:run_1:/README.md:",
    );
  });

  test("renders an empty text file without mounting Pierre File", () => {
    filesState = {
      ...makeEmptyFilesState(),
      data: { data: [{ name: ".dockerenv", is_dir: false, size: 0 }] },
    };
    fileState = {
      ...makeEmptyFileState(),
      data: new TextEncoder().encode("").buffer as ArrayBuffer,
    };
    const renderer = renderPanel();
    act(() => {
      lastTreeOptions?.onSelectionChange?.([".dockerenv"]);
    });
    expect(lastFileArgs).toEqual({
      id:   "run_1",
      path: "/.dockerenv",
    });
    expect(findByTestId(renderer, "pierre-file")).toHaveLength(0);
    const titles = renderer.root.findAll(
      (node) =>
        node.type === "p" &&
        Array.isArray(node.children) &&
        node.children.includes("Empty file"),
    );
    expect(titles).toHaveLength(1);
  });

  test("renders binary fallback when file contents contain a null byte", () => {
    filesState = {
      ...makeEmptyFilesState(),
      data: { data: [{ name: "logo.png", is_dir: false, size: 4 }] },
    };
    fileState = {
      ...makeEmptyFileState(),
      data: new Uint8Array([0x89, 0x50, 0x00, 0x47]).buffer as ArrayBuffer,
    };
    const renderer = renderPanel();
    act(() => {
      lastTreeOptions?.onSelectionChange?.(["logo.png"]);
    });
    const titles = renderer.root.findAll(
      (node) =>
        node.type === "p" &&
        Array.isArray(node.children) &&
        node.children.includes("Binary file"),
    );
    expect(titles).toHaveLength(1);
  });

  test("renders too-large fallback when declared size exceeds the limit", () => {
    filesState = {
      ...makeEmptyFilesState(),
      data: {
        data: [
          {
            name:   "huge.log",
            is_dir: false,
            size:   filesystemPanelModule.TEXT_PREVIEW_BYTE_LIMIT + 1,
          },
        ],
      },
    };
    const renderer = renderPanel();
    act(() => {
      lastTreeOptions?.onSelectionChange?.(["huge.log"]);
    });
    const titles = renderer.root.findAll(
      (node) =>
        node.type === "p" &&
        Array.isArray(node.children) &&
        node.children.includes("File too large to preview"),
    );
    expect(titles).toHaveLength(1);
    // Skips the file fetch entirely when the declared size is too large.
    expect(lastFileArgs?.path).toBeNull();
  });
});
