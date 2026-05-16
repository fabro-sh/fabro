import { getObject } from "./unknown";

export {
  getArray,
  getBool,
  getNumber,
  getObject,
  getString,
  isRecord,
  type UnknownRecord,
} from "./unknown";

export function objectKeyCount(o: unknown, key: string): number {
  const v = getObject(o, key);
  return v ? Object.keys(v).length : 0;
}
