-- Petri's durable run record in Fabro's database.
--
-- `petri_runs` is a run's existence and its writer lease: one row per run
-- key, with the owner holding the lease and when it took it. Petri opens runs
-- by keys of its own, so this row is separate from the Fabro `runs` summary
-- row the create handler writes.
CREATE TABLE petri_runs (
    run_id TEXT PRIMARY KEY NOT NULL,
    created_at_ms INTEGER NOT NULL,
    owner_id TEXT NULL,
    acquired_at_ms INTEGER NULL
);

-- Every record of every log of a run, keyed by (run, log, seq). `log` is the
-- Petri log id as text (`coordinator`, `resources`, `execution <n>`),
-- `recorded_at` is lifted out of the record for indexing, and `record_json`
-- is the record itself, stored and read back unchanged. Blobs share the
-- `blobs` table.
CREATE TABLE petri_records (
    run_id TEXT NOT NULL,
    log TEXT NOT NULL,
    seq INTEGER NOT NULL,
    recorded_at INTEGER NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY (run_id, log, seq),
    CHECK (json_valid(record_json))
);
