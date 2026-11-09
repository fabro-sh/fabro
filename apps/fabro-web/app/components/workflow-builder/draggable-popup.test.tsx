import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";
import type { MouseEventHandler, ReactNode } from "react";
import TestRenderer, { act } from "react-test-renderer";

import { setupReactTestEnv } from "../../lib/test-utils";
import { clampPopup, placeByAnchor, popupArea, popupSize } from "./use-draggable-popup";

// Headless UI's Dialog needs a DOM. Its focus trap, Escape and click handling are its own and
// are checked in the browser. These tests cover what the popup hands it.
let dialogProps: Record<string, unknown> = {};
const actualHeadless = await import("@headlessui/react");
mock.module("@headlessui/react", () => ({
  ...actualHeadless,
  Dialog: ({ children, ...props }: { children: ReactNode; className?: string }) => {
    dialogProps = props;
    return <div className={props.className}>{children}</div>;
  },
  DialogPanel: ({
    children,
    className,
    onMouseDown,
  }: {
    children: ReactNode;
    className?: string;
    onMouseDown?: MouseEventHandler;
  }) => (
    <div className={className} onMouseDown={onMouseDown}>
      {children}
    </div>
  ),
}));

const { DraggablePopup } = await import("./draggable-popup");

const OVERHANG = 32;
const SIZE = { width: 400, height: 544 };
const DESKTOP = { left: 0, top: 0, width: 1000, height: 800 };
const ANCHOR = { left: 660, top: 100, width: 300, height: 44 };

describe("where the frame may sit", () => {
  const area = popupArea(DESKTOP, OVERHANG);

  test("the area keeps a margin on every side, and room for the tabs above", () => {
    expect(area).toEqual({ left: 12, top: 36, right: 988, bottom: 788 });
  });

  test("the tabs stay on screen however far up it is pushed", () => {
    const { y } = clampPopup({ x: 0, y: -5000 }, SIZE, area);
    expect(y - OVERHANG).toBeGreaterThanOrEqual(DESKTOP.top);
  });

  test("the frame stays on screen however far it is pushed", () => {
    expect(clampPopup({ x: -5000, y: -5000 }, SIZE, area)).toEqual({ x: 12, y: 36 });
    expect(clampPopup({ x: 5000, y: 5000 }, SIZE, area)).toEqual({ x: 588, y: 244 });
  });

  test("a phone gets the full width less the margins", () => {
    const phone = popupArea({ left: 0, top: 0, width: 320, height: 640 }, OVERHANG);
    expect(popupSize(SIZE, phone)).toEqual({ width: 296, height: 544 });
  });

  test("a short screen gets a shorter frame rather than one that runs off the bottom", () => {
    const short = popupArea({ left: 0, top: 0, width: 800, height: 320 }, OVERHANG);
    expect(popupSize(SIZE, short)).toEqual({ width: 400, height: 272 });
  });

  test("the visual viewport's offset counts, as when a phone's keyboard scrolls the page", () => {
    const keyboardOpen = popupArea({ left: 0, top: 100, width: 320, height: 300 }, OVERHANG);
    const size = popupSize(SIZE, keyboardOpen);
    const { y } = clampPopup({ x: 0, y: 0 }, size, keyboardOpen);
    expect(y - OVERHANG).toBeGreaterThanOrEqual(100);
    expect(y + size.height).toBeLessThanOrEqual(400);
  });
});

