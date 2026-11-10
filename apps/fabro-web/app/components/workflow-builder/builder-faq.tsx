import { Dialog, DialogPanel, DialogTitle } from "@headlessui/react";
import { ChevronDownIcon, XMarkIcon } from "@heroicons/react/16/solid";
import type { ReactNode } from "react";

import { CopyButton, GHOST_BUTTON_CLASS } from "../ui";
import { MAX_ATTACHMENT_BYTES } from "./attachments";

const RUN_COMMANDS = `fabro validate .fabro/workflows/frontend-component-changes/workflow.fabro
fabro preflight .fabro/workflows/frontend-component-changes/workflow.toml
fabro run frontend-component-changes`;

const MAX_ATTACHMENT_MB = MAX_ATTACHMENT_BYTES / (1024 * 1024);

export interface FaqEntry {
  question: string;
  /** The two questions people come here for start open. The rest stay out of the way. */
  defaultOpen?: boolean;
  answer: ReactNode;
}

/**
 * Answers for someone new to the builder. Each one describes what the builder does today,
 * so a change to the behavior named here should change the words too. The test beside this
 * file checks the labels named here against the source.
 */
export const BUILDER_FAQ: FaqEntry[] = [
  {
    question: "How do I add a stage?",
    defaultOpen: true,
    answer: (
      <>
        <ol className="wfb-faq-steps">
          <li>
            In the Visual view, select <strong>Add stage</strong> in the canvas toolbar.{" "}
            <strong>Settings</strong> has the same button under <strong>Stages</strong>.
          </li>
          <li>Type a name in the panel on the right.</li>
          <li>
            Under <strong>Nodes</strong>, check the nodes that belong in it. You can also select a node and choose from
            its <strong>Stage</strong> menu.
          </li>
        </ol>
        <p>
          Dragging a node over a stage doesn't add it. The frame grows to fit its nodes, and dragging the frame's title
          moves them together. <strong>Delete stage</strong> removes the frame and keeps the nodes.
        </p>
      </>
    ),
  },
  {
    question: "How do I delete a node?",
    defaultOpen: true,
    answer: (
      <>
        <ul className="wfb-faq-list">
          <li>
            <strong>Keyboard:</strong> click the node, then press <kbd>Delete</kbd> or <kbd>Backspace</kbd>.
          </li>
          <li>
            <strong>Button:</strong> click the node, then select <strong>Delete node</strong> at the bottom of the panel
            on the right.
          </li>
        </ul>
        <p>
          Its connections are deleted with it. <strong>Undo</strong> in the canvas toolbar, or <kbd>⌘Z</kbd> /{" "}
          <kbd>Ctrl+Z</kbd>, brings it all back.
        </p>
        <p>
          If your cursor is in a text field, the key edits the text instead, so click the node again first. Deleting is
          paused while the code has errors.
        </p>
      </>
    ),
  },
  {
    question: "Do stages control what runs first?",
    answer: (
      <p>
        No. Arrows decide what runs next, including branches and loops. Drag from the dot on one node to the dot on
        another to connect them, then select the arrow to add a condition. Stages only organize the canvas and can hold
        default settings for the nodes inside them.
      </p>
    ),
  },
  {
    question: "How do I add prompts and files?",
    answer: (
      <>
        <p>
          Select an Agent, Prompt or Fan-in node and find <strong>Prompt</strong> in the panel on the right. Choose{" "}
          <strong>Text</strong> to write instructions, or <strong>File</strong> and then <strong>Choose file</strong> to
          attach a text file of up to {MAX_ATTACHMENT_MB} MB.
        </p>
        <p>
          <strong>Add prompt</strong> adds another block. Several blocks are joined, in order, into one generated prompt
          file. Every attached file is listed in <strong>Settings</strong> under <strong>Files in this draft</strong>.
        </p>
      </>
    ),
  },
  {
    question: "Where is my work saved?",
    answer: (
      <>
        <p>
          In this browser. Edits save automatically, <strong>Save</strong> writes right away, and the status beside the
          name tells you if a save failed. Drafts aren't sent to Fabro's server or GitHub, and clearing this site's data
          removes them.
        </p>
        <p>
          To keep or move a copy, open <strong>More actions</strong> (⋯) and select{" "}
          <strong>Download ZIP bundle</strong>. <strong>Import as new draft…</strong> opens one again. The ZIP needs code
          that parses.
        </p>
      </>
    ),
  },
  {
    question: "What do Visual, Code and Issues do?",
    answer: (
      <>
        <p>
          <strong>Visual</strong> and <strong>Code</strong> show the same workflow. Canvas changes update the code, and
          valid code updates the canvas. If the code has an error, your text is kept, the canvas shows the last valid
          version, and canvas edits pause until you fix it or select <strong>Restore last valid code</strong>.
        </p>
        <p>
          The <strong>Issues</strong> button lists errors and warnings. Select one to jump to it. These checks can't tell
          whether your providers and models are ready.
        </p>
      </>
    ),
  },
  {
    question: "How do I run a workflow?",
    answer: (
      <>
        <p>The builder doesn't start runs. You run the exported files with the Fabro CLI:</p>
        <ol className="wfb-faq-steps">
          <li>
            Download the ZIP bundle and extract it into your project at <code>.fabro/workflows/&lt;name&gt;/</code>. The
            project must be a Git checkout with a <code>.fabro/project.toml</code>. <code>fabro repo init</code> creates
            one.
          </li>
          <li>Run these from the project. They use the example's folder name, so swap in your own.</li>
        </ol>
        <div className="wfb-faq-code">
          {/* Long commands scroll sideways on a phone, so the block has to be reachable by keyboard. */}
          <pre tabIndex={0} aria-label="Commands to validate and run the workflow">
            {RUN_COMMANDS}
          </pre>
          <CopyButton value={RUN_COMMANDS} label="Copy commands" />
        </div>
        <p>
          The example's <code>README.md</code> lists what to set up first: your ticket, a provider and your test
          commands.
        </p>
      </>
    ),
  },
];

