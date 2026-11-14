import TerminalView from "../components/terminal-view";
import { ToastProvider } from "../components/toast";
import { usePageTitle } from "../lib/page-title";

export const handle = { title: "Terminal" };

export default function RunTerminal({ params }: { params: { id: string } }) {
  usePageTitle(["Terminal", params.id]);

  return (
    <ToastProvider>
      <div className="h-screen w-screen overflow-hidden">
        <TerminalView runId={params.id} chromeless />
      </div>
    </ToastProvider>
  );
}
