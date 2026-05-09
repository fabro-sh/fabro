import { Fragment, useMemo, type ReactNode } from "react";
import { Lexer, type Token } from "marked";

const CODE_CLASSNAME =
  "rounded bg-overlay-strong px-1 py-0.5 font-mono text-[0.85em] text-fg-2";

function renderTokens(tokens: Token[]): ReactNode[] {
  return tokens.map((token, index) => (
    <Fragment key={index}>{renderToken(token)}</Fragment>
  ));
}

function renderToken(token: Token): ReactNode {
  switch (token.type) {
    case "codespan":
      return <code className={CODE_CLASSNAME}>{token.text}</code>;
    case "strong":
      return <strong>{renderTokens(token.tokens)}</strong>;
    case "em":
      return <em>{renderTokens(token.tokens)}</em>;
    case "del":
      return renderTokens(token.tokens);
    case "link":
      return token.tokens.length > 0 ? renderTokens(token.tokens) : token.text;
    case "image":
      return token.text;
    case "html":
      return token.raw;
    case "br":
      return " ";
    case "escape":
      return token.text;
    case "text":
      return token.tokens && token.tokens.length > 0
        ? renderTokens(token.tokens)
        : token.text;
    default:
      return "raw" in token ? token.raw : "";
  }
}

export function InlineMarkdown({
  content,
  className,
}: {
  content: string;
  className?: string;
}) {
  const children = useMemo(() => renderTokens(Lexer.lexInline(content)), [content]);
  return <span className={className}>{children}</span>;
}
