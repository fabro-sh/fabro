/**
 * Read-only checks of the two small languages inside attribute values: edge
 * conditions (Petri's `condition.rs`) and model stylesheets (`stylesheet.rs`).
 * Nothing here rewrites them; a condition's text stays exactly as written.
 */

export interface ExpressionIssue {
  severity: "error" | "warning";
  code: string;
  message: string;
}

const OUTCOMES = ["succeeded", "partially_succeeded", "failed", "skipped"];

type ConditionToken =
  | { kind: "word"; text: string }
  | { kind: "op"; op: "=" | "!=" | ">" | "<" | ">=" | "<=" | "contains" | "matches" }
  | { kind: "and" | "or" | "not" };

type Clause = { key: string; op: string | null; value: string };

class ConditionSyntaxError extends Error {}

function tokenizeCondition(input: string): ConditionToken[] {
  const chars = Array.from(input.trim());
  const tokens: ConditionToken[] = [];
  let index = 0;
  while (index < chars.length) {
    const char = chars[index];
    const next = chars[index + 1];
    if (/\s/.test(char)) {
      index++;
      continue;
    }
    if (char === "&" && next === "&") {
      tokens.push({ kind: "and" });
      index += 2;
      continue;
    }
    if (char === "|" && next === "|") {
      tokens.push({ kind: "or" });
      index += 2;
      continue;
    }
    if ((char === "!" || char === ">" || char === "<") && next === "=") {
      tokens.push({ kind: "op", op: `${char}=` as "!=" | ">=" | "<=" });
      index += 2;
      continue;
    }
    if (char === "=" || char === ">" || char === "<") {
      tokens.push({ kind: "op", op: char });
      index++;
      continue;
    }
    if (char === "!") {
      tokens.push({ kind: "not" });
      index++;
      continue;
    }
    if (char === '"') {
      const start = index;
      index++;
      while (index < chars.length && chars[index] !== '"') {
        if (chars[index] === "\\") index++;
        index++;
      }
      if (index >= chars.length) throw new ConditionSyntaxError("unterminated string");
      index++;
      tokens.push({ kind: "word", text: chars.slice(start, index).join("") });
      continue;
    }
    const start = index;
    while (index < chars.length && !/\s/.test(chars[index]) && !"=!><&|\"".includes(chars[index])) index++;
    if (index === start) throw new ConditionSyntaxError(`unexpected character ${JSON.stringify(char)}`);
    const word = chars.slice(start, index).join("");
    const afterWord = tokens[tokens.length - 1]?.kind === "word";
    if (afterWord && (word === "contains" || word === "matches")) tokens.push({ kind: "op", op: word });
    else tokens.push({ kind: "word", text: word });
  }
  return tokens;
}

function conditionLiteral(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed.length >= 2 && trimmed.startsWith('"') && trimmed.endsWith('"')) {
    return trimmed.slice(1, -1).replace(/\\"/g, '"').replace(/\\\\/g, "\\");
  }
  return trimmed;
}

function parseCondition(text: string): Clause[] {
  const tokens = tokenizeCondition(text);
  const clauses: Clause[] = [];
  if (tokens.length === 0) return clauses;
  let position = 0;
  const peek = () => tokens[position];

  const clause = () => {
    const first = tokens[position++];
    if (!first) throw new ConditionSyntaxError("unexpected end of condition");
    if (first.kind !== "word") throw new ConditionSyntaxError("expected a key");
    const next = peek();
    if (next?.kind !== "op") {
      clauses.push({ key: first.text, op: null, value: "" });
      return;
    }
    position++;
    const value = tokens[position++];
    if (value === undefined) {
      if (next.op !== "=" && next.op !== "!=") throw new ConditionSyntaxError("expected a value after the operator");
      clauses.push({ key: first.text, op: next.op, value: "" });
      return;
    }
    if (value.kind !== "word") throw new ConditionSyntaxError("expected a value after the operator");
    clauses.push({ key: first.text, op: next.op, value: conditionLiteral(value.text) });
  };
  const unary = () => {
    while (peek()?.kind === "not") position++;
    clause();
  };
  const and = () => {
    unary();
    while (peek()?.kind === "and") {
      position++;
      unary();
    }
  };
  and();
  while (peek()?.kind === "or") {
    position++;
    and();
  }
  if (position < tokens.length) throw new ConditionSyntaxError("unexpected text after the condition");
  return clauses;
}

