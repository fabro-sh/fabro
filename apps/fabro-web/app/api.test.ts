import { afterEach, describe, expect, mock, test } from "bun:test";
import { getAuthConfig, isNotImplemented, loginDevToken } from "./api";

afterEach(() => {
  mock.restore();
});

describe("isNotImplemented", () => {
  test("returns true for 501 status", () => {
    expect(isNotImplemented(501)).toBe(true);
  });

  test("returns false for 200 status", () => {
    expect(isNotImplemented(200)).toBe(false);
  });

  test("returns false for 404 status", () => {
    expect(isNotImplemented(404)).toBe(false);
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
