import { watch as fsWatch } from "node:fs";
import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { join, relative } from "node:path";

declare const Bun: any;

const root = new URL("..", import.meta.url);
const rootPath = Bun.fileURLToPath(root);
const distDir = join(rootPath, "dist");
const assetsDir = join(distDir, "assets");
const publicDir = join(rootPath, "public");
const templatePath = join(rootPath, "index.template.html");
const watch = Bun.argv.includes("--watch");

async function buildOnce() {
  await rm(distDir, { recursive: true, force: true });
  await mkdir(assetsDir, { recursive: true });

  const result = await Bun.build({
    entrypoints: [join(rootPath, "app", "entry.tsx")],
    outdir: assetsDir,
    naming: "[name]-[hash].[ext]",
    minify: true,
    splitting: true,
    target: "browser",
    sourcemap: "external",
  });

  if (!result.success) {
    throw new Error(result.logs.map((log: any) => log.message).join("\n"));
  }

  const cssResult = await Bun.spawn([
    "bunx",
    "@tailwindcss/cli",
    "-i",
    "app/app.css",
    "-o",
    "dist/assets/app.css",
    "--minify",
  ], {
    cwd: rootPath,
    stdout: "inherit",
    stderr: "inherit",
  }).exited;

  if (cssResult !== 0) {
    throw new Error("Tailwind build failed");
  }

  await cp(publicDir, distDir, { recursive: true });
  await writeIndexHtml(result.outputs.map((output: any) => relative(distDir, output.path)));
}

async function writeIndexHtml(outputs: string[]) {
  const template = await readFile(templatePath, "utf8");
  const scripts = outputs
    .filter((path) => path.endsWith(".js"))
    .map((path) => `<script type="module" src="/${path.replaceAll("\\\\", "/")}"></script>`)
    .join("\n    ");
  const styles = [
    "/assets/app.css",
    ...outputs.filter((path) => path.endsWith(".css")).map((path) => `/${path.replaceAll("\\\\", "/")}`),
  ]
    .filter((value, index, array) => array.indexOf(value) === index)
    .map((path) => `<link rel="stylesheet" href="${path}" />`)
    .join("\n    ");

  const html = template
    .replace("{{styles}}", styles)
    .replace("{{scripts}}", scripts);

  await writeFile(join(distDir, "index.html"), html, "utf8");
}

async function main() {
  if (!watch) {
    await buildOnce();
    return;
  }

  await buildOnce();
  let building = false;
  let rebuildQueued = false;

  async function rebuild() {
    if (building) {
      rebuildQueued = true;
      return;
    }

    building = true;
    do {
      rebuildQueued = false;
      try {
        await buildOnce();
      } catch (error) {
        console.error(error);
      }
    } while (rebuildQueued);
    building = false;
  }

  const watchers = [
    fsWatch(join(rootPath, "app"), { recursive: true }, rebuild),
    fsWatch(publicDir, { recursive: true }, rebuild),
    fsWatch(templatePath, rebuild),
  ];

  process.on("SIGINT", () => {
    for (const watcher of watchers) {
      watcher.close();
    }
    process.exit(0);
  });
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
