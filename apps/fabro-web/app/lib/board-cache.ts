import type { KeyMatcher, KeyOrMatcher } from "./sse";

type Mutator = (key: KeyOrMatcher) => unknown;

const isRunListKey: KeyMatcher = (key) =>
  Array.isArray(key) &&
  key[0] === "runs" &&
  (key[1] === "all" || key[1] === "page");

export function runListCacheMatchers(): KeyOrMatcher[] {
  return [isRunListKey];
}

export function mutateRunListCaches(mutate: Mutator) {
  for (const matcher of runListCacheMatchers()) {
    void mutate(matcher);
  }
}
