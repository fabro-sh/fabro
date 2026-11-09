import {
  ClockIcon,
  Cog6ToothIcon,
  PlayIcon,
  ShareIcon,
} from "@heroicons/react/24/outline";
import type { ComponentType, SVGProps } from "react";

export interface NavigationItem {
  name: string;
  href: string;
  icon: ComponentType<SVGProps<SVGSVGElement>>;
  /** What's there, in a few words. Shown on hover and focus. */
  purpose: string;
  /** Where it sits: with the main destinations, or at the foot of the sidebar. */
  placement: "main" | "footer";
}

/** Where the global Create button lands: a clean starter draft on the canvas. */
export const createWorkflowHref = "/workflows/new";
export const createWorkflowPurpose = "Start a new workflow on a blank canvas.";

export const navigation: NavigationItem[] = [
  {
    name: "Automations",
    href: "/automations",
    icon: ClockIcon,
    purpose: "Start runs on a schedule or from a trigger.",
    placement: "main",
  },
  {
    name: "Runs",
    href: "/runs",
    icon: PlayIcon,
    purpose: "Follow workflows as they run and review what they did.",
    placement: "main",
  },
  {
    name: "Workflows",
    href: "/workflows",
    icon: ShareIcon,
    purpose: "Build and edit workflow drafts saved in this browser.",
    placement: "main",
  },
  {
    name: "Settings",
    href: "/settings",
    icon: Cog6ToothIcon,
    purpose: "Configure models, sandboxes, integrations and appearance.",
    placement: "footer",
  },
];

export function isNavigationItemActive(item: NavigationItem, pathname: string): boolean {
  return pathname === item.href || pathname.startsWith(`${item.href}/`);
}
