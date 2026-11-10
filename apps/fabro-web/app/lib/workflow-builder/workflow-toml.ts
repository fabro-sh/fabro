/**
 * Just enough TOML to find the workflow's `graph` setting in a `workflow.toml`
 * someone else wrote, so that one value can change and nothing else does.
 *
 * Strings and comments are blanked out first, so text inside a hook's script can't
 * be mistaken for a table or a key. Anything it isn't sure of comes back as null,
 * and the caller leaves the file alone.
 */

export interface WorkflowGraphValue {
  /** Where the quoted value sits in the file, quotes included. */
  start: number;
  end: number;
  value: string;
}

const STRING_OR_COMMENT = /"""(?:\\[\s\S]|[^\\])*?"""|'''[\s\S]*?'''|"(?:\\.|[^"\\\n])*"|'[^'\n]*'|#[^\n]*/g;
const PLAIN_STRING = /^(?:"[^"\\\n]*"|'[^'\n]*')$/;
const TABLE_HEADER = /^\s*\[\[?\s*([^[\]]*?)\s*\]\]?\s*$/;
const KEY = /^\s*([\w.\- ]+?)\s*=/;

function bracketBalance(line: string): number {
  return (line.match(/[[{]/g)?.length ?? 0) - (line.match(/[\]}]/g)?.length ?? 0);
}

/**
 * The value of `[workflow] graph`, or of `workflow.graph` at the top level. Null
 * when there's no such setting, or when it isn't a plain one-line string in a file
 * this can read: `"graph" = ...`, `workflow = { graph = ... }`, or broken quoting.
 */
export function findWorkflowGraph(toml: string): WorkflowGraphValue | null {
  const tokens = new Map<number, string>();
  const structure = toml.replace(STRING_OR_COMMENT, (token: string, offset: number) => {
    tokens.set(offset, token);
    return token.replace(/[^\n]/g, " ");
  });
  if (/["']/.test(structure)) return null;

  let table = "";
  let depth = 0;
  let offset = 0;
  let found: WorkflowGraphValue | null = null;
  for (const line of structure.split("\n")) {
    const header = depth === 0 ? TABLE_HEADER.exec(line) : null;
    if (header) {
      // A quoted table name is blank here, and can't be told apart. It's never this one's spelling.
      table = header[1] || "?";
    } else {
      const key = depth === 0 ? KEY.exec(line) : null;
      if (key && [table, key[1].replace(/\s/g, "")].filter(Boolean).join(".") === "workflow.graph") {
        // Blanks after the `=` include the value itself, so look for it in the file.
        let start = offset + key[0].length;
        while (toml[start] === " " || toml[start] === "\t") start++;
        const token = tokens.get(start);
        if (found || !token || !PLAIN_STRING.test(token)) return null;
        found = { start, end: start + token.length, value: token.slice(1, -1) };
      }
      depth += bracketBalance(line);
    }
    offset += line.length + 1;
  }
  return depth === 0 ? found : null;
}
