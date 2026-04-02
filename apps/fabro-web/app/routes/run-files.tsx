import { useCallback, useEffect, useRef, useState } from "react";
import {
  MultiFileDiff,
  type AnnotationSide,
  type DiffLineAnnotation,
} from "@pierre/diffs/react";
import { useTheme } from "../lib/theme";
import { apiJson } from "../api";
import type { PaginatedRunFileList } from "@qltysh/fabro-api-client";

export const handle = { wide: true };

export async function loader({ request, params }: any) {
  const data = await apiJson<PaginatedRunFileList>(`/runs/${params.id}/files`, { request });
  return data;
}

const fallbackFiles = [
  {
    oldFile: {
      name: "src/commands/run.ts",
      contents: `import { parseArgs } from "node:util";
import { loadConfig } from "../config.js";
import { execute } from "../executor.js";

interface RunOptions {
  config: string;
  dryRun: boolean;
}

export async function run(argv: string[]) {
  const { values } = parseArgs({
    args: argv,
    options: {
      config: { type: "string", short: "c", default: "fabro.toml" },
      "dry-run": { type: "boolean", default: false },
    },
  });

  const opts: RunOptions = {
    config: values.config ?? "fabro.toml",
    dryRun: values["dry-run"] ?? false,
  };

  const config = await loadConfig(opts.config);
  const result = await execute(config, { dryRun: opts.dryRun });

  if (result.success) {
    console.log("Run completed successfully.");
  } else {
    console.error("Run failed:", result.error);
    process.exitCode = 1;
  }
}
`,
    },
    newFile: {
      name: "src/commands/run.ts",
      contents: `import { parseArgs } from "node:util";
import { loadConfig } from "../config.js";
import { execute } from "../executor.js";
import { createLogger, type Logger } from "../logger.js";

interface RunOptions {
  config: string;
  dryRun: boolean;
  verbose: boolean;
}

export async function run(argv: string[]) {
  const { values } = parseArgs({
    args: argv,
    options: {
      config: { type: "string", short: "c", default: "fabro.toml" },
      "dry-run": { type: "boolean", default: false },
      verbose: { type: "boolean", short: "v", default: false },
    },
  });

  const opts: RunOptions = {
    config: values.config ?? "fabro.toml",
    dryRun: values["dry-run"] ?? false,
    verbose: values.verbose ?? false,
  };

  const logger: Logger = createLogger({ verbose: opts.verbose });

  const config = await loadConfig(opts.config);
  logger.debug("Loaded config from %s", opts.config);

  const result = await execute(config, { dryRun: opts.dryRun, logger });
  logger.debug("Execution finished in %dms", result.elapsed);

  if (result.success) {
    console.log("Run completed successfully.");
  } else {
    console.error("Run failed:", result.error);
    process.exitCode = 1;
  }
}
`,
    },
  },
  {
    oldFile: {
      name: "src/logger.ts",
      contents: "",
    },
    newFile: {
      name: "src/logger.ts",
      contents: `export interface Logger {
  info(message: string, ...args: unknown[]): void;
  debug(message: string, ...args: unknown[]): void;
  error(message: string, ...args: unknown[]): void;
}

interface LoggerOptions {
  verbose: boolean;
}

export function createLogger({ verbose }: LoggerOptions): Logger {
  return {
    info(message, ...args) {
      console.log(message, ...args);
    },
    debug(message, ...args) {
      if (verbose) {
        console.log("[debug]", message, ...args);
      }
    },
    error(message, ...args) {
      console.error(message, ...args);
    },
  };
}
`,
    },
  },
  {
    oldFile: {
      name: "src/executor.ts",
      contents: `import type { Config } from "./config.js";

interface ExecuteOptions {
  dryRun: boolean;
}

interface ExecuteResult {
  success: boolean;
  error?: string;
}

export async function execute(
  config: Config,
  options: ExecuteOptions,
): Promise<ExecuteResult> {
  if (options.dryRun) {
    console.log("Dry run — skipping execution.");
    return { success: true };
  }

  try {
    for (const step of config.steps) {
      await step.run();
    }
    return { success: true };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return { success: false, error: message };
  }
}
`,
    },
    newFile: {
      name: "src/executor.ts",
      contents: `import type { Config } from "./config.js";
import type { Logger } from "./logger.js";

interface ExecuteOptions {
  dryRun: boolean;
  logger: Logger;
}

interface ExecuteResult {
  success: boolean;
  elapsed: number;
  error?: string;
}

export async function execute(
  config: Config,
  options: ExecuteOptions,
): Promise<ExecuteResult> {
  const start = performance.now();

  if (options.dryRun) {
    options.logger.info("Dry run — skipping execution.");
    return { success: true, elapsed: performance.now() - start };
  }

  try {
    for (const step of config.steps) {
      options.logger.debug("Running step: %s", step.name);
      await step.run();
    }
    return { success: true, elapsed: performance.now() - start };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return { success: false, elapsed: performance.now() - start, error: message };
  }
}
`,
    },
  },
];

