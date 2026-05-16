import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useReducer,
  useRef,
  type ReactNode,
} from "react";

import type { Chat } from "./types";
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
    }
  | { type: "advance_script"; chatId: string };

function deriveTitle(text: string): string {
  const trimmed = text.trim().replace(/\s+/g, " ");
  if (trimmed.length <= 40) return trimmed || "New chat";
  const cut = trimmed.slice(0, 40);
  const lastSpace = cut.lastIndexOf(" ");
  const base = lastSpace > 20 ? cut.slice(0, lastSpace) : cut;
  return `${base}…`;
}

function seedChat(
  id: string,
  title: string,
  ageDays: number,
  scriptIndex: number,
  userText: string,
): Chat {
  return {
    id,
    title,
    createdAt: Date.now() - ageDays * 86_400_000,
    scriptIndex: scriptIndex + 1, // seeded reply already "consumed"
    seedMessages: [
      {
        role: "user",
        content: [{ kind: "text", data: { text: userText } }],
      },
      pickReply(scriptIndex),
    ],
  };
}

const initialState: State = (() => {
  const seeds: Chat[] = [
    seedChat(
      "seed_email",
      "Draft a launch email",
      0.5,
      0,
      "Help me draft a launch announcement email for our new analytics dashboard.",
    ),
    seedChat(
      "seed_hook",
      "Refactor a React hook",
      2,
      3,
      "My useChat hook has grown to 200 lines and I keep tangling concerns. How should I think about refactoring it?",
    ),
    seedChat(
      "seed_db",
      "Compare Postgres vs SQLite",
      6,
      5,
      "For a side project with ~50 daily users, should I reach for Postgres or stick with SQLite?",
    ),
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
      };
      return {
        chats: { ...state.chats, [action.id]: chat },
        order: [action.id, ...state.order],
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

type ChatsContextValue = {
  state: State;
  /** Create a chat with a derived title and return its id. */
  createChat: (firstUserText: string) => string;
  /** Read current scriptIndex without subscribing. */
  peekScriptIndex: (chatId: string) => number;
  /** Bump scriptIndex after a scripted reply completes. */
  advanceScriptIndex: (chatId: string) => void;
};

const ChatsContext = createContext<ChatsContextValue | null>(null);

function shortId(): string {
  return `c_${Math.random().toString(36).slice(2, 8)}`;
}

export function ChatsProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, initialState);
  const stateRef = useRef(state);
  stateRef.current = state;

  const createChat = useCallback((firstUserText: string) => {
    const id = shortId();
    dispatch({
      type: "create",
      id,
      title: deriveTitle(firstUserText),
      createdAt: Date.now(),
    });
    return id;
  }, []);

  const peekScriptIndex = useCallback(
    (chatId: string) => stateRef.current.chats[chatId]?.scriptIndex ?? 0,
    [],
  );

  const advanceScriptIndex = useCallback((chatId: string) => {
    dispatch({ type: "advance_script", chatId });
  }, []);

  const value = useMemo<ChatsContextValue>(
    () => ({ state, createChat, peekScriptIndex, advanceScriptIndex }),
    [state, createChat, peekScriptIndex, advanceScriptIndex],
  );

  return <ChatsContext.Provider value={value}>{children}</ChatsContext.Provider>;
}

export function useChatsStore(): ChatsContextValue {
  const value = useContext(ChatsContext);
  if (!value) throw new Error("useChatsStore must be used inside <ChatsProvider>");
  return value;
}
