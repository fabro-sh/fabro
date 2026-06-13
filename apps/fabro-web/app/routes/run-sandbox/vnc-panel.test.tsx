import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import type { VncPreviewResponse } from "@qltysh/fabro-api-client";

interface VncQueryState {
  data?:        VncPreviewResponse;
  error?:       Error;
  isLoading:    boolean;
  isValidating: boolean;
  mutate:       ReturnType<typeof mock>;
}

let lastEnabled: boolean | undefined;
let vncState: VncQueryState = makeIdleState();

function makeIdleState(): VncQueryState {
  return {
    isLoading:    false,
    isValidating: false,
    mutate:       mock(() => Promise.resolve()),
  };
}

mock.module("../../lib/queries", () => ({
  useSandboxVncPreview: (_id: string | undefined, enabled: boolean) => {
    lastEnabled = enabled;
    return vncState;
  },
}));

const vncPanelModule = await import("./vnc-panel");
const { default: VncPanel, describeVncError, vncSupported } = vncPanelModule;
const { ApiError } = await import("../../lib/api-client");
mock.restore();

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function renderPanel(provider: string | null): TestRenderer.ReactTestRenderer {
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(
      <VncPanel runId="run_1" provider={provider} />,
    );
  });
  mountedRenderers.push(renderer);
  return renderer;
}

beforeEach(() => {
  lastEnabled = undefined;
  vncState = makeIdleState();
});

afterEach(() => {
  for (const renderer of mountedRenderers.splice(0)) {
    act(() => renderer.unmount());
  }
});

describe("vncSupported", () => {
  test("only daytona is supported", () => {
    expect(vncSupported("daytona")).toBe(true);
    expect(vncSupported("docker")).toBe(false);
    expect(vncSupported("local")).toBe(false);
    expect(vncSupported(null)).toBe(false);
  });
});

describe("describeVncError", () => {
  test("treats 501 as non-recoverable unsupported", () => {
    const error = new ApiError({
      status:    501,
      message:   "VNC unsupported on docker",
      requestId: null,
      body:      null,
    });
    const described = describeVncError(error);
    expect(described.recoverable).toBe(false);
    expect(described.title).toBe("VNC not available");
  });

  test("treats 409 as recoverable startup failure", () => {
    const error = new ApiError({
      status:    409,
      message:   "Computer Use start failed",
      requestId: null,
      body:      null,
    });
    const described = describeVncError(error);
    expect(described.recoverable).toBe(true);
    expect(described.description).toContain("Computer Use start failed");
  });

  test("treats 404 as non-recoverable", () => {
    const error = new ApiError({
      status:    404,
      message:   "Run not found",
      requestId: null,
      body:      null,
    });
    expect(describeVncError(error).recoverable).toBe(false);
  });

  test("falls back to a generic recoverable error for unknown shapes", () => {
    expect(describeVncError(new Error("network down"))).toEqual({
      title:       "VNC unavailable",
      description: "network down",
      recoverable: true,
    });
    expect(describeVncError("oops")).toEqual({
      title:       "VNC unavailable",
      description: "Could not load the VNC preview.",
      recoverable: true,
    });
  });
});

describe("VncPanel render", () => {
  test("disables the query and shows unsupported state for non-Daytona providers", () => {
    const renderer = renderPanel("docker");
    expect(lastEnabled).toBe(false);
    const titles = renderer.root.findAll(
      (node) =>
        node.type === "p" &&
        Array.isArray(node.children) &&
        node.children.includes("VNC desktop unavailable"),
    );
    expect(titles).toHaveLength(1);
    expect(renderer.root.findAll((node) => node.type === "iframe")).toHaveLength(0);
  });

  test("enables the query and shows loading state on Daytona while fetching", () => {
    vncState = { ...makeIdleState(), isLoading: true };
    const renderer = renderPanel("daytona");
    expect(lastEnabled).toBe(true);
    const labels = renderer.root.findAll(
      (node) =>
        node.type === "p" &&
        Array.isArray(node.children) &&
        node.children.includes("Connecting to sandbox desktop…"),
    );
    expect(labels).toHaveLength(1);
  });

  test("renders an iframe with the signed URL on success", () => {
    vncState = {
      ...makeIdleState(),
      data: {
        url:             "https://preview.example.com/sb-1/6080?token=abc",
        provider:        "daytona",
        port:            6080,
        expires_in_secs: 3600,
      },
    };
    const renderer = renderPanel("daytona");
    const iframes = renderer.root.findAll((node) => node.type === "iframe");
    expect(iframes).toHaveLength(1);
    expect(iframes[0]?.props.src).toBe(
      "https://preview.example.com/sb-1/6080?token=abc",
    );
    expect(iframes[0]?.props.allow).toContain("clipboard-write");
    expect(iframes[0]?.props.sandbox).toContain("allow-same-origin");
  });

  test("renders an actionable error state for 409 startup failures", () => {
    vncState = {
      ...makeIdleState(),
      error: new ApiError({
        status:    409,
        message:   "Sandbox not running",
        requestId: null,
        body:      null,
      }),
    };
    const renderer = renderPanel("daytona");
    const titles = renderer.root.findAll(
      (node) =>
        node.type === "p" &&
        Array.isArray(node.children) &&
        node.children.includes("VNC unavailable"),
    );
    expect(titles).toHaveLength(1);
    const tryAgain = renderer.root.findAll(
      (node) =>
        node.type === "button" &&
        Array.isArray(node.children) &&
        node.children.includes("Try again"),
    );
    expect(tryAgain).toHaveLength(1);
  });

  test("reconnect button refetches the signed URL", () => {
    const mutate = mock(() => Promise.resolve());
    vncState = {
      ...makeIdleState(),
      mutate,
      data: {
        url:             "https://preview.example.com/sb-1/6080?token=abc",
        provider:        "daytona",
        port:            6080,
        expires_in_secs: 3600,
      },
    };
    const renderer = renderPanel("daytona");
    const reconnectButton = renderer.root.find(
      (node) =>
        node.type === "button" &&
        node.props["aria-label"] === "Reconnect VNC",
    );
    act(() => reconnectButton.props.onClick());
    expect(mutate).toHaveBeenCalledTimes(1);
  });
});
