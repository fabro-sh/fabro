-- Fabro's own facts about a Petri run, beside Petri's records.
--
-- `platform_records` holds every fact Fabro records about a run that Petri
-- does not: the lifecycle before and after the engine, a checkpoint commit,
-- a pull request, a notification, a pairing. `seq` is per run and assigned
-- by the store; `kind` is the record's kind and `record_json` the typed
-- record with its kind tag; `execution` and `firing` name the Petri stage a
-- record belongs to, when it belongs to one.
CREATE TABLE platform_records (
    run_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    recorded_at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    record_json TEXT NOT NULL,
    execution INTEGER NULL,
    firing INTEGER NULL,
    PRIMARY KEY (run_id, seq),
    CHECK (seq >= 1),
    CHECK (json_valid(record_json))
);

CREATE INDEX platform_records_by_kind
ON platform_records(run_id, kind, seq);

-- The projection of a Petri run: the view document Fabro's read side serves,
-- derived from the run's Petri records and platform records, rewritten in
-- one transaction per view pass together with the positions it covers.
-- `projection_json` is the `RunProjection`; `fold_json` is the projector's
-- own bookkeeping; `positions_json` is the last event consumed per Petri
-- log and the last platform record consumed; `stream_seq` is the last
-- delivery sequence assigned to `petri_stream`.
CREATE TABLE petri_projection (
    run_id TEXT PRIMARY KEY NOT NULL,
    projection_json TEXT NOT NULL,
    fold_json TEXT NOT NULL,
    positions_json TEXT NOT NULL,
    stream_seq INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK (stream_seq >= 0),
    CHECK (json_valid(projection_json)),
    CHECK (json_valid(fold_json)),
    CHECK (json_valid(positions_json))
);

-- One ordered stream per run of everything the projection consumed: each
-- Petri event and each platform record, in the order the view committed
-- them. `stream_seq` is the cursor a client resumes from; `item_kind` and
-- `item_id` are the item's own identity (a Petri event id as
-- `<log>/<seq>/<index>`, or a platform record's `seq`), for deduplication.
CREATE TABLE petri_stream (
    run_id TEXT NOT NULL,
    stream_seq INTEGER NOT NULL,
    item_kind TEXT NOT NULL,
    item_id TEXT NOT NULL,
    event_json TEXT NOT NULL,
    PRIMARY KEY (run_id, stream_seq),
    CHECK (stream_seq >= 1),
    CHECK (item_kind IN ('petri', 'platform')),
    CHECK (json_valid(event_json))
);
