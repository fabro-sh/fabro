export type FabroMode = "normal" | "install";

export function resolveFabroMode(value: unknown): FabroMode {
  return value === "install" ? "install" : "normal";
}

export function consumeInstallTokenFromUrl(url: string): {
  token: string | null;
  sanitizedUrl: string;
} {
  const parsed = new URL(url);
  const token = parsed.searchParams.get("token");
  if (!token) {
    return { token: null, sanitizedUrl: parsed.toString() };
  }

  parsed.searchParams.delete("token");
  return {
    token,
    sanitizedUrl: parsed.toString(),
  };
}
