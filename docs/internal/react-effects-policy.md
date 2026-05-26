# React Effects Policy

This document defines how `apps/fabro-web` should use React effects.

The goal is not to hide `useEffect` behind nicer names. The goal is to keep
component data flow declarative, localize real external integrations, and make
the codebase easier for people and agents to reason about.

## Policy

Do not call `useEffect` directly from route or component code.

New code should treat every direct `useEffect`, `React.useEffect`,
`useLayoutEffect`, or `useInsertionEffect` call as a policy violation unless it
lives inside an approved integration hook.

The only generic effect primitive exposed to component code should be
`useMountEffect`, and it is only for true mount/unmount integrations. Prefer a
purpose-named hook over `useMountEffect` whenever the integration has domain
meaning, such as `useRunEvents(runId)`, `useDocumentTitle(title)`, or
`useWindowEvent(...)`.

`useMountEffect` must not become a way to opt out of React dependencies. If an
integration depends on a changing identity, that identity belongs in the API of
a purpose-named hook or in a keyed component boundary.

Existing direct effects should be migrated opportunistically when touching the
same area. Do not make a behavior-preserving effect harder to understand just to
remove the word `useEffect`; the replacement must improve or preserve clarity,
testability, and lifecycle correctness.

## What Counts As An External Integration

Effects are only for synchronizing React with a system outside React.

Allowed external systems include:

- browser globals: `window`, `document`, history, media queries, clipboard, focus
- browser resources: timers, animation frames, `ResizeObserver`, `MutationObserver`
- network streams and sockets: `EventSource`, WebSocket, cross-tab channels
- imperative third-party widgets that must be constructed, attached, and disposed
- durable browser storage when the write cannot happen in an event handler
- external notifications such as analytics or telemetry for a route/view becoming
  visible, when they are safe under Strict Mode and do not perform user-visible
  writes

These are not external systems for this policy:

- props
- React state
- SWR data
- derived values
- route params
- search params used only for rendering
- mutation result objects
- "after this state changes, do another state update"

If the effect mostly moves data from one React value to another React value, it
is almost certainly the wrong tool.

## Preferred Alternatives

### Derive during render

If a value can be computed from props, route params, query data, or state, compute
it during render. Use `useMemo` only when the computation is expensive or object
identity matters to a child API.

Avoid:

```tsx
const [filtered, setFiltered] = useState<Item[]>([]);

useEffect(() => {
  setFiltered(items.filter(matchesQuery));
}, [items, matchesQuery]);
```

Prefer:

```tsx
const filtered = useMemo(
  () => items.filter(matchesQuery),
  [items, matchesQuery],
);
```

### Handle events in event handlers

If the work is caused by a click, submit, key press, or mutation trigger, do the
work from that event path. Do not set a flag and wait for an effect to notice it.

Avoid watching mutation data just to show a toast or navigate. Prefer mutation
callbacks, an explicit `try`/`catch` around `trigger(...)`, or a route action
result consumed by the same event flow.

### Use SWR for server state

Server reads belong in shared query hooks in `app/lib/queries.ts` or an adjacent
domain query module. Do not fetch server data in a component effect.

Use SWR options such as `keepPreviousData`, `refreshInterval`,
`revalidateOnFocus`, and `shouldRetryOnError` instead of local effect state when
they describe the behavior directly.

Polling that is not a normal SWR refresh should live in a purpose-named hook or a
small state machine, not inline in a route component.

### Use mutations for writes

Writes should happen in event handlers, route actions, or shared mutation hooks.
Success and failure handling should stay on the write path.

If many callers need the same success behavior, put that behavior in the shared
mutation hook instead of making every component watch `mutation.data`.

### Use `key` to reset local state

When state should reset because an identity changed, prefer a keyed component
boundary.

Avoid:

```tsx
function Details({ selectedId }: Props) {
  const [tab, setTab] = useState("summary");

  useEffect(() => {
    setTab("summary");
  }, [selectedId]);
}
```

Prefer:

