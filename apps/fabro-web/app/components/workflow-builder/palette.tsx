import { MagnifyingGlassIcon, XMarkIcon } from "@heroicons/react/16/solid";
import { useRef, useState, type KeyboardEvent } from "react";

import { NODE_KINDS } from "../../lib/workflow-builder/catalog";
import type { NodeKind } from "../../lib/workflow-builder/types";
import { ShapeIcon } from "./shapes";

export const PALETTE_DRAG_TYPE = "application/x-fabro-node-kind";

function matches(kind: NodeKind, query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (!needle) return true;
  return [kind.label, kind.description, kind.shape, kind.type].some((text) =>
    text.toLowerCase().includes(needle),
  );
}

/**
 * The searchable node list. Enter in the search box adds the first match, arrows
 * walk the list, and each entry can be clicked or dragged onto the canvas.
 */
export function NodePalette({
  connecting,
  onAdd,
  onClose,
}: {
  /** Set when the palette opened from a connection dropped on empty canvas. */
  connecting: string | null;
  onAdd: (kind: NodeKind) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const listRef = useRef<HTMLUListElement>(null);
  const results = NODE_KINDS.filter((kind) => matches(kind, query));

  const focusItem = (index: number) => {
    const buttons = listRef.current?.querySelectorAll("button");
    if (!buttons?.length) return;
    buttons[(index + buttons.length) % buttons.length].focus();
  };

  const onListKeyDown = (event: KeyboardEvent<HTMLUListElement>) => {
    const buttons = [...(listRef.current?.querySelectorAll("button") ?? [])];
    const current = buttons.indexOf(event.target as HTMLButtonElement);
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusItem(current + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusItem(current - 1);
    }
  };

  return (
    <div
      role="dialog"
      aria-label={connecting ? `Add a node connected from ${connecting}` : "Add a node"}
      className="wfb-palette nodrag nopan nowheel"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          onClose();
        }
      }}
    >
      <div className="wfb-palette-header">
        <div className="wfb-palette-search">
          <MagnifyingGlassIcon className="size-4" aria-hidden="true" />
          <input
            // The palette opens on an explicit action, so its search takes focus straight away.
            autoFocus
            type="search"
            aria-label="Search node types"
            placeholder={connecting ? `Connect from ${connecting}…` : "Search node types"}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && results[0]) {
                event.preventDefault();
                onAdd(results[0]);
              } else if (event.key === "ArrowDown") {
                event.preventDefault();
                focusItem(0);
              }
            }}
          />
        </div>
        <button type="button" className="wfb-icon-button" aria-label="Close node list" onClick={onClose}>
          <XMarkIcon className="size-4" aria-hidden="true" />
        </button>
      </div>
      {results.length === 0 ? <p className="wfb-help p-3">No node types match “{query}”.</p> : null}
      <ul ref={listRef} className="wfb-palette-list" onKeyDown={onListKeyDown}>
        {results.map((kind) => (
          <li key={kind.shape}>
            <button
              type="button"
              className="wfb-palette-item"
              draggable
              onDragStart={(event) => {
                event.dataTransfer.setData(PALETTE_DRAG_TYPE, kind.shape);
                event.dataTransfer.effectAllowed = "copy";
              }}
              onClick={() => onAdd(kind)}
            >
              <ShapeIcon shape={kind.shape} size={36} />
              <span className="min-w-0 flex-1">
                <span className="flex items-baseline justify-between gap-2">
                  <span className="wfb-palette-name">{kind.label}</span>
                  <span className="wfb-mono wfb-palette-shape">{kind.shape}</span>
                </span>
                <span className="wfb-palette-description">{kind.description}</span>
              </span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}
