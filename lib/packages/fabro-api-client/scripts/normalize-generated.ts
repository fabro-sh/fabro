// Normalizes openapi-generator output so `bun run generate` is idempotent.
//
// The typescript-axios templates emit trailing spaces on some lines and extra
// blank lines at end of file. Left alone, every regeneration produces a noisy
// whitespace diff that masks real spec/client drift. This pass strips trailing
// whitespace from every line and ends each file with exactly one newline.

import { Glob } from "bun";

const glob = new Glob("src/**/*.ts");
let changed = 0;

for await (const path of glob.scan(".")) {
  const original = await Bun.file(path).text();
  const normalized =
    original
      .split("\n")
      .map((line) => line.replace(/\s+$/, ""))
      .join("\n")
      .replace(/\n+$/, "") + "\n";
  if (normalized !== original) {
    await Bun.write(path, normalized);
    changed += 1;
  }
}

console.log(`normalize-generated: ${changed} file(s) updated`);