```tsx
function DetailsRoute({ selectedId }: Props) {
  return <Details key={selectedId} selectedId={selectedId} />;
}

function Details({ selectedId }: Props) {
  const [tab, setTab] = useState("summary");
}
```

Use a reducer when only part of the state should reset or when the reset is part
of an explicit domain transition.

### Use URL and router primitives

Route and URL state should be the source of truth for route-owned preferences.
Parse search params during render, and update them from event handlers.

Prefer route loader/action redirects when route data or auth determines the
redirect. Use `navigate(...)` from the event path for user-initiated navigation.
Use `<Navigate replace />` sparingly for render-known route gates when the
temporary null or fallback frame is acceptable.

Avoid `navigate(...)` in an effect unless the navigation follows an asynchronous
external result that cannot be represented by a loader, action, mutation callback,
or render-time route gate.

### Use `useSyncExternalStore` for external stores

When React renders from a mutable external store or browser source, prefer
`useSyncExternalStore` over an effect that subscribes and mirrors a snapshot into
local state.

Good candidates include cross-tab stores, browser storage-backed state, and
imperative models where React needs a consistent current snapshot.

### Use refs deliberately

A ref can hold an imperative handle or the latest value for a stable callback
passed to an external integration. Updating `ref.current` during render is
acceptable when the ref is not used to render UI.

In React 19, prefer `useEffectEvent` inside approved hooks when an effect-owned
timer, listener, subscription, or third-party callback must see the latest props
or state without forcing the external resource to resubscribe. Use refs for
imperative objects and for APIs that cannot call an Effect Event directly.

Do not use refs to avoid dependency arrays while still depending on changing
React data. That usually hides temporal coupling instead of removing it.

## Approved Effect Hooks

Approved hooks may call React effects internally. They should expose the
external integration they manage and keep dependency behavior obvious at the call
site.

Recommended primitives:

- `useMountEffect(setup)` for mount/unmount-only setup
- `useInterval(callback, delayMs, active?)`
- `useTimeout(callback, delayMs, active?)`
- `useDebouncedValue(value, delayMs)`
- `useWindowEvent(type, handler, options?)`
- `useDocumentTitle(title)`
- `useMediaQuery(query)`
- `useResizeObserver(ref, callback)`
- `useSseSubscription(...)`
- domain hooks such as `useRunEvents(runId)` and `useBoardEvents()`

Approved hooks should separate resource identity from non-reactive callbacks.
Values that decide what resource exists, such as `runId`, URL, media query, or
delay, should be explicit hook inputs that control setup and cleanup. Callback
bodies that only need the latest committed React values should use
`useEffectEvent` internally instead of ref mirrors when that API fits.

`useMountEffect` should have no dependency array at the call site. If the setup
depends on a changing identity, make that identity explicit by:

- rendering a keyed child so the integration remounts for that identity
- writing a purpose-named hook whose API says what identity controls the resource
- using an event handler or router/data primitive instead, if no external
  resource exists

New approved hooks should include a short doc comment naming the external system
they synchronize with and the cleanup guarantees they provide. For one-shot
notification hooks with no cleanup, document why duplicate development calls are
harmless.

## `useMountEffect` Rules

`useMountEffect` is allowed for resource setup only when all of these are true:

- the code attaches to, creates, starts, or subscribes to an external resource
- the cleanup detaches, disposes, stops, or unsubscribes from that resource
- the effect is not deriving React state from React inputs
- the setup does not read changing props, state, route params, search params, or
  SWR data unless those values are stable for the mounted lifetime by construction
- the setup is safe under React Strict Mode mount/unmount/remount behavior
- the component still renders a correct initial frame before the effect runs

Good examples:

- open an `EventSource` and close it on unmount
- create an xterm terminal instance for a DOM node and dispose it on unmount
- add a `window` event listener and remove it on unmount
- start a timer whose only purpose is to tick a clock display

Bad examples:

- copy `props.title` into local state
- copy SWR data into local state
- inspect a mutation result and then show a toast
- repair a URL after the first render
- reset selection because a prop changed
- fetch data on mount when a query hook can own the request

