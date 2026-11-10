import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { strToU8, unzipSync, zipSync, strFromU8 } from "fflate";

import { exportDraft, GENERATED_WORKFLOW_TOML, newDraft } from "../../lib/workflow-builder/model";
import type { WorkflowDraft } from "../../lib/workflow-builder/types";
import { MAX_ATTACHMENT_BYTES, attachmentPathProblem, freeAttachmentPath, readTextFile, sanitizeFileName } from "./attachments";
import { builderJson, bundleProblem, importWorkflowFile } from "./bundle";

function zipFile(name: string, files: Record<string, string>): File {
  const entries: Record<string, Uint8Array> = {};
  for (const [path, content] of Object.entries(files)) entries[path] = strToU8(content);
  return new File([zipSync(entries)], name, { type: "application/zip" });
}

describe("attachments", () => {
  test("reads a text file", async () => {
    expect(await readTextFile(new File(["hello"], "a.md"))).toBe("hello");
  });

  test("refuses a file too big for browser storage, with a reason", async () => {
    const big = new File([new Uint8Array(MAX_ATTACHMENT_BYTES + 1)], "big.md");
    await expect(readTextFile(big)).rejects.toThrow("big.md");
  });

  test("refuses a binary file, with a reason", async () => {
    await expect(readTextFile(new File([new Uint8Array([104, 0, 105])], "image.png"))).rejects.toThrow("binary");
  });

  test("keeps attached paths inside the workflow folder", () => {
    expect(attachmentPathProblem("prompts/a.md")).toBeNull();
    expect(attachmentPathProblem("../a.md")).toContain("climbs out");
    expect(attachmentPathProblem("/etc/passwd")).toContain("absolute");
    expect(attachmentPathProblem("~/a.md")).toContain("home directory");
    expect(attachmentPathProblem("workflow.fabro")).toContain("reserved");
  });

  test("a new upload never replaces a different file", () => {
    const assets = { "prompts/a.md": "first" };
    expect(freeAttachmentPath(assets, "prompts/a.md", "first")).toBe("prompts/a.md");
    expect(freeAttachmentPath(assets, "prompts/a.md", "second")).toBe("prompts/a-2.md");
    expect(freeAttachmentPath({ ...assets, "prompts/a-2.md": "x" }, "prompts/a.md", "second")).toBe("prompts/a-3.md");
  });

  test("cleans uploaded file names", () => {
    expect(sanitizeFileName("../../my prompt.md")).toBe("my-prompt.md");
    expect(sanitizeFileName("...")).toBe("file.txt");
  });
});

