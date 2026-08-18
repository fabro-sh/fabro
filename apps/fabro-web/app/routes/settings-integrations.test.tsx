import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type {
  SystemIntegrationsResponse,
  SystemIntegrationStatus,
} from "@qltysh/fabro-api-client";
import TestRenderer, { act } from "react-test-renderer";
import { setupReactTestEnv } from "../lib/test-utils";

let systemIntegrations: SystemIntegrationsResponse | undefined;
let teardownReactTestEnv: (() => void) | undefined;

mock.module("../lib/queries", () => ({
  useSystemIntegrations: () => ({ data: systemIntegrations }),
}));

const { default: SettingsIntegrations } = await import("./settings-integrations");

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function renderSettingsIntegrations() {
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(<SettingsIntegrations />);
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

function sampleStatus(
  overrides: Partial<SystemIntegrationStatus> = {},
): SystemIntegrationStatus {
  return {
    provider:            "slack",
    enabled:             true,
    configured:          true,
    status:              "connected",
    missing_credentials: [],
    connection:          {
      kind:              "socket_mode",
      status:            "connected",
      last_connected_at: "2026-05-26T04:00:00Z",
      last_error:        null,
    },
    metadata:            {},
    ...overrides,
  };
}

function sampleIntegrations(
  slack: Partial<SystemIntegrationStatus> = {},
): SystemIntegrationsResponse {
  return {
    data: [
      sampleStatus({
        provider:   "github",
        status:     "configured",
        connection: null,
        metadata:   { strategy: "app", slug: "fabro-sh" },
      }),
      sampleStatus({
        metadata: { default_channel: "#fabro" },
        ...slack,
      }),
      sampleStatus({
        provider:            "plane",
        enabled:             true,
        configured:          true,
        status:              "configured",
        missing_credentials: [],
        connection:          null,
        metadata:            {
          api_base:  "https://plane.artesanosdigitales.cloud/api/v1",
          workspace: "artesanos-digitales-workspace",
        },
      }),
    ],
  };
}

describe("SettingsIntegrations route", () => {
  beforeEach(() => {
    teardownReactTestEnv = setupReactTestEnv();
  });

  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    systemIntegrations = undefined;
    teardownReactTestEnv?.();
    teardownReactTestEnv = undefined;
  });

  test("renders Slack runtime connection status", () => {
    systemIntegrations = sampleIntegrations();

    const renderer = renderSettingsIntegrations();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("Slack");
    expect(text).toContain("Connected");
    expect(text).toContain("channel: #fabro");
    expect(text).not.toContain("Disabled");
  });

  test("renders missing Slack credential names", () => {
    systemIntegrations = sampleIntegrations({
      configured:          false,
      status:              "missing_credentials",
      missing_credentials: ["SLACK_APP_TOKEN", "SLACK_BOT_TOKEN"],
      connection:          null,
    });

    const renderer = renderSettingsIntegrations();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("Missing credentials");
    expect(text).toContain("missing: SLACK_APP_TOKEN, SLACK_BOT_TOKEN");
  });

  test("renders Plane workspace and api base when configured", () => {
    systemIntegrations = sampleIntegrations();

    const renderer = renderSettingsIntegrations();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("Plane");
    expect(text).toContain("Configured");
    expect(text).toContain(
      "artesanos-digitales-workspace · https://plane.artesanosdigitales.cloud/api/v1",
    );
  });

  test("renders missing Plane credential names", () => {
    systemIntegrations = sampleIntegrations();
    systemIntegrations.data[2] = sampleStatus({
      provider:            "plane",
      enabled:             true,
      configured:          false,
      status:              "missing_credentials",
      missing_credentials: ["PLANE_API_KEY"],
      connection:          null,
      metadata:            {},
    });

    const renderer = renderSettingsIntegrations();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("Plane");
    expect(text).toContain("missing: PLANE_API_KEY");
  });

  test("shows configuration hint when Plane status is absent", () => {
    systemIntegrations = {
      data: [systemIntegrations?.data[0] ?? sampleStatus({ provider: "github" })],
    };

    const renderer = renderSettingsIntegrations();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("Configure [server.integrations.plane] to enable");
  });
});
