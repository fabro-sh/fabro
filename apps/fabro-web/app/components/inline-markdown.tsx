import { useMemo, type ReactNode } from "react";
import { Lexer, type Token } from "marked";

const CODE_CLASSNAME =
  "rounded bg-overlay-strong px-1 py-0.5 font-mono text-[0.85em] text-fg-2";

function tokenKey(token: Token, index: number): string {
  const raw = "raw" in token ? token.raw : "";
  return `${token.type}:${raw}:${index}`;
}

function TokenList({ tokens }: { tokens: Token[] }) {
  return (
    <>
      {tokens.map((token, index) => (
        <TokenNode key={tokenKey(token, index)} token={token} />
      ))}
    </>
  );
}

function TokenNode({ token }: { token: Token }): ReactNode {
  switch (token.type) {
    case "codespan":
      return <code className={CODE_CLASSNAME}>{token.text}</code>;
    case "strong":
      return <strong><TokenList tokens={token.tokens} /></strong>;
    case "em":
      return <em><TokenList tokens={token.tokens} /></em>;
    case "del":
      return <TokenList tokens={token.tokens} />;
    case "link":
      return token.tokens.length > 0 ? <TokenList tokens={token.tokens} /> : token.text;
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
        ? <TokenList tokens={token.tokens} />
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
  const tokens = useMemo(() => Lexer.lexInline(content), [content]);
  return <span className={className}><TokenList tokens={tokens} /></span>;
}
