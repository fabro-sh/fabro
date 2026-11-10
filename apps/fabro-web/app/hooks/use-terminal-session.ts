import { useEffect, useRef, type Dispatch, type RefObject, type SetStateAction } from "react";
import type { Terminal as XtermTerminal } from "@xterm/xterm";
import type { FitAddon as XtermFitAddon } from "@xterm/addon-fit";

import {
  buildTerminalWebSocketUrl,
  parseTerminalServerMessage,
} from "../components/terminal-view-helpers";
import { useAppearance } from "../components/appearance-provider";
import { importChunk } from "../lib/import-chunk";

export type ConnectionStatus = "connecting" | "ready" | "closed" | "error";

export type TerminalConnectionError = {
  message: string;
  recoverable: boolean;
};

// Pin the cell to a whole-pixel height so xterm's fit math stays exact.
// fontSize × lineHeight = 13 × (19/13) = 19px → no sub-pixel rounding,
// no bottom-row clipping.
const TERMINAL_FONT_SIZE = 13;
const TERMINAL_CELL_HEIGHT_PX = 19;
const TERMINAL_LINE_HEIGHT = TERMINAL_CELL_HEIGHT_PX / TERMINAL_FONT_SIZE;

const TERMINAL_FONT_FAMILY =
  '"IBM Plex Mono", ui-monospace, SFMono-Regular, Menlo, monospace';

// Programs pick their own ANSI colors, often assuming a dark background. xterm nudges
// any cell below this ratio until it reads on the active palette.
const TERMINAL_MINIMUM_CONTRAST = 4.5;

function sendResize(socket: WebSocket | null, terminal: XtermTerminal | null) {
  if (!socket || socket.readyState !== WebSocket.OPEN || !terminal) return;
  socket.send(JSON.stringify({
    type: "resize",
    cols: terminal.cols,
    rows: terminal.rows,
  }));
}

/**
 * Synchronizes a mounted DOM node with xterm, its FitAddon, ResizeObserver, and
 * the run terminal WebSocket. All listeners, observers, sockets, and xterm
 * disposables are cleaned up before reconnect and on unmount.
 */
export function useTerminalSession({
  connectionKey,
  runId,
  setError,
  setStatus,
  terminalEl,
}: {
  connectionKey: number;
  runId: string;
  setError: Dispatch<SetStateAction<TerminalConnectionError | null>>;
  setStatus: Dispatch<SetStateAction<ConnectionStatus>>;
  terminalEl: RefObject<HTMLDivElement | null>;
}) {
  const terminalRef = useRef<XtermTerminal | null>(null);
  const fitRef = useRef<XtermFitAddon | null>(null);
  const socketRef = useRef<WebSocket | null>(null);
  const { terminalColors } = useAppearance();
  const terminalColorsRef = useRef(terminalColors);
  terminalColorsRef.current = terminalColors;

  // A theme switch repaints the live terminal without touching its session.
  useEffect(() => {
    if (terminalRef.current) terminalRef.current.options.theme = terminalColors;
  }, [terminalColors]);

  useEffect(() => {
    if (!terminalEl.current) return undefined;

    let disposed = false;
    let resizeObserver: ResizeObserver | null = null;
    const textEncoder = new TextEncoder();
    const disposables: Array<{ dispose: () => void }> = [];

    function disposeResources() {
      resizeObserver?.disconnect();
      resizeObserver = null;
      for (const disposable of disposables.splice(0)) disposable.dispose();

      const socket = socketRef.current;
      if (socket) {
        if (socket.readyState === WebSocket.OPEN) {
          socket.send(JSON.stringify({ type: "close" }));
        }
        socket.close();
        socketRef.current = null;
      }

      terminalRef.current?.dispose();
      terminalRef.current = null;
      fitRef.current = null;
    }

    async function connect() {
      setStatus("connecting");
      setError(null);

      const [{ Terminal }, { FitAddon }] = await Promise.all([
        importChunk(() => import("@xterm/xterm")),
        importChunk(() => import("@xterm/addon-fit")),
      ]);
      if (disposed || !terminalEl.current) return;

      const terminal = new Terminal({
        cursorBlink: true,
        convertEol: true,
        fontFamily: TERMINAL_FONT_FAMILY,
        fontSize: TERMINAL_FONT_SIZE,
        lineHeight: TERMINAL_LINE_HEIGHT,
        minimumContrastRatio: TERMINAL_MINIMUM_CONTRAST,
        scrollback: 5000,
        theme: terminalColorsRef.current,
      });
      const fitAddon = new FitAddon();
      terminalRef.current = terminal;
      fitRef.current = fitAddon;
      terminal.loadAddon(fitAddon);
      terminal.open(terminalEl.current);
      fitAddon.fit();
      terminal.focus();

      const socket = new WebSocket(buildTerminalWebSocketUrl(window.location, runId));
      socket.binaryType = "arraybuffer";
      socketRef.current = socket;

      disposables.push(terminal.onData((data) => {
        if (socket.readyState === WebSocket.OPEN) {
          socket.send(textEncoder.encode(data));
        }
      }));

      const handleOpen = () => {
        sendResize(socket, terminal);
      };
      const handleMessage = (event: MessageEvent) => {
        if (typeof event.data === "string") {
          const message = parseTerminalServerMessage(event.data);
          if (!message) return;
          if (message.type === "ready") {
            setStatus("ready");
            return;
          }
          if (message.type === "closed") {
            setStatus("closed");
            return;
          }
          setStatus("error");
          setError({
            message: message.message ?? "Terminal session failed.",
            recoverable: false,
          });
          return;
        }
        const bytes = event.data instanceof ArrayBuffer
          ? new Uint8Array(event.data)
          : event.data;
        terminal.write(bytes);
      };
      const handleClose = () => {
        setStatus((current) => current === "error" ? current : "closed");
      };
      const handleError = () => {
        setStatus("error");
        setError({
          message: "Terminal WebSocket connection failed.",
          recoverable: true,
        });
      };
      socket.addEventListener("open", handleOpen);
      socket.addEventListener("message", handleMessage);
      socket.addEventListener("close", handleClose);
      socket.addEventListener("error", handleError);
      disposables.push({
        dispose: () => {
          socket.removeEventListener("open", handleOpen);
          socket.removeEventListener("message", handleMessage);
          socket.removeEventListener("close", handleClose);
          socket.removeEventListener("error", handleError);
        },
      });

      resizeObserver = new ResizeObserver(() => {
        fitAddon.fit();
        sendResize(socket, terminal);
      });
      resizeObserver.observe(terminalEl.current);

      if (typeof document !== "undefined" && document.fonts?.ready) {
        void document.fonts.ready.then(() => {
          if (disposed) return;
          fitAddon.fit();
          sendResize(socket, terminal);
        });
      }
    }

    void connect().catch((error: unknown) => {
      if (disposed) return;
      disposeResources();
      setStatus("error");
      setError({
        message: error instanceof Error
          ? `Terminal initialization failed: ${error.message}`
          : "Terminal initialization failed.",
        recoverable: true,
      });
    });

    return () => {
      disposed = true;
      disposeResources();
    };
  }, [connectionKey, runId, setError, setStatus, terminalEl]);
}
