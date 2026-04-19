import { describe, expect, test } from "bun:test";

import { buildInstallGithubAppOwner, readInstallError } from "./install-api";

describe("readInstallError", () => {
  test("prefers the structured install error payload", async () => {
    const response = new Response(
      JSON.stringify({
        errors: [{ status: "422", title: "Unprocessable Entity", detail: "invalid token" }],
      }),
      {
        status: 422,
        headers: { "Content-Type": "application/json" },
      },
    );

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

describe("buildInstallGithubAppOwner", () => {
  test("uses the personal tag for personal app installs", () => {
    expect(buildInstallGithubAppOwner("personal", "")).toEqual({ kind: "personal" });
  });

  test("trims organization slugs and tags them with `org`", () => {
    expect(buildInstallGithubAppOwner("org", "  acme  ")).toEqual({
      kind: "org",
      slug: "acme",
    });
  });
});
