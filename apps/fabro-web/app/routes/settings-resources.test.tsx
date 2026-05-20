import { afterEach, describe, expect, mock, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

let systemInfo: any;
let systemResources: any;

mock.module("../lib/queries", () => ({
  useSystemInfo: () => ({ data: systemInfo }),
  useSystemResources: () => ({ data: systemResources }),
}));

const { default: SettingsResources } = await import("./settings-resources");

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function renderSettingsResources() {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(<SettingsResources />);
  });
  mountedRenderers.push(renderer!);
  return renderer!;
}

function textContent(node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textContent).join("");
  return node.children?.map(textContent).join("") ?? "";
}

function sampleSystemInfo() {
  return {
    os:               "macos",
    arch:             "aarch64",
    uptime_secs:      3661,
    sandbox_provider: "docker",
    storage_dir:      "/var/lib/fabro",
  };
}

function sampleResources(overrides: Record<string, unknown> = {}) {
  return {
    sampled_at: "2026-05-20T15:42:10Z",
    cpu:        {
      supported:          true,
      scope:              "server_environment",
      unavailable_reason: null,
      logical_cpus:       10,
      usage_percent:      18.4,
      sample_window_ms:   5000,
    },
    memory:     {
      supported:          true,
      scope:              "cgroup",
      unavailable_reason: null,
      total_bytes:        8 * 1024 * 1024 * 1024,
      used_bytes:         3 * 1024 * 1024 * 1024,
      available_bytes:    5 * 1024 * 1024 * 1024,
      used_percent:       37.5,
      host_total_bytes:   32 * 1024 * 1024 * 1024,
    },
    disk:       {
      supported:              true,
      scope:                  "storage_filesystem",
      unavailable_reason:     null,
      storage_path:           "/var/lib/fabro",
      mount_point:            "/",
      filesystem:             "apfs",
      total_bytes:            500 * 1024 * 1024 * 1024,
      used_bytes:             200 * 1024 * 1024 * 1024,
      available_bytes:        300 * 1024 * 1024 * 1024,
      used_percent:           40,
      fabro_managed_bytes:    2 * 1024 * 1024 * 1024,
      fabro_reclaimable_bytes: 512 * 1024 * 1024,
    },
    notes:      [],
    ...overrides,
  };
}

describe("SettingsResources route", () => {
  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    systemInfo = undefined;
    systemResources = undefined;
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  test("renders loaded runtime and resource data", () => {
    systemInfo = sampleSystemInfo();
    systemResources = sampleResources();

    const renderer = renderSettingsResources();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("macos");
    expect(text).toContain("aarch64");
    expect(text).toContain("docker");
    expect(text).toContain("/var/lib/fabro");
    expect(text).toContain("18.4%");
    expect(text).toContain("10");
    expect(text).toContain("5s");
    expect(text).toContain("3 GiB");
    expect(text).toContain("8 GiB");
    expect(text).toContain("Container limit");
    expect(text).toContain("apfs");
    expect(text).toContain("2 GiB");
    expect(text).toContain("512 MiB");
  });

  test("shows CPU warmup state while usage is null", () => {
    systemInfo = sampleSystemInfo();
    systemResources = sampleResources({
      cpu: {
        supported:          true,
        scope:              "server_environment",
        unavailable_reason: null,
        logical_cpus:       10,
        usage_percent:      null,
        sample_window_ms:   5000,
      },
    });

    const renderer = renderSettingsResources();

    expect(textContent(renderer.toJSON())).toContain("Collecting sample");
  });

  test("renders unsupported resource sections", () => {
    systemInfo = sampleSystemInfo();
    systemResources = sampleResources({
      cpu:  {
        supported:          false,
        scope:              "server_environment",
        unavailable_reason: "CPU metrics unavailable",
        logical_cpus:       null,
        usage_percent:      null,
        sample_window_ms:   null,
      },
      disk: {
        supported:              false,
        scope:                  "storage_filesystem",
        unavailable_reason:     "No storage filesystem matched",
        storage_path:           "/var/lib/fabro",
        mount_point:            null,
        filesystem:             null,
        total_bytes:            null,
        used_bytes:             null,
        available_bytes:        null,
        used_percent:           null,
        fabro_managed_bytes:    0,
        fabro_reclaimable_bytes: 0,
      },
    });

    const renderer = renderSettingsResources();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("Unsupported");
    expect(text).toContain("CPU metrics unavailable");
    expect(text).toContain("No storage filesystem matched");
  });

  test("renders notes only when present", () => {
    systemInfo = sampleSystemInfo();
    systemResources = sampleResources({
      notes: ["Memory is scoped to the current container."],
    });

    const renderer = renderSettingsResources();

    expect(textContent(renderer.toJSON())).toContain(
      "Memory is scoped to the current container.",
    );
  });
});
