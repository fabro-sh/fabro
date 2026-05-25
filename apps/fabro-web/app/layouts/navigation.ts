import {
  ClockIcon,
  Cog6ToothIcon,
  PlayIcon,
} from "@heroicons/react/24/outline";

export const allNavigation = [
  { name: "Automations", href: "/automations", icon: ClockIcon, demoOnly: true },
  { name: "Runs", href: "/runs", icon: PlayIcon, demoOnly: false },
  { name: "Settings", href: "/settings", icon: Cog6ToothIcon, demoOnly: false },
];

export function getVisibleNavigation(demoMode: boolean) {
  return allNavigation.filter((item) => !item.demoOnly || demoMode);
}
