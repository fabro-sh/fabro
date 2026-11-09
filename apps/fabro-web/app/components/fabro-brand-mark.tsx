import { classNames } from "../lib/class-names";

/** The app's mark and highlight tile share the current palette everywhere. */
export function FabroBrandMark({ className }: { className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={classNames(
        "grid shrink-0 place-items-center rounded-[var(--radius-btn,var(--radius-md))] bg-[var(--brand-mark,var(--color-teal-500))] [background-image:var(--brand-mark-image,none)] text-[var(--brand-mark-fg,var(--color-on-primary))] [box-shadow:var(--e-signal,0_0_#0000)]",
        className,
      )}
    >
      <svg viewBox="0 0 272 348" fill="currentColor" className="h-[62.5%] w-auto">
        <path d="M1 237 L62 272 L61 348 L0 312 Z M98 257 L132 275 L132 312 L71 347 L70 272 Z M202 168 L201 230 L141 264 L142 202 Z M70 169 L132 203 L132 264 L70 230 Z M70 241 L90 251 L70 262 Z M3 129 L63 164 L61 262 L1 227 Z M137 125 L196 160 L137 195 L78 160 Z M271 44 L272 119 L211 154 L210 79 Z M132 43 L132 118 L71 153 L70 78 Z M142 43 L202 77 L201 152 L141 118 Z M1 44 L62 78 L62 152 L1 118 Z M206 0 L266 36 L206 72 L146 36 Z M66 1 L126 36 L66 71 L6 36 Z" />
      </svg>
    </span>
  );
}

/** The development account uses the app mark; personal avatars keep their image. */
export function UserAvatar({ avatarUrl, className }: { avatarUrl: string; className?: string }) {
  if (avatarUrl === "/images/logo.svg") return <FabroBrandMark className={className} />;
  return (
    <img
      alt=""
      src={avatarUrl}
      className={classNames("shrink-0 rounded-full outline -outline-offset-1 outline-line-strong", className)}
    />
  );
}
