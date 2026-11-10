import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { EXAMPLE_ASSETS, EXAMPLE_SOURCE } from "./example";
import { effectiveNodeAttributes, parseWorkflow, validateWorkflow } from "./model";
import type { WorkflowGraph } from "./types";

/**
 * The sample's checks run for real here: its scripts, taken from the workflow
 * files as Fabro would read them, in throwaway Git repositories. No model and no
 * Fabro run is involved. What the agents would do is out of scope; what happens
 * to a failing project is not.
 */

function childGraph(path: string): WorkflowGraph {
  const { graph } = parseWorkflow(EXAMPLE_ASSETS[path]);
  if (!graph) throw new Error(`${path} does not parse`);
  return graph;
}

function scriptOf(path: string, nodeId: string): string {
  const graph = childGraph(path);
  const node = graph.nodes.find((candidate) => candidate.id === nodeId);
  if (!node) throw new Error(`${path} has no node ${nodeId}`);
  const script = effectiveNodeAttributes(graph, node).script;
  if (script === undefined) throw new Error(`${nodeId} has no script`);
  return script;
}

const TESTS = () => scriptOf("shared/testing.fabro", "project_tests");
const SCANS = () => scriptOf("shared/security.fabro", "automated_scans");
const COLLECT = () => scriptOf("shared/code-review.fabro", "collect_change");

let project: string;

function git(...args: string[]): void {
  const result = spawnSync("git", args, { cwd: project, encoding: "utf8" });
  if (result.status !== 0) throw new Error(`git ${args.join(" ")}: ${result.stderr}`);
}

function write(path: string, content: string): void {
  mkdirSync(dirname(join(project, path)), { recursive: true });
  writeFileSync(join(project, path), content);
}

function read(path: string): string {
  return readFileSync(join(project, path), "utf8");
}

function run(script: string): { status: number | null; output: string } {
  const result = spawnSync("bash", ["-c", script], {
    cwd: project,
    encoding: "utf8",
    // Bun itself is what the fixtures' test scripts run on.
    env: { ...process.env, PATH: `${dirname(process.execPath)}:${process.env.PATH}` },
  });
  return { status: result.status, output: `${result.stdout}${result.stderr}` };
}

/** A committed project, with a plan naming its first commit as the base. */
function initProject(): string {
  git("init", "-q");
  git("config", "user.email", "test@example.com");
  git("config", "user.name", "Test");
  git("config", "commit.gpgsign", "false");
  write("src.txt", "first\n");
  git("add", ".");
  git("commit", "-q", "-m", "first");
  const head = spawnSync("git", ["rev-parse", "HEAD"], { cwd: project, encoding: "utf8" }).stdout.trim();
  write("plan.md", `Base commit: ${head}\n\nThe plan.\n`);
  return head;
}

/** A Bun project whose test script does what it's told. Its lockfile selects the package manager. */
function bunProject(scripts: Record<string, string>): void {
  write("package.json", `${JSON.stringify({ name: "fixture", scripts }, null, 2)}\n`);
  write("bun.lock", "");
  mkdirSync(join(project, "node_modules"), { recursive: true });
}

beforeEach(() => {
  project = mkdtempSync(join(tmpdir(), "fabro-sample-"));
});

afterEach(() => {
  rmSync(project, { recursive: true, force: true });
});

