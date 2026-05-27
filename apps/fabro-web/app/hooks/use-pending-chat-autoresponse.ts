import { useEffect, useRef } from "react";

/**
 * Synchronizes a pending scripted chat response with assistant-ui's imperative
 * runtime. There is no resource to clean up; duplicate Strict Mode calls are
 * harmless because the local ref dedupes a mount cycle and the chat store flag
 * dedupes remounts.
 */
export function usePendingChatAutoresponse({
  chatId,
  pendingResponse,
  consumePendingResponse,
  startRun,
}: {
  chatId: string;
  pendingResponse: boolean;
  consumePendingResponse: (chatId: string) => void;
  startRun: () => void;
}) {
  const startRunRef = useRef(startRun);
  startRunRef.current = startRun;
  const didStartRef = useRef(false);

  useEffect(() => {
    if (!pendingResponse || didStartRef.current) return;
    didStartRef.current = true;
    consumePendingResponse(chatId);
    startRunRef.current();
  }, [chatId, consumePendingResponse, pendingResponse]);
}