describe("importing", () => {
  test("a .fabro file opens in a new draft", async () => {
    const source = "digraph A {\n  start [shape=Mdiamond]\n}\n";
    const { draft } = await importWorkflowFile(new File([source], "my-flow.fabro"));
    expect(draft.source).toBe(source);
    expect(draft.name).toBe("my-flow");
  });

  test("invalid code imports intact, so it can be fixed", async () => {
    const source = "digraph Broken {\n  start [";
    const { draft } = await importWorkflowFile(new File([source], "broken.fabro"));
    expect(draft.source).toBe(source);
  });

  test("builder JSON round-trips everything, under a new ID", async () => {
    const original: WorkflowDraft = {
      ...newDraft("Round trip"),
      assets: { "prompts/a.md": "body" },
      positions: { start: { x: 4, y: 8 }, agent: { x: 12, y: 16 }, exit: { x: 20, y: 24 } },
    };
    const { draft } = await importWorkflowFile(new File([builderJson(original)], "x.builder.json"));
    expect(draft.id).not.toBe(original.id);
    expect(draft.name).toBe("Round trip");
    expect(draft.source).toBe(original.source);
    expect(draft.positions).toEqual(original.positions);
    expect(draft.assets).toEqual(original.assets);
    expect(draft.promptParts).toEqual(original.promptParts);
  });

  test("JSON with no source is refused, naming what to do instead", async () => {
    await expect(importWorkflowFile(new File(['{"name":"x"}'], "x.json"))).rejects.toThrow("ZIP");
  });

  test("broken JSON is refused, not half-imported", async () => {
    await expect(importWorkflowFile(new File(["{nope"], "x.json"))).rejects.toThrow("not valid JSON");
  });

  test("a bundle made by export comes back whole", async () => {
    const original: WorkflowDraft = {
      ...newDraft("Bundle"),
      assets: { "prompts/extra.md": "Extra body" },
      positions: { start: { x: 4, y: 8 }, agent: { x: 12, y: 16 }, exit: { x: 20, y: 24 } },
    };
    const exported = exportDraft(original);
    expect(exported.blocked).toBe(false);
    const { draft, notes } = await importWorkflowFile(zipFile("bundle.zip", exported.files));

    expect(draft.source).toBe(original.source);
    expect(draft.name).toBe("Bundle");
    expect(draft.positions).toEqual(original.positions);
    expect(draft.promptParts).toEqual(original.promptParts);
    expect(draft.assets["prompts/extra.md"]).toBe("Extra body");
    expect(draft.assets["workflow.toml"]).toBeUndefined();
    expect(draft.id).not.toBe(original.id);
    expect(notes).toEqual([]);
  });

  test("a ZIP without builder data still opens, and says what it did", async () => {
    const { draft, notes } = await importWorkflowFile(
      zipFile("plain.zip", { "workflow.fabro": "digraph A {\n}\n", "prompts/a.md": "body" }),
    );
    expect(draft.assets["prompts/a.md"]).toBe("body");
    expect(notes.join(" ")).toContain("laid out automatically");
  });

  test("files in a single top folder are unwrapped", async () => {
    const { draft } = await importWorkflowFile(
      zipFile("wrapped.zip", { "flow/workflow.fabro": "digraph A {\n}\n", "flow/prompts/a.md": "body" }),
    );
    expect(draft.source).toBe("digraph A {\n}\n");
    expect(draft.assets["prompts/a.md"]).toBe("body");
  });

  test("nothing is dropped silently: unsafe, binary and oversized files are named", async () => {
    const entries: Record<string, Uint8Array> = {
      "workflow.fabro": strToU8("digraph A {\n}\n"),
      "../evil.md": strToU8("nope"),
      "image.bin": new Uint8Array([1, 0, 2]),
      "huge.md": new Uint8Array(MAX_ATTACHMENT_BYTES + 1).fill(97),
    };
    const { draft, notes } = await importWorkflowFile(new File([zipSync(entries)], "mixed.zip"));
    const text = notes.join(" ");
    expect(text).toContain("../evil.md");
    expect(text).toContain("image.bin");
    expect(text).toContain("huge.md");
    expect(Object.keys(draft.assets)).toEqual([]);
  });

  describe("workflow.toml", () => {
    const WORKFLOW = "digraph A {\n}\n";

    test("the builder's own file adds nothing, and needs no note", async () => {
      const { draft, notes } = await importWorkflowFile(
        zipFile("own.zip", { "workflow.fabro": WORKFLOW, "workflow.toml": GENERATED_WORKFLOW_TOML, "workflow.builder.json": "{}" }),
      );
      expect(notes).toEqual([]);
      expect(draft.assets["workflow.toml"]).toBeUndefined();
      expect(exportDraft(draft).files["workflow.toml"]).toBe(GENERATED_WORKFLOW_TOML);
    });

    test("edits that leave the generated header in place are kept, byte for byte", async () => {
      const customized = `${GENERATED_WORKFLOW_TOML}\n[run.integrations.github.permissions]\npull_requests = "write"\n`;
      expect(customized).toContain("Exported by the visual builder");
      const { draft, notes } = await importWorkflowFile(
        zipFile("edited.zip", { "workflow.fabro": WORKFLOW, "workflow.toml": customized }),
      );
      expect(notes.join(" ")).toContain("workflow.toml has custom settings");
      const exported = exportDraft(draft);
      expect(exported.blocked).toBe(false);
      expect(exported.files["workflow.toml"]).toBe(customized);
      expect(exported.diagnostics.map((item) => item.code)).toContain("builder.custom_workflow_toml");
    });

    test("it survives export and import again, and again", async () => {
      const customized = "_version = 1\n\n[run]\ngoal = \"Mine\"\n";
      let { draft } = await importWorkflowFile(zipFile("a.zip", { "workflow.fabro": WORKFLOW, "workflow.toml": customized }));
      for (let round = 0; round < 2; round++) {
        ({ draft } = await importWorkflowFile(zipFile("b.zip", exportDraft(draft).files)));
      }
      expect(exportDraft(draft).files["workflow.toml"]).toBe(customized);
    });

    test("removing the kept file goes back to the builder's own", async () => {
      const { draft } = await importWorkflowFile(
        zipFile("a.zip", { "workflow.fabro": WORKFLOW, "workflow.toml": "[run]\nmine = true\n" }),
      );
      const { "workflow.toml": _removed, ...assets } = draft.assets;
      expect(exportDraft({ ...draft, assets }).files["workflow.toml"]).toBe(GENERATED_WORKFLOW_TOML);
    });

    test("a graph under another name is pointed at the name it exports under, and nothing else changes", async () => {
      const toml = '_version = 1\n\n[workflow]\ngraph = "flow.fabro" # mine\n\n[run]\ngraph = "not-this-one"\n';
      const { draft, notes } = await importWorkflowFile(zipFile("a.zip", { "flow.fabro": WORKFLOW, "workflow.toml": toml }));
      expect(draft.source).toBe(WORKFLOW);
      expect(notes.join(" ")).toContain("its graph setting now says workflow.fabro");
      expect(exportDraft(draft).files["workflow.toml"]).toBe(
        '_version = 1\n\n[workflow]\ngraph = "workflow.fabro" # mine\n\n[run]\ngraph = "not-this-one"\n',
      );
    });

    describe("a hook script holding text that looks like the graph setting", () => {
      const HOOK = `\n[[run.hooks]]\nevent = "run_complete"\nscript = '''\ncat <<'END'\n[workflow]\ngraph = "example.fabro"\nEND\n'''\n`;
      const hookScript = (toml: string) => (Bun.TOML.parse(toml) as { run: { hooks: { script: string }[] } }).run.hooks[0].script;

      test("is left exactly alone when the graph is already right", async () => {
        const customized = `${GENERATED_WORKFLOW_TOML}${HOOK}`;
        const { draft, notes } = await importWorkflowFile(zipFile("a.zip", { "workflow.fabro": WORKFLOW, "workflow.toml": customized }));
        expect(exportDraft(draft).files["workflow.toml"]).toBe(customized);
        expect(notes.join(" ")).toContain("exported as written");
        expect(notes.join(" ")).not.toContain("graph setting");
      });

      test("is left exactly alone when the real setting changes, and only that changes", async () => {
        const customized = `${GENERATED_WORKFLOW_TOML.replace('graph = "workflow.fabro"', 'graph = "flow.fabro"')}${HOOK}`;
        const { draft, notes } = await importWorkflowFile(zipFile("a.zip", { "flow.fabro": WORKFLOW, "workflow.toml": customized }));
        const exported = exportDraft(draft).files["workflow.toml"];
        expect(exported).toBe(`${GENERATED_WORKFLOW_TOML}${HOOK}`);
        expect(hookScript(exported)).toBe(hookScript(customized));
        expect(hookScript(exported)).toContain('graph = "example.fabro"');
        expect(notes.join(" ")).toContain("its graph setting now says workflow.fabro");
        expect(notes.join(" ")).not.toContain("exported as written");
      });

      test("is left alone when the hook comes first, too", async () => {
        const customized = `_version = 1\n${HOOK}\n[workflow]\ngraph = "flow.fabro"\n`;
        const { draft } = await importWorkflowFile(zipFile("a.zip", { "flow.fabro": WORKFLOW, "workflow.toml": customized }));
        expect(exportDraft(draft).files["workflow.toml"]).toBe(`_version = 1\n${HOOK}\n[workflow]\ngraph = "workflow.fabro"\n`);
      });

      test("isn't taken for the setting when there's no real one", async () => {
        const customized = `_version = 1\n${HOOK}`;
        const { draft, notes } = await importWorkflowFile(zipFile("a.zip", { "flow.fabro": WORKFLOW, "workflow.toml": customized }));
        expect(exportDraft(draft).files["workflow.toml"]).toBe(customized);
        expect(notes.join(" ")).not.toContain("graph setting now says");
      });
    });

    test("a setting naming a different workflow is kept, and the mismatch is said out loud", async () => {
      const toml = '[workflow]\ngraph = "other.fabro"\n';
      for (const opened of ["workflow.fabro", "flow.fabro"]) {
        const { draft, notes } = await importWorkflowFile(
          zipFile("a.zip", { [opened]: WORKFLOW, "other.fabro": WORKFLOW, "workflow.toml": toml }),
        );
        expect(exportDraft(draft).files["workflow.toml"]).toBe(toml);
        expect(notes.join(" ")).toContain(`runs other.fabro, but this import opened ${opened}`);
      }
    });

    test("a setting in a form it won't change is kept, with the fix left to the author", async () => {
      const toml = 'workflow = { graph = "flow.fabro" }\n';
      const { draft, notes } = await importWorkflowFile(zipFile("a.zip", { "flow.fabro": WORKFLOW, "workflow.toml": toml }));
      expect(exportDraft(draft).files["workflow.toml"]).toBe(toml);
      expect(notes.join(" ")).toContain("mentions flow.fabro");
    });

    test("./flow.fabro is the same setting, and a setting that's already right changes nothing", async () => {
      const dotted = await importWorkflowFile(zipFile("a.zip", { "flow.fabro": WORKFLOW, "workflow.toml": '[workflow]\ngraph = "./flow.fabro"\n' }));
      expect(exportDraft(dotted.draft).files["workflow.toml"]).toBe('[workflow]\ngraph = "workflow.fabro"\n');
      const right = '[workflow]\ngraph = "./workflow.fabro"\n';
      const kept = await importWorkflowFile(zipFile("a.zip", { "workflow.fabro": WORKFLOW, "workflow.toml": right }));
      expect(exportDraft(kept.draft).files["workflow.toml"]).toBe(right);
      expect(kept.notes.join(" ")).not.toContain("Check which one");
    });

    test("builder metadata can't vouch for a file: only its bytes count", async () => {
      const customized = `${GENERATED_WORKFLOW_TOML}\n[run]\nmine = true\n`;
      const metadata = JSON.stringify({ generated: true, trusted: true, workflowToml: "generated" });
      const { draft } = await importWorkflowFile(
        zipFile("a.zip", { "workflow.fabro": WORKFLOW, "workflow.toml": customized, "workflow.builder.json": metadata }),
      );
      expect(exportDraft(draft).files["workflow.toml"]).toBe(customized);
    });

    test("a builder JSON holding its own toml keeps it and says so", async () => {
      const original: WorkflowDraft = { ...newDraft("Json"), assets: { "workflow.toml": "[run]\nmine = true\n" } };
      const { draft, notes } = await importWorkflowFile(new File([builderJson(original)], "a.builder.json"));
      expect(notes.join(" ")).toContain("workflow.toml");
      expect(exportDraft(draft).files["workflow.toml"]).toBe("[run]\nmine = true\n");
    });

    test("it still can't be attached by hand", () => {
      expect(attachmentPathProblem("workflow.toml")).toContain("reserved");
    });
  });

  test("what macOS adds to an archive is dropped without a word, flat or under one top folder", async () => {
    const binary = "\0\u0005\u0016\u0007";
    for (const prefix of ["", "bundle/"]) {
      const { draft, notes } = await importWorkflowFile(
        zipFile("mac.zip", {
          [`${prefix}workflow.fabro`]: "digraph A {\n}\n",
          [`${prefix}workflow.builder.json`]: "{}",
          [`${prefix}notes.md`]: "Kept",
          [`${prefix}._workflow.fabro`]: binary,
          [`${prefix}.DS_Store`]: binary,
          [`__MACOSX/${prefix}._notes.md`]: binary,
        }),
      );
      expect(draft.assets).toEqual({ "notes.md": "Kept" });
      expect(notes).toEqual([]);
    }
  });

  describe("two spellings of one path", () => {
    const WORKFLOW = "digraph A {\n}\n";

    test("in a ZIP with different content, the import is refused rather than losing one", async () => {
      await expect(
        importWorkflowFile(
          zipFile("alias.zip", { "workflow.fabro": WORKFLOW, "./prompts/a.md": "one", "prompts/a.md": "two" }),
        ),
      ).rejects.toThrow('"./prompts/a.md" and "prompts/a.md" name the same file');
    });

    test("in a ZIP with the same content, they are one file", async () => {
      const { draft } = await importWorkflowFile(
        zipFile("alias.zip", { "workflow.fabro": WORKFLOW, "./prompts/a.md": "same", "prompts/a.md": "same" }),
      );
      expect(draft.assets).toEqual({ "prompts/a.md": "same" });
    });

    test("in a builder JSON with different content, the import is refused", async () => {
      const original: WorkflowDraft = { ...newDraft("Json"), assets: { "./a.md": "one", "a.md": "two" } };
      await expect(importWorkflowFile(new File([builderJson(original)], "a.builder.json"))).rejects.toThrow(
        "name the same file",
      );
    });

    test("in a builder JSON with the same content, or just one spelling, they are canonical", async () => {
      const both: WorkflowDraft = { ...newDraft("Json"), assets: { "./a.md": "same", "a.md": "same", "./b.md": "alone" } };
      const { draft } = await importWorkflowFile(new File([builderJson(both)], "a.builder.json"));
      expect(draft.assets).toEqual({ "a.md": "same", "b.md": "alone" });
    });
  });

  test("a ZIP with no workflow is refused", async () => {
    await expect(importWorkflowFile(zipFile("empty.zip", { "notes.md": "x" }))).rejects.toThrow("workflow.fabro");
  });

  test("a file that isn't a ZIP is refused", async () => {
    await expect(importWorkflowFile(new File(["not a zip"], "fake.zip"))).rejects.toThrow("not a readable ZIP");
  });
});

