export function prettyJson(raw: string): { text: string; isJson: boolean } {
  if (!raw || !raw.trim()) return { text: "", isJson: false };
  try {
    return { text: JSON.stringify(JSON.parse(raw), null, 2), isJson: true };
  } catch {
    return { text: raw, isJson: false };
  }
}
