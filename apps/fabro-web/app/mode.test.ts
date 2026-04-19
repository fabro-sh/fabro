import { describe, expect, test } from "bun:test";

import { consumeInstallTokenFromUrl, resolveFabroMode } from "./mode";

describe("resolveFabroMode", () => {
  test("returns install only for the explicit install marker", () => {
    expect(resolveFabroMode("install")).toBe("install");
    expect(resolveFabroMode("normal")).toBe("normal");
    expect(resolveFabroMode(undefined)).toBe("normal");
  });
});

describe("consumeInstallTokenFromUrl", () => {
  test("extracts the install token and preserves other query params", () => {
    expect(
      consumeInstallTokenFromUrl("https://fabro.example.com/install?token=abc123&step=welcome"),
    ).toEqual({
      token: "abc123",
      sanitizedUrl: "https://fabro.example.com/install?step=welcome",
    });
  });

  test("returns the original url when no install token is present", () => {
    expect(
      consumeInstallTokenFromUrl("https://fabro.example.com/install?step=welcome"),
    ).toEqual({
      token: null,
      sanitizedUrl: "https://fabro.example.com/install?step=welcome",
    });
  });
});
