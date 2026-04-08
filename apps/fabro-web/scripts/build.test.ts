import { test, expect } from "bun:test";

const root = Bun.fileURLToPath(new URL("..", import.meta.url));

test("watch mode keeps running until interrupted", async () => {
  const process = Bun.spawn([
    "bun",
    "run",
    "scripts/build.ts",
    "--watch",
  ], {
    cwd: root,
    stdout: "pipe",
    stderr: "pipe",
  });

  const result = await Promise.race([
    process.exited.then((code) => ({ kind: "exited" as const, code })),
    Bun.sleep(1000).then(() => ({ kind: "running" as const })),
  ]);

  if (result.kind === "exited") {
    const stderr = await new Response(process.stderr).text();
    const stdout = await new Response(process.stdout).text();
    throw new Error(
      `watch process exited unexpectedly with code ${result.code}\nstdout:\n${stdout}\nstderr:\n${stderr}`,
    );
  }

  process.kill("SIGINT");
  expect(await process.exited).toBe(0);
});
