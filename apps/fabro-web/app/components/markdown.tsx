import { useMemo } from "react";
import { Marked } from "marked";

const SAFE_HTTP_URL_RE = /^https?:\/\//i;
const SAFE_MAILTO_URL_RE = /^mailto:/i;

function isSafeMarkdownHref(href: string): boolean {
  return (
    SAFE_HTTP_URL_RE.test(href) ||
    SAFE_MAILTO_URL_RE.test(href) ||
    href.startsWith("#") ||
    (href.startsWith("/") && !href.startsWith("//"))
  );
}

const markedSafe = new Marked();
markedSafe.use({
  async: false,
  walkTokens(token) {
    if (
      (token.type === "link" || token.type === "image") &&
      typeof token.href === "string" &&
      !isSafeMarkdownHref(token.href)
    ) {
      token.href = "";
    }
  },
  renderer: {
    html() {
      return "";
    },
  },
});

export function Markdown({ content }: { content: string }) {
  const html = useMemo(
    () => markedSafe.parse(content, { async: false }) as string,
    [content],
  );
  return (
    // react-doctor-disable-next-line react-doctor/no-danger -- markedSafe strips raw HTML and unsafe link/image URLs before rendering Markdown.
    <div
      className="prose prose-sm max-w-none text-fg-3 prose-headings:text-fg-2 prose-strong:text-fg-2 prose-code:rounded prose-code:bg-overlay-strong prose-code:px-1 prose-code:py-0.5 prose-code:text-[0.8em] prose-code:font-mono prose-code:text-fg-3 prose-code:before:content-none prose-code:after:content-none prose-pre:bg-overlay-strong prose-pre:text-fg-3 prose-a:text-teal-500"
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
