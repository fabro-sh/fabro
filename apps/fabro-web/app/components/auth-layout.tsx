import type { ReactNode } from "react";

function FabroLogo({ className }: { className?: string }) {
  return (
    <img
      src="/images/logo.svg"
      alt="Fabro"
      className={className}
      draggable={false}
    />
  );
}

export function AuthLayout({
  children,
  footer,
}: {
  children: ReactNode;
  footer?: ReactNode;
}) {
  return (
    <div className="flex min-h-screen flex-col items-center justify-center bg-atmosphere px-4">
      <div className="w-full max-w-sm">
        <div className="mb-8 flex justify-center">
          <FabroLogo className="h-12 w-12" />
        </div>
        <div className="rounded-xl border border-line bg-panel/80 p-8 shadow-lg backdrop-blur-sm">
          {children}
        </div>
        {footer && (
          <div className="mt-4 text-center text-xs text-fg-muted">
            {footer}
          </div>
        )}
      </div>
    </div>
  );
}
