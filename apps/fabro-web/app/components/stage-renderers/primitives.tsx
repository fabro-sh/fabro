import { useMemo, type ReactNode } from "react";
import { Marked } from "marked";

import { highlightJson } from "../event-debug-helpers";
import { prettyJson } from "./pretty-json";

export function DetailField({
  label,
  children,
  mono = false,
}: {
  label: string;
  children: ReactNode;
  mono?: boolean;
}) {
  return (
    <div>
      <div className="mb-1 text-xs font-medium uppercase tracking-wider text-fg-muted">
        {label}
      </div>
      <div className={mono ? "font-mono text-sm text-fg-3" : "text-sm text-fg-3"}>
        {children}
      </div>
    </div>
  );
}

export function CodeBlock({ children }: { children: string }) {
  return (
    <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
      {children || <span className="text-fg-muted">empty</span>}
    </pre>
  );
}

export function JsonBlock({ value }: { value: string }) {
  const pretty = useMemo(() => prettyJson(value), [value]);
  const tokens = useMemo(
    () => (pretty.isJson ? highlightJson(pretty.text) : null),
    [pretty.isJson, pretty.text],
  );
  return (
    <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
      {!pretty.text ? (
        <span className="text-fg-muted">empty</span>
      ) : (
        tokens ?? pretty.text
      )}
    </pre>
  );
}

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
