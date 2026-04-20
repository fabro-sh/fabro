import type { BundledLanguage } from "@pierre/diffs";
import {
  attachResolvedLanguages,
  getSharedHighlighter,
} from "@pierre/diffs";
import { dotLanguage } from "./dot-grammar";

export async function registerDotLanguage(): Promise<void> {
  const highlighter = await getSharedHighlighter({
    themes: ["pierre-dark"],
    langs: [],
  });
  attachResolvedLanguages(
    { name: "dot" as BundledLanguage, data: [dotLanguage] },
    highlighter,
  );
}
