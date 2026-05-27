import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { useEffect } from "react";
import TestRenderer, { act } from "react-test-renderer";
import { toast as sonnerToast, useSonner } from "sonner";

import { ToastProvider, useToast } from "./toast";

function textFromNode(node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>): string {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(textFromNode).join("");
  return (node.children ?? []).map(textFromNode).join("");
}

function CaptureToastApi({
  onReady,
}: {
  onReady?: (api: ReturnType<typeof useToast>) => void;
}) {
  const api = useToast();

  useEffect(() => {
    onReady?.(api);
  }, [api, onReady]);

  return null;
}

function SonnerToastText() {
  const { toasts } = useSonner();

  return (
    <output aria-live="polite">
      {toasts.map((toast) => (
        <p key={toast.id}>
          {typeof toast.title === "function" ? toast.title() : toast.title}
        </p>
      ))}
    </output>
  );
}

async function flushSonnerUpdates() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

describe("useToast", () => {
  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    (globalThis as { requestAnimationFrame?: (callback: FrameRequestCallback) => number }).requestAnimationFrame = (
      callback,
    ) => setTimeout(callback, 0) as unknown as number;
  });

  afterEach(() => {
    sonnerToast.dismiss();
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
    delete (globalThis as { requestAnimationFrame?: (callback: FrameRequestCallback) => number }).requestAnimationFrame;
  });

  test("push renders a Sonner toast with the message", async () => {
    let api: ReturnType<typeof useToast> | null = null;
    let renderer: TestRenderer.ReactTestRenderer | null = null;
    await act(async () => {
      renderer = TestRenderer.create(
        <>
          <SonnerToastText />
          <CaptureToastApi
            onReady={(value) => {
              api = value;
            }}
          />
        </>,
      );
    });

    await act(async () => {
      api!.push({ message: "Run archived." });
    });
    await flushSonnerUpdates();

    expect(textFromNode(renderer!.toJSON())).toContain("Run archived.");

    await act(async () => {
      renderer?.unmount();
    });
  });

  test("error toasts are red and persistent", async () => {
    let api: ReturnType<typeof useToast> | null = null;
    let toastId = "";
    let renderer: TestRenderer.ReactTestRenderer | null = null;
    await act(async () => {
      renderer = TestRenderer.create(
        <>
          <SonnerToastText />
          <CaptureToastApi
            onReady={(value) => {
              api = value;
            }}
          />
        </>,
      );
    });

    await act(async () => {
      toastId = api!.push({ message: "Conflict", tone: "error", autoDismissMs: 5 });
    });
    await flushSonnerUpdates();

    expect(textFromNode(renderer!.toJSON())).toContain("Conflict");
    expect(
      sonnerToast.getToasts().find((toast) => toast.id === toastId),
    ).toMatchObject({
      duration: Infinity,
      title:    "Conflict",
      type:     "error",
    });

    await act(async () => {
      renderer?.unmount();
    });
  });

  test("dismiss removes one toast from the Sonner store", async () => {
    let api: ReturnType<typeof useToast> | null = null;
    let secondId = "";
    let renderer: TestRenderer.ReactTestRenderer | null = null;
    await act(async () => {
      renderer = TestRenderer.create(
        <>
          <SonnerToastText />
          <CaptureToastApi
            onReady={(value) => {
              api = value;
            }}
          />
        </>,
      );
    });

    await act(async () => {
      api!.push({ message: "First" });
      secondId = api!.push({ message: "Second" });
    });
    await flushSonnerUpdates();

    await act(async () => {
      api!.dismiss(secondId);
    });
    await flushSonnerUpdates();

    const text = textFromNode(renderer!.toJSON());
    expect(text).toContain("First");
    expect(text).not.toContain("Second");

    await act(async () => {
      renderer?.unmount();
    });
  });

  test("clear removes all Sonner toasts", async () => {
    let api: ReturnType<typeof useToast> | null = null;
    let renderer: TestRenderer.ReactTestRenderer | null = null;

    await act(async () => {
      renderer = TestRenderer.create(
        <ToastProvider>
          <SonnerToastText />
          <CaptureToastApi
            onReady={(value) => {
              api = value;
            }}
          />
        </ToastProvider>,
      );
    });

    await act(async () => {
      api!.push({ message: "First" });
      api!.push({ message: "Second" });
    });
    await flushSonnerUpdates();

    await act(async () => {
      api!.clear();
    });
    await flushSonnerUpdates();

    const text = textFromNode(renderer!.toJSON());
    expect(text).not.toContain("First");
    expect(text).not.toContain("Second");

    await act(async () => {
      renderer?.unmount();
    });
  });

  test("ToastProvider is transparent for existing test wrappers", async () => {
    let renderer: TestRenderer.ReactTestRenderer | null = null;
    await act(async () => {
      renderer = TestRenderer.create(
        <ToastProvider>
          <span>wrapped child</span>
        </ToastProvider>,
      );
    });

    expect(textFromNode(renderer!.toJSON())).toContain("wrapped child");

    await act(async () => {
      renderer?.unmount();
    });
  });
});
