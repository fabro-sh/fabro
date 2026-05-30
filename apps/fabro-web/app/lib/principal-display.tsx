import type { ReactNode } from "react";
import {
  BoltIcon,
  ChatBubbleLeftEllipsisIcon,
  Cog6ToothIcon,
  CpuChipIcon,
  ServerIcon,
} from "@heroicons/react/20/solid";
import type { Principal } from "@qltysh/fabro-api-client";

export interface PrincipalDisplay {
  glyph: ReactNode;
  label: string;
}

function principalIconGlyph(icon: ReactNode) {
  return (
    <span className="grid size-5 place-items-center rounded-full bg-teal-500/20 text-teal-500">
      {icon}
    </span>
  );
}

export function principalDisplay(actor: Principal): PrincipalDisplay {
  switch (actor.kind) {
    case "user": {
      let glyph: ReactNode;
      if (actor.avatar_url) {
        glyph = (
          <img
            alt=""
            src={actor.avatar_url}
            className="size-5 rounded-full outline -outline-offset-1 outline-line-strong"
          />
        );
      } else {
        const initial = actor.login.charAt(0).toUpperCase() || "?";
        glyph = (
          <span className="grid size-5 place-items-center rounded-full bg-teal-500/20 font-mono text-[10px] font-medium text-teal-500">
            {initial}
          </span>
        );
      }
      return { glyph, label: actor.login };
    }
    case "agent":
      return { glyph: principalIconGlyph(<CpuChipIcon className="size-3" />), label: "agent" };
    case "system":
      return { glyph: principalIconGlyph(<Cog6ToothIcon className="size-3" />), label: "system" };
    case "slack":
      return {
        glyph: principalIconGlyph(<ChatBubbleLeftEllipsisIcon className="size-3" />),
        label: "slack",
      };
    case "webhook":
      return { glyph: principalIconGlyph(<BoltIcon className="size-3" />), label: "webhook" };
    case "worker":
      return { glyph: principalIconGlyph(<ServerIcon className="size-3" />), label: "worker" };
  }
}
