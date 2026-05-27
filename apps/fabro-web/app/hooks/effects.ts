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
 * Synchronizes React with a browser `window` event listener. The listener is
 * removed before resubscribe and on unmount; the handler sees the latest render.
 */
export function useWindowEvent<K extends keyof WindowEventMap>(
  type: K,
  handler: (event: WindowEventMap[K]) => void,
  options?: AddEventListenerOptions | boolean,
  active = true,
): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(() => {
    if (!active || typeof window === "undefined") return undefined;
    const listener = (event: WindowEventMap[K]) => handlerRef.current(event);
    window.addEventListener(type, listener as EventListener, options);
    return () => {
      window.removeEventListener(type, listener as EventListener, options);
    };
  }, [active, options, type]);
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
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(() => {
    if (!active || typeof document === "undefined") return undefined;
    const listener = (event: DocumentEventMap[K]) => handlerRef.current(event);
    document.addEventListener(type, listener as EventListener, options);
    return () => {
      document.removeEventListener(type, listener as EventListener, options);
    };
  }, [active, options, type]);
}

/**
 * Synchronizes React with `document.title`. The previous title is restored when
 * the title changes or the component unmounts.
 */
export function useDocumentTitle(title: string): void {
  useEffect(() => {
    if (typeof document === "undefined") return undefined;
    const previous = document.title;
    document.title = title;
    return () => {
      document.title = previous;
    };
  }, [title]);
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