function FaqItem({ entry, autoFocus }: { entry: FaqEntry; autoFocus: boolean }) {
  return (
    <details className="wfb-faq-item" open={entry.defaultOpen}>
      <summary className="wfb-faq-question" data-autofocus={autoFocus ? true : undefined}>
        <span>{entry.question}</span>
        <ChevronDownIcon className="wfb-faq-chevron size-4 shrink-0" aria-hidden="true" />
      </summary>
      <div className="wfb-faq-answer">{entry.answer}</div>
    </details>
  );
}

/**
 * Quick answers for the builder. Headless UI's Dialog keeps focus inside, closes on Escape
 * or a click outside, and hands focus back to the FAQ button. Native `<details>` give each
 * question its keyboard handling and announce whether it is open.
 */
export function BuilderFaq({ open, onClose }: { open: boolean; onClose: () => void }) {
  return (
    <Dialog open={open} onClose={onClose} className="relative z-50">
      <div className="modal-backdrop fixed inset-0" aria-hidden="true" />
      <div className="fixed inset-0 flex items-center justify-center p-4">
        <DialogPanel className="modal wfb-theme flex flex-col">
          <div className="flex items-center justify-between gap-3 border-b border-line px-5 py-3">
            <DialogTitle className="text-base font-extrabold">Builder help</DialogTitle>
            <button type="button" onClick={onClose} aria-label="Close help" className={`${GHOST_BUTTON_CLASS} btn-sm`}>
              <XMarkIcon className="size-4" aria-hidden="true" />
            </button>
          </div>
          <div className="scroll-pane min-h-0 flex-1 px-5 py-1 text-[13px] text-fg-2">
            {BUILDER_FAQ.map((entry, index) => (
              <FaqItem key={entry.question} entry={entry} autoFocus={index === 0} />
            ))}
          </div>
        </DialogPanel>
      </div>
    </Dialog>
  );
}