describe("opening by the anchor", () => {
  const area = popupArea(DESKTOP, OVERHANG);

  test("sits below with right edges lined up, leaving room for the tabs so they don't cover the anchor", () => {
    const { x, y } = placeByAnchor(ANCHOR, SIZE, area, OVERHANG);
    expect(x).toBe(ANCHOR.left + ANCHOR.width - SIZE.width);
    expect(y - OVERHANG).toBeGreaterThanOrEqual(ANCHOR.top + ANCHOR.height);
  });

  test("goes above when below has too little room and above has more", () => {
    const low = { ...ANCHOR, top: 620 };
    const { y } = placeByAnchor(low, SIZE, area, OVERHANG);
    expect(y + SIZE.height).toBeLessThanOrEqual(low.top);
    expect(y - OVERHANG).toBeGreaterThanOrEqual(0);
  });

  test("stays on screen when neither side fits", () => {
    const short = popupArea({ left: 0, top: 0, width: 1000, height: 500 }, OVERHANG);
    const size = popupSize(SIZE, short);
    expect(placeByAnchor({ ...ANCHOR, top: 200 }, size, short, OVERHANG).y).toBe(short.top);
  });

  test("an anchor near the left edge doesn't push the frame off it", () => {
    expect(placeByAnchor({ ...ANCHOR, left: 20, width: 100 }, SIZE, area, OVERHANG).x).toBe(12);
  });

  test("with no anchor to read, it centers", () => {
    const { x, y } = placeByAnchor(null, SIZE, area, OVERHANG);
    expect(x).toBe((12 + 988 - 400) / 2);
    expect(y).toBe((36 + 788 - 544) / 2);
  });
});

// ── The window ──────────────────────────────────────────────────────────────

interface FakeWindow {
  innerWidth: number;
  innerHeight: number;
  visualViewport?: EventTarget & { offsetLeft: number; offsetTop: number; width: number; height: number };
  addEventListener: (type: string, listener: (event: unknown) => void) => void;
  removeEventListener: (type: string, listener: (event: unknown) => void) => void;
}

const previousWindow = globalThis.window;
const listeners = new Map<string, Set<(event: unknown) => void>>();
let fakeWindow: FakeWindow;
let teardown: () => void;

function fire(type: string, event: unknown = {}) {
  act(() => {
    for (const listener of [...(listeners.get(type) ?? [])]) listener(event);
  });
}

function resizeTo(width: number, height: number) {
  fakeWindow.innerWidth = width;
  fakeWindow.innerHeight = height;
  fire("resize");
}

beforeEach(() => {
  teardown = setupReactTestEnv();
  listeners.clear();
  fakeWindow = {
    innerWidth: DESKTOP.width,
    innerHeight: DESKTOP.height,
    addEventListener: (type, listener) => {
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type)?.add(listener);
    },
    removeEventListener: (type, listener) => void listeners.get(type)?.delete(listener),
  };
  Object.defineProperty(globalThis, "window", { configurable: true, writable: true, value: fakeWindow });
});

afterEach(() => {
  teardown();
  Object.defineProperty(globalThis, "window", { configurable: true, writable: true, value: previousWindow });
});

function mount() {
  const onClose = mock(() => {});
  const anchorRef = {
    current: { getBoundingClientRect: () => ANCHOR } as unknown as HTMLElement,
  };
  const popup = (content: string) => (
    <DraggablePopup
      label="Choose model"
      moveLabel="Move model picker"
      closeLabel="Close model picker"
      anchorRef={anchorRef}
      preferredSize={SIZE}
      onClose={onClose}
    >
      <p>{content}</p>
    </DraggablePopup>
  );
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(popup("first"));
  });
  const { root } = renderer;
  const frame = () => root.find((node) => node.type === "div" && node.props.className === "wfb-popup");
  const grip = () => root.findByProps({ "aria-label": "Move model picker" });
  const capture = mock((_pointerId: number) => {});
  const pointer = (clientX: number, clientY: number, overrides: Record<string, unknown> = {}) => ({
    button: 0,
    pointerId: 1,
    clientX,
    clientY,
    currentTarget: { setPointerCapture: capture },
    ...overrides,
  });
  return {
    root,
    renderer,
    onClose,
    capture,
    frame,
    grip,
    pointer,
    at: () => ({ left: frame().props.style.left, top: frame().props.style.top }),
    update: (content: string) => act(() => renderer.update(popup(content))),
    down: (x: number, y: number, overrides?: Record<string, unknown>) =>
      act(() => grip().props.onPointerDown(pointer(x, y, overrides))),
    move: (x: number, y: number, overrides?: Record<string, unknown>) =>
      act(() => grip().props.onPointerMove(pointer(x, y, overrides))),
    up: (overrides?: Record<string, unknown>) => act(() => grip().props.onPointerUp(pointer(0, 0, overrides))),
    press: (key: string, shiftKey = false) => {
      const preventDefault = mock(() => {});
      act(() => grip().props.onKeyDown({ key, shiftKey, preventDefault }));
      return preventDefault;
    },
  };
}

