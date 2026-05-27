import { useEffect, useState } from "react";

import { registerDotLanguage } from "../data/register-dot-language";

let dotLanguageRegistration: Promise<void> | null = null;

function ensureDotLanguageRegistered(): Promise<void> {
  dotLanguageRegistration ??= registerDotLanguage();
  return dotLanguageRegistration;
}

/**
 * Synchronizes React with the shared Pierre syntax highlighter's Graphviz DOT
 * language registration. Registration is shared across mounts; cleanup only
 * suppresses stale state updates because the highlighter registration is global.
 */
export function useDotLanguageReady(): boolean {
  const [ready, setReady] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void ensureDotLanguageRegistered().then(() => {
      if (!cancelled) setReady(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  return ready;
}
