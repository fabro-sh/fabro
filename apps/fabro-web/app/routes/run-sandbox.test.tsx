import { afterEach, describe, expect, mock, test } from "bun:test";
import type { ReactNode } from "react";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter, Route, Routes } from "react-router";

import type { SandboxDetails } from "@qltysh/fabro-api-client";

let currentDetails: SandboxDetails | null = null;
let currentLoading = false;
let currentError: Error | null = null;

mock.module("../lib/queries", () => ({
  useRunSandboxDetails: () => ({
    data:         currentDetails,
    error:        currentError,
    isLoading:    currentLoading,
    isValidating: false,
    mutate:       mock(() => Promise.resolve(currentDetails)),
  }),
  // FilesystemPanel and VncPanel import these. They never run in this
  // file's tests, but the export shape needs to exist for module evaluation.
  useSandboxFiles: () => ({
    data:         undefined,
    error:        undefined,
    isValidating: false,
    mutate:       mock(() => Promise.resolve()),
  }),
  useSandboxFile: () => ({
    data:   undefined,
    error:  undefined,
    mutate: mock(() => Promise.resolve()),
  }),
  useSandboxVncPreview: () => ({
    data:         undefined,
    error:        undefined,
    isLoading:    false,
    isValidating: false,
    mutate:       mock(() => Promise.resolve()),
  }),
  useSandboxServices: () => ({
    data:         undefined,
    error:        undefined,
    isLoading:    false,
    isValidating: false,
    mutate:       mock(() => Promise.resolve()),
  }),
}));

mock.module("../components/terminal-view", () => ({
  // Render the leading slot so the mode toggle (now hosted inside each panel
  // header) is reachable from outer tab-presence assertions.
  default: ({ leading }: { leading?: ReactNode }) => <div>{leading}</div>,
  TERMINAL_DOCK_CLEARANCE_CLASS: "",
}));

// Stub the services panel the same way as terminal-view: render only the
// leading slot so tab-presence assertions reach the mode toggle without
// pulling in the panel's own data-fetching dependencies.
mock.module("./run-sandbox/services-panel", () => ({
  default: ({ leading }: { leading?: ReactNode }) => <div>{leading}</div>,
}));

// Stub @pierre/trees and @pierre/diffs runtime so the filesystem panel can
// render in this test without pulling in shiki/highlighter modules. The
// filesystem panel's own behavior is exercised in filesystem-panel.test.tsx.
mock.module("@pierre/trees/react", () => ({
  FileTree:             () => <div data-test-id="file-tree-stub" />,
  useFileTree:          () => ({ model: { resetPaths: () => {} } }),
  useFileTreeSelection: () => [],
}));
mock.module("@pierre/trees", () => ({ themeToTreeStyles: () => ({}) }));
mock.module("@pierre/theme/pierre-dark", () => ({ default: {} }));
mock.module("@pierre/diffs/react", () => ({
  File: () => <div data-test-id="pierre-file-stub" />,
  Virtualizer: ({ children }: { children?: ReactNode }) => (
    <div data-test-id="pierre-virtualizer-stub">{children}</div>
  ),
  WorkerPoolContextProvider: ({ children }: { children?: ReactNode }) => (
    <div data-test-id="pierre-worker-pool-stub">{children}</div>
  ),
}));

const { default: RunSandbox, formatBytesAsMemory, normalizeSandboxMode } =
  await import("./run-sandbox");
mock.restore();

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function sandboxDetails(
  overrides: Partial<SandboxDetails> & {
    sandbox?: Partial<SandboxDetails["sandbox"]> & {
      runtime?: Partial<NonNullable<SandboxDetails["sandbox"]["runtime"]>>;
    };
  } = {},
): SandboxDetails {
  const sandbox = overrides.sandbox ?? {};
  return {
    sandbox: {
      provider: "docker",
      image:    null,
      snapshot: null,
      runtime:  {
        id:                null,
        working_directory: null,
        repo_cloned:       null,
        clone_origin_url:  null,
        clone_branch:      null,
        ...sandbox.runtime,
      },
      ...sandbox,
    },
    state:        "running",
    native_state: null,
    region:       null,
    resources:    { cpu_cores: null, memory_bytes: null, disk_bytes: null },
    network:      networkDetails(),
    labels:       {},
    timestamps:   { created_at: null, last_activity_at: null },
    ...overrides,
  };
}

function networkDetails(
  overrides: Partial<SandboxDetails["network"]> = {},
): SandboxDetails["network"] {
  return {
    egress:  networkPolicy("unknown"),
    ingress: networkPolicy("unknown"),
    ...overrides,
  };
}

function networkPolicy(
  mode: SandboxDetails["network"]["egress"]["mode"],
  cidrs: string[] = [],
): SandboxDetails["network"]["egress"] {
  return { mode, cidrs };
}

