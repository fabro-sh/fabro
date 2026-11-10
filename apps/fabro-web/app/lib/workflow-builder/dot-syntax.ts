/**
 * The DOT subset Fabro accepts: a TypeScript port of Petri's lexer and parser
 * (`crates/attractor/frontend/src/dot/{lexer,parser}.rs`), which is what every
 * run goes through. It accepts and refuses the same text, reports the same
 * first problem, and additionally keeps the comments and bare/quoted spelling
 * that Petri throws away, because the builder has to write them back.
 */

export interface SourcePosition {
  line: number;
  column: number;
}

export interface AstAttr extends SourcePosition {
  key: string;
  /** The value as written, escapes resolved. */
  value: string;
  /** Written as a bare word rather than a quoted string. */
  bare: boolean;
}

export interface AstIdent extends SourcePosition {
  name: string;
}

interface AstSpan {
  /** Index of the statement's first token. */
  start: number;
  /** Index just past the statement's last token. */
  end: number;
}

export type AstStatement = AstSpan &
  (
    | { kind: "graphAttrs"; attrs: AstAttr[] }
    | { kind: "graphAttr"; attr: AstAttr }
    | { kind: "nodeDefaults"; attrs: AstAttr[] }
    | { kind: "edgeDefaults"; attrs: AstAttr[] }
    | { kind: "node"; id: AstIdent; attrs: AstAttr[] | null }
    | { kind: "edge"; nodes: AstIdent[]; attrs: AstAttr[] | null }
    | { kind: "subgraph"; name: AstIdent | null; statements: AstStatement[] }
  );

export interface AstComment extends SourcePosition {
  /** Delimiters included. */
  text: string;
  /** How many tokens precede the comment. */
  tokenIndex: number;
  /** The comment shares a line with the token before it. */
  sameLineAsPrevious: boolean;
}

export interface DotAst {
  name: AstIdent;
  statements: AstStatement[];
  comments: AstComment[];
  tokenCount: number;
}

export class DotSyntaxError extends Error {
  readonly line: number;
  readonly column: number;
  readonly code: string;

  constructor(code: string, message: string, position: SourcePosition) {
    super(message);
    this.name = "DotSyntaxError";
    this.code = code;
    this.line = position.line;
    this.column = position.column;
  }
}

type TokenKind =
  | "word"
  | "quoted"
  | "lbrace"
  | "rbrace"
  | "lbracket"
  | "rbracket"
  | "eq"
  | "comma"
  | "semi"
  | "colon"
  | "arrow";

interface Token extends SourcePosition {
  kind: TokenKind;
  /** The text of a word or quoted string. */
  text: string;
  /** The line the token ends on; a quoted string can span lines. */
  endLine: number;
}

const SIMPLE_TOKENS: Record<string, TokenKind> = {
  "{": "lbrace",
  "}": "rbrace",
  "[": "lbracket",
  "]": "rbracket",
  "=": "eq",
  ",": "comma",
  ";": "semi",
  ":": "colon",
};

/** Rust's `char::is_whitespace`: Unicode White_Space, which JavaScript's `\s` does not match exactly. */
const WHITESPACE = new Set(
  Array.from(
    "\t\n\u000b\f\r \u0085                 　",
  ),
);

const PUNCTUATION = new Set(["{", "}", "[", "]", "=", ",", ";", ":", '"', "<", ">"]);

const MAX_SUBGRAPH_DEPTH = 64;

