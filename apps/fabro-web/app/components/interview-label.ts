export function displayLabel(label: string): string {
  const trimmed = label.trim();
  const stripped = trimmed
    .replace(/^\[[^\]]+\]\s*/, "")
    .replace(/^[A-Za-z0-9]+\)\s*/, "")
    .replace(/^[A-Za-z0-9]+\s*-\s+/, "")
    .trim();
  return stripped || trimmed;
}
