import { useRef, useState, type FormEvent } from "react";
import { useNavigate } from "react-router";
import { ArrowUpIcon } from "@heroicons/react/24/solid";

import { useChatsActions } from "../lib/chats-store";
import ComposerChips from "../components/chats/composer-chips";

export function meta() {
  return [{ title: "New chat — Fabro" }];
}

export default function ChatsNew() {
  const navigate = useNavigate();
  const { createChatWithFirstMessage } = useChatsActions();
  const [text, setText] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  function submit(value: string) {
    const trimmed = value.trim();
    if (!trimmed) return;
    const id = createChatWithFirstMessage(trimmed);
    navigate(`/chats/${id}`);
  }

  function onSubmit(e: FormEvent) {
    e.preventDefault();
    submit(text);
  }

  return (
    <div className="flex h-full flex-col items-center px-6 pt-[18vh] pb-10">
      <div className="w-full max-w-2xl">
        <form
          onSubmit={onSubmit}
          className="w-full overflow-hidden rounded-2xl bg-panel-alt/80 shadow-2xl shadow-black/40 ring-1 ring-line-strong backdrop-blur-sm transition-all focus-within:ring-teal-500/40"
        >
          <textarea
            ref={textareaRef}
            name="prompt"
            aria-label="Message"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                submit(text);
              }
            }}
            placeholder="Ask anything…"
            rows={2}
            autoFocus
            className="block max-h-72 w-full resize-none bg-transparent px-4 pt-4 pb-2 text-base text-fg placeholder:text-fg-muted focus:outline-none"
          />
          <div className="flex items-center justify-between gap-3 px-4 pt-2 pb-3">
            <ComposerChips />
            <button
              type="submit"
              disabled={!text.trim()}
              aria-label="Send message"
              className="inline-flex size-9 items-center justify-center rounded-full bg-teal-500 text-on-primary transition-colors hover:bg-teal-300 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-teal-500"
            >
              <ArrowUpIcon className="size-4" />
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
