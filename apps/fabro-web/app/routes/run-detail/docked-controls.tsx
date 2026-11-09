import {
  useCallback,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";
import { SparklesIcon } from "@heroicons/react/20/solid";

import { useResizeObserver } from "../../hooks/effects";

import AskFabroSidebar, {
  SIDEBAR_WIDTH,
} from "../../components/chats/ask-fabro-sidebar";
import { InterviewDock } from "../../components/interview-dock";
import { SteerBar, type SteerBarHandle } from "../../components/steer-bar";
import {
  SECONDARY_BUTTON_CLASS,
  Tooltip,
} from "../../components/ui";
import { classNames } from "../../lib/class-names";
import {
  AskFabroUnavailableReasonEnum,
  type ApiQuestion,
  type AskFabro,
} from "@qltysh/fabro-api-client";

const ASK_FABRO_UNAVAILABLE_TOOLTIPS: Record<
  AskFabroUnavailableReasonEnum,
  string
> = {
  [AskFabroUnavailableReasonEnum.NO_SANDBOX]:        "Run sandbox isn't ready",
  [AskFabroUnavailableReasonEnum.SANDBOX_NOT_READY]: "Run sandbox isn't ready",
  [AskFabroUnavailableReasonEnum.LLM_UNCONFIGURED]:  "No LLM configured",
};

interface AskFabroShellRenderProps {
  askTrigger: ReactNode;
  sidebarWidth: number;
  isResizing: boolean;
}

export function RunDetailAskFabroShell({
  runId,
  askFabro,
  children,
}: {
  runId: string;
  askFabro: AskFabro | null;
  children: (props: AskFabroShellRenderProps) => ReactNode;
}) {
  const askAvailable = askFabro?.available ?? false;
  const askDefaultModel = askFabro?.default_model ?? null;
  const [askOpen, setAskOpen] = useState(false);
  const [askWidth, setAskWidth] = useState(SIDEBAR_WIDTH);
  const [resizeActive, setResizeActive] = useState(false);
  const sidebarWidth = askAvailable && askOpen ? askWidth : 0;
  const isResizing = askAvailable && resizeActive;
  const shellLayoutStyle = `:root {
  --fabro-ask-sidebar-width: ${sidebarWidth}px;
  --fabro-ask-sidebar-transition: ${
    isResizing ? "none" : "padding 300ms cubic-bezier(0.16, 1, 0.3, 1)"
  };
}`;

  return (
    <>
      <style>{shellLayoutStyle}</style>
      {children({
        askTrigger: (
          <AskFabroTriggerButton
            askFabro={askFabro}
            askOpen={askOpen}
            onToggle={() => setAskOpen((open) => !open)}
          />
        ),
        sidebarWidth,
        isResizing,
      })}

      {askAvailable && (
        <div className="fixed top-[var(--fabro-topbar-height,0px)] right-0 bottom-0 z-40">
          <AskFabroSidebar
            isOpen={askOpen}
            onClose={() => setAskOpen(false)}
            runId={runId}
            defaultModel={askDefaultModel}
            width={askWidth}
            onWidthChange={setAskWidth}
            onResizeActiveChange={setResizeActive}
          />
        </div>
      )}
    </>
  );
}

function AskFabroTriggerButton({
  askFabro,
  askOpen,
  onToggle,
}: {
  askFabro: AskFabro | null;
  askOpen: boolean;
  onToggle: () => void;
}) {
  const available = askFabro?.available ?? false;
  const disabled = !available;
  const unavailableReason = askFabro?.unavailable_reason ?? null;
  const button = (
    <button
      type="button"
      onClick={onToggle}
      disabled={disabled}
      aria-expanded={askOpen}
      className={classNames(
        SECONDARY_BUTTON_CLASS,
        "disabled:cursor-not-allowed disabled:opacity-60",
      )}
    >
      <SparklesIcon className="size-4 text-teal-300" aria-hidden="true" />
      Ask Fabro
    </button>
  );
  if (!available && unavailableReason) {
    const tooltip = ASK_FABRO_UNAVAILABLE_TOOLTIPS[unavailableReason] ?? "Ask Fabro is unavailable";
    return <Tooltip label={tooltip}>{button}</Tooltip>;
  }
  return button;
}

export function RunDetailDockedControls({
  runId,
  dockIdentity,
  hasPendingQuestions,
  pendingQuestions,
  sidebarWidth,
  isResizing,
  steerBarRef,
  waitingForSteer,
  onHeightChange,
}: {
  runId: string;
  dockIdentity: string | null;
  hasPendingQuestions: boolean;
  pendingQuestions: ApiQuestion[];
  sidebarWidth: number;
  isResizing: boolean;
  steerBarRef: RefObject<SteerBarHandle | null>;
  waitingForSteer: boolean;
  /**
   * Reports the dock's rendered height so the page can reserve exactly that
   * much room beneath the scrolling content.
   */
  onHeightChange: (identity: string, height: number | null) => void;
}) {
  const dockRef = useRef<HTMLDivElement | null>(null);
  const reportedHeightRef = useRef<number | null>(null);
  const visible = dockIdentity !== null;
  const reportHeight = useCallback(
    (height: number | null) => {
      if (dockIdentity === null || reportedHeightRef.current === height) return;
      reportedHeightRef.current = height;
      onHeightChange(dockIdentity, height);
    },
    [dockIdentity, onHeightChange],
  );
  const setDockRef = useCallback(
    (node: HTMLDivElement | null) => {
      dockRef.current = node;
      if (node === null) reportHeight(null);
    },
    [reportHeight],
  );

  useResizeObserver(
    dockRef,
    (entries) => {
      const entry = entries[0];
      if (!entry) return;
      const borderBoxSize = Array.isArray(entry.borderBoxSize)
        ? entry.borderBoxSize[0]
        : (entry.borderBoxSize as unknown as ResizeObserverSize);
      reportHeight(
        borderBoxSize?.blockSize ?? dockRef.current?.offsetHeight ?? null,
      );
    },
    visible,
  );

  if (!visible) return null;

  return (
    <div
      ref={setDockRef}
      className={classNames(
        "fixed bottom-0 left-[var(--fabro-nav-width,0px)] z-30 border-t border-line bg-page",
        !isResizing &&
          "transition-[left,right] duration-300 ease-[cubic-bezier(0.16,1,0.3,1)]",
      )}
      style={{ right: sidebarWidth }}
    >
      {hasPendingQuestions ? (
        <InterviewDock runId={runId} questions={pendingQuestions} />
      ) : (
        <SteerBar
          ref={steerBarRef}
          runId={runId}
          waitingForSteer={waitingForSteer}
        />
      )}
    </div>
  );
}
