import { useEffect, useMemo, useRef } from "react";
import { useLocation, useNavigate, useParams } from "react-router";
import {
  AssistantRuntimeProvider,
  useLocalRuntime,
} from "@assistant-ui/react";
import { Thread, makeMarkdownText } from "@assistant-ui/react-ui";

import { useChatsStore } from "../lib/chats-store";
import {
  createScriptedAdapter,
  toThreadMessages,
} from "../lib/chats-runtime";
import CustomComposer from "../components/custom-composer";
import ToolFallback from "../components/tool-fallback";
import type { CompletionMessage } from "../lib/types";

const MarkdownText = makeMarkdownText();

export default function ChatsDetail() {
  const { chatId } = useParams<{ chatId: string }>();
  const navigate = useNavigate();
  const { state } = useChatsStore();
  const chat = chatId ? state.chats[chatId] : undefined;

  if (!chatId || !chat) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-center">
          <p className="text-sm text-fg-muted">That chat doesn't exist.</p>
          <button
            type="button"
            onClick={() => navigate("/chats/new")}
            className="mt-3 text-sm font-medium text-teal-300 hover:text-teal-500"
          >
            Start a new chat
          </button>
        </div>
      </div>
    );
  }

  return <ChatRuntime key={chatId} chatId={chatId} />;
}

function ChatRuntime({ chatId }: { chatId: string }) {
  const location = useLocation();
  const navigate = useNavigate();
  const { state, peekScriptIndex, advanceScriptIndex } = useChatsStore();
  const chat = state.chats[chatId]!;
  const pendingText =
    (location.state as { pendingText?: string } | null)?.pendingText ?? null;

  const initialMessages = useMemo(
    () => toThreadMessages(chat.seedMessages ?? []),
    [chat.seedMessages],
  );

  const adapter = useMemo(
    () =>
      createScriptedAdapter({
        getChat: () => ({
          ...chat,
          scriptIndex: peekScriptIndex(chatId),
        }),
        onReplyComplete: (_reply: CompletionMessage) => advanceScriptIndex(chatId),
      }),
    [chat, chatId, peekScriptIndex, advanceScriptIndex],
  );

  const runtime = useLocalRuntime(adapter, { initialMessages });

  const didSendPendingRef = useRef(false);
  useEffect(() => {
    if (!pendingText || didSendPendingRef.current) return;
    didSendPendingRef.current = true;
    // Clear router state so a refresh or back-nav doesn't resend.
    navigate(`/chats/${chatId}`, { replace: true, state: null });
    runtime.thread.append(pendingText);
  }, [pendingText, chatId, navigate, runtime]);

  return (
    <AssistantRuntimeProvider runtime={runtime}>
      <div className="h-full">
        <Thread
          components={{ Composer: CustomComposer }}
          assistantMessage={{
            components: { Text: MarkdownText, ToolFallback },
          }}
        />
      </div>
    </AssistantRuntimeProvider>
  );
}
