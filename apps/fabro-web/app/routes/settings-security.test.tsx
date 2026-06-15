import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type { ServerSettings } from "@qltysh/fabro-api-client";
import TestRenderer, { act } from "react-test-renderer";
import { setupReactTestEnv } from "../lib/test-utils";

let serverSettings: ServerSettings | undefined;
let teardownReactTestEnv: (() => void) | undefined;

mock.restore();
mock.module("../lib/queries", () => ({
  useServerSettings:      () => ({ data: serverSettings }),
  useSystemIntegrations:  () => ({ data: undefined }),
  useSystemInfo:          () => ({ data: undefined }),
  useSystemResources:     () => ({ data: undefined }),
}));

const { default: SettingsSecurity } = await import("./settings-security");

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function renderSettingsSecurity() {
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(<SettingsSecurity />);
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

function sampleServerSettings(
  overrides: {
    methods?: string[];
    gitlabUsernames?: string[];
    gitlabGroups?: string[];
  } = {},
): ServerSettings {
  return {
    server: {
      auth: {
        methods: overrides.methods ?? ["github"],
        github:  {
          allowed_usernames: ["octocat"],
        },
        gitlab:  {
          allowed_usernames: overrides.gitlabUsernames ?? [],
          allowed_groups:    overrides.gitlabGroups ?? [],
        },
      },
    },
  } as unknown as ServerSettings;
}

describe("SettingsSecurity route", () => {
  beforeEach(() => {
    teardownReactTestEnv = setupReactTestEnv();
  });

  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    serverSettings = undefined;
    teardownReactTestEnv?.();
    teardownReactTestEnv = undefined;
  });

  test("hides GitLab auth rows when GitLab auth is not enabled", () => {
    serverSettings = sampleServerSettings();

    const renderer = renderSettingsSecurity();
    const text = textContent(renderer.toJSON());

    expect(text).not.toContain("GitLab allowed usernames");
    expect(text).not.toContain("GitLab allowed groups");
    expect(text).toContain("Allowed usernames");
  });

  test("shows closed GitLab auth state for empty allowlists", () => {
    serverSettings = sampleServerSettings({ methods: ["gitlab"] });

    const renderer = renderSettingsSecurity();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("GitLab allowed usernames");
    expect(text).toContain("GitLab allowed groups");
    expect(text).toContain(
      "No GitLab users can authenticate until an allowed username or group is configured.",
    );
  });

  test("shows GitLab allowed usernames and groups separately", () => {
    serverSettings = sampleServerSettings({
      methods:         ["github", "gitlab"],
      gitlabUsernames: ["alice", "bob"],
      gitlabGroups:    ["platform/fabro-admins"],
    });

    const renderer = renderSettingsSecurity();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("GitLab allowed usernames");
    expect(text).toContain("alice");
    expect(text).toContain("bob");
    expect(text).toContain("GitLab allowed groups");
    expect(text).toContain("platform/fabro-admins");
  });
});
