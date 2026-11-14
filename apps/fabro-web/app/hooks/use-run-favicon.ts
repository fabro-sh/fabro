import { useEffect } from "react";

import { dotColorVariable, faviconWithDot } from "../lib/run-favicon";

const LOGO_URL = "/images/favicon.svg";
let logo: Promise<string> | null = null;

/** The Fabro icon's SVG, fetched the first time a run needs a dot. */
function loadLogo(): Promise<string> {
  logo ??= fetch(LOGO_URL).then((response) => {
    if (!response.ok) throw new Error(`favicon request failed with ${response.status}`);
    return response.text();
  });
  logo.catch(() => {
    logo = null;
  });
  return logo;
}

/**
 * Puts the run's status dot on the tab's Fabro icon, in its badge's colour,
 * and restores the plain icon when the run page goes. A run with a muted
 * status keeps the plain icon and fetches nothing.
 */
export function useRunFavicon(status: string | undefined): void {
  useEffect(() => {
    const variable = dotColorVariable(status);
    if (typeof document === "undefined" || !variable) return undefined;
    const theme = getComputedStyle(document.documentElement);
    const fill = theme.getPropertyValue(variable).trim();
    const ring = theme.getPropertyValue("--color-page").trim();
    if (!fill || !ring) return undefined;

    const links = Array.from(document.querySelectorAll<HTMLLinkElement>('link[rel="icon"]'));
    const previous = links.map((link) => ({ link, href: link.getAttribute("href"), type: link.type }));
    let cancelled = false;

    loadLogo()
      .then((svg) => {
        if (cancelled) return;
        const href = `data:image/svg+xml,${encodeURIComponent(faviconWithDot(svg, { fill, ring }))}`;
        for (const link of links) {
          link.type = "image/svg+xml";
          link.href = href;
        }
      })
      .catch(() => {
        // The plain icon stays; the title still names the run.
      });

    return () => {
      cancelled = true;
      for (const { link, href, type } of previous) {
        if (href === null) link.removeAttribute("href");
        else link.setAttribute("href", href);
        link.type = type;
      }
    };
  }, [status]);
}
