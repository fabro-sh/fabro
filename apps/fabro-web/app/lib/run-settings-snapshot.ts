export type Snapshot = Record<string, unknown>;

function isRecord(v: unknown): v is Snapshot {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function read(o: unknown, key: string): unknown {
  if (!isRecord(o)) return undefined;
  return o[key];
}

export function getString(o: unknown, key: string): string | undefined {
  const v = read(o, key);
  return typeof v === "string" && v.length > 0 ? v : undefined;
}

export function getNumber(o: unknown, key: string): number | undefined {
  const v = read(o, key);
  return typeof v === "number" ? v : undefined;
}

export function getBool(o: unknown, key: string): boolean | undefined {
  const v = read(o, key);
  return typeof v === "boolean" ? v : undefined;
}

export function getObject(o: unknown, key: string): Snapshot | undefined {
  const v = read(o, key);
  return isRecord(v) ? v : undefined;
}

export function getArray(o: unknown, key: string): unknown[] | undefined {
  const v = read(o, key);
  return Array.isArray(v) ? v : undefined;
}

export function objectKeyCount(o: unknown, key: string): number {
  const v = getObject(o, key);
  return v ? Object.keys(v).length : 0;
}
