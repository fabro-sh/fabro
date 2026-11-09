import { afterAll, beforeAll, describe, expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";
import type { ReactNode } from "react";
import TestRenderer, { act } from "react-test-renderer";

import { setupReactTestEnv, textContent } from "../lib/test-utils";

// HeadlessUI's dialog needs a DOM. Its focus trap and Escape handling are its own and stay
// untested here; these tests cover what ConfirmDialog adds on top.
let lastOnClose: (() => void) | undefined;
mock.module("@headlessui/react", () => ({
  Dialog: ({ open, onClose, children }: { open: boolean; onClose: () => void; children: ReactNode }) => {
    lastOnClose = onClose;
    return open ? <div data-dialog="true">{children}</div> : null;
  },
  DialogPanel: ({ children, className }: { children: ReactNode; className: string }) => (
    <div className={className}>{children}</div>
  ),
  DialogTitle: ({ children, className }: { children: ReactNode; className: string }) => (
    <h2 className={className}>{children}</h2>
  ),
}));

const {
  COMPACT_SECONDARY_BUTTON_CLASS,
  ConfirmDialog,
  DANGER_BUTTON_CLASS,
  ErrorMessage,
  GHOST_BUTTON_CLASS,
  INPUT_CLASS,
  POPOVER_SURFACE_CLASS,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
  SIGNAL_BUTTON_CLASS,
  TOOLBAR_BUTTON_CLASS,
  TOOLTIP_SURFACE_CLASS,
  Tooltip,
} = await import("./ui");

let restore: () => void;
beforeAll(() => {
  restore = setupReactTestEnv();
});
afterAll(() => restore());

function create(element: Parameters<typeof TestRenderer.create>[0]) {
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(element);
  });
  return renderer;
}

function mountDialog(overrides: Partial<Parameters<typeof ConfirmDialog>[0]> = {}) {
  const onConfirm = mock(() => {});
  const onCancel = mock(() => {});
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(
      <ConfirmDialog
        open
        title="Delete this run?"
        description="This cannot be undone."
        confirmLabel="Delete run"
        pendingLabel="Deleting…"
        onConfirm={onConfirm}
        onCancel={onCancel}
        {...overrides}
      />,
    );
  });
  const button = (word: string) =>
    renderer.root
      .findAllByType("button")
      .find((node) => textContent(node) === word || node.props["aria-label"] === word)!;
  return { renderer, button, onConfirm, onCancel };
}

describe("ConfirmDialog", () => {
  test("opening lands on the safe answer, and each button does its job", () => {
    const { button, onConfirm, onCancel } = mountDialog();
    expect(button("Cancel").props["data-autofocus"]).toBe(true);
    expect(button("Delete run").props["data-autofocus"]).toBeUndefined();
    act(() => button("Delete run").props.onClick());
    expect(onConfirm).toHaveBeenCalledTimes(1);
    act(() => button("Cancel").props.onClick());
    act(() => button("Close").props.onClick());
    expect(onCancel).toHaveBeenCalledTimes(2);
  });

  test("the destructive tone is Factory's danger button, and primary is its primary", () => {
    expect(mountDialog().button("Delete run").props.className).toBe(DANGER_BUTTON_CLASS);
    expect(mountDialog({ tone: "primary" }).button("Delete run").props.className).toBe(
      PRIMARY_BUTTON_CLASS,
    );
  });

  test("while pending, every way out is shut and the button says what is happening", () => {
    const { button, onCancel } = mountDialog({ pending: true });
    expect(button("Deleting…").props.disabled).toBe(true);
    expect(button("Cancel").props.disabled).toBe(true);
    expect(button("Close").props.disabled).toBe(true);
    act(() => lastOnClose?.());
    expect(onCancel).not.toHaveBeenCalled();
  });

  test("Escape and an outside click cancel when nothing is pending", () => {
    const { onCancel } = mountDialog();
    act(() => lastOnClose?.());
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  test("renders nothing when closed", () => {
    expect(mountDialog({ open: false }).renderer.toJSON()).toBeNull();
  });
});

describe("ErrorMessage", () => {
  test("is announced as an alert", () => {
    const renderer = create(<ErrorMessage message="Nope" />);
    expect(renderer.root.findByType("p").props.role).toBe("alert");
  });
});

describe("Tooltip", () => {
  test("keeps the wrapper display callers ask for", () => {
    const renderer = create(
      <Tooltip label="Hint" side="right" triggerClassName="block w-full">
        <span>Row</span>
      </Tooltip>,
    );
    expect(JSON.stringify(renderer.toJSON())).toContain("block w-full");
  });
});

describe("the class contract with factory-components.css", () => {
  const css = readFileSync(new URL("../styles/factory-components.css", import.meta.url), "utf8");
  const ruleBody = (selector: string) => {
    const start = css.indexOf(`${selector} {`);
    expect(start).toBeGreaterThan(-1);
    return css.slice(start, css.indexOf("}", start));
  };

  test("every exported control class has a rule, unless it is a Tailwind utility", () => {
    const classes = [
      COMPACT_SECONDARY_BUTTON_CLASS,
      DANGER_BUTTON_CLASS,
      GHOST_BUTTON_CLASS,
      INPUT_CLASS,
      POPOVER_SURFACE_CLASS,
      PRIMARY_BUTTON_CLASS,
      SECONDARY_BUTTON_CLASS,
      SIGNAL_BUTTON_CLASS,
      TOOLBAR_BUTTON_CLASS,
      TOOLTIP_SURFACE_CLASS,
    ].flatMap((value) => value.split(" "));
    for (const name of new Set(classes)) {
      if (name === "h-9") continue;
      expect(css).toContain(`.${name} {`);
    }
  });

  test("an ordinary primary button is the palette's primary, and only signal is the highlight", () => {
    expect(ruleBody(".btn-primary")).toContain("background: var(--primary)");
    expect(ruleBody(".btn-primary")).toContain("color: var(--primary-fg)");
    expect(ruleBody(".btn-signal")).toContain("background: var(--accent-pop)");
    expect(PRIMARY_BUTTON_CLASS).not.toContain("signal");
  });

  test("no palette color lives in the stylesheet", () => {
    const withoutComments = css.replace(/\/\*[\s\S]*?\*\//g, "");
    expect(withoutComments).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    // The running bar's stripes are the one literal, a translucent white as in Factory.
    const literals = withoutComments.match(/(?:rgb|rgba|hsl|hsla|oklch)\([^)]*\)/g) ?? [];
    expect(new Set(literals)).toEqual(new Set(["rgb(255 255 255 / 0.25)"]));
  });
});
