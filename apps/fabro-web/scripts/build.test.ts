import { test, expect } from "bun:test";
import { existsSync } from "node:fs";
import { lstat, readdir, readlink } from "node:fs/promises";
import { basename, join } from "node:path";

const root = Bun.fileURLToPath(new URL("..", import.meta.url));

async function runBuild() {
  const process = Bun.spawn(["bun", "run", "scripts/build.ts"], {
    cwd:    root,
    stdout: "pipe",
    stderr: "pipe",
  });

  const code = await process.exited;
  if (code !== 0) {
    const stderr = await new Response(process.stderr).text();
    const stdout = await new Response(process.stdout).text();
    throw new Error(
      `build failed with code ${code}\nstdout:\n${stdout}\nstderr:\n${stderr}`,
    );
  }
}

test("production build copies Pierre worker assets", async () => {
  await runBuild();

  const workerDist = join(root, "dist", "assets", "pierre-diffs-worker");
  expect(existsSync(join(workerDist, "worker-portable.js"))).toBe(true);

  const upstreamWorkerDir = join(
    root,
    "node_modules",
    "@pierre",
    "diffs",
    "dist",
    "worker",
  );
  const wasmFiles = (await readdir(upstreamWorkerDir))
    .filter((file) => /^wasm-.*\.js$/.test(file))
    .map((file) => basename(file));

  for (const wasmFile of wasmFiles) {
    expect(existsSync(join(workerDist, wasmFile))).toBe(true);
  }
}, 60000);

test("dist is a symlink into .dist-builds and old builds are pruned", async () => {
  await runBuild();
  await runBuild();

  const distPath = join(root, "dist");
  const stat = await lstat(distPath);
  expect(stat.isSymbolicLink()).toBe(true);

  const target = await readlink(distPath);
  expect(target.startsWith(".dist-builds/")).toBe(true);

  const buildId = target.slice(".dist-builds/".length);
  const buildsRoot = join(root, ".dist-builds");
  const remaining = await readdir(buildsRoot);
  expect(remaining).toEqual([buildId]);

  expect(existsSync(join(distPath, "index.html"))).toBe(true);
}, 60000);

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
  expect([0, 130]).toContain(await process.exited);
});
