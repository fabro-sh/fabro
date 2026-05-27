import { useMemo, useRef } from "react";
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
import { usePendingChatAutoresponse } from "../hooks/use-pending-chat-autoresponse";

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
  chatRef.current = chat;

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

  usePendingChatAutoresponse({
    chatId,
    pendingResponse: chat.pendingResponse,
    consumePendingResponse,
    startRun: () => runtime.thread.startRun({ parentId: null }),
  });

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
