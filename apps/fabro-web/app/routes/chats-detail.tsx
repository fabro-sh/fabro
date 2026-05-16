import { useEffect, useMemo, useRef } from "react";
import { useNavigate, useParams } from "react-router";
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
import CustomComposer from "../components/chats/custom-composer";
import ToolFallback from "../components/chats/tool-fallback";
import type { Chat, CompletionMessage } from "../lib/chats-types";

// AppShell handle lives on the parent chats-layout route; do not redeclare it
// here.

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
          <p className="text-sm text-fg-muted">That chat doesn&rsquo;t exist.</p>
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

  return <ChatRuntime key={chatId} chatId={chatId} chat={chat} />;
}

function ChatRuntime({ chatId, chat }: { chatId: string; chat: Chat }) {
  const {
    peekScriptIndex,
    advanceScriptIndex,
    consumePendingResponse,
  } = useChatsStore();

  const initialMessages = useMemo(
    () => toThreadMessages(chat.seedMessages),
    [chat.seedMessages],
  );

  const adapter = useMemo(
    () =>
      createScriptedAdapter({
        getChat: () => ({
          ...chat,
          scriptIndex: peekScriptIndex(chatId),
        }),
        onReplyComplete: (_reply: CompletionMessage) =>
          advanceScriptIndex(chatId),
      }),
    [chat, chatId, peekScriptIndex, advanceScriptIndex],
  );

  const runtime = useLocalRuntime(adapter, { initialMessages });

  // Autorespond: chats arriving here from /chats/new carry the user's first
  // message in seedMessages with pendingResponse=true. Trigger one startRun
  // and immediately mark the pending flag consumed so the next render is a
  // no-op. Safe under StrictMode because startRun is idempotent on a thread
  // whose last message is a user message — the store-level flag dedupes.
  const didStartRef = useRef(false);
  useEffect(() => {
    if (!chat.pendingResponse || didStartRef.current) return;
    didStartRef.current = true;
    consumePendingResponse(chatId);
    runtime.thread.startRun({ parentId: null });
  }, [chat.pendingResponse, chatId, consumePendingResponse, runtime]);

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
