import Playground from "../components/playground/playground";

// hideHeader/fullHeight/wide mirror the chats / ask-fabro routes so the
// workspace and the docked playground sidebar bleed edge-to-edge below the
// top nav. AppShell already auth-gates this tree, so `authMode="required"`
// here is descriptive (it tells the Playground component what guarantee
// its parent provides), not enforced inside the playground itself.
export const handle = { hideHeader: true, fullHeight: true, wide: true };

export function meta() {
  return [{ title: "Playground — Fabro" }];
}

export default function PlaygroundRoute() {
  return (
    <Playground
      chatEndpoint="/api/v1/playground/chat"
      authMode="required"
    />
  );
}
