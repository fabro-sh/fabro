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
});