### One-shot external notifications

Some effects legitimately notify an external system because a route or view
became visible, such as analytics, telemetry, or impression tracking. Do not use
`useMountEffect` for these unless there is also a real resource to clean up.
Prefer a purpose-named hook such as `usePageVisit(url)` or
`useImpressionEvent(id)`.

One-shot notification hooks must be harmless under Strict Mode's development
mount/unmount/remount cycle. They should be disabled, de-duplicated, or directed
away from production metrics in development and tests. They must not perform
user-visible writes, billable actions, purchases, destructive mutations, or any
operation whose duplicate execution would be observable to the user.

## Migration Workflow

Use this workflow when auditing existing direct effects.

1. List direct effect usage:

   ```sh
   rg -n "\buseEffect\b|React\.useEffect|\buse(Layout|Insertion)?Effect\b" apps/fabro-web/app --glob '*.{ts,tsx}'
   ```

2. For each hit, classify it:

   - `derived-state`: replace with render-time derivation, `useMemo`, reducer, or keyed remount
   - `event-reaction`: move into the event handler, mutation callback, route action, or submit path
   - `server-data`: move into SWR query/mutation hooks
   - `url-router`: move into URL-derived render state, event-time URL updates, loader, or `<Navigate>`
   - `external-integration`: move into `useMountEffect` or a purpose-named integration hook
   - `imperative-dom`: move into a narrow DOM hook such as `useDocumentTitle`, `useWindowEvent`, or `useResizeObserver`
   - `one-shot-notification`: move into a purpose-named analytics/telemetry hook with Strict Mode behavior documented

3. Write down the replacement before editing. If the replacement is less clear,
   keep researching instead of performing a mechanical rewrite.

4. Preserve the user-visible initial frame. The migration should not introduce a
   flash that the old code avoided.

5. Add or update focused tests for behavior that previously depended on effect
   timing, especially redirects, toasts, focus, polling, and state resets.

6. After migration, run:

   ```sh
   rg -n "\buseEffect\b|React\.useEffect|\buse(Layout|Insertion)?Effect\b" apps/fabro-web/app --glob '*.{ts,tsx}'
   cd apps/fabro-web && bun test
   cd apps/fabro-web && bun run typecheck
   ```

## Existing Hotspots

Based on the current codebase survey, prioritize these areas first:

- `routes/run-detail.tsx`: mutation-result watcher effects for preview and
  lifecycle toasts. Prefer moving success handling into the mutation/action path.
- `routes/run-files.tsx`: several effects are legitimate DOM/timer bridges, but
  they should be extracted into named hooks. The SWR data/ref bridge needs a
  careful replacement that preserves failed-revalidation behavior.
- `install-app.tsx`: session loading and health polling are component-level
  async effects. Prefer SWR/query hooks or a small install state machine before
  enforcing the policy there.
- state reset effects in run stages, child runs, file trees, and filesystem
  panels. Prefer keyed boundaries or reducers where they keep ownership clearer.
- repeated timer/media-query/focus/document-title/listener effects. Replace with
  shared hooks before auditing the harder cases.

## Enforcement

Enforcement should happen after the initial wrapper hooks exist. Until then,
reviewers should request a replacement plan for any new direct effect and PR
descriptions for effect migrations should name the category being removed.

Do not add a lint or CI gate until the approved hook surface exists and the
initial migration path is clear.

## Review Checklist

When reviewing React code, ask:

- Does the component render correctly before any effect runs?
- Is this effect synchronizing with a real external system?
- Could this value be derived during render?
- Could this happen in the event handler that caused it?
- Could SWR or a route action own this data flow?
- Is a `key` boundary a clearer reset than a reset effect?
- Does cleanup exactly undo setup?
- Is the Strict Mode double-mount behavior harmless?
- Is the dependency behavior visible in the API, rather than hidden in refs?
- Did the migration reduce temporal coupling instead of moving it elsewhere?

If the answer is unclear, keep the effect local until the correct abstraction is
obvious. A vague wrapper is worse than an honest direct effect.
