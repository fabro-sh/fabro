import type { CompletionMessage } from "@qltysh/fabro-api-client";

/**
 * Stricter discriminated-union view over @qltysh/fabro-api-client's
 * `CompletionContentPart` ({ kind: string; data: any }). Each variant in our
 * union is assignable to the API client type at the boundary, but inside the
 * chat code we get exhaustive switch checking.
 */
export type ChatContentPart =
  | { kind: "text"; data: { text: string } }
  | {
      kind: "tool_call";
      data: {
        tool_call_id: string;
        name: string;
        arguments: { [key: string]: JsonValue };
      };
    }
  | {
      kind: "tool_result";
      data: {
        tool_call_id: string;
        content: JsonValue;
        is_error?: boolean;
      };
    };

export type JsonValue =
  | null
  | string
  | number
  | boolean
  | JsonValue[]
  | { [key: string]: JsonValue };

/**
 * Sidebar/store wrapper around a single chat. Messages and the in-flight
 * stream live inside assistant-ui's runtime; the store holds the metadata
 * needed to render the sidebar, derive titles, and drive the scripted
 * reply bank. `seedMessages` is the initial history fed to the runtime via
 * `initialMessages` on mount. `pendingResponse` flags a chat where the user
 * sent the first message but the assistant has not yet replied.
 */
export type Chat = {
  id: string;
  title: string;
  createdAt: number;
  scriptIndex: number;
  seedMessages: CompletionMessage[];
  pendingResponse: boolean;
};

export type { CompletionMessage } from "@qltysh/fabro-api-client";
