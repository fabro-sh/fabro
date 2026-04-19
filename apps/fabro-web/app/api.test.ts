import { afterEach, describe, expect, mock, test } from "bun:test";
import { apiPaginatedJson, getAuthConfig, isNotAvailable, loginDevToken } from "./api";

afterEach(() => {
  mock.restore();
});

describe("isNotAvailable", () => {
  test("returns true for 501 status", () => {
    expect(isNotAvailable(501)).toBe(true);
  });

  test("returns true for 404 status", () => {
    expect(isNotAvailable(404)).toBe(true);
  });

  test("returns false for 200 status", () => {
    expect(isNotAvailable(200)).toBe(false);
  });
});

describe("auth helpers", () => {
  test("getAuthConfig fetches auth methods without triggering auth redirect behavior", async () => {
    const fetchMock = mock(() =>
      Promise.resolve(
        new Response(JSON.stringify({ methods: ["dev-token"] }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      ),
    );
    globalThis.fetch = fetchMock as typeof fetch;

    const result = await getAuthConfig();

    expect(result).toEqual({ methods: ["dev-token"] });
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/auth/config", {
      credentials: "include",
    });
  });

  test("loginDevToken posts the token payload", async () => {
    const fetchMock = mock(() =>
      Promise.resolve(
        new Response(JSON.stringify({ ok: true }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      ),
    );
    globalThis.fetch = fetchMock as typeof fetch;

    const result = await loginDevToken("fabro_dev_token");

    expect(result).toEqual({ ok: true });
    expect(fetchMock).toHaveBeenCalledWith("/auth/login/dev-token", {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token: "fabro_dev_token" }),
    });
  });
});

describe("apiPaginatedJson", () => {
  test("loads and concatenates all pages while preserving first-page extras", async () => {
    const fetchMock = mock((input: string | URL | Request) => {
      const url = String(input);
      if (url.includes("page%5Boffset%5D=0")) {
        return Promise.resolve(
          new Response(
            JSON.stringify({
              columns: [{ id: "running", name: "Running" }],
              data: [{ id: "run-1" }, { id: "run-2" }],
              meta: { has_more: true },
            }),
            {
              status: 200,
              headers: { "Content-Type": "application/json" },
            },
          ),
        );
      }

      return Promise.resolve(
        new Response(
          JSON.stringify({
            columns: [{ id: "ignored", name: "Ignored" }],
            data: [{ id: "run-3" }],
            meta: { has_more: false },
          }),
          {
            status: 200,
            headers: { "Content-Type": "application/json" },
          },
        ),
      );
    });
    globalThis.fetch = fetchMock as typeof fetch;

    const result = await apiPaginatedJson<{ id: string }, { columns: { id: string; name: string }[] }>(
      "/boards/runs",
    );

    expect(result.columns).toEqual([{ id: "running", name: "Running" }]);
    expect(result.data).toEqual([{ id: "run-1" }, { id: "run-2" }, { id: "run-3" }]);
    expect(result.meta).toEqual({ has_more: false });
    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      "/api/v1/boards/runs?page%5Blimit%5D=100&page%5Boffset%5D=0",
      {
        credentials: "include",
        headers: undefined,
      },
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "/api/v1/boards/runs?page%5Blimit%5D=100&page%5Boffset%5D=2",
      {
        credentials: "include",
        headers: undefined,
      },
    );
  });

  test("stops after a bounded number of pages when the server keeps advertising more data", async () => {
    const warnMock = mock(() => {});
    const originalWarn = console.warn;
    console.warn = warnMock;

    let callCount = 0;
    const fetchMock = mock(() => {
      callCount += 1;
      if (callCount > 50) {
        throw new Error("apiPaginatedJson should have stopped at the page cap");
      }

      return Promise.resolve(
        new Response(
          JSON.stringify({
            data: [{ id: `run-${callCount}` }],
            meta: { has_more: true },
          }),
          {
            status: 200,
            headers: { "Content-Type": "application/json" },
          },
        ),
      );
    });
    globalThis.fetch = fetchMock as typeof fetch;

    try {
      const result = await apiPaginatedJson<{ id: string }>("/boards/runs");

      expect(result.data).toHaveLength(50);
      expect(result.meta).toEqual({ has_more: true });
      expect(warnMock).toHaveBeenCalledTimes(1);
    } finally {
      console.warn = originalWarn;
    }
  });
});