function lex(text: string): { tokens: Token[]; comments: AstComment[] } {
  const chars = Array.from(text);
  const tokens: Token[] = [];
  const comments: AstComment[] = [];
  let index = 0;
  let line = 1;
  let column = 1;
  let atLineStart = true;

  const bump = (): string | undefined => {
    const char = chars[index];
    if (char === undefined) return undefined;
    index++;
    if (char === "\n") {
      line++;
      column = 1;
    } else {
      column++;
    }
    return char;
  };

  const pushComment = (start: number, position: SourcePosition) => {
    const previous = tokens[tokens.length - 1];
    comments.push({
      ...position,
      text: chars.slice(start, index).join("").replace(/\r$/, ""),
      tokenIndex: tokens.length,
      sameLineAsPrevious: previous !== undefined && previous.endLine === position.line,
    });
  };

  while (index < chars.length) {
    const char = chars[index];
    const position = { line, column };
    const next = chars[index + 1];
    if (char === "\n") {
      bump();
      atLineStart = true;
      continue;
    }
    if (WHITESPACE.has(char)) {
      bump();
      continue;
    }
    if (char === "#" && atLineStart) {
      const start = index;
      while (index < chars.length && chars[index] !== "\n") bump();
      pushComment(start, position);
      continue;
    }
    atLineStart = false;
    if (char === "/" && next === "/") {
      const start = index;
      while (index < chars.length && chars[index] !== "\n") bump();
      pushComment(start, position);
      continue;
    }
    if (char === "/" && next === "*") {
      const start = index;
      bump();
      bump();
      for (;;) {
        const inner = bump();
        if (inner === undefined) {
          throw new DotSyntaxError("dot.syntax", "unterminated `/*` comment", position);
        }
        if (inner === "*" && chars[index] === "/") {
          bump();
          break;
        }
      }
      pushComment(start, position);
      continue;
    }
    const simple = Object.hasOwn(SIMPLE_TOKENS, char) ? SIMPLE_TOKENS[char] : undefined;
    if (simple) {
      bump();
      tokens.push({ kind: simple, text: char, ...position, endLine: position.line });
      continue;
    }
    if (char === "<") {
      throw new DotSyntaxError(
        "unsupported.dot.html_string",
        "HTML-like `<...>` strings are not supported; write the label as a double-quoted string",
        position,
      );
    }
    if (char === ">") {
      throw new DotSyntaxError("dot.syntax", `unexpected character ${JSON.stringify(char)}`, position);
    }
    if (char === '"') {
      bump();
      let value = "";
      for (;;) {
        const inner = bump();
        if (inner === undefined) {
          throw new DotSyntaxError("dot.syntax", "unterminated string", position);
        }
        if (inner === '"') break;
        if (inner !== "\\") {
          value += inner;
          continue;
        }
        const escaped = bump();
        if (escaped === undefined) {
          throw new DotSyntaxError("dot.syntax", "unterminated string", position);
        }
        if (escaped === '"') value += '"';
        else if (escaped === "\\") value += "\\";
        else if (escaped === "n") value += "\n";
        else if (escaped === "t") value += "\t";
        // A backslash before a newline joins the lines; any other escape stays verbatim.
        else if (escaped !== "\n") value += `\\${escaped}`;
      }
      tokens.push({ kind: "quoted", text: value, ...position, endLine: line });
      continue;
    }
    if (char === "-") {
      if (next === ">") {
        bump();
        bump();
        tokens.push({ kind: "arrow", text: "->", ...position, endLine: position.line });
        continue;
      }
      if (next === "-") {
        throw new DotSyntaxError(
          "unsupported.dot.undirected",
          "`--` is an undirected edge; Fabro graphs are `digraph`s with `->`",
          position,
        );
      }
    }
    if (PUNCTUATION.has(char) || char === "/") {
      throw new DotSyntaxError("dot.syntax", `unexpected character ${JSON.stringify(char)}`, position);
    }
    let word = "";
    while (index < chars.length) {
      const inner = chars[index];
      const after = chars[index + 1];
      if (WHITESPACE.has(inner) || PUNCTUATION.has(inner)) break;
      if (inner === "-" && after === ">") break;
      if (inner === "/" && (after === "/" || after === "*")) break;
      word += inner;
      bump();
    }
    tokens.push({ kind: "word", text: word, ...position, endLine: position.line });
  }
  return { tokens, comments };
}

export function asciiLower(text: string): string {
  return text.replace(/[A-Z]/g, (char) => char.toLowerCase());
}

function describe(token: Token): string {
  if (token.kind === "word") return `\`${token.text}\``;
  if (token.kind === "quoted") return JSON.stringify(token.text);
  return `\`${token.text}\``;
}

