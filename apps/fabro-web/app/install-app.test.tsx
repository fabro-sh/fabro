import { afterEach, describe, expect, mock, test } from "bun:test";
import { MemoryRouter, Route, Routes } from "react-router";
import TestRenderer, { act } from "react-test-renderer";

import InstallApp from "./install-app";

const INSTALL_ERROR_MESSAGE =
  "GitHub App setup failed before Fabro could save the app credentials. Continue again to retry the callback.";

const SESSION_RESPONSE = {
  completed_steps: ["server", "object_store", "llm"],
  llm: null,
  server: { canonical_url: "https://fabro.example.com" },
  object_store: { provider: "local" },
  github: null,
  prefill: { canonical_url: "https://fabro.example.com" },
};

type TestWindow = {
  history: {
    state: unknown;
    replaceState: (state: unknown, unused: string, url?: string | URL | null) => void;
  };
  location: {
    href: string;
    pathname: string;
    search: string;
  };
  sessionStorage: {
    clear: () => void;
    getItem: (key: string) => string | null;
    removeItem: (key: string) => void;
    setItem: (key: string, value: string) => void;
  };
  setInterval: typeof setInterval;
  clearInterval: typeof clearInterval;
  setTimeout: typeof setTimeout;
  clearTimeout: typeof clearTimeout;
};

function createTestWindow(initialUrl: string): TestWindow {
  let current = new URL(initialUrl);
  const sessionStorage = new Map<string, string>();

  const location = {
    get href() {
      return current.toString();
    },
    set href(value: string) {
      current = new URL(value, current.origin);
    },
    get pathname() {
      return current.pathname;
    },
    get search() {
      return current.search;
    },
  };

  return {
    history: {
      state: null,
      replaceState(state, _unused, url) {
        this.state = state;
        if (url) {
          current = new URL(String(url), current.origin);
        }
      },
    },
    location,
    sessionStorage: {
      clear() {
        sessionStorage.clear();
      },
      getItem(key) {
        return sessionStorage.get(key) ?? null;
      },
      removeItem(key) {
        sessionStorage.delete(key);
      },
      setItem(key, value) {
        sessionStorage.set(key, value);
      },
    },
    setInterval,
    clearInterval,
    setTimeout,
    clearTimeout,
  };
}

function renderTreeText(
  node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>,
): string {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(renderTreeText).join("");
  return (node.children ?? []).map(renderTreeText).join("");
}

