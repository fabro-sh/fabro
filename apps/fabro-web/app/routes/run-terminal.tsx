import { useEffect } from "react";
import { Toaster } from "sonner";

import TerminalView from "../components/terminal-view";
import { ToastProvider } from "../components/toast";

export default function RunTerminal({ params }: { params: { id: string } }) {
  useEffect(() => {
    const previous = document.title;
    document.title = `Terminal · ${params.id} · Fabro`;
    return () => {
      document.title = previous;
    };
  }, [params.id]);

  return (
    <ToastProvider>
      <div className="h-screen w-screen overflow-hidden">
        <TerminalView runId={params.id} chromeless />
      </div>
      {typeof document !== "undefined" && (
        <Toaster richColors position="bottom-right" />
      )}
    </ToastProvider>
  );
}
