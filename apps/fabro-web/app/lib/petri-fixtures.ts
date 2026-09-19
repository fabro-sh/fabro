/**
 * The Petri scenario fixtures the server tests capture
 * (`lib/apps/fabro-server/tests/it/scenario/petri_stream.rs` under
 * `FABRO_CAPTURE_PETRI_FIXTURES`): a settled run's projection and its whole
 * stream. Test-only; `tsc` excludes the tests that import this module.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { RunProjection, RunStreamItem } from "@qltysh/fabro-api-client";

export type PetriFixtureName = "hello" | "command" | "parallel" | "gate";

export interface PetriFixture {
  run_id: string;
  projection: RunProjection;
  stream: RunStreamItem[];
}

const FIXTURES_DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "test-fixtures", "petri");

export function loadPetriFixture(name: PetriFixtureName): PetriFixture {
  const text = readFileSync(join(FIXTURES_DIR, `${name}.json`), "utf8");
  return JSON.parse(text) as PetriFixture;
}
