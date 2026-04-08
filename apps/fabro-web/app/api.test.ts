import { describe, expect, test } from "bun:test";
import { isNotImplemented } from "./api";

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