function textContent(renderer: TestRenderer.ReactTestRenderer): string {
  return renderer.root
    .findAll((node) => typeof node.type === "string")
    .flatMap((node) => node.children)
    .filter((child): child is string => typeof child === "string")
    .join(" ");
}

function renderRoute(initialPath: string = "/runs/run_1/sandbox") {
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter initialEntries={[initialPath]}>
        <Routes>
          <Route path="/runs/:id/sandbox" element={<RunSandbox params={{ id: "run_1" }} />} />
        </Routes>
      </MemoryRouter>,
    );
  });
  mountedRenderers.push(renderer);
  return renderer;
}

afterEach(() => {
  for (const renderer of mountedRenderers.splice(0)) {
    act(() => renderer.unmount());
  }
  currentDetails = null;
  currentLoading = false;
  currentError = null;
});

describe("formatBytesAsMemory", () => {
  test("renders gibibytes for round values", () => {
    expect(formatBytesAsMemory(2 * 1024 * 1024 * 1024)).toBe("2 GiB");
  });

  test("renders fractional gibibytes with one decimal", () => {
    expect(formatBytesAsMemory(2.5 * 1024 * 1024 * 1024)).toBe("2.5 GiB");
  });

  test("falls back to mebibytes when below a gibibyte", () => {
    expect(formatBytesAsMemory(512 * 1024 * 1024)).toBe("512 MiB");
  });
});