class Parser {
  private position = 0;

  constructor(private readonly tokens: Token[]) {}

  private peek(offset = 0): Token | undefined {
    return this.tokens[this.position + offset];
  }

  private here(): SourcePosition {
    const token = this.peek() ?? this.tokens[this.tokens.length - 1];
    return token ? { line: token.line, column: token.column } : { line: 1, column: 1 };
  }

  private fail(message: string, code = "dot.syntax"): DotSyntaxError {
    return new DotSyntaxError(code, message, this.here());
  }

  private expect(kind: TokenKind, what: string): Token {
    const token = this.peek();
    if (token?.kind === kind) {
      this.position++;
      return token;
    }
    if (token) throw this.fail(`expected ${what}, found ${describe(token)}`);
    throw this.fail(`expected ${what}, found the end of the file`);
  }

  private eat(kind: TokenKind): boolean {
    if (this.peek()?.kind === kind) {
      this.position++;
      return true;
    }
    return false;
  }

  private atKeyword(word: string): boolean {
    const token = this.peek();
    return token?.kind === "word" && asciiLower(token.text) === word;
  }

  graph(): Pick<DotAst, "name" | "statements"> {
    if (this.atKeyword("strict")) {
      throw this.fail(
        "`strict` graphs are not supported; drop `strict`",
        "unsupported.dot.strict",
      );
    }
    if (this.atKeyword("graph")) {
      throw this.fail("an undirected `graph` is not a workflow; write `digraph`", "unsupported.dot.undirected");
    }
    if (!this.atKeyword("digraph")) throw this.fail("a Fabro workflow starts with `digraph`");
    this.position++;
    const name: AstIdent =
      this.peek()?.kind === "lbrace" ? { name: "", ...this.here() } : this.ident("the graph name");
    this.expect("lbrace", "`{`");
    const statements = this.statements(0);
    this.expect("rbrace", "`}`");
    const extra = this.peek();
    if (extra) throw this.fail(`unexpected ${describe(extra)} after the graph`);
    return { name, statements };
  }

  private statements(depth: number): AstStatement[] {
    if (depth > MAX_SUBGRAPH_DEPTH) {
      throw this.fail(`subgraphs nest more than ${MAX_SUBGRAPH_DEPTH} levels deep`);
    }
    const out: AstStatement[] = [];
    for (;;) {
      const token = this.peek();
      if (!token) throw this.fail("expected `}` before the end of the file");
      if (token.kind === "rbrace") return out;
      if (token.kind === "semi") {
        this.position++;
        continue;
      }
      out.push(this.statement(depth));
    }
  }

  private statement(depth: number): AstStatement {
    const start = this.position;
    const finish = <T extends object>(statement: T): T & AstSpan => ({ ...statement, start, end: this.position });
    if (this.atKeyword("graph")) {
      this.position++;
      const attrs = this.attrBlock();
      this.eat("semi");
      return finish({ kind: "graphAttrs" as const, attrs });
    }
    if (this.atKeyword("node")) {
      this.position++;
      const attrs = this.attrBlock();
      this.eat("semi");
      return finish({ kind: "nodeDefaults" as const, attrs });
    }
    if (this.atKeyword("edge")) {
      this.position++;
      const attrs = this.attrBlock();
      this.eat("semi");
      return finish({ kind: "edgeDefaults" as const, attrs });
    }
    if (this.atKeyword("subgraph")) return finish(this.subgraph(depth));
    const token = this.peek();
    if (token?.kind === "lbrace") {
      throw this.fail(
        "anonymous `{ ... }` subgraphs are not supported; name the subgraph: `subgraph cluster_name { ... }`",
        "unsupported.dot.anonymous_subgraph",
      );
    }
    if (token?.kind === "word" || token?.kind === "quoted") {
      if (this.peek(1)?.kind === "eq") {
        const attr = this.attr();
        this.eat("semi");
        return finish({ kind: "graphAttr" as const, attr });
      }
      return finish(this.nodeOrEdge());
    }
    throw this.fail(token ? `expected a statement, found ${describe(token)}` : "expected a statement");
  }

