import { useEffect, useMemo, useRef } from "react";
import { useNavigate, useParams } from "react-router";
import {
  AssistantRuntimeProvider,
  useLocalRuntime,
} from "@assistant-ui/react";
import { Thread, makeMarkdownText } from "@assistant-ui/react-ui";

import { useChat, useChatsActions } from "../lib/chats-store";
import {
  createScriptedAdapter,
  toThreadMessages,
} from "../lib/chats-runtime";
import CustomComposer from "../components/chats/custom-composer";
import ToolFallback from "../components/chats/tool-fallback";
import { EmptyState } from "../components/state";
import type { Chat, ChatMessage } from "../lib/chats-types";

// AppShell handle lives on the parent chats-layout route; do not redeclare it
// here.

const MarkdownText = makeMarkdownText();

export default function ChatsDetail() {
  const { chatId } = useParams<{ chatId: string }>();
  const navigate = useNavigate();
  const chat = useChat(chatId);

  if (!chatId || !chat) {
    return (
      <div className="flex h-full items-center justify-center p-8">
        <EmptyState
          title="That chat doesn’t exist."
          action={
            <button
              type="button"
              onClick={() => navigate("/chats/new")}
              className="text-sm font-medium text-teal-300 hover:text-teal-500"
            >
              Start a new chat
            </button>
          }
        />
      </div>
    );
  }

  return <ChatRuntime key={chatId} chatId={chatId} chat={chat} />;
}

function ChatRuntime({ chatId, chat }: { chatId: string; chat: Chat }) {
  const { advanceScriptIndex, consumePendingResponse } = useChatsActions();

  // Keep latest `chat` accessible to the stable adapter closure below without
  // recreating the adapter (and the assistant-ui runtime) on every store dispatch.
  const chatRef = useRef(chat);
  useEffect(() => {
    chatRef.current = chat;
  });

  const initialMessages = useMemo(
    () => toThreadMessages(chat.seedMessages),
    [chat.seedMessages],
  );

  const adapter = useMemo(
    () =>
      createScriptedAdapter({
        getChat: () => chatRef.current,
        onReplyComplete: (_reply: ChatMessage) => advanceScriptIndex(chatId),
      }),
    [chatId, advanceScriptIndex],
  );

  const runtime = useLocalRuntime(adapter, { initialMessages });

  // Autorespond: chats arriving here from /chats/new carry the user's first
  // message in seedMessages with pendingResponse=true. Trigger one startRun
  // once per mount; the ref dedupes within a StrictMode mount cycle (state
  // updates from consumePendingResponse aren't visible to the re-fired effect
  // closure), and the store flag dedupes across mounts (e.g. navigating away
  // and back to the same chat).
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