describe("the sample's files", () => {
  test("each child is a valid workflow on its own, with no warnings", () => {
    for (const path of ["shared/testing.fabro", "shared/security.fabro", "shared/code-review.fabro"]) {
      const diagnostics = validateWorkflow(childGraph(path), { "shared/ticket.md": "x", "ticket.md": "x" });
      expect(diagnostics).toEqual([]);
    }
  });

  test("each child fails as a whole if any of its steps fails, and ends in a command", () => {
    for (const [path, gate] of [
      ["shared/testing.fabro", "testing_gate"],
      ["shared/security.fabro", "security_gate"],
      ["shared/code-review.fabro", "review_gate"],
    ]) {
      const graph = childGraph(path);
      expect(graph.attrs.on_failure).toBe("exit");
      expect(graph.edges.find((edge) => edge.target === "exit")?.source).toBe(gate);
      // No condition anywhere: a failed step has no route, so the child can't reach exit.
      expect(graph.edges.every((edge) => edge.attrs.condition === undefined)).toBe(true);
    }
  });

  test("a child reads the ticket beside it, since Fabro refuses a path through ..", () => {
    for (const path of ["shared/testing.fabro", "shared/security.fabro", "shared/code-review.fabro"]) {
      expect(childGraph(path).attrs.goal).toBe("@ticket.md");
    }
    expect(EXAMPLE_ASSETS["shared/ticket.md"]).toContain("## Acceptance criteria");
  });

  test("the ticket renders as a template without tripping on anything", () => {
    expect(EXAMPLE_ASSETS["shared/ticket.md"]).not.toMatch(/\{\{|\{%|\{#/);
  });

  test("agents must report a status, and only a pass counts as success", () => {
    for (const [path, id] of [
      ["shared/testing.fabro", "acceptance_check"],
      ["shared/security.fabro", "security_review"],
      ["shared/code-review.fabro", "code_review"],
    ]) {
      const graph = childGraph(path);
      const node = graph.nodes.find((candidate) => candidate.id === id);
      const attrs = effectiveNodeAttributes(graph, node as WorkflowGraph["nodes"][number]);
      expect(attrs.output_schema).toBe("routing");
      expect(attrs.prompt).toContain('{"outcome":"failed"');
      expect(attrs.prompt).toContain("Do not change application code");
    }
  });

  test("delivery keeps the fallback artifacts and never merges", () => {
    const parent = parseWorkflow(EXAMPLE_SOURCE).graph as WorkflowGraph;
    const promptOf = (id: string) =>
      effectiveNodeAttributes(parent, parent.nodes.find((node) => node.id === id) as WorkflowGraph["nodes"][number]).prompt;
    expect(promptOf("pr")).toContain("Never merge automatically");
    expect(promptOf("pr")).toContain("write PR.md and explicitly report that no pull request was created");
    expect(promptOf("jira")).toContain("prepare jira-update.md and explicitly report that Jira was not changed");
  });

  test("the README covers what someone has to set up, and promises nothing", () => {
    const readme = EXAMPLE_ASSETS["README.md"];
    for (const needed of [
      "shared/ticket.md",
      "fabro repo init",
      "fabro model list",
      ".fabro/checks/test.sh",
      ".fabro/checks/security.sh",
      "[run.integrations.github.permissions]",
      "Nothing is ever merged automatically",
      "does not promise the run succeeds",
    ]) {
      expect(readme).toContain(needed);
    }
  });
});

describe("the project tests step", () => {
  test("a passing project moves on, and the report says the check isn't finished", () => {
    initProject();
    bunProject({ test: "echo all good" });
    const result = run(TESTS());
    expect(result.status).toBe(0);
    expect(read("testing-report.md").split("\n")[0]).toBe("Status: INCOMPLETE");
    expect(result.output).toContain("all good");
  });

  test("a failing test exits non-zero and leaves blocking findings with the output", () => {
    initProject();
    bunProject({ test: "echo boom-from-test && exit 3" });
    const result = run(TESTS());
    expect(result.status).not.toBe(0);
    const report = read("testing-report.md");
    expect(report.split("\n")[0]).toBe("Status: FAIL");
    expect(report).toContain("- [blocking] bun run test failed");
    expect(report).toContain("boom-from-test");
  });

  test("it runs every check it finds, so a fix can see all of them", () => {
    initProject();
    bunProject({ lint: "exit 1", typecheck: "echo types-fine", test: "exit 1" });
    const result = run(TESTS());
    expect(result.status).not.toBe(0);
    const report = read("testing-report.md");
    expect(report).toContain("- [blocking] bun run lint failed");
    expect(report).toContain("- [blocking] bun run test failed");
    expect(report).toContain("types-fine");
    expect(report).not.toContain("bun run typecheck failed");
  });

  test("a project with no test command can't pass by default", () => {
    initProject();
    const result = run(TESTS());
    expect(result.status).not.toBe(0);
    expect(read("testing-report.md").split("\n")[0]).toBe("Status: BLOCKED");
  });

  test("a package.json without a test script can't pass on lint alone", () => {
    initProject();
    bunProject({ lint: "echo fine" });
    const result = run(TESTS());
    expect(result.status).not.toBe(0);
    expect(read("testing-report.md").split("\n")[0]).toBe("Status: BLOCKED");
  });

  test("a missing tool says BLOCKED, so remediation doesn't try to fix it in code", () => {
    if (existsSync("/usr/bin/pnpm") || existsSync("/bin/pnpm")) return;
    initProject();
    write("package.json", '{"scripts": {"test": "x"}}\n');
    write("pnpm-lock.yaml", "");
    const result = spawnSync("bash", ["-c", TESTS()], {
      cwd: project,
      encoding: "utf8",
      env: { ...process.env, PATH: "/usr/bin:/bin" },
    });
    expect(result.status).not.toBe(0);
    const report = read("testing-report.md");
    expect(report.split("\n")[0]).toBe("Status: BLOCKED");
    expect(report).toContain("pnpm is not installed");
  });

  test("a project's own script replaces detection, pass or fail", () => {
    initProject();
    bunProject({ test: "echo detection-should-not-run" });
    write(".fabro/checks/test.sh", "echo from-my-script\nexit 7\n");
    const failing = run(TESTS());
    expect(failing.status).not.toBe(0);
    expect(read("testing-report.md")).toContain("from-my-script");
    expect(read("testing-report.md")).not.toContain("detection-should-not-run");
    write(".fabro/checks/test.sh", "echo from-my-script\n");
    expect(run(TESTS()).status).toBe(0);
  });

  test("a stale report from an earlier loop can't survive", () => {
    initProject();
    write("testing-report.md", "Status: PASS\n");
    const result = run(TESTS());
    expect(result.status).not.toBe(0);
    expect(read("testing-report.md").split("\n")[0]).toBe("Status: BLOCKED");
  });
});

describe("the security scans step", () => {
  const FAKE_KEY = () => `${"AKIA"}${"ABCDEFGHIJKLMNOP"}`;

  function commitChange(file: string, content: string): void {
    write(file, content);
    git("add", ".");
    git("commit", "-q", "-m", "change");
  }

  test("without a known base commit it can't pass", () => {
    git("init", "-q");
    write("plan.md", "No base here.\n");
    const result = run(SCANS());
    expect(result.status).not.toBe(0);
    expect(read("security-report.md").split("\n")[0]).toBe("Status: BLOCKED");
  });

  test("a clean change moves on, and the report says the check isn't finished", () => {
    initProject();
    commitChange("src.txt", "second\n");
    const result = run(SCANS());
    expect(result.status).toBe(0);
    expect(read("security-report.md").split("\n")[0]).toBe("Status: INCOMPLETE");
  });

  test("a secret added since the base fails, and the report never repeats it", () => {
    initProject();
    commitChange("config.txt", `key=${FAKE_KEY()}\n`);
    const result = run(SCANS());
    expect(result.status).not.toBe(0);
    const report = read("security-report.md");
    expect(report.split("\n")[0]).toBe("Status: FAIL");
    expect(report).toContain("[blocking]");
    expect(report).not.toContain(FAKE_KEY());
  });

  test("a secret not committed yet is still caught", () => {
    initProject();
    write("new-file.txt", `key=${FAKE_KEY()}\n`);
    const result = run(SCANS());
    // gitleaks, when installed, reads commits only; the pattern check covers the working tree.
    if (spawnSync("bash", ["-c", "command -v gitleaks"]).status === 0) return;
    expect(result.status).not.toBe(0);
    expect(read("security-report.md")).toContain("- [blocking] A likely secret was added in new-file.txt");
  });

  test("a failing audit is a blocking finding", () => {
    initProject();
    commitChange("src.txt", "second\n");
    write(".fabro/checks/security.sh", "echo vulnerable-package\nexit 1\n");
    const result = run(SCANS());
    expect(result.status).not.toBe(0);
    const report = read("security-report.md");
    expect(report.split("\n")[0]).toBe("Status: FAIL");
    expect(report).toContain("- [blocking] Dependency audit failed");
    expect(report).toContain("vulnerable-package");
  });
});

describe("the code review step", () => {
  test("with nothing changed there is nothing to deliver, so it fails", () => {
    initProject();
    const result = run(COLLECT());
    expect(result.status).not.toBe(0);
    const report = read("code-review-report.md");
    expect(report.split("\n")[0]).toBe("Status: FAIL");
    expect(report).toContain("[blocking] Nothing changed");
  });

  test("changes to the workflow's own notes don't count as a change", () => {
    initProject();
    write("remediation.md", "Notes.\n");
    write("testing-report.md", "Status: PASS\n");
    expect(run(COLLECT()).status).not.toBe(0);
  });

  test("a real change moves on, committed or not", () => {
    initProject();
    write("src.txt", "second\n");
    expect(run(COLLECT()).status).toBe(0);
    expect(read("code-review-report.md").split("\n")[0]).toBe("Status: INCOMPLETE");
  });

  test("without a known base commit it can't pass", () => {
    git("init", "-q");
    const result = run(COLLECT());
    expect(result.status).not.toBe(0);
    expect(read("code-review-report.md").split("\n")[0]).toBe("Status: BLOCKED");
  });
});

describe("the gates", () => {
  const gates: [string, string, string][] = [
    ["shared/testing.fabro", "testing_gate", "testing-report.md"],
    ["shared/security.fabro", "security_gate", "security-report.md"],
    ["shared/code-review.fabro", "review_gate", "code-review-report.md"],
  ];

  for (const [path, id, report] of gates) {
    describe(id, () => {
      const gate = () => scriptOf(path, id);

      test("pass only on a clean PASS", () => {
        write(report, "Status: PASS\n\n- [advisory] Could be tidier.\n");
        expect(run(gate()).status).toBe(0);
      });

      test.each([
        ["a failing report", "Status: FAIL\n- [blocking] Broken.\n"],
        ["a blocked report", "Status: BLOCKED\n"],
        ["a check that never finished", "Status: INCOMPLETE\n"],
        ["a PASS with a blocking finding", "Status: PASS\n- [blocking] Oops.\n"],
        ["prose claiming success", "Everything looks great! Status: PASS\n"],
        ["a status that isn't on the first line", "Notes\nStatus: PASS\n"],
        ["an empty report", ""],
      ])("block %s", (_name, content) => {
        write(report, content);
        expect(run(gate()).status).not.toBe(0);
      });

      test("block a report that isn't there", () => {
        expect(run(gate()).status).not.toBe(0);
      });
    });
  }
});
