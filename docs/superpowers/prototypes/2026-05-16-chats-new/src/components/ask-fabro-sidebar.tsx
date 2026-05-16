import { useMemo, useRef } from "react";
import {
  AssistantRuntimeProvider,
  useLocalRuntime,
} from "@assistant-ui/react";
import { Thread, makeMarkdownText } from "@assistant-ui/react-ui";
import { XMarkIcon } from "@heroicons/react/24/outline";

import { createScriptedAdapter } from "../lib/chats-runtime";
import { useAskFabro } from "../lib/ask-fabro-context";
import SidebarComposer from "./sidebar-composer";
import ToolFallback from "./tool-fallback";
import type { Chat } from "../lib/types";

const MarkdownText = makeMarkdownText();

const EMPTY_CHAT: Chat = {
  id: "ask-fabro",
  title: "",
  createdAt: 0,
  scriptIndex: 0,
};

const SIDEBAR_WIDTH = 420;

export default function AskFabroSidebar() {
  const { isOpen, close } = useAskFabro();
  const scriptIndexRef = useRef(0);
  const adapter = useMemo(
    () =>
      createScriptedAdapter({
        getChat: () => ({ ...EMPTY_CHAT, scriptIndex: scriptIndexRef.current }),
        onReplyComplete: () => {
          scriptIndexRef.current += 1;
        },
      }),
    [],
  );
  const runtime = useLocalRuntime(adapter);

  return (
    <aside
      aria-label="Ask Fabro"
      aria-hidden={!isOpen}
      style={{ width: isOpen ? SIDEBAR_WIDTH : 0 }}
      className="h-full shrink-0 overflow-hidden transition-[width] duration-300 ease-[cubic-bezier(0.16,1,0.3,1)]"
    >
      <div
        className="fabro-chat ask-fabro-sidebar relative isolate flex h-full flex-col border-l border-line bg-panel/40 backdrop-blur-sm"
        style={{ width: SIDEBAR_WIDTH }}
      >
        <header className="flex h-12 shrink-0 items-center justify-end px-2">
          <button
            type="button"
            onClick={close}
            aria-label="Close assistant"
            className="inline-flex size-8 items-center justify-center rounded-md text-fg-3 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
          >
            <XMarkIcon className="size-4" />
          </button>
        </header>
        <div className="min-h-0 flex-1">
          <AssistantRuntimeProvider runtime={runtime}>
            <Thread
              components={{ Composer: SidebarComposer, ThreadWelcome: () => null }}
              assistantMessage={{
                components: { Text: MarkdownText, ToolFallback },
                allowCopy: false,
                allowReload: false,
                allowSpeak: false,
                allowFeedbackPositive: false,
                allowFeedbackNegative: false,
              }}
            />
          </AssistantRuntimeProvider>
        </div>
      </div>
    </aside>
  );
}
