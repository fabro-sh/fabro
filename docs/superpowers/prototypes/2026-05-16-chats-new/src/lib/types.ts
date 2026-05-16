/**
 * Local mirror of the relevant subset of @qltysh/fabro-api-client types.
 * In the real fabro-web integration we import these directly from the client.
 */

export type CompletionRole =
  | "system"
  | "user"
  | "assistant"
  | "tool"
  | "developer";

export type JsonValue =
  | null
  | string
  | number
  | boolean
  | JsonValue[]
  | { [key: string]: JsonValue };

export type CompletionContentPart =
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

export type CompletionMessage = {
  role: CompletionRole;
  content: CompletionContentPart[];
  name?: string;
  tool_call_id?: string;
};

/**
 * Sidebar/store wrapper around a single chat thread. Messages themselves live
 * in assistant-ui's runtime; the store holds only the metadata needed to render
 * the sidebar and drive the scripted reply bank.
 */
export type Chat = {
  id: string;
  title: string;
  createdAt: number;
  scriptIndex: number;
  seedMessages?: CompletionMessage[];
};
