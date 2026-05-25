import {
  createContext,
  use,
  useCallback,
  useMemo,
  useReducer,
  type ReactNode,
} from "react";

import type { Chat, ChatMessage } from "./chats-types";
import { pickReply } from "./chats-script";

type State = {
  chats: Record<string, Chat>;
  order: string[]; // newest first
};

type Action =
  | {
      type: "create";
      id: string;
      title: string;
      createdAt: number;
      userMessage: ChatMessage;
    }
  | { type: "consume_pending"; chatId: string }
  | { type: "advance_script"; chatId: string };

function deriveTitle(text: string): string {
  const trimmed = text.trim().replace(/\s+/g, " ");
  if (trimmed.length <= 40) return trimmed || "New chat";
  const cut = trimmed.slice(0, 40);
  const lastSpace = cut.lastIndexOf(" ");
  const base = lastSpace > 20 ? cut.slice(0, lastSpace) : cut;
  return `${base}…`;
}

function userMessage(text: string): ChatMessage {
  return {
    role: "user",
    content: [{ kind: "text", data: { text } }],
  };
}

function seedChat(args: {
  id: string;
  title: string;
  ageDays: number;
  scriptIndex: number;
  userText: string;
}): Chat {
  return {
    id: args.id,
    title: args.title,
    createdAt: Date.now() - args.ageDays * 86_400_000,
    scriptIndex: args.scriptIndex + 1, // seeded reply already "consumed"
    pendingResponse: false,
    seedMessages: [userMessage(args.userText), pickReply(args.scriptIndex)],
  };
}

const initialState: State = (() => {
  const seeds: Chat[] = [
    seedChat({
      id: "seed_email",
      title: "Draft a launch email",
      ageDays: 0.5,
      scriptIndex: 0,
      userText:
        "Help me draft a launch announcement email for our new analytics dashboard.",
    }),
    seedChat({
      id: "seed_hook",
      title: "Refactor a React hook",
      ageDays: 2,
      scriptIndex: 3,
      userText:
        "My useChat hook has grown to 200 lines and I keep tangling concerns. How should I think about refactoring it?",
    }),
    seedChat({
      id: "seed_db",
      title: "Compare Postgres vs SQLite",
      ageDays: 6,
      scriptIndex: 5,
      userText:
        "For a side project with ~50 daily users, should I reach for Postgres or stick with SQLite?",
    }),
  ];
  const chats: Record<string, Chat> = {};
  for (const s of seeds) chats[s.id] = s;
  return { chats, order: seeds.map((s) => s.id) };
})();

function reducer(state: State, action: Action): State {
  switch (action.type) {
    case "create": {
      const chat: Chat = {
        id: action.id,
        title: action.title,
        createdAt: action.createdAt,
        scriptIndex: 0,
        pendingResponse: true,
        seedMessages: [action.userMessage],
      };
      return {
        chats: { ...state.chats, [action.id]: chat },
        order: [action.id, ...state.order],
      };
    }
    case "consume_pending": {
      const existing = state.chats[action.chatId];
      if (!existing || !existing.pendingResponse) return state;
      return {
        ...state,
        chats: {
          ...state.chats,
          [action.chatId]: { ...existing, pendingResponse: false },
        },
      };
    }
    case "advance_script": {
      const existing = state.chats[action.chatId];
      if (!existing) return state;
      return {
        ...state,
        chats: {
          ...state.chats,
          [action.chatId]: {
            ...existing,
            scriptIndex: existing.scriptIndex + 1,
          },
        },
      };
    }
  }
}

export type ChatsActions = {
  createChatWithFirstMessage: (text: string) => string;
  consumePendingResponse: (chatId: string) => void;
  advanceScriptIndex: (chatId: string) => void;
};

const ChatsStateContext = createContext<State | null>(null);
const ChatsActionsContext = createContext<ChatsActions | null>(null);

function shortId(): string {
  return `c_${Math.random().toString(36).slice(2, 8)}`;
}

export function ChatsProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, initialState);

  const actions = useMemo<ChatsActions>(
    () => ({
      createChatWithFirstMessage(text: string) {
        const id = shortId();
        dispatch({
          type: "create",
          id,
          title: deriveTitle(text),
          createdAt: Date.now(),
          userMessage: userMessage(text),
        });
        return id;
      },
      consumePendingResponse(chatId: string) {
        dispatch({ type: "consume_pending", chatId });
      },
      advanceScriptIndex(chatId: string) {
        dispatch({ type: "advance_script", chatId });
      },
    }),
    [],
  );

  return (
    <ChatsStateContext.Provider value={state}>
      <ChatsActionsContext.Provider value={actions}>
        {children}
      </ChatsActionsContext.Provider>
    </ChatsStateContext.Provider>
  );
}

export function useChatsState(): State {
  const value = use(ChatsStateContext);
  if (!value) throw new Error("useChatsState must be used inside <ChatsProvider>");
  return value;
}

export function useChatsActions(): ChatsActions {
  const value = use(ChatsActionsContext);
  if (!value) throw new Error("useChatsActions must be used inside <ChatsProvider>");
  return value;
}

export function useChat(chatId: string | undefined): Chat | undefined {
  const state = useChatsState();
  return chatId ? state.chats[chatId] : undefined;
}