interface SteerAnnotation {
  fileName: string;
  lineNumber: number;
  side: AnnotationSide;
}

function steerKey(fileName: string, side: AnnotationSide, lineNumber: number) {
  return `${fileName}:${side}:${lineNumber}`;
}

function DiffHeaderToggles({
  diffStyle,
  onDiffStyleChange,
  disableBackground,
  onDisableBackgroundChange,
}: {
  diffStyle: "split" | "unified";
  onDiffStyleChange: (style: "split" | "unified") => void;
  disableBackground: boolean;
  onDisableBackgroundChange: (disabled: boolean) => void;
}) {
  return (
    <div className="flex items-center gap-1">
      <button
        type="button"
        onClick={() => onDiffStyleChange(diffStyle === "split" ? "unified" : "split")}
        title={diffStyle === "split" ? "Switch to unified" : "Switch to split"}
        aria-label={diffStyle === "split" ? "Switch to unified" : "Switch to split"}
        className="p-1 opacity-60 hover:opacity-100"
      >
        <svg xmlns="http://www.w3.org/2000/svg" fill="currentColor" viewBox="0 0 16 16" width="16" height="16">
          <path d="M14 0H8.5v16H14a2 2 0 0 0 2-2V2a2 2 0 0 0-2-2m-1.5 6.5v1h1a.5.5 0 0 1 0 1h-1v1a.5.5 0 0 1-1 0v-1h-1a.5.5 0 0 1 0-1h1v-1a.5.5 0 0 1 1 0" />
          <path d="M2 0a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h5.5V0zm.5 7.5h3a.5.5 0 0 1 0 1h-3a.5.5 0 0 1 0-1" opacity="0.3" />
        </svg>
      </button>
      <button
        type="button"
        onClick={() => onDisableBackgroundChange(!disableBackground)}
        title={disableBackground ? "Enable background" : "Disable background"}
        aria-label={disableBackground ? "Enable background" : "Disable background"}
        className="p-1 opacity-60 hover:opacity-100"
      >
        <svg xmlns="http://www.w3.org/2000/svg" fill="currentColor" viewBox="0 0 16 16" width="16" height="16">
          <path d="M0 2.25a.75.75 0 0 1 .75-.75h10.5a.75.75 0 0 1 0 1.5H.75A.75.75 0 0 1 0 2.25" opacity="0.4" />
          <path fillRule="evenodd" d="M15 5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H1a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1zM2.5 9a.5.5 0 0 0 0 1h8a.5.5 0 0 0 0-1zm0-2a.5.5 0 0 0 0 1h11a.5.5 0 0 0 0-1z" clipRule="evenodd" />
          <path d="M0 14.75A.75.75 0 0 1 .75 14h5.5a.75.75 0 0 1 0 1.5H.75a.75.75 0 0 1-.75-.75" opacity="0.4" />
        </svg>
      </button>
    </div>
  );
}

