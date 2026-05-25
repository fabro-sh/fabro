import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type {
  ServerSettings,
  SystemCpuResources,
  SystemDiskResources,
  SystemInfoResponse,
  SystemMemoryResources,
  SystemResourcesResponse,
} from "@qltysh/fabro-api-client";
import TestRenderer, { act } from "react-test-renderer";
import { setupReactTestEnv } from "../lib/test-utils";

let systemResources: SystemResourcesResponse | undefined;
let systemInfo: SystemInfoResponse | undefined;
let serverSettings: ServerSettings | undefined;
let teardownReactTestEnv: (() => void) | undefined;

mock.module("../lib/queries", () => ({
  useServerSettings: () => ({ data: serverSettings }),
  useSystemInfo: () => ({ data: systemInfo }),
  useSystemResources: () => ({ data: systemResources }),
}));

const { default: SettingsMonitoring } = await import("./settings-monitoring");

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function renderSettingsMonitoring() {
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(<SettingsMonitoring />);
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

type ResourceOverrides = Partial<
  Omit<SystemResourcesResponse, "cpu" | "memory" | "disk">
> & {
  cpu?: Partial<SystemCpuResources>;
  memory?: Partial<SystemMemoryResources>;
  disk?: Partial<SystemDiskResources>;
};

function sampleResources(overrides: ResourceOverrides = {}): SystemResourcesResponse {
  const resources: SystemResourcesResponse = {
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
  };
  return {
    sampled_at: overrides.sampled_at ?? resources.sampled_at,
    cpu:        { ...resources.cpu, ...overrides.cpu },
    memory:     { ...resources.memory, ...overrides.memory },
    disk:       { ...resources.disk, ...overrides.disk },
    notes:      overrides.notes ?? resources.notes,
  };
}

function sampleServerSettings(maxConcurrentRuns = 8): ServerSettings {
  return {
    server: {
      scheduler: { max_concurrent_runs: maxConcurrentRuns },
    },
  } as unknown as ServerSettings;
}

describe("SettingsMonitoring route", () => {
  beforeEach(() => {
    teardownReactTestEnv = setupReactTestEnv();
    systemInfo = { runs: { active: 3, total: 12 } };
    serverSettings = sampleServerSettings();
  });

  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    systemResources = undefined;
    systemInfo = undefined;
    serverSettings = undefined;
    teardownReactTestEnv?.();
    teardownReactTestEnv = undefined;
  });

  test("renders loaded resource data", () => {
    systemResources = sampleResources();

    const renderer = renderSettingsMonitoring();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("18%");
    expect(text).toContain("5s");
    expect(text).toContain("3 GiB");
    expect(text).toContain("8 GiB");
    expect(text).toContain("3 / 8 active");
  });

  test("shows CPU warmup state while usage is null", () => {
    systemResources = sampleResources({
      cpu: {
        supported:          true,
        scope:              "server_environment",
        unavailable_reason: null,
        logical_cpus:       10,
        usage_percent:      null,
        sample_window_ms:   null,
      },
    });

    const renderer = renderSettingsMonitoring();

    expect(textContent(renderer.toJSON())).toContain("Collecting sample");
  });

  test("renders unsupported resource sections", () => {
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

    const renderer = renderSettingsMonitoring();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("Unsupported");
    expect(text).toContain("CPU metrics unavailable");
    expect(text).toContain("No storage filesystem matched");
  });

  test("renders notes only when present", () => {
    systemResources = sampleResources({
      notes: ["Memory is scoped to the current container."],
    });

    const renderer = renderSettingsMonitoring();

    expect(textContent(renderer.toJSON())).toContain(
      "Memory is scoped to the current container.",
    );
  });
});
