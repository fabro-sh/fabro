import { describe, expect, test } from "bun:test";

import { buildGithubOwnerValue, readInstallError } from "./install-api";

describe("readInstallError", () => {
  test("prefers the structured install error payload", async () => {
    const response = new Response(JSON.stringify({ error: "invalid token" }), {
      status: 422,
      headers: { "Content-Type": "application/json" },
    });

    await expect(
      readInstallError(response, "install request failed"),
    ).resolves.toBe("invalid token");
  });

  test("falls back to the provided message when the body is not structured JSON", async () => {
    const response = new Response("boom", {
      status: 500,
      headers: { "Content-Type": "text/plain" },
    });

    await expect(
      readInstallError(response, "install request failed"),
    ).resolves.toBe("install request failed (500)");
  });
});

describe("buildGithubOwnerValue", () => {
  test("uses personal for personal app installs", () => {
    expect(buildGithubOwnerValue("personal", "")).toBe("personal");
  });

  test("formats organization owners with the expected prefix", () => {
    expect(buildGithubOwnerValue("org", "  acme  ")).toBe("org:acme");
  });
});
