import {
  useEffect,
  useState,
  type ReactNode,
  type RefObject,
} from "react";
import { SparklesIcon } from "@heroicons/react/20/solid";

import AskFabroSidebar, {
  SIDEBAR_WIDTH,
} from "../../components/chats/ask-fabro-sidebar";
import { InterviewDock } from "../../components/interview-dock";
import { SteerBar, type SteerBarHandle } from "../../components/steer-bar";
import {
  SECONDARY_BUTTON_CLASS,
  Tooltip,
} from "../../components/ui";
import {
  AskFabroUnavailableReasonEnum,
  type ApiQuestion,
  type AskFabro,
} from "@qltysh/fabro-api-client";
import { useAskFabroLayout } from "../../lib/ask-fabro-layout";
import { classNames } from "./model";

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
  const sidebarWidth = askAvailable && askOpen ? askWidth : 0;
  const { setSidebarWidth, isResizing } = useAskFabroLayout();

  useEffect(() => {
    setSidebarWidth(sidebarWidth);
    return () => setSidebarWidth(0);
  }, [sidebarWidth, setSidebarWidth]);

  return (
    <>
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
        <div className="fixed top-16 right-0 bottom-0 z-40">
          <AskFabroSidebar
            isOpen={askOpen}
            onClose={() => setAskOpen(false)}
            runId={runId}
            defaultModel={askDefaultModel}
            width={askWidth}
            onWidthChange={setAskWidth}
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
  hideSteerBar,
  hasPendingQuestions,
  pendingQuestions,
  sidebarWidth,
  isResizing,
  steerBarRef,
}: {
  runId: string;
  hideSteerBar: boolean;
  hasPendingQuestions: boolean;
  pendingQuestions: ApiQuestion[];
  sidebarWidth: number;
  isResizing: boolean;
  steerBarRef: RefObject<SteerBarHandle | null>;
}) {
  if (hideSteerBar && !hasPendingQuestions) return null;

  return (
    <div
      className={`fixed bottom-0 left-0 z-30 border-t border-line bg-page ${
        isResizing
          ? ""
          : "transition-[right] duration-300 ease-[cubic-bezier(0.16,1,0.3,1)]"
      }`}
      style={{ right: sidebarWidth }}
    >
      {hasPendingQuestions ? (
        <InterviewDock runId={runId} questions={pendingQuestions} />
      ) : (
        <SteerBar ref={steerBarRef} runId={runId} />
      )}
    </div>
  );
}
