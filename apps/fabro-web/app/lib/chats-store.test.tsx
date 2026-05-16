import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { act } from "react-test-renderer";

import { renderHook, setupReactTestEnv } from "./test-utils";
import {
  ChatsProvider,
  useChatsActions,
  useChatsState,
} from "./chats-store";

function useStore() {
  return { ...useChatsActions(), state: useChatsState() };
}

function wrapper({ children }: { children: React.ReactNode }) {
  return <ChatsProvider>{children}</ChatsProvider>;
}

describe("chats-store reducer", () => {
  let teardown: () => void = () => {};
  beforeEach(() => {
    teardown = setupReactTestEnv();
  });
  afterEach(() => {
    teardown();
  });


  test("createChatWithFirstMessage seeds title and user message", () => {
    const { result } = renderHook(() => useStore(), { wrapper });
    let id = "";
    act(() => {
      id = result.current.createChatWithFirstMessage("Help me with React");
    });
    const chat = result.current.state.chats[id];
    expect(chat?.title).toBe("Help me with React");
    expect(chat?.pendingResponse).toBe(true);
    expect(chat?.seedMessages).toHaveLength(1);
    expect(chat?.seedMessages[0]?.role).toBe("user");
    expect(chat?.seedMessages[0]?.content[0]).toEqual({
      kind: "text",
      data: { text: "Help me with React" },
    });
  });

  test("title is truncated to 40 chars at word boundary", () => {
    const { result } = renderHook(() => useStore(), { wrapper });
    let id = "";
    act(() => {
      id = result.current.createChatWithFirstMessage(
        "Help me draft a launch announcement email for our new analytics dashboard",
      );
    });
    expect(result.current.state.chats[id]?.title).toBe(
      "Help me draft a launch announcement…",
    );
  });

  test("consumePendingResponse clears the flag", () => {
    const { result } = renderHook(() => useStore(), { wrapper });
    let id = "";
    act(() => {
      id = result.current.createChatWithFirstMessage("hi");
    });
    expect(result.current.state.chats[id]?.pendingResponse).toBe(true);
    act(() => {
      result.current.consumePendingResponse(id);
    });
    expect(result.current.state.chats[id]?.pendingResponse).toBe(false);
  });

  test("advanceScriptIndex increments by one", () => {
    const { result } = renderHook(() => useStore(), { wrapper });
    let id = "";
    act(() => {
      id = result.current.createChatWithFirstMessage("hi");
    });
    expect(result.current.state.chats[id]?.scriptIndex).toBe(0);
    act(() => {
      result.current.advanceScriptIndex(id);
    });
    expect(result.current.state.chats[id]?.scriptIndex).toBe(1);
  });

  test("seed chats appear in order on mount", () => {
    const { result } = renderHook(() => useStore(), { wrapper });
    expect(result.current.state.order.length).toBeGreaterThanOrEqual(3);
    const titles = result.current.state.order.map(
      (id) => result.current.state.chats[id]?.title,
    );
    expect(titles).toContain("Draft a launch email");
    expect(titles).toContain("Refactor a React hook");
    expect(titles).toContain("Compare Postgres vs SQLite");
  });
});