/** Problems with a condition. An empty condition has none: it means unconditional. */
export function checkCondition(text: string): ExpressionIssue[] {
  let clauses: Clause[];
  try {
    clauses = parseCondition(text);
  } catch (error) {
    if (!(error instanceof ConditionSyntaxError)) throw error;
    return [
      {
        severity: "error",
        code: "attractor.condition.syntax",
        message: `Condition "${text}" does not parse: ${error.message}.`,
      },
    ];
  }
  const issues: ExpressionIssue[] = [];
  for (const clause of clauses) {
    if (clause.key === "outcome") {
      if (clause.op === null) continue;
      if (clause.op !== "=" && clause.op !== "!=") {
        issues.push({
          severity: "error",
          code: "attractor.condition.outcome_op",
          message: `Condition "${text}" compares \`outcome\` with \`${clause.op}\`; it supports only = and !=.`,
        });
      } else if (clause.value === "success") {
        issues.push({
          severity: "warning",
          code: "deprecated.outcome_alias",
          message: `Condition "${text}" uses outcome=success, which is not a stage outcome. Write outcome=succeeded.`,
        });
      } else if (!OUTCOMES.includes(clause.value)) {
        issues.push({
          severity: "error",
          code: "unsupported.outcome_value",
          message: `Condition "${text}" names outcome \`${clause.value}\`. The outcomes are ${OUTCOMES.join(", ")}. A custom routing signal belongs in context, read as context.${clause.value}.`,
        });
      }
      continue;
    }
    if (clause.op === ">" || clause.op === "<" || clause.op === ">=" || clause.op === "<=") {
      if (clause.value.trim().length === 0 || Number.isNaN(Number(clause.value.trim()))) {
        issues.push({
          severity: "warning",
          code: "attractor.condition.non_numeric",
          message: `Condition "${text}" compares \`${clause.key}\` against "${clause.value}", which is not a number, so that clause is always false.`,
        });
      }
    }
    if (clause.op === "matches") {
      try {
        new RegExp(clause.value);
      } catch {
        // Fabro's engine is Rust `regex`, not this one, so this can only be a warning.
        issues.push({
          severity: "warning",
          code: "attractor.condition.regex",
          message: `Condition "${text}" has a pattern the browser cannot read: ${clause.value}. Fabro checks it with Rust's regex engine when the workflow loads.`,
        });
      }
    }
  }
  return issues;
}

export type StyleSelector =
  | { kind: "universal" }
  | { kind: "shape"; name: string }
  | { kind: "class"; name: string }
  | { kind: "id"; name: string };

export interface StyleRule {
  selector: StyleSelector;
  declarations: [string, string][];
}

export const STYLESHEET_PROPERTIES = ["model", "provider", "reasoning_effort", "speed", "backend"];

const IDENT = /^[A-Za-z0-9_-]+/;

/** Parse a stylesheet, or return why it can't be. */
export function parseStylesheet(input: string): { rules: StyleRule[] } | { error: string } {
  let text = input;
  for (;;) {
    const start = text.indexOf("/*");
    if (start < 0) break;
    const end = text.indexOf("*/", start + 2);
    if (end < 0) return { error: "unterminated comment" };
    text = `${text.slice(0, start)} ${text.slice(end + 2)}`;
  }
  let rest = text.trim();
  const rules: StyleRule[] = [];
  while (rest.length > 0) {
    let selector: StyleSelector;
    if (rest.startsWith("*")) {
      selector = { kind: "universal" };
      rest = rest.slice(1).trimStart();
    } else {
      const sigil = rest[0] === "#" || rest[0] === "." ? rest[0] : "";
      const body = rest.slice(sigil.length);
      const name = IDENT.exec(body)?.[0];
      if (!name) {
        if (sigil === "#") return { error: "expected a node id after `#`" };
        if (sigil === ".") return { error: "expected a class name after `.`" };
        return { error: "expected a selector (`*`, `#id`, `.class` or a shape)" };
      }
      selector = { kind: sigil === "#" ? "id" : sigil === "." ? "class" : "shape", name };
      rest = body.slice(name.length).trimStart();
    }
    if (!rest.startsWith("{")) return { error: "expected `{` after the selector" };
    rest = rest.slice(1).trimStart();
    const declarations: [string, string][] = [];
    for (;;) {
      if (rest.startsWith("}")) break;
      if (rest.length === 0) return { error: "expected `}`" };
      if (rest.startsWith(";")) {
        rest = rest.slice(1).trimStart();
        continue;
      }
      const propertyEnd = rest.search(/[:\s]/);
      const property = propertyEnd < 0 ? rest : rest.slice(0, propertyEnd);
      rest = propertyEnd < 0 ? "" : rest.slice(propertyEnd).trimStart();
      if (!rest.startsWith(":")) return { error: `expected \`:\` after the property \`${property}\`` };
      rest = rest.slice(1).trimStart();
      const valueEnd = rest.search(/[;}]/);
      const value = (valueEnd < 0 ? rest : rest.slice(0, valueEnd)).trim();
      rest = valueEnd < 0 ? "" : rest.slice(valueEnd).trimStart();
      if (value.length === 0) return { error: `the property \`${property}\` has no value` };
      declarations.push([property, value]);
      if (rest.startsWith(";")) rest = rest.slice(1).trimStart();
    }
    rest = rest.slice(1).trimStart();
    rules.push({ selector, declarations });
  }
  return { rules };
}

export function selectorSpecificity(selector: StyleSelector): number {
  return { universal: 0, shape: 1, class: 2, id: 3 }[selector.kind];
}

/** Whether a value contains MiniJinja syntax, so it can only be judged once rendered. */
export function hasTemplateSyntax(value: string): boolean {
  return value.includes("{{") || value.includes("{%") || value.includes("{#");
}
