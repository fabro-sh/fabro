import { ArrowTopRightOnSquareIcon } from "@heroicons/react/20/solid";
import type { ReviewTarget } from "@qltysh/fabro-api-client";

/**
 * Re-check the URL before putting it in an `href`. The server already rejects
 * unsafe targets (see `ReviewTarget::new` in
 * `lib/foundation/fabro-types/src/interview.rs`), but React does not sanitize
 * `href`, so a `javascript:` URL reaching this component would execute. Length
 * and control-character limits stay server-side; they cannot affect the DOM.
 */
export function safeReviewTarget(
  target: ReviewTarget | null | undefined,
): ReviewTarget | null {
  if (!target?.url || !target.label) return null;
  try {
    const parsed = new URL(target.url);
    const safe =
      (parsed.protocol === "http:" || parsed.protocol === "https:") &&
      Boolean(parsed.host) &&
      !parsed.username &&
      !parsed.password;
    return safe ? target : null;
  } catch {
    return null;
  }
}

/**
 * The review question sentence, with the target label as an external link.
 * Mirrors `ReviewTarget::question_text_with_link` in
 * `lib/foundation/fabro-types/src/interview.rs`.
 */
export function ReviewTargetQuestion({
  target,
  className,
}: {
  target: ReviewTarget;
  className?: string;
}) {
  return (
    <p className={className}>
      Review the{" "}
      <a
        href={target.url}
        target="_blank"
        rel="noopener noreferrer"
        referrerPolicy="no-referrer"
        className="inline-flex items-baseline gap-1 font-semibold text-teal-300 underline decoration-teal-500/50 underline-offset-2 transition-colors hover:text-fg focus-visible:rounded-sm focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-current"
      >
        <span>{target.label}</span>
        <ArrowTopRightOnSquareIcon
          className="size-3 shrink-0 self-center"
          aria-hidden="true"
        />
      </a>{" "}
      {target.kind}, then choose the next action.
    </p>
  );
}
