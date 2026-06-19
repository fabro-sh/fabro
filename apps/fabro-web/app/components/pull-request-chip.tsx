import type { ReactNode } from "react";
import type { PullRequestProvider } from "@qltysh/fabro-api-client";

import { GitPullRequestIcon } from "./icons";

export function pullRequestDisplay(provider?: PullRequestProvider | null): {
  label: string;
  prefix: string;
} {
  if (provider === "gitlab") {
    return { label: "Merge request", prefix: "!" };
  }
  return { label: "Pull request", prefix: "#" };
}

export function PullRequestChip({
  number,
  provider,
  url,
  className = "inline-flex items-center gap-1.5 font-mono text-xs text-fg-muted",
  iconClassName = "size-3",
  children,
}: {
  number: number;
  provider?: PullRequestProvider | null;
  url?: string;
  className?: string;
  iconClassName?: string;
  children?: ReactNode;
}) {
  const display = pullRequestDisplay(provider);
  const content = (
    <>
      <GitPullRequestIcon className={iconClassName} />
      {`${display.prefix}${number}`}
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
