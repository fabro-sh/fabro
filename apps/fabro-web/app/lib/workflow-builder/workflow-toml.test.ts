import { describe, expect, test } from "bun:test";

import { findWorkflowGraph } from "./workflow-toml";

const REAL = 'graph = "real.fabro"';
const HOOK_WITH_LOOKALIKE = `[[run.hooks]]\nscript = '''\ncat <<'END'\n[workflow]\ngraph = "lookalike.fabro"\nEND\n'''\n`;

/** Bun's own TOML parser is the referee: what's found has to be what TOML means. */
function expectFound(toml: string, value: string): void {
  const found = findWorkflowGraph(toml);
  expect(found?.value).toBe(value);
  expect(toml.slice(found?.start, found?.end).slice(1, -1)).toBe(value);
  expect((Bun.TOML.parse(toml) as { workflow: { graph: string } }).workflow.graph).toBe(value);
}

describe("the actual graph setting is found", () => {
  test.each<[string, string, string]>([
    ["the usual form", `[workflow]\n${REAL}\n`, "real.fabro"],
    ["with a comment after it", `[workflow]\ngraph = "real.fabro" # mine\n`, "real.fabro"],
    ["in single quotes", `[workflow]\ngraph = 'real.fabro'\n`, "real.fabro"],
    ["without spaces", `[workflow]\ngraph="real.fabro"\n`, "real.fabro"],
    ["as a dotted key", `workflow.graph = "real.fabro"\n[run]\nx = 1\n`, "real.fabro"],
    ["with spaces in the header", `[ workflow ]\n${REAL}\n`, "real.fabro"],
    ["indented", `  [workflow]\n  ${REAL}\n`, "real.fabro"],
    ["with Windows line endings", `[workflow]\r\n${REAL}\r\n[run]\r\nx = 1\r\n`, "real.fabro"],
    ["after a hook holding a lookalike", `${HOOK_WITH_LOOKALIKE}[workflow]\n${REAL}\n`, "real.fabro"],
    ["before a hook holding a lookalike", `[workflow]\n${REAL}\n${HOOK_WITH_LOOKALIKE}`, "real.fabro"],
    ["beside a lookalike in a multiline basic string", `x = """\n[workflow]\ngraph = "no"\n"""\n[workflow]\n${REAL}\n`, "real.fabro"],
    ["beside an escaped quote run in a multiline string", `x = """a\\"""\n[workflow]\ngraph = "no"\n"""\n[workflow]\n${REAL}\n`, "real.fabro"],
    ["beside a lookalike in a comment", `# [workflow]\n# graph = "no"\n[workflow]\n${REAL}\n`, "real.fabro"],
    ["beside a lookalike in an array of strings", `args = [\n  '''\n[workflow]\ngraph = "no"\n''',\n]\n[workflow]\n${REAL}\n`, "real.fabro"],
    ["past nested arrays that look like headers", `[workflow]\nmatrix = [\n  [1, 2],\n  [3, 4]\n]\n${REAL}\n`, "real.fabro"],
    ["past a string holding the other quote", `note = "it's"\nother = 'say "hi"'\n[workflow]\n${REAL}\n`, "real.fabro"],
    ["past a string holding triple quotes", `note = "'''"\n[workflow]\n${REAL}\n`, "real.fabro"],
    ["past a comment holding a quote", `# don't\n[workflow]\n${REAL}\n`, "real.fabro"],
    ["past a hash inside a string", `note = "#1"\n[workflow]\n${REAL}\n`, "real.fabro"],
  ])("%s", (_name, toml, value) => {
    expectFound(toml, value);
  });
});

describe("nothing is claimed that isn't the setting", () => {
  test.each<[string, string]>([
    ["no setting at all", "_version = 1\n[run]\nx = 1\n"],
    ["a workflow table without one", "[workflow]\nname = 'x'\n"],
    ["graph in another table", `[other]\n${REAL}\n`],
    ["graph in a table below workflow", `[workflow.sub]\n${REAL}\n`],
    ["graph at the top level", `${REAL}\n`],
    ["graph in a hook table after workflow", `[workflow]\n[[run.hooks]]\n${REAL}\n`],
    ["only a lookalike in a hook", HOOK_WITH_LOOKALIKE],
    ["only a lookalike in a comment", `# [workflow]\n# ${REAL}\n`],
    ["a dotted key under another table", `[run]\nworkflow.graph = "real.fabro"\n`],
    ["a dotted key under a quoted table", `["other"]\nworkflow.graph = "real.fabro"\n`],
  ])("%s", (_name, toml) => {
    expect(findWorkflowGraph(toml)).toBeNull();
    expect((Bun.TOML.parse(toml) as { workflow?: { graph?: string } }).workflow?.graph).toBeUndefined();
  });
});

describe("real settings it won't touch come back as null, so the file is left alone", () => {
  test.each<[string, string]>([
    ["a quoted key", `[workflow]\n"graph" = "real.fabro"\n`],
    ["a quoted table", `["workflow"]\n${REAL}\n`],
    ["an inline table", `workflow = { graph = "real.fabro" }\n`],
    ["a multiline string", `[workflow]\ngraph = """real.fabro"""\n`],
    ["an escape in the value", `[workflow]\ngraph = "real\\u002efabro"\n`],
  ])("%s", (_name, toml) => {
    expect(findWorkflowGraph(toml)).toBeNull();
    expect((Bun.TOML.parse(toml) as { workflow: { graph: string } }).workflow.graph).toBe("real.fabro");
  });

  test.each<[string, string]>([
    ["an unterminated string", `x = "oops\n[workflow]\n${REAL}\n`],
    ["an unterminated multiline string", `x = '''oops\n[workflow]\n${REAL}\n`],
    ["an unclosed array", `x = [\n[workflow]\n${REAL}\n`],
    ["the setting twice", `[workflow]\n${REAL}\n${REAL}\n`],
    ["the setting not being a string", `[workflow]\ngraph = 1\n`],
  ])("broken or unexpected: %s", (_name, toml) => {
    expect(findWorkflowGraph(toml)).toBeNull();
  });
});