function DiffWithSteer({
  oldFile,
  newFile,
  openSteers,
  submittedSteers,
  onSteer,
  onSubmit,
  onCancel,
}: {
  oldFile: { name: string; contents: string };
  newFile: { name: string; contents: string };
  openSteers: Map<string, SteerAnnotation>;
  submittedSteers: Map<string, SteerAnnotation & { text: string }>;
  onSteer: (annotation: SteerAnnotation) => void;
  onSubmit: (annotation: SteerAnnotation, text: string) => void;
  onCancel: (annotation: SteerAnnotation) => void;
}) {
  const [diffStyle, setDiffStyle] = useState<"split" | "unified">("split");
  const [disableBackground, setDisableBackground] = useState(false);
  const { theme } = useTheme();

  const containerRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLDivElement>(null);
  const hoveredRef = useRef<{ lineNumber: number; side: AnnotationSide } | null>(null);
  const leaveTimeoutRef = useRef<number>(0);

  const showButton = useCallback((lineElement: HTMLElement, lineNumber: number, side: AnnotationSide) => {
    clearTimeout(leaveTimeoutRef.current);
    hoveredRef.current = { lineNumber, side };

    const btn = buttonRef.current;
    const container = containerRef.current;
    if (!btn || !container) return;

    const containerRect = container.getBoundingClientRect();
    const lineRect = lineElement.getBoundingClientRect();

    btn.style.top = `${lineRect.top - containerRect.top}px`;
    btn.style.height = `${lineRect.height}px`;
    btn.style.display = "flex";
  }, []);

  const hideButton = useCallback(() => {
    leaveTimeoutRef.current = window.setTimeout(() => {
      hoveredRef.current = null;
      if (buttonRef.current) {
        buttonRef.current.style.display = "none";
      }
    }, 100);
  }, []);

  return (
    <div ref={containerRef} className="relative rounded-md overflow-hidden border border-line">
      <MultiFileDiff<SteerAnnotation & { text?: string }>
        oldFile={oldFile}
        newFile={newFile}
        options={{
          diffStyle,
          disableBackground,
          theme: theme === "dark" ? "pierre-dark" : "pierre-light",
          lineDiffType: "word",
          onLineEnter({ lineNumber, annotationSide, lineElement }) {
            showButton(lineElement, lineNumber, annotationSide);
          },
          onLineLeave() {
            hideButton();
          },
        }}
        renderHeaderMetadata={() => (
          <DiffHeaderToggles
            diffStyle={diffStyle}
            onDiffStyleChange={setDiffStyle}
            disableBackground={disableBackground}
            onDisableBackgroundChange={setDisableBackground}
          />
        )}
        lineAnnotations={buildAnnotationsForFile(
          newFile.name,
          openSteers,
          submittedSteers,
        )}
        renderAnnotation={(annotation) => {
          const meta = annotation.metadata;
          if ("text" in meta && meta.text != null) {
            return <SubmittedSteerComment text={meta.text} />;
          }
          return (
            <SteerCommentForm
              annotation={meta}
              onSubmit={onSubmit}
              onCancel={() => onCancel(meta)}
            />
          );
        }}
      />
      <div
        ref={buttonRef}
        style={{ display: "none" }}
        className="absolute right-2 z-10 items-center justify-end"
        onMouseEnter={() => clearTimeout(leaveTimeoutRef.current)}
        onMouseLeave={() => {
          hoveredRef.current = null;
          if (buttonRef.current) {
            buttonRef.current.style.display = "none";
          }
        }}
      >
        <button
          type="button"
          onClick={() => {
            const h = hoveredRef.current;
            if (h) {
              onSteer({
                fileName: newFile.name,
                lineNumber: h.lineNumber,
                side: h.side,
              });
            }
          }}
          className="flex items-center gap-1.5 rounded bg-teal-600 px-2 py-0.5 text-xs font-medium text-white shadow-sm transition-colors hover:bg-teal-500"
        >
          Steer
          <kbd className="rounded bg-teal-500/50 px-1 py-px font-sans text-[10px] text-teal-100">
            ⌘Y
          </kbd>
        </button>
      </div>
    </div>
  );
}

