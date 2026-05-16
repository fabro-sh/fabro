import type { ToolCallMessagePartProps } from "@assistant-ui/react";
import { WrenchScrewdriverIcon } from "@heroicons/react/24/outline";

export default function ToolFallback(props: ToolCallMessagePartProps) {
  const { toolName, args, result } = props;
  return (
    <div className="my-2 rounded-lg border border-line bg-overlay/70 text-fg-2">
      <div className="flex items-center gap-2 border-b border-line px-3 py-2 text-xs font-medium uppercase tracking-wide text-fg-muted">
        <WrenchScrewdriverIcon className="size-3.5" />
        <span>tool</span>
        <code className="rounded bg-overlay-strong px-1.5 py-0.5 font-mono text-[11px] normal-case tracking-normal text-fg">
          {toolName}
        </code>
      </div>
      <Section label="arguments">
        <pre className="whitespace-pre-wrap break-words font-mono text-[12px] leading-relaxed text-fg-2">
          {formatJson(args)}
        </pre>
      </Section>
      {result !== undefined && (
        <Section label="result">
          <pre className="whitespace-pre-wrap break-words font-mono text-[12px] leading-relaxed text-fg-2">
            {formatJson(result)}
          </pre>
        </Section>
      )}
    </div>
  );
}

function Section({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="border-b border-line last:border-b-0">
      <div className="px-3 pt-2 text-[10px] font-medium uppercase tracking-wider text-fg-muted">
        {label}
      </div>
      <div className="px-3 pb-2">{children}</div>
    </div>
  );
}

function formatJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
