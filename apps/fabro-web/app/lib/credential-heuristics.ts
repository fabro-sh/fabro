const TEMPLATE_REFERENCE_PATTERN = /\{\{\s*(secrets|env|vars)\./i;
const CREDENTIAL_KEY_PATTERN = /authorization|password|passwd|secret|token|api[-_]?key|_(key|token|secret)$/i;

// True when a key/value pair looks credential-bearing and should be nudged
// toward a secret. Never flags an already-templated value.
export function looksLikeCredential(key: string, value: string): boolean {
  if (value === "" || TEMPLATE_REFERENCE_PATTERN.test(value)) return false;
  if (CREDENTIAL_KEY_PATTERN.test(key)) return true;
  return looksHighEntropy(value);
}

function looksHighEntropy(value: string): boolean {
  if (value.length < 20 || /\s/.test(value)) return false;
  const classes = [/[a-z]/.test(value), /[A-Z]/.test(value), /\d/.test(value)]
    .filter(Boolean).length;
  return classes >= 2;
}

// Suggest a secret name derived from the key (UPPER_SNAKE_CASE, alnum + _).
export function secretNameForKey(key: string): string {
  const name = key
    .toUpperCase()
    .replace(/[^A-Z0-9]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_+|_+$/g, "");
  return name || "SECRET";
}

// The interpolation reference to store in place of a literal secret value.
export function secretReference(name: string): string {
  return `{{ secrets.${name} }}`;
}
