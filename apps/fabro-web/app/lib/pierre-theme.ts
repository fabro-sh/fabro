// How the Pierre code, diff and tree viewers follow the active appearance.
//
// Highlighting produces both the light and dark token colors at once, so switching
// themes only flips `themeType` on a mounted viewer: nothing is re-tokenized, and the
// shared worker pool can stay a singleton. Surfaces and line numbers come from the
// `--diffs-*-override` variables each palette publishes in styles/appearance.css.

import pierreDark from "@pierre/theme/pierre-dark";
import pierreLight from "@pierre/theme/pierre-light";

import type { ColorMode } from "./appearance";

export const PIERRE_THEMES = {
  dark: "pierre-dark",
  light: "pierre-light",
} as const;

export const PIERRE_HIGHLIGHTER_OPTIONS = { theme: PIERRE_THEMES };

/** The resolved theme object `themeToTreeStyles` needs for the file trees. */
export function pierreTreeTheme(mode: ColorMode) {
  return mode === "light" ? pierreLight : pierreDark;
}
