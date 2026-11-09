import { useEffect, useMemo, useRef, type KeyboardEvent, type RefObject } from "react";

import type { Diagnostic } from "../../lib/workflow-builder/types";
import { DiagnosticsList, diagnosticSummary } from "./diagnostics";

const LINE_HEIGHT_PX = 20;

export type CodeFocusRequest = { line: number; column?: number };

/**
 * Synchronizes the browser's focus and caret with a jump request from outside the
 * editor, such as picking a diagnostic in another panel. A new request object jumps
 * again. Nothing to clean up: it only moves the caret.
 */
function useJumpToLine(
  textareaRef: RefObject<HTMLTextAreaElement | null>,
  request: CodeFocusRequest | null,
  afterScroll: () => void,
) {
  const afterScrollRef = useRef(afterScroll);
  afterScrollRef.current = afterScroll;
  useEffect(() => {
    const textarea = textareaRef.current;
    if (!request || !textarea) return;
    const start = textarea.value
      .split("\n")
      .slice(0, request.line - 1)
      .reduce((sum, text) => sum + text.length + 1, 0);
    const offset = start + Math.max(0, (request.column ?? 1) - 1);
    textarea.focus();
    textarea.setSelectionRange(offset, offset);
    textarea.scrollTop = Math.max(0, (request.line - 4) * LINE_HEIGHT_PX);
    afterScrollRef.current();
  }, [request, textareaRef]);
}

/**
 * The `.fabro` source, as text. Everything typed lands in the draft verbatim,
 * valid or not, so nothing here ever rewrites what the author wrote. Tab indents;
 * Escape then Tab moves focus on, so the keyboard never gets trapped.
 */
export function CodeEditor({
  source,
  diagnostics,
  sourceIsValid,
  hasLastValid,
  focusRequest,
  onChange,
  onRestoreLastValid,
  onPick,
}: {
  source: string;
  diagnostics: Diagnostic[];
  sourceIsValid: boolean;
  hasLastValid: boolean;
  /** A line to jump to, set from the outside. A new object means jump again. */
  focusRequest: CodeFocusRequest | null;
  onChange: (source: string) => void;
  onRestoreLastValid: () => void;
  onPick: (diagnostic: Diagnostic) => void;
}) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const gutterRef = useRef<HTMLDivElement>(null);
  const tabLeavesField = useRef(false);
  const lineCount = useMemo(() => source.split("\n").length, [source]);
  const errorLines = useMemo(
    () =>
      new Set(
        diagnostics.filter((item) => item.line !== undefined && item.severity === "error").map((item) => item.line),
      ),
    [diagnostics],
  );

  const syncGutter = () => {
    if (gutterRef.current && textareaRef.current) {
      gutterRef.current.style.transform = `translateY(${-textareaRef.current.scrollTop}px)`;
    }
  };

  useJumpToLine(textareaRef, focusRequest, syncGutter);

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Escape") {
      tabLeavesField.current = true;
      return;
    }
    if (event.key === "Tab" && !event.shiftKey && !tabLeavesField.current) {
      event.preventDefault();
      // execCommand keeps the browser's own undo stack intact, unlike setting the value.
      if (!document.execCommand("insertText", false, "  ")) {
        const textarea = event.currentTarget;
        textarea.setRangeText("  ", textarea.selectionStart, textarea.selectionEnd, "end");
        onChange(textarea.value);
      }
    }
    tabLeavesField.current = false;
  };

  const issuesLabel = diagnosticSummary(diagnostics);

  return (
    <div className="wfb-code">
      {!sourceIsValid ? (
        <div role="status" className="wfb-callout wfb-callout-danger wfb-code-banner">
          <p>
            <strong>This code doesn't parse yet.</strong> Keep editing: it is saved as you type and nothing rewrites it.
            {hasLastValid ? " The canvas stays on the last valid version until the code parses again." : ""}
          </p>
          {hasLastValid ? (
            <button type="button" className="wfb-btn shrink-0" onClick={onRestoreLastValid}>
              Restore last valid code
            </button>
          ) : null}
        </div>
      ) : null}
      <div className="wfb-code-body">
        <div className="wfb-code-gutter" aria-hidden="true">
          <div ref={gutterRef}>
            {Array.from({ length: lineCount }, (_, index) => (
              <div key={index} className={errorLines.has(index + 1) ? "wfb-code-line-error" : undefined}>
                {index + 1}
              </div>
            ))}
          </div>
        </div>
        <textarea
          ref={textareaRef}
          className="wfb-code-input"
          aria-label="Workflow source, .fabro syntax"
          aria-describedby="wfb-code-issues"
          value={source}
          wrap="off"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          onChange={(event) => onChange(event.target.value)}
          onKeyDown={onKeyDown}
          onScroll={syncGutter}
        />
      </div>
      <div className="wfb-code-issues" id="wfb-code-issues">
        <p className="wfb-eyebrow">{issuesLabel}</p>
        {diagnostics.length > 0 ? <DiagnosticsList diagnostics={diagnostics} onPick={onPick} /> : null}
      </div>
    </div>
  );
}