async function waitFor(assertion: () => void, timeoutMs = 1000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastError: unknown;
  while (Date.now() < deadline) {
    try {
      assertion();
      return;
    } catch (error) {
      lastError = error;
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
  }
  throw lastError;
}

describe("InstallApp", () => {
  afterEach(() => {
    delete (globalThis as { window?: unknown }).window;
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
    mock.restore();
  });

  test("renders the GitHub callback error on the GitHub install step", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const originalConsoleError = console.error;
    console.error = ((...args: unknown[]) => {
      if (
        typeof args[0] === "string" &&
        args[0].startsWith("react-test-renderer is deprecated")
      ) {
        return;
      }
      originalConsoleError(...args);
    }) as typeof console.error;
    try {
      const fetchMock = mock((input: RequestInfo | URL) => {
        expect(String(input)).toBe("/install/session");
        return Promise.resolve(
          new Response(JSON.stringify(SESSION_RESPONSE), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          }),
        );
      });
      globalThis.fetch = fetchMock as typeof fetch;

      const testWindow = createTestWindow(
        "https://fabro.example.com/install/github?error=github-app-manifest-conversion-failed",
      );
      testWindow.sessionStorage.setItem("fabro-install-token", "test-install-token");
      (globalThis as { window?: unknown }).window = testWindow;

      let renderer: TestRenderer.ReactTestRenderer | null = null;
      await act(async () => {
        renderer = TestRenderer.create(
          <MemoryRouter initialEntries={["/install/github?error=github-app-manifest-conversion-failed"]}>
            <Routes>
              <Route path="/install/*" element={<InstallApp />} />
            </Routes>
          </MemoryRouter>,
        );
      });

      await waitFor(() => {
        expect(renderTreeText(renderer!.toJSON())).toContain(INSTALL_ERROR_MESSAGE);
      });
      expect(testWindow.location.search).toBe("");

      await act(async () => {
        renderer?.unmount();
      });
    } finally {
      console.error = originalConsoleError;
    }
  });

  test("renders the GitHub App done screen after the manifest callback resolves", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const originalConsoleError = console.error;
    console.error = ((...args: unknown[]) => {
      if (
        typeof args[0] === "string" &&
        args[0].startsWith("react-test-renderer is deprecated")
      ) {
        return;
      }
      originalConsoleError(...args);
    }) as typeof console.error;
    try {
      // Simulate the server-side /install/github/app/redirect handler having
      // just run — session returns a fully-populated `github.app` payload.
      const sessionResponse = {
        completed_steps: ["server", "object_store", "llm", "github"],
        llm: null,
        server: { canonical_url: "https://fabro.example.com" },
        object_store: { provider: "local" },
        github: {
          strategy: "app",
          owner: { kind: "personal" },
          app_name: "Fabro",
          slug: "fabro-brynary",
          allowed_username: "brynary",
        },
        prefill: { canonical_url: "https://fabro.example.com" },
      };
      const fetchMock = mock((input: RequestInfo | URL) => {
        expect(String(input)).toBe("/install/session");
        return Promise.resolve(
          new Response(JSON.stringify(sessionResponse), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          }),
        );
      });
      globalThis.fetch = fetchMock as typeof fetch;

      const testWindow = createTestWindow(
        "https://fabro.example.com/install/github/done?token=test-install-token",
      );
      testWindow.sessionStorage.setItem("fabro-install-token", "test-install-token");
      (globalThis as { window?: unknown }).window = testWindow;

      let renderer: TestRenderer.ReactTestRenderer | null = null;
      await act(async () => {
        renderer = TestRenderer.create(
          <MemoryRouter initialEntries={["/install/github/done?token=test-install-token"]}>
            <Routes>
              <Route path="/install/*" element={<InstallApp />} />
            </Routes>
          </MemoryRouter>,
        );
      });

      await waitFor(() => {
        const text = renderTreeText(renderer!.toJSON());
        expect(text).toContain("GitHub App connected");
        expect(text).toContain("fabro-brynary");
      });

      await act(async () => {
        renderer?.unmount();
      });
    } finally {
      console.error = originalConsoleError;
    }
  });

  test("saves local disk object-store settings and advances to the LLM step", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const originalConsoleError = console.error;
    console.error = ((...args: unknown[]) => {
      if (
        typeof args[0] === "string" &&
        args[0].startsWith("react-test-renderer is deprecated")
      ) {
        return;
      }
      originalConsoleError(...args);
    }) as typeof console.error;
    try {
      const fetchCalls: Array<{ input: RequestInfo | URL; init?: RequestInit }> = [];
      const fetchMock = mock((input: RequestInfo | URL, init?: RequestInit) => {
        fetchCalls.push({ input, init });
        if (String(input) === "/install/session" && fetchCalls.length === 1) {
          return Promise.resolve(
            new Response(
              JSON.stringify({
                completed_steps: ["server"],
                llm: null,
                server: { canonical_url: "https://fabro.example.com" },
                object_store: null,
                github: null,
                prefill: { canonical_url: "https://fabro.example.com" },
              }),
              {
                status: 200,
                headers: { "Content-Type": "application/json" },
              },
            ),
          );
        }
        if (String(input) === "/install/object-store") {
          return Promise.resolve(new Response(null, { status: 204 }));
        }
        if (String(input) === "/install/session" && fetchCalls.length === 3) {
          return Promise.resolve(
            new Response(
              JSON.stringify({
                completed_steps: ["server", "object_store"],
                llm: null,
                server: { canonical_url: "https://fabro.example.com" },
                object_store: { provider: "local" },
                github: null,
                prefill: { canonical_url: "https://fabro.example.com" },
              }),
              {
                status: 200,
                headers: { "Content-Type": "application/json" },
              },
            ),
          );
        }
        throw new Error(`unexpected fetch: ${String(input)}`);
      });
      globalThis.fetch = fetchMock as typeof fetch;

      const testWindow = createTestWindow("https://fabro.example.com/install/object-store");
      testWindow.sessionStorage.setItem("fabro-install-token", "test-install-token");
      (globalThis as { window?: unknown }).window = testWindow;

      let renderer: TestRenderer.ReactTestRenderer | null = null;
      await act(async () => {
        renderer = TestRenderer.create(
          <MemoryRouter initialEntries={["/install/object-store"]}>
            <Routes>
              <Route path="/install/*" element={<InstallApp />} />
            </Routes>
          </MemoryRouter>,
        );
      });

      await waitFor(() => {
        expect(renderTreeText(renderer!.toJSON())).toContain(
          "Choose the shared object store",
        );
      });

      const form = renderer!.root.findByType("form");
      await act(async () => {
        form.props.onSubmit({ preventDefault() {} });
      });

      await waitFor(() => {
        expect(renderTreeText(renderer!.toJSON())).toContain("Add your LLM credentials");
      });
      const backLink = renderer!.root.findAll(
        (node) =>
          node.type === "a" &&
          node.props.href === "/install/object-store" &&
          node.children.includes("Back"),
      );
      expect(backLink).toHaveLength(1);
      expect(fetchCalls.map((call) => String(call.input))).toEqual([
        "/install/session",
        "/install/object-store",
        "/install/session",
      ]);
      expect(fetchCalls[1]?.init?.body).toBe(JSON.stringify({ provider: "local" }));

      await act(async () => {
        renderer?.unmount();
      });
    } finally {
      console.error = originalConsoleError;
    }
  });

  test("rehydrates saved manual S3 credentials without exposing the secrets", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const originalConsoleError = console.error;
    console.error = ((...args: unknown[]) => {
      if (
        typeof args[0] === "string" &&
        args[0].startsWith("react-test-renderer is deprecated")
      ) {
        return;
      }
      originalConsoleError(...args);
    }) as typeof console.error;
    try {
      const fetchMock = mock((input: RequestInfo | URL) => {
        expect(String(input)).toBe("/install/session");
        return Promise.resolve(
          new Response(
            JSON.stringify({
              completed_steps: ["server", "object_store"],
              llm: null,
              server: { canonical_url: "https://fabro.example.com" },
              object_store: {
                provider: "s3",
                bucket: "fabro-data",
                region: "us-east-1",
                credential_mode: "access_key",
                manual_credentials_saved: true,
              },
              github: null,
              prefill: { canonical_url: "https://fabro.example.com" },
            }),
            {
              status: 200,
              headers: { "Content-Type": "application/json" },
            },
          ),
        );
      });
      globalThis.fetch = fetchMock as typeof fetch;

      const testWindow = createTestWindow("https://fabro.example.com/install/object-store");
      testWindow.sessionStorage.setItem("fabro-install-token", "test-install-token");
      (globalThis as { window?: unknown }).window = testWindow;

      let renderer: TestRenderer.ReactTestRenderer | null = null;
      await act(async () => {
        renderer = TestRenderer.create(
          <MemoryRouter initialEntries={["/install/object-store"]}>
            <Routes>
              <Route path="/install/*" element={<InstallApp />} />
            </Routes>
          </MemoryRouter>,
        );
      });

      await waitFor(() => {
        expect(renderTreeText(renderer!.toJSON())).toContain(
          "Credentials saved. Leave both fields blank to keep them, or enter both fields to replace them.",
        );
      });

      expect(renderer!.root.findByProps({ name: "aws_access_key_id" }).props.value).toBe("");
      expect(renderer!.root.findByProps({ name: "aws_secret_access_key" }).props.value).toBe("");
      expect(renderTreeText(renderer!.toJSON())).not.toContain("AKIA");

      await act(async () => {
        renderer?.unmount();
      });
    } finally {
      console.error = originalConsoleError;
    }
  });

  test("shows the redacted object-store summary on the review step", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const originalConsoleError = console.error;
    console.error = ((...args: unknown[]) => {
      if (
        typeof args[0] === "string" &&
        args[0].startsWith("react-test-renderer is deprecated")
      ) {
        return;
      }
      originalConsoleError(...args);
    }) as typeof console.error;
    try {
      const fetchMock = mock((input: RequestInfo | URL) => {
        expect(String(input)).toBe("/install/session");
        return Promise.resolve(
          new Response(
            JSON.stringify({
              completed_steps: ["server", "object_store", "llm", "github"],
              llm: {
                providers: [{ provider: "anthropic" }],
              },
              server: { canonical_url: "https://fabro.example.com" },
              object_store: {
                provider: "s3",
                bucket: "fabro-data",
                region: "us-east-1",
                credential_mode: "access_key",
                manual_credentials_saved: true,
              },
              github: { strategy: "token", username: "octocat" },
              prefill: { canonical_url: "https://fabro.example.com" },
            }),
            {
              status: 200,
              headers: { "Content-Type": "application/json" },
            },
          ),
        );
      });
      globalThis.fetch = fetchMock as typeof fetch;

      const testWindow = createTestWindow("https://fabro.example.com/install/review");
      testWindow.sessionStorage.setItem("fabro-install-token", "test-install-token");
      (globalThis as { window?: unknown }).window = testWindow;

      let renderer: TestRenderer.ReactTestRenderer | null = null;
      await act(async () => {
        renderer = TestRenderer.create(
          <MemoryRouter initialEntries={["/install/review"]}>
            <Routes>
              <Route path="/install/*" element={<InstallApp />} />
            </Routes>
          </MemoryRouter>,
        );
      });

      await waitFor(() => {
        const text = renderTreeText(renderer!.toJSON());
        expect(text).toContain("AWS S3");
        expect(text).toContain("fabro-data");
        expect(text).toContain("us-east-1");
        expect(text).toContain("Access key");
        expect(text).toContain("slatedb/, artifacts/");
      });

      await act(async () => {
        renderer?.unmount();
      });
    } finally {
      console.error = originalConsoleError;
    }
  });
});
