import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { MemoryRouter, Routes, Route } from "react-router";
import TestRenderer, { act } from "react-test-renderer";

import { setupReactTestEnv } from "../lib/test-utils";
import { ChatsProvider } from "../lib/chats-store";
import * as ChatsLayoutModule from "./chats-layout";
import * as ChatsNewModule from "./chats-new";
import * as ChatsDetailModule from "./chats-detail";

describe("chats route module exports", () => {
  let teardown: () => void = () => {};
  beforeEach(() => {
    teardown = setupReactTestEnv();
  });
  afterEach(() => {
    teardown();
  });


  test("each route exports a default component", () => {
    expect(typeof ChatsLayoutModule.default).toBe("function");
    expect(typeof ChatsNewModule.default).toBe("function");
    expect(typeof ChatsDetailModule.default).toBe("function");
  });

  test("chats-layout declares the AppShell handle (children inherit via useMatches)", () => {
    expect(ChatsLayoutModule.handle).toEqual({
      hideHeader: true,
      fullHeight: true,
      wide: true,
    });
  });

  test("chats-new renders inside MemoryRouter without crashing", () => {
    let tree: TestRenderer.ReactTestRenderer | null = null;
    act(() => {
      tree = TestRenderer.create(
        <ChatsProvider>
          <MemoryRouter initialEntries={["/"]}>
            <Routes>
              <Route path="/" Component={ChatsNewModule.default} />
            </Routes>
          </MemoryRouter>
        </ChatsProvider>,
      );
    });
    expect(tree).not.toBeNull();
    act(() => {
      tree?.unmount();
    });
  });
});