function SteerCommentForm({
  annotation,
  onSubmit,
  onCancel,
}: {
  annotation: SteerAnnotation;
  onSubmit: (annotation: SteerAnnotation, text: string) => void;
  onCancel: () => void;
}) {
  const [text, setText] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  return (
    <div className="flex flex-col gap-2 rounded-md border border-teal-500/30 bg-panel-alt/90 p-3">
      <textarea
        ref={textareaRef}
        value={text}
        onChange={(e) => setText(e.target.value)}
        placeholder="Add steering guidance..."
        rows={3}
        className="w-full resize-none rounded border border-line bg-panel/80 px-2 py-1.5 text-sm text-fg-2 placeholder:text-fg-muted outline-none focus:border-focus"
      />
      <div className="flex gap-2">
        <button
          type="button"
          onClick={() => onSubmit(annotation, text)}
          disabled={text.trim().length === 0}
          className="rounded bg-teal-600 px-3 py-1 text-xs font-medium text-white transition-colors hover:bg-teal-500 disabled:opacity-40 disabled:hover:bg-teal-600"
        >
          Submit
        </button>
        <button
          type="button"
          onClick={onCancel}
          className="rounded border border-line bg-panel/80 px-3 py-1 text-xs font-medium text-fg-3 transition-colors hover:bg-overlay"
        >
          Cancel
        </button>
      </div>
    </div>
  );
}

function SubmittedSteerComment({ text }: { text: string }) {
  return (
    <div className="rounded-md border border-teal-500/20 bg-teal-950/30 px-3 py-2 text-sm text-fg-2">
      {text}
    </div>
  );
}

function buildAnnotationsForFile(
  fileName: string,
  openSteers: Map<string, SteerAnnotation>,
  submittedSteers: Map<string, SteerAnnotation & { text: string }>,
): DiffLineAnnotation<SteerAnnotation & { text?: string }>[] {
  const annotations: DiffLineAnnotation<SteerAnnotation & { text?: string }>[] = [];

  for (const [, annotation] of openSteers) {
    if (annotation.fileName === fileName) {
      annotations.push({
        side: annotation.side,
        lineNumber: annotation.lineNumber,
        metadata: annotation,
      });
    }
  }

  for (const [, annotation] of submittedSteers) {
    if (annotation.fileName === fileName) {
      annotations.push({
        side: annotation.side,
        lineNumber: annotation.lineNumber,
        metadata: annotation,
      });
    }
  }

  return annotations;
}

export default function RunFiles({ loaderData }: any) {
  const runFiles = loaderData;
  const files = runFiles.data.length > 0
    ? runFiles.data.map((f) => ({
        oldFile: { name: f.old_file.name, contents: f.old_file.contents },
        newFile: { name: f.new_file.name, contents: f.new_file.contents },
      }))
    : fallbackFiles;
  const [openSteers, setOpenSteers] = useState(
    () => new Map<string, SteerAnnotation>(),
  );
  const [submittedSteers, setSubmittedSteers] = useState(
    () => new Map<string, SteerAnnotation & { text: string }>(),
  );

  function handleSteer(annotation: SteerAnnotation) {
    const key = steerKey(annotation.fileName, annotation.side, annotation.lineNumber);
    if (openSteers.has(key) || submittedSteers.has(key)) return;
    setOpenSteers((prev) => new Map(prev).set(key, annotation));
  }

  function handleSubmit(annotation: SteerAnnotation, text: string) {
    const key = steerKey(annotation.fileName, annotation.side, annotation.lineNumber);
    setOpenSteers((prev) => {
      const next = new Map(prev);
      next.delete(key);
      return next;
    });
    setSubmittedSteers((prev) =>
      new Map(prev).set(key, { ...annotation, text }),
    );
    console.log("Steer submitted", { ...annotation, text });
  }

  function handleCancel(annotation: SteerAnnotation) {
    const key = steerKey(annotation.fileName, annotation.side, annotation.lineNumber);
    setOpenSteers((prev) => {
      const next = new Map(prev);
      next.delete(key);
      return next;
    });
  }

  return (
    <div className="flex flex-col gap-4">
      {files.map(({ oldFile, newFile }) => (
        <DiffWithSteer
          key={newFile.name}
          oldFile={oldFile}
          newFile={newFile}
          openSteers={openSteers}
          submittedSteers={submittedSteers}
          onSteer={handleSteer}
          onSubmit={handleSubmit}
          onCancel={handleCancel}
        />
      ))}
    </div>
  );
}
