import {
  useCallback,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type EffectCallback,
  type RefObject,
} from "react";

/**
 * Synchronizes React with a resource that is created for the mounted lifetime
 * only. The returned cleanup is run on unmount, including Strict Mode remounts.
 */
export function useMountEffect(setup: EffectCallback): void {
  useEffect(setup, []);
}

/**
 * Synchronizes React with the browser timer queue. The interval is started
 * while `active` is true and is always cleared before the hook resubscribes or
 * unmounts.
 */
export function useInterval(
  callback: () => void,
  delayMs: number,
  active = true,
): void {
  const callbackRef = useRef(callback);
  callbackRef.current = callback;

  useEffect(() => {
    if (!active) return undefined;
    const id = setInterval(() => callbackRef.current(), delayMs);
    return () => clearInterval(id);
  }, [active, delayMs]);
}

/**
 * Synchronizes React with the browser timer queue. The timeout is scheduled
 * while `active` is true and is always cleared before it can fire after
 * unmount.
 */
export function useTimeout(
  callback: () => void,
  delayMs: number,
  active = true,
): void {
  const callbackRef = useRef(callback);
  callbackRef.current = callback;

  useEffect(() => {
    if (!active) return undefined;
    const id = setTimeout(() => callbackRef.current(), delayMs);
    return () => clearTimeout(id);
  }, [active, delayMs]);
}

/**
 * Synchronizes a value with the browser timer queue. Pending debounce timers are
 * cleared when the value or delay changes and on unmount.
 */
export function useDebouncedValue<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value);

  useEffect(() => {
    const id = setTimeout(() => setDebounced(value), delayMs);
    return () => clearTimeout(id);
  }, [value, delayMs]);

  return debounced;
}

/**
 * Shared body of the event-listener hooks below. The target is read when the
 * effect runs (so `window`/`document` absence and not-yet-mounted elements are
 * handled uniformly); the listener is removed before resubscribe and on unmount;
 * the handler sees the latest render without forcing a resubscribe. Callers must
 * pass a stable `options` value — a fresh object every render resubscribes the
 * listener every render.
 */
function useEventTargetEvent<E extends Event>(
  target: { readonly current: EventTarget | null },
  type: string,
  handler: (event: E) => void,
  options: AddEventListenerOptions | boolean | undefined,
  active: boolean,
): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(() => {
    const el = target.current;
    if (!active || !el) return undefined;
    const listener: EventListener = (event) => handlerRef.current(event as E);
    el.addEventListener(type, listener, options);
    return () => {
      el.removeEventListener(type, listener, options);
    };
  }, [active, options, target, type]);
}

// Lazy getters so `window`/`document` are only touched when an effect runs, never
// at module load (matching the previous per-hook `typeof window` checks).
const WINDOW_TARGET = {
  get current(): EventTarget | null {
    return typeof window === "undefined" ? null : window;
  },
};
const DOCUMENT_TARGET = {
  get current(): EventTarget | null {
    return typeof document === "undefined" ? null : document;
  },
};

/**
 * Synchronizes React with a browser `window` event listener. The listener is
 * removed before resubscribe and on unmount; the handler sees the latest render.
 */
export function useWindowEvent<K extends keyof WindowEventMap>(
  type: K,
  handler: (event: WindowEventMap[K]) => void,
  options?: AddEventListenerOptions | boolean,
  active = true,
): void {
  useEventTargetEvent(WINDOW_TARGET, type, handler, options, active);
}

/**
 * Synchronizes React with a browser `document` event listener. The listener is
 * removed before resubscribe and on unmount; the handler sees the latest render.
 */
export function useDocumentEvent<K extends keyof DocumentEventMap>(
  type: K,
  handler: (event: DocumentEventMap[K]) => void,
  options?: AddEventListenerOptions | boolean,
  active = true,
): void {
  useEventTargetEvent(DOCUMENT_TARGET, type, handler, options, active);
}

/**
 * Synchronizes React with an event listener on a ref'd element. The listener is
 * removed before resubscribe and on unmount; the handler sees the latest render.
 * Unlike a JSX event prop, this can pass `{ passive: false }` so the handler may
 * `preventDefault()` (e.g. to own wheel/⌘-scroll instead of the browser).
 *
 * The element must exist when the effect runs: if it renders conditionally, gate
 * with `active` on the same condition so the effect re-runs once it mounts.
 */
export function useElementEvent<K extends keyof HTMLElementEventMap>(
  ref: RefObject<HTMLElement | null>,
  type: K,
  handler: (event: HTMLElementEventMap[K]) => void,
  options?: AddEventListenerOptions | boolean,
  active = true,
): void {
  useEventTargetEvent(ref, type, handler, options, active);
}

/**
 * Synchronizes React rendering with a browser media query using
 * `useSyncExternalStore`. The media query listener is removed on unsubscribe.
 */
export function useMediaQuery(query: string, serverSnapshot = false): boolean {
  const subscribe = useCallback(
    (onStoreChange: () => void) => {
      if (typeof window === "undefined") return () => undefined;
      const mediaQuery = window.matchMedia(query);
      mediaQuery.addEventListener("change", onStoreChange);
      return () => mediaQuery.removeEventListener("change", onStoreChange);
    },
    [query],
  );
  const getSnapshot = useCallback(
    () => typeof window !== "undefined" && window.matchMedia(query).matches,
    [query],
  );
  const getServerSnapshot = useCallback(
    () => serverSnapshot,
    [serverSnapshot],
  );

  return useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
}

/**
 * Synchronizes React rendering with `window.location.hash` using
 * `useSyncExternalStore`. The `hashchange` listener is removed on unsubscribe.
 */
export function useLocationHash(serverSnapshot = ""): string {
  const subscribe = useCallback((onStoreChange: () => void) => {
    if (typeof window === "undefined") return () => undefined;
    window.addEventListener("hashchange", onStoreChange);
    return () => window.removeEventListener("hashchange", onStoreChange);
  }, []);
  const getSnapshot = useCallback(
    () => typeof window === "undefined" ? serverSnapshot : window.location.hash,
    [serverSnapshot],
  );
  const getServerSnapshot = useCallback(
    () => serverSnapshot,
    [serverSnapshot],
  );

  return useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
}

/**
 * Synchronizes React with a browser `ResizeObserver`. The observer is
 * disconnected before resubscribe and on unmount; the callback sees the latest
 * render.
 */
export function useResizeObserver<T extends Element>(
  ref: RefObject<T | null>,
  callback: ResizeObserverCallback,
  active = true,
): void {
  const callbackRef = useRef(callback);
  callbackRef.current = callback;

  useEffect(() => {
    if (!active || typeof ResizeObserver === "undefined") return undefined;
    const node = ref.current;
    if (!node) return undefined;
    const observer = new ResizeObserver((entries, resizeObserver) => {
      callbackRef.current(entries, resizeObserver);
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, [active, ref]);
}
