import { useMemo, useState } from "react";
import { ChevronRightIcon } from "@heroicons/react/20/solid";
import type { FileContents } from "@pierre/diffs";
import { File } from "@pierre/diffs/react";

import { PIERRE_THEMES } from "../lib/pierre-theme";
import { useAppearance } from "./appearance-provider";

export function CollapsibleFile({
  file,
  defaultOpen = true,
}: {
  file: FileContents;
  defaultOpen?: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const { appearance } = useAppearance();
  const options = useMemo(
    () => ({
      theme: PIERRE_THEMES,
      themeType: appearance.mode,
      disableFileHeader: true,
    }),
    [appearance.mode],
  );

  const lines = file.contents.split("\n");
  const lineCount = lines.length;
  const loc = lines.filter((l) => l.trim().length > 0).length;

  return (
    <div className="rounded-md border border-line bg-panel/50 overflow-hidden">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2 px-4 py-2.5 text-left hover:bg-overlay transition-colors"
      >
        <ChevronRightIcon
          className={`size-4 text-fg-muted transition-transform duration-150 ${open ? "rotate-90" : ""}`}
        />
        <span className="font-mono text-xs text-fg-muted">{file.name}</span>
        <span className="ml-auto font-mono text-xs text-fg-muted">
          {lineCount} lines ({loc} loc)
        </span>
      </button>

      <div className={open ? "" : "hidden"}>
        <div className="border-t border-line" />
        <File file={file} options={options} />
      </div>
    </div>
  );
}