  private subgraph(depth: number) {
    this.position++;
    const token = this.peek();
    const name = token?.kind === "word" || token?.kind === "quoted" ? this.ident("the subgraph name") : null;
    this.expect("lbrace", "`{`");
    const statements = this.statements(depth + 1);
    this.expect("rbrace", "`}`");
    if (this.peek()?.kind === "arrow") {
      throw this.fail("a subgraph cannot be an edge endpoint; write one edge per node", "unsupported.dot.subgraph_edge");
    }
    this.eat("semi");
    return { kind: "subgraph" as const, name, statements };
  }

  private nodeOrEdge() {
    const first = this.ident("a node id");
    this.rejectPort();
    if (this.peek()?.kind !== "arrow") {
      const attrs = this.optionalAttrBlock();
      this.eat("semi");
      return { kind: "node" as const, id: first, attrs };
    }
    const nodes = [first];
    while (this.eat("arrow")) {
      if (this.peek()?.kind === "lbrace" || this.atKeyword("subgraph")) {
        throw this.fail(
          "a subgraph cannot be an edge endpoint; write one edge per node",
          "unsupported.dot.subgraph_edge",
        );
      }
      nodes.push(this.ident("a node id after `->`"));
      this.rejectPort();
    }
    const attrs = this.optionalAttrBlock();
    this.eat("semi");
    return { kind: "edge" as const, nodes, attrs };
  }

  private rejectPort(): void {
    if (this.peek()?.kind === "colon") {
      throw this.fail(
        "node ports (`node:port`) are not supported; Fabro routes on edges, not on node ports",
        "unsupported.dot.port",
      );
    }
  }

  private ident(what: string): AstIdent {
    const position = this.here();
    const token = this.peek();
    if (!token) throw new DotSyntaxError("dot.syntax", `expected ${what}, found the end of the file`, position);
    this.position++;
    if (token.kind === "word" || token.kind === "quoted") return { name: token.text, ...position };
    throw new DotSyntaxError("dot.syntax", `expected ${what}, found ${describe(token)}`, position);
  }

  private optionalAttrBlock(): AstAttr[] | null {
    return this.peek()?.kind === "lbracket" ? this.attrBlock() : null;
  }

  private attrBlock(): AstAttr[] {
    this.expect("lbracket", "`[`");
    const attrs: AstAttr[] = [];
    for (;;) {
      const token = this.peek();
      if (!token) throw this.fail("expected `]` before the end of the file");
      if (token.kind === "rbracket") {
        this.position++;
        return attrs;
      }
      if (token.kind === "comma" || token.kind === "semi") {
        this.position++;
      } else if (token.kind === "word" || token.kind === "quoted") {
        attrs.push(this.attr());
      } else {
        throw this.fail(`expected an attribute or \`]\`, found ${describe(token)}`);
      }
    }
  }

  private attr(): AstAttr {
    const key = this.ident("an attribute name");
    this.expect("eq", "`=`");
    const token = this.peek();
    if (!token) throw this.fail(`expected a value for \`${key.name}\``);
    this.position++;
    if (token.kind === "quoted" || token.kind === "word") {
      return { key: key.name, value: token.text, bare: token.kind === "word", line: key.line, column: key.column };
    }
    this.position--;
    throw this.fail(`expected a value for \`${key.name}\`, found ${describe(token)}`);
  }
}

/** Parse `text`, or throw `DotSyntaxError` naming the first problem. There is no partial result. */
export function parseDot(text: string): DotAst {
  const { tokens, comments } = lex(text);
  const { name, statements } = new Parser(tokens).graph();
  return { name, statements, comments, tokenCount: tokens.length };
}

/** Whether a bare word reads as a string in Fabro's typed reader, rather than a boolean or number. */
export function bareWordIsString(word: string): boolean {
  if (word === "true" || word === "false") return false;
  if (/^[+-]?\d+$/.test(word)) return false;
  return !/^-?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?$/.test(word);
}