describe("the first frame", () => {
  test("is already placed by the anchor, fixed and sized, with no flash", () => {
    const { frame } = mount();
    expect(frame().props.style).toEqual({
      position: "fixed",
      left: ANCHOR.left + ANCHOR.width - SIZE.width,
      top: ANCHOR.top + ANCHOR.height + 6 + OVERHANG,
      width: SIZE.width,
      height: SIZE.height,
    });
  });
});

describe("dragging by the grip", () => {
  test("captures the pointer and follows it", () => {
    const { capture, frame, at, down, move, up } = mount();
    const start = at();

    down(100, 100);
    expect(capture).toHaveBeenCalledWith(1);
    expect(frame().props["data-dragging"]).toBe(true);

    move(70, 130);
    expect(at()).toEqual({ left: start.left - 30, top: start.top + 30 });

    up();
    expect(frame().props["data-dragging"]).toBeUndefined();
  });

  test("stops at the screen's edges, with the tabs still on screen, and comes back without lag", () => {
    const { at, down, move } = mount();
    const start = at();
    down(100, 100);

    move(-5000, -5000);
    expect(at()).toEqual({ left: 12, top: 36 });
    move(5000, 5000);
    expect(at()).toEqual({ left: 588, top: 244 });
    move(100, 100);
    expect(at()).toEqual(start);
  });

  test("moving after the drag ended does nothing", () => {
    const { at, down, move, up } = mount();
    down(100, 100);
    move(90, 90);
    up();
    const dropped = at();
    move(0, 0);
    expect(at()).toEqual(dropped);
  });

  test("a cancelled gesture ends the drag where it was", () => {
    const { frame, at, down, move, grip, pointer } = mount();
    down(100, 100);
    move(90, 90);
    const held = at();
    act(() => grip().props.onPointerCancel(pointer(0, 0)));
    expect(frame().props["data-dragging"]).toBeUndefined();
    move(0, 0);
    expect(at()).toEqual(held);
  });

  test("a right click doesn't start one", () => {
    const { capture, at, down, move } = mount();
    const start = at();
    down(100, 100, { button: 2 });
    move(0, 0);
    expect(capture).not.toHaveBeenCalled();
    expect(at()).toEqual(start);
  });

  test("a second finger can't take over, and its movement and release are ignored", () => {
    const { capture, at, frame, down, move, up } = mount();
    down(100, 100);
    down(500, 500, { pointerId: 2 });
    expect(capture).toHaveBeenCalledTimes(1);

    const start = at();
    move(0, 0, { pointerId: 2 });
    expect(at()).toEqual(start);
    up({ pointerId: 2 });
    expect(frame().props["data-dragging"]).toBe(true);
  });

  test("a mouse press leaves focus where it is, so the search box keeps it", () => {
    const { grip } = mount();
    const preventDefault = mock(() => {});
    grip().props.onMouseDown({ preventDefault });
    expect(preventDefault).toHaveBeenCalled();
  });

  test("the spot survives a re-render with new content, as when a search changes the list", () => {
    const { root, at, down, move, up, update } = mount();
    down(100, 100);
    move(50, 80);
    up();
    const spot = at();

    update("second");
    expect(root.findByType("p").children).toEqual(["second"]);
    expect(at()).toEqual(spot);
  });

  test("double-clicking the grip puts it back", () => {
    const { at, down, move, up, grip } = mount();
    const start = at();
    down(100, 100);
    move(50, 80);
    up();
    act(() => grip().props.onDoubleClick());
    expect(at()).toEqual(start);
  });
});