describe("RunSandbox route", () => {
  test("renders panels for a fully populated sandbox", () => {
    currentDetails = sandboxDetails({
      sandbox:           {
        provider: "docker",
        image:    "ghcr.io/fabro/sandbox:latest",
        runtime:  {
          id:                "abcdef123456",
          working_directory: "/workspace",
        },
      },
      state:             "running",
      native_state:      "running",
      region:            undefined,
      resources:         {
        cpu_cores:    2,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        disk_bytes:   undefined,
      },
      network:           networkDetails({
        egress:  networkPolicy("open"),
        ingress: networkPolicy("blocked"),
      }),
      labels:            { run: "abc" },
      timestamps:        {
        created_at:       "2026-05-09T12:00:00Z",
        last_activity_at: undefined,
      },
    });
    const renderer = renderRoute();

    const panelHeadings = renderer.root
      .findAll((node) => node.type === "h3")
      .map((node) => node.children.find((child) => typeof child === "string"))
      .filter((text): text is string => typeof text === "string");
    expect(panelHeadings).toEqual(["Overview", "Resources", "Network", "Labels", "Timestamps"]);
    const copy = textContent(renderer);
    expect(copy).toContain("Open");
    expect(copy).toContain("Blocked");
  });

  test("links to the provider dashboard when a sandbox web URL is present", () => {
    currentDetails = sandboxDetails({
      sandbox: {
        provider: "daytona",
        runtime:  {
          id:                "ad65029a-2d01-421e-8936-49451653fcd9",
          working_directory: "/workspace",
        },
      },
      web_url:
        "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9",
    });
    const renderer = renderRoute();

    const providerLinks = renderer.root.findAll(
      (node) =>
        node.type === "a" &&
        node.props.href ===
          "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9",
    );
    expect(providerLinks).toHaveLength(1);
    expect(providerLinks[0]?.props.target).toBe("_blank");
    expect(providerLinks[0]?.props.rel).toBe("noopener noreferrer");
    const linkText = providerLinks[0]?.findByType("span");
    expect(linkText?.children).toContain("Open in Daytona");
  });

  test("renders without crashing when most fields are null", () => {
    currentDetails = sandboxDetails({
      sandbox:           {
        provider: "local",
        runtime:  {
          id:                "local:run_1",
          working_directory: "/tmp/project",
        },
      },
      state:             "unknown",
      native_state:      undefined,
      region:            undefined,
      resources:         {
        cpu_cores:    undefined,
        memory_bytes: undefined,
        disk_bytes:   undefined,
      },
      labels:            {},
      timestamps:        {
        created_at:       undefined,
        last_activity_at: undefined,
      },
    });
    const renderer = renderRoute();

    const labelsHeading = renderer.root.findAll(
      (node) =>
        node.type === "h3" &&
        node.children.find((child) => typeof child === "string") === "Labels",
    );
    expect(labelsHeading).toHaveLength(1);

    const noLabelsCopy = renderer.root.findAll(
      (node) =>
        node.type === "div" &&
        Array.isArray(node.children) &&
        node.children.includes("No labels"),
    );
    expect(noLabelsCopy).toHaveLength(1);
  });

  test("renders unknown network policies", () => {
    currentDetails = sandboxDetails({
      network: networkDetails({
        egress:  networkPolicy("unknown"),
        ingress: networkPolicy("unknown"),
      }),
    });
    const renderer = renderRoute();

    const copy = textContent(renderer);
    expect(copy).toContain("Network");
    expect(copy).toContain("Egress");
    expect(copy).toContain("Ingress");
    expect(copy).toContain("Unknown");
  });

  test("renders blocked, essentials, and CIDR network policies", () => {
    currentDetails = sandboxDetails({
      network: networkDetails({
        egress:  networkPolicy("cidr_allow_list", ["10.0.0.0/8", "192.168.0.0/16"]),
        ingress: networkPolicy("essentials_only"),
      }),
    });
    const renderer = renderRoute();

    const copy = textContent(renderer);
    expect(copy).toContain("CIDR allow list");
    expect(copy).toContain("10.0.0.0/8, 192.168.0.0/16");
    expect(copy).toContain("Essentials only");
  });

  test("shows the empty state when no sandbox is reported", () => {
    currentDetails = null;
    const renderer = renderRoute();

    const titles = renderer.root.findAll(
      (node) =>
        node.type === "p" &&
        Array.isArray(node.children) &&
        node.children.includes("No sandbox"),
    );
    expect(titles).toHaveLength(1);
  });

  test("Terminal is the default right-column mode", () => {
    currentDetails = sandboxDetails({ sandbox: { provider: "docker" } });
    const renderer = renderRoute();

    const tabs = renderer.root.findAll(
      (node) =>
        node.type === "button" && node.props.role === "tab",
    );
    // Docker provider hides the VNC tab.
    expect(tabs).toHaveLength(3);
    const labels = tabs.map((tab) => tab.children.find((c) => typeof c === "string"));
    expect(labels).toEqual(["Terminal", "Services", "Filesystem"]);
    const selected = tabs.find((tab) => tab.props["aria-selected"] === true);
    expect(selected?.children.find((c) => typeof c === "string")).toBe("Terminal");
  });

  test("Daytona provider exposes a VNC tab", () => {
    currentDetails = sandboxDetails({ sandbox: { provider: "daytona" } });
    const renderer = renderRoute();
    const tabs = renderer.root.findAll(
      (node) => node.type === "button" && node.props.role === "tab",
    );
    expect(tabs).toHaveLength(4);
    const labels = tabs.map((tab) => tab.children.find((c) => typeof c === "string"));
    expect(labels).toEqual(["Terminal", "Services", "Filesystem", "VNC"]);
  });

  test("Services mode is selected when ?mode=services is requested", () => {
    currentDetails = sandboxDetails({ sandbox: { provider: "docker" } });
    const renderer = renderRoute("/runs/run_1/sandbox?mode=services");
    const tabs = renderer.root.findAll(
      (node) => node.type === "button" && node.props.role === "tab",
    );
    const selected = tabs.find((tab) => tab.props["aria-selected"] === true);
    expect(selected?.children.find((c) => typeof c === "string")).toBe("Services");
  });

  test("Docker provider falls back to terminal when ?mode=vnc is requested", () => {
    currentDetails = sandboxDetails({ sandbox: { provider: "docker" } });
    const renderer = renderRoute("/runs/run_1/sandbox?mode=vnc");
    const tabs = renderer.root.findAll(
      (node) => node.type === "button" && node.props.role === "tab",
    );
    const selected = tabs.find((tab) => tab.props["aria-selected"] === true);
    expect(selected?.children.find((c) => typeof c === "string")).toBe("Terminal");
  });

  test("Filesystem mode keeps sandbox details visible in the left column", () => {
    currentDetails = sandboxDetails({ sandbox: { provider: "docker" } });
    const renderer = renderRoute("/runs/run_1/sandbox?mode=filesystem");

    const panelHeadings = renderer.root
      .findAll((node) => node.type === "h3")
      .map((node) => node.children.find((child) => typeof child === "string"))
      .filter((text): text is string => typeof text === "string");
    expect(panelHeadings).toEqual(["Overview", "Resources", "Network", "Labels", "Timestamps"]);

    const tabs = renderer.root.findAll(
      (node) => node.type === "button" && node.props.role === "tab",
    );
    const selected = tabs.find((tab) => tab.props["aria-selected"] === true);
    expect(selected?.children.find((c) => typeof c === "string")).toBe("Filesystem");
  });
});

describe("normalizeSandboxMode", () => {
  test("defaults to terminal", () => {
    expect(normalizeSandboxMode(null)).toBe("terminal");
    expect(normalizeSandboxMode("")).toBe("terminal");
    expect(normalizeSandboxMode("unknown")).toBe("terminal");
  });

  test("accepts services, filesystem, and vnc", () => {
    expect(normalizeSandboxMode("services")).toBe("services");
    expect(normalizeSandboxMode("filesystem")).toBe("filesystem");
    expect(normalizeSandboxMode("vnc")).toBe("vnc");
  });
});
