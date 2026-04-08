import { describe, expect, test } from "bun:test";
import { renderToString } from "react-dom/server";
import { DemoModeProvider, useDemoMode } from "./demo-mode";

function TestConsumer() {
  const demoMode = useDemoMode();
  return <span data-demo={demoMode}>{demoMode ? "demo" : "prod"}</span>;
}

describe("DemoModeProvider", () => {
  test("provides demo mode value to children", () => {
    const html = renderToString(
      <DemoModeProvider value={true}>
        <TestConsumer />
      </DemoModeProvider>,
    );
    expect(html).toContain("demo");
    expect(html).toContain('data-demo="true"');
  });

  test("defaults to false", () => {
    const html = renderToString(
      <DemoModeProvider value={false}>
        <TestConsumer />
      </DemoModeProvider>,
    );
    expect(html).toContain("prod");
  });
});