describe("moving with the keyboard", () => {
  test("arrows move a step, Shift moves further, and the keys are consumed", () => {
    const { at, press } = mount();
    const start = at();

    expect(press("ArrowLeft")).toHaveBeenCalled();
    expect(at()).toEqual({ left: start.left - 16, top: start.top });
    press("ArrowUp");
    expect(at()).toEqual({ left: start.left - 16, top: start.top - 16 });
    press("ArrowLeft", true);
    expect(at()).toEqual({ left: start.left - 80, top: start.top - 16 });
    press("ArrowRight");
    expect(at()).toEqual({ left: start.left - 64, top: start.top - 16 });
    press("ArrowDown", true);
    expect(at()).toEqual({ left: start.left - 64, top: start.top + 48 });
  });

  test("the keyboard stops at the same edges as the mouse", () => {
    const { at, press } = mount();
    for (let step = 0; step < 40; step += 1) press("ArrowUp", true);
    expect(at().top).toBe(36);
  });

  test("Home puts it back", () => {
    const { at, press } = mount();
    const start = at();
    press("ArrowDown");
    expect(press("Home")).toHaveBeenCalled();
    expect(at()).toEqual(start);
  });

  test("other keys pass through, so Escape and Tab still work on the grip", () => {
    const { at, press } = mount();
    const start = at();
    expect(press("Tab")).not.toHaveBeenCalled();
    expect(press("Escape")).not.toHaveBeenCalled();
    expect(at()).toEqual(start);
  });

  test("the grip names itself and points at its instructions", () => {
    const { root, grip } = mount();
    const hint = root.findByProps({ id: grip().props["aria-describedby"] });
    expect(hint.children).toEqual(["Drag or use arrow keys. Home resets."]);
  });
});

describe("the viewport changing", () => {
  test("a smaller window fits the frame inside it, and a larger one returns it to the chosen spot", () => {
    const { frame, at, down, move, up } = mount();
    down(100, 100);
    move(50, 80);
    up();
    const spot = at();

    resizeTo(500, 400);
    const { style } = frame().props;
    expect(style.left + style.width).toBeLessThanOrEqual(500 - 12);
    expect(style.top + style.height).toBeLessThanOrEqual(400 - 12);
    expect(style.top - OVERHANG).toBeGreaterThanOrEqual(0);

    resizeTo(DESKTOP.width, DESKTOP.height);
    expect(at()).toEqual(spot);
  });

  test("a phone's keyboard shrinks the frame to what is left", () => {
    const visual = Object.assign(new EventTarget(), { offsetLeft: 0, offsetTop: 0, width: 390, height: 800 });
    fakeWindow.visualViewport = visual;
    const { frame } = mount();

    visual.height = 340;
    act(() => {
      visual.dispatchEvent(new Event("resize"));
    });
    const { style } = frame().props;
    expect(style.width).toBe(366);
    expect(style.height).toBe(340 - 36 - 12);
    expect(style.top).toBe(36);
  });

  test("it stops listening when it closes", () => {
    const { renderer } = mount();
    expect(listeners.get("resize")?.size).toBeGreaterThan(0);
    act(() => renderer.unmount());
    expect(listeners.get("resize")?.size).toBe(0);
  });
});

