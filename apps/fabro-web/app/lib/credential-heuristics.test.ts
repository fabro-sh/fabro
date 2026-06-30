import { describe, expect, test } from "bun:test";

import {
  looksLikeCredential,
  secretNameForKey,
  secretReference,
} from "./credential-heuristics";

describe("credential heuristics", () => {
  test("flags Authorization bearer values", () => {
    expect(looksLikeCredential("Authorization", "Bearer abc123"))
      .toBe(true);
  });

  test("flags API key names", () => {
    expect(looksLikeCredential("API_KEY", "abc"))
      .toBe(true);
    expect(looksLikeCredential("x-api-key", "abc"))
      .toBe(true);
  });

  test("does not flag ordinary environment values", () => {
    expect(looksLikeCredential("NODE_ENV", "production"))
      .toBe(false);
  });

  test("does not flag already templated references", () => {
    expect(looksLikeCredential("API_KEY", "{{ secrets.OPENAI_API_KEY }}"))
      .toBe(false);
    expect(looksLikeCredential("TOKEN", "{{ env.GITHUB_TOKEN }}"))
      .toBe(false);
    expect(looksLikeCredential("PASSWORD", "{{ vars.RUNTIME_PASSWORD }}"))
      .toBe(false);
  });

  test("does not flag empty values", () => {
    expect(looksLikeCredential("PASSWORD", ""))
      .toBe(false);
  });

  test("flags long high-entropy-looking values under benign keys", () => {
    expect(looksLikeCredential("session_id", "aBcdEf1234567890Ghij"))
      .toBe(true);
  });

  test("derives secret names from keys", () => {
    expect(secretNameForKey("x-api-key"))
      .toBe("X_API_KEY");
    expect(secretNameForKey("  "))
      .toBe("SECRET");
  });

  test("builds secret interpolation references", () => {
    expect(secretReference("X_API_KEY"))
      .toBe("{{ secrets.X_API_KEY }}");
  });
});