describe("exporting", () => {
  test("the bundle holds the workflow, its toml, builder metadata and attached files", () => {
    const draft: WorkflowDraft = { ...newDraft("Export"), assets: { "prompts/a.md": "body" } };
    const { files } = exportDraft(draft);
    const zipped = zipSync(Object.fromEntries(Object.entries(files).map(([path, text]) => [path, strToU8(text)])));
    const unpacked = unzipSync(zipped);
    expect(Object.keys(unpacked).sort()).toEqual(["prompts/a.md", "workflow.builder.json", "workflow.fabro", "workflow.toml"]);
    expect(strFromU8(unpacked["prompts/a.md"])).toBe("body");
  });

  test("invalid code can't be bundled, and the reason is available to show", () => {
    expect(bundleProblem({ ...newDraft(), source: "digraph {" })).toBeTruthy();
    expect(bundleProblem(newDraft())).toBeNull();
  });
});

describe("the example", () => {
  const backing = new Map<string, string>();
  const previous = globalThis.localStorage;

  beforeEach(() => {
    backing.clear();
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      value: {
        getItem: (key: string) => backing.get(key) ?? null,
        setItem: (key: string, value: string) => void backing.set(key, value),
        removeItem: (key: string) => void backing.delete(key),
        key: (index: number) => [...backing.keys()][index] ?? null,
        get length() {
          return backing.size;
        },
      },
    });
  });

  afterEach(() => {
    Object.defineProperty(globalThis, "localStorage", { configurable: true, value: previous });
  });

  test("always opens as a new draft, leaving the others alone", async () => {
    const { createExampleDraft } = await import("./bundle");
    const first = createExampleDraft();
    const second = createExampleDraft();
    expect(first.id).not.toBe(second.id);
    expect(first.source).toContain("FrontendComponentChanges");
    expect(backing.size).toBe(2);
    expect(first.name).toBe("Tutorial Workflow");
    expect(first.promptParts.analyze[0].id).not.toBe(second.promptParts.analyze[0].id);
  });

  test("opens with everything it needs, and nothing to fix", async () => {
    const { createExampleDraft } = await import("./bundle");
    const draft = createExampleDraft();
    expect(Object.keys(draft.assets).sort()).toEqual([
      "README.md",
      "shared/code-review.fabro",
      "shared/security.fabro",
      "shared/testing.fabro",
      "shared/ticket.md",
    ]);
    const exported = exportDraft(draft);
    expect(exported).toMatchObject({ blocked: false, diagnostics: [] });
    expect(exported.files["workflow.fabro"]).toBe(draft.source);
  });

  test("editing one copy of its files leaves the sample and other copies alone", async () => {
    const { createExampleDraft } = await import("./bundle");
    const first = createExampleDraft();
    first.assets["shared/ticket.md"] = "edited";
    expect(createExampleDraft().assets["shared/ticket.md"]).not.toBe("edited");
  });

  test("survives export and import as a ZIP, with new ids", async () => {
    const { createExampleDraft, importWorkflowFile } = await import("./bundle");
    const original = createExampleDraft();
    const { files } = exportDraft(original);
    const { draft, notes } = await importWorkflowFile(zipFile("sample.zip", files));
    expect(notes).toEqual([]);
    expect(draft.id).not.toBe(original.id);
    expect(draft.source).toBe(original.source);
    expect(draft.assets).toEqual(original.assets);
    expect(draft.promptParts).toEqual(original.promptParts);
    expect(exportDraft(draft)).toMatchObject({ blocked: false, diagnostics: [] });
  });
});
