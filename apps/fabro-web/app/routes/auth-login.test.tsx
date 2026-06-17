import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { createMemoryRouter, RouterProvider } from "react-router";

import { setupReactTestEnv } from "../lib/test-utils";

let authMethods: string[] = [];
let teardownReactTestEnv: (() => void) | undefined;
const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

mock.module("../lib/queries", () => ({
  useAuthConfig: () => ({ data: { methods: authMethods } }),
}));

mock.module("../lib/mutations", () => ({
  useLoginDevToken: () => ({ trigger: () => Promise.resolve(undefined) }),
}));

const { default: AuthLogin } = await import("./auth-login");

async function renderAuthLogin() {
  const router = createMemoryRouter(
    [{ path: "/login", element: <AuthLogin /> }],
    { initialEntries: ["/login"] },
  );
  let renderer!: TestRenderer.ReactTestRenderer;
  await act(async () => {
    renderer = TestRenderer.create(<RouterProvider router={router} />);
  });
  mountedRenderers.push(renderer);
  return renderer;
}

function textContent(node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textContent).join("");
  return node.children?.map(textContent).join("") ?? "";
}

describe("AuthLogin route", () => {
  beforeEach(() => {
    teardownReactTestEnv = setupReactTestEnv();
  });

  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    authMethods = [];
    teardownReactTestEnv?.();
    teardownReactTestEnv = undefined;
  });

  test("uses GitLab OAuth as the primary login when GitLab and dev token auth are configured", async () => {
    authMethods = ["dev-token", "gitlab"];

    const renderer = await renderAuthLogin();
    const text = textContent(renderer.toJSON());

    expect(text).toContain("Authenticate with your GitLab account to continue.");
    expect(text).toContain("Sign in with GitLab");
    expect(text).toContain("Use a dev token instead");
    expect(text).not.toContain("Sign in with dev token");
    expect(
      renderer.root.findAll(
        (node) =>
          node.type === "a" &&
          node.props.href === "/auth/login/gitlab",
      ),
    ).toHaveLength(1);
  });
});