describe("closing", () => {
  test("the X has a name and closes", () => {
    const { root, onClose } = mount();
    root.findByProps({ "aria-label": "Close model picker" }).props.onClick();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test("a press on the X can't start a drag, because only the grip handles the pointer", () => {
    const { root, frame } = mount();
    const close = root.findByProps({ "aria-label": "Close model picker" });
    for (const node of [close, frame()]) {
      expect(node.props.onPointerDown).toBeUndefined();
      expect(node.props.onPointerMove).toBeUndefined();
    }
  });

  test("the grip doesn't close it", () => {
    const { onClose, down, move, up } = mount();
    down(100, 100);
    move(0, 0);
    up();
    expect(onClose).not.toHaveBeenCalled();
  });

  test("the dialog's close, which Escape calls, is the same way out", () => {
    const { onClose } = mount();
    expect(dialogProps.open).toBe(true);
    (dialogProps.onClose as () => void)();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test("the dialog is named and left to place focus and return it", () => {
    mount();
    expect(dialogProps["aria-label"]).toBe("Choose model");
    expect(dialogProps.autoFocus).toBeUndefined();
    expect(dialogProps.initialFocus).toBeUndefined();
  });
});

describe("the background", () => {
  function boundaryOf(root: TestRenderer.ReactTestInstance) {
    return root.find((node) => node.type === "div" && node.props.className === "wfb-popup-boundary");
  }

  test("the window sits inside the dialog's panel, so a click around it lands inside the dialog", () => {
    const { root } = mount();
    expect(
      boundaryOf(root).findAll((node) => node.type === "div" && node.props.className === "wfb-popup"),
    ).toHaveLength(1);
  });

  test("a press on the empty boundary closes nothing and keeps focus where it was", () => {
    const { root, onClose } = mount();
    const boundary = boundaryOf(root);
    const preventDefault = mock(() => {});
    boundary.props.onMouseDown({ target: boundary, currentTarget: boundary, preventDefault });
    expect(preventDefault).toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });

  test("a press on something inside the window is left alone, so its controls take focus", () => {
    const { root, frame } = mount();
    const boundary = boundaryOf(root);
    const preventDefault = mock(() => {});
    boundary.props.onMouseDown({ target: frame(), currentTarget: boundary, preventDefault });
    expect(preventDefault).not.toHaveBeenCalled();
  });

  test("the popup adds no listeners of its own beyond the viewport's", () => {
    mount();
    const active = [...listeners].filter(([, set]) => set.size > 0).map(([type]) => type);
    expect(active).toEqual(["resize"]);
  });
});

describe("the stylesheet", () => {
  const css = readFileSync(new URL("./model-picker.css", import.meta.url), "utf8");
  const rule = (selector: string) => {
    const start = css.indexOf(`${selector} {`);
    if (start < 0) throw new Error(`no rule for ${selector}`);
    return css.slice(start, css.indexOf("}", start));
  };

  test("the window is solid and framed", () => {
    const popup = rule(".wfb-popup");
    expect(popup).toContain("background: var(--surface);");
    expect(popup).toContain("border: 1px solid var(--line-strong);");
    expect(popup).toContain("box-shadow: var(--e3);");
    expect(popup).not.toContain("opacity");
  });

  test("the boundary covers the screen and stays clear, with no dim", () => {
    const boundary = rule(".wfb-popup-boundary");
    expect(boundary).toContain("position: fixed;");
    expect(boundary).toContain("inset: 0;");
    expect(boundary).not.toContain("background");
    expect(boundary).not.toContain("pointer-events");
  });

  test("the layer sits above the app and keeps a real width, or Headless UI closes the dialog as gone", () => {
    const layer = rule(".wfb-popup-layer");
    expect(layer).toContain("z-index: 60;");
    expect(layer).toContain("position: relative;");
    expect(layer).not.toContain("display: contents");
  });

  test("every color comes from a palette token", () => {
    const withoutComments = css.replace(/\/\*[\s\S]*?\*\//g, "");
    expect(withoutComments).not.toMatch(/#[0-9a-f]{3,8}\b|\brgba?\(|\bhsla?\(/i);
  });

  test("focus cues use the control's own text color and never sit outside it", () => {
    expect(css).not.toContain("var(--ring)");
    expect(rule(".wfb-popup :focus-visible")).toContain("outline: 2px solid currentColor;");
    expect(rule(".wfb-popup :focus-visible")).toContain("outline-offset: -3px;");
    expect(css.match(/outline-offset:\s*[^-\s;][^;]*;/g)).toBeNull();
  });

  test("a click in a text field shows a border, never a ring around it", () => {
    const input = rule(".wfb-popup .input:focus");
    expect(input).toContain("outline: none;");
    expect(input).toContain("border-color: currentColor;");
  });

  test("only the grip stops touch scrolling, so the list still scrolls under a finger", () => {
    expect(css.match(/touch-action:/g)).toHaveLength(1);
    expect(rule(".wfb-popup-grip")).toContain("touch-action: none;");
  });

  test("the tallest tab fits the room the geometry keeps for it", () => {
    expect(css).toContain(`height: ${OVERHANG}px;`);
  });
});
