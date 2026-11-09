import { BoardColumn } from "@qltysh/fabro-api-client";

const STATUS_CHIP: Record<BoardColumn, string> = {
  [BoardColumn.PENDING]: "chip-outline",
  [BoardColumn.RUNNABLE]: "chip-outline",
  [BoardColumn.INITIALIZING]: "chip-info",
  [BoardColumn.RUNNING]: "chip-live",
  [BoardColumn.BLOCKED]: "chip-warn",
  [BoardColumn.SUCCEEDED]: "chip-ok",
  [BoardColumn.FAILED]: "chip-bad",
  [BoardColumn.ARCHIVED]: "",
  [BoardColumn.REMOVING]: "chip-outline",
};

/** The same mapping as Factory's run status chip. The word always sits inside it. */
export function statusChipClass(status: BoardColumn): string {
  return `chip ${STATUS_CHIP[status]}`.trim();
}
