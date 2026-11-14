import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { useMatches, type UIMatch } from "react-router";

const SEPARATOR = " — ";

/** What a route module's `handle` may declare about the browser tab. */
export interface TitleHandle {
  /** The page's name in the tab, before " — Fabro". */
  title?: string;
}

/** The tab title for these parts: `Models — Fabro`. */
export function documentTitle(parts: ReadonlyArray<string | null | undefined>): string {
  return [...parts.filter((part): part is string => !!part?.trim()), "Fabro"].join(SEPARATOR);
}

/** The title of the deepest matched route that declares one. */
export function routeTitle(matches: ReadonlyArray<Pick<UIMatch, "handle">>): string | null {
  for (let index = matches.length - 1; index >= 0; index -= 1) {
    const title = (matches[index]!.handle as TitleHandle | undefined)?.title;
    if (title) return title;
  }
  return null;
}

const SetPageTitle = createContext<((title: string | null) => void) | null>(null);

/**
 * Keeps `document.title` on the deepest matched route's `handle.title`, or on
 * the title a page names from its data with `usePageTitle`. One owner, so a
 * layout's title can't overwrite its child's.
 */
export function PageTitleProvider({ children }: { children: ReactNode }) {
  const matches = useMatches();
  const [pageTitle, setPageTitle] = useState<string | null>(null);
  const title = documentTitle([pageTitle ?? routeTitle(matches)]);

  useEffect(() => {
    if (typeof document !== "undefined") document.title = title;
  }, [title]);

  return <SetPageTitle.Provider value={setPageTitle}>{children}</SetPageTitle.Provider>;
}

/**
 * Names the tab from data the route's `handle.title` can't know, such as a
 * run's title. Parts are joined in order; empty parts are dropped.
 */
export function usePageTitle(parts: ReadonlyArray<string | null | undefined>): void {
  const setPageTitle = useContext(SetPageTitle);
  const title = parts.filter((part): part is string => !!part?.trim()).join(SEPARATOR) || null;

  useEffect(() => {
    if (!setPageTitle) return undefined;
    setPageTitle(title);
    return () => setPageTitle(null);
  }, [setPageTitle, title]);
}
