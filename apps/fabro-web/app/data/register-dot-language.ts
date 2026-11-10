import type { BundledLanguage } from "@pierre/diffs";
import {
  attachResolvedLanguages,
  getSharedHighlighter,
} from "@pierre/diffs";
import { PIERRE_THEMES } from "../lib/pierre-theme";
import { dotLanguage } from "./dot-grammar";

export async function registerDotLanguage(): Promise<void> {
  const highlighter = await getSharedHighlighter({
    themes: [PIERRE_THEMES.dark, PIERRE_THEMES.light],
    langs: [],
  });
  attachResolvedLanguages(
    { name: "dot" as BundledLanguage, data: [dotLanguage] },
    highlighter,
  );
}
