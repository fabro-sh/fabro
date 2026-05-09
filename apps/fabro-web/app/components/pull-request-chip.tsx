import type { ReactNode } from "react";

import { GitPullRequestIcon } from "./icons";

export function PullRequestChip({
  number,
  url,
  className = "inline-flex items-center gap-1.5 font-mono text-xs text-fg-muted",
  iconClassName = "size-3",
  children,
}: {
  number: number;
  url?: string;
  className?: string;
  iconClassName?: string;
  children?: ReactNode;
}) {
  const content = (
    <>
      <GitPullRequestIcon className={iconClassName} />
      #{number}
      {children}
    </>
  );

  if (url == null) {
    return <span className={className}>{content}</span>;
  }

  return (
    <a
      href={url}
      target="_blank"
      rel="noreferrer"
      className={`${className} hover:text-fg`}
    >
      {content}
    </a>
  );
}
