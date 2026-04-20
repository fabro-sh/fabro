import { afterEach, describe, expect, test } from "bun:test";
import { useEffect } from "react";
import TestRenderer, { act } from "react-test-renderer";

import { ToastProvider, useToast } from "./toast";

function textFromNode(node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>): string {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(textFromNode).join("");
  return (node.children ?? []).map(textFromNode).join("");
}

function textFromInstance(node: TestRenderer.ReactTestInstance): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : textFromInstance(child)))
    .join("");
}

function PushOnMount({
  toast,
  onReady,
}: {
  toast: Parameters<ReturnType<typeof useToast>["push"]>[0];
  onReady?: (api: ReturnType<typeof useToast>) => void;
}) {
  const api = useToast();

  useEffect(() => {
    onReady?.(api);
    api.push(toast);
  }, [api, onReady, toast]);

  return null;
}

describe("ToastProvider", () => {
  afterEach(() => {
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  test("push renders a toast with the message", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

    let renderer: TestRenderer.ReactTestRenderer | null = null;
    await act(async () => {
      renderer = TestRenderer.create(
        <ToastProvider autoDismissMs={1000}>
          <PushOnMount toast={{ message: "Run archived." }} />
        </ToastProvider>,
      );
    });

    expect(textFromNode(renderer!.toJSON())).toContain("Run archived.");
    const liveRegions = renderer!.root.findAll(
      (node) => node.props?.role === "status" && node.props?.["aria-live"] === "polite",
    );
    expect(liveRegions.length).toBeGreaterThan(0);

    await act(async () => {
      renderer?.unmount();
    });
  });

  test("action toasts render a button and fire onClick", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

    let clicked = 0;
    let renderer: TestRenderer.ReactTestRenderer | null = null;
    await act(async () => {
      renderer = TestRenderer.create(
        <ToastProvider autoDismissMs={1000}>
          <PushOnMount
            toast={{
              message: "Run archived.",
              action: {
                label: "Unarchive",
                onClick: () => {
                  clicked += 1;
                },
              },
            }}
          />
        </ToastProvider>,
      );
    });

    const button = renderer!.root.findByType("button");
    expect(textFromNode(renderer!.toJSON())).toContain("Unarchive");

    await act(async () => {
      button.props.onClick();
    });

    expect(clicked).toBe(1);

    await act(async () => {
      renderer?.unmount();
    });
  });

  test("error toasts do not auto-dismiss", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

    let renderer: TestRenderer.ReactTestRenderer | null = null;
    await act(async () => {
      renderer = TestRenderer.create(
        <ToastProvider autoDismissMs={5}>
          <PushOnMount toast={{ message: "Conflict", tone: "error" }} />
        </ToastProvider>,
      );
    });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });

    expect(textFromNode(renderer!.toJSON())).toContain("Conflict");

    await act(async () => {
      renderer?.unmount();
    });
  });

  test("multiple toasts stack in insertion order", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

    let api: ReturnType<typeof useToast> | null = null;
    let renderer: TestRenderer.ReactTestRenderer | null = null;

    await act(async () => {
      renderer = TestRenderer.create(
        <ToastProvider autoDismissMs={1000}>
          <PushOnMount
            toast={{ message: "First" }}
            onReady={(value) => {
              api = value;
            }}
          />
        </ToastProvider>,
      );
    });

    await act(async () => {
      api!.push({ message: "Second" });
    });

    const toasts = renderer!.root.findAll(
      (node) => node.props?.["data-toast-id"] != null,
    );
    expect(toasts).toHaveLength(2);
    expect(textFromInstance(toasts[0]!)).toContain("First");
    expect(textFromInstance(toasts[1]!)).toContain("Second");

    await act(async () => {
      renderer?.unmount();
    });
  });

  test("dismiss removes a toast and leaves the rest reflowed", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

    let api: ReturnType<typeof useToast> | null = null;
    let firstId = "";
    let renderer: TestRenderer.ReactTestRenderer | null = null;

    await act(async () => {
      renderer = TestRenderer.create(
        <ToastProvider autoDismissMs={1000}>
          <PushOnMount
            toast={{ message: "First" }}
            onReady={(value) => {
              api = value;
            }}
          />
        </ToastProvider>,
      );
    });

    await act(async () => {
      firstId = api!.push({ message: "Second" });
    });

    await act(async () => {
      api!.dismiss(firstId);
    });

    const text = textFromNode(renderer!.toJSON());
    expect(text).toContain("First");
    expect(text).not.toContain("Second");

    await act(async () => {
      renderer?.unmount();
    });
  });
});
