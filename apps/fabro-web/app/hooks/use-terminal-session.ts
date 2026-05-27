import { useEffect, useRef, type Dispatch, type RefObject, type SetStateAction } from "react";
import type { Terminal as XtermTerminal } from "@xterm/xterm";
import type { FitAddon as XtermFitAddon } from "@xterm/addon-fit";

import {
  buildTerminalWebSocketUrl,
  parseTerminalServerMessage,
} from "../components/terminal-view-helpers";

export type ConnectionStatus = "connecting" | "ready" | "closed" | "error";

export type TerminalConnectionError = {
  message: string;
  recoverable: boolean;
};

export const TERMINAL_BACKGROUND = "#05080F";

// Pin the cell to a whole-pixel height so xterm's fit math stays exact.
// fontSize × lineHeight = 13 × (19/13) = 19px → no sub-pixel rounding,
// no bottom-row clipping.
const TERMINAL_FONT_SIZE = 13;
const TERMINAL_CELL_HEIGHT_PX = 19;
const TERMINAL_LINE_HEIGHT = TERMINAL_CELL_HEIGHT_PX / TERMINAL_FONT_SIZE;

const TERMINAL_THEME = {
  background:          TERMINAL_BACKGROUND,
  foreground:          "#E6EDF3",
  cursor:              "#7AC4E5",
  cursorAccent:        TERMINAL_BACKGROUND,
  selectionBackground: "#1F4F73",

  black:   TERMINAL_BACKGROUND,
  red:     "#FF6B6B",
  green:   "#5EE6A8",
  yellow:  "#FFC857",
  blue:    "#82AAFF",
  magenta: "#C792EA",
  cyan:    "#7AC4E5",
  white:   "#D5DCE3",

  brightBlack:   "#4B5563",
  brightRed:     "#FF8B8B",
  brightGreen:   "#85F5C2",
  brightYellow:  "#FFD98A",
  brightBlue:    "#A4C4FF",
  brightMagenta: "#E0B6FF",
  brightCyan:    "#A8DFF5",
  brightWhite:   "#FFFFFF",
};

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

  useEffect(() => {
    if (!terminalEl.current) return undefined;

    let disposed = false;
    let resizeObserver: ResizeObserver | null = null;
    const textEncoder = new TextEncoder();
    const disposables: Array<{ dispose: () => void }> = [];

    async function connect() {
      setStatus("connecting");
      setError(null);

      const [{ Terminal }, { FitAddon }] = await Promise.all([
        import("@xterm/xterm"),
        import("@xterm/addon-fit"),
      ]);
      if (disposed || !terminalEl.current) return;

      const terminal = new Terminal({
        cursorBlink: true,
        convertEol: true,
        fontFamily: "\"JetBrains Mono\", ui-monospace, monospace",
        fontSize: TERMINAL_FONT_SIZE,
        lineHeight: TERMINAL_LINE_HEIGHT,
        scrollback: 5000,
        theme: TERMINAL_THEME,
      });
      const fitAddon = new FitAddon();
      terminal.loadAddon(fitAddon);
      terminal.open(terminalEl.current);
      fitAddon.fit();
      terminal.focus();
      terminalRef.current = terminal;
      fitRef.current = fitAddon;

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

    void connect();

    return () => {
      disposed = true;
      resizeObserver?.disconnect();
      for (const disposable of disposables) disposable.dispose();
      socketRef.current?.send(JSON.stringify({ type: "close" }));
      socketRef.current?.close();
      socketRef.current = null;
      terminalRef.current?.dispose();
      terminalRef.current = null;
      fitRef.current = null;
    };
  }, [connectionKey, runId, setError, setStatus, terminalEl]);
}
