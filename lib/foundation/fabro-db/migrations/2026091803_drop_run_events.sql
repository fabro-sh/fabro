-- The legacy executor's run event log is gone: every run is a Petri run
-- whose history is `petri_records` and `platform_records`. Fabro is
-- greenfield here, so the rows are dropped, not converted, and the
-- one-time activation bookkeeping of that log goes with them.
DROP TABLE IF EXISTS run_events;
DROP TABLE IF EXISTS legacy_run_history_activation;
DROP TABLE IF EXISTS legacy_run_history_deletions;

-- The `runs` row loses the columns only that log wrote or read:
-- `source_last_seq`, its write-path concurrency guard (the projection's
-- per-log positions replace it), and the six token and file-count columns
-- nothing read (`summary_json` keeps the values). SQLite cannot drop a
-- column a table CHECK names, so the table is rebuilt.
CREATE TABLE runs_next (
    id TEXT PRIMARY KEY NOT NULL,
    created_at_ms INTEGER NOT NULL,
    started_at_ms INTEGER,
    last_event_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    status TEXT NOT NULL,
    archived_at_ms INTEGER,
    parent_id TEXT,
    title TEXT NOT NULL,
    workflow_slug TEXT,
    workflow_name TEXT,
    repository_name TEXT,
    automation_id TEXT,
    diff_additions INTEGER NOT NULL DEFAULT 0,
    diff_deletions INTEGER NOT NULL DEFAULT 0,
    total_usd_micros INTEGER,
    summary_json TEXT NOT NULL,
    CHECK (status IN (
        'submitted',
        'pending',
        'runnable',
        'starting',
        'running',
        'blocked',
        'paused',
        'removing',
        'succeeded',
        'failed',
        'dead'
    )),
    CHECK (diff_additions >= 0),
    CHECK (diff_deletions >= 0),
    CHECK (total_usd_micros IS NULL OR total_usd_micros >= 0),
    CHECK (json_valid(summary_json))
);

INSERT INTO runs_next (
    id, created_at_ms, started_at_ms, last_event_at_ms, completed_at_ms, status,
    archived_at_ms, parent_id, title, workflow_slug, workflow_name, repository_name,
    automation_id, diff_additions, diff_deletions, total_usd_micros, summary_json
)
SELECT
    id, created_at_ms, started_at_ms, last_event_at_ms, completed_at_ms, status,
    archived_at_ms, parent_id, title, workflow_slug, workflow_name, repository_name,
    automation_id, diff_additions, diff_deletions, total_usd_micros, summary_json
FROM runs;

DROP TABLE runs;
ALTER TABLE runs_next RENAME TO runs;

CREATE INDEX runs_by_created_at ON runs(created_at_ms DESC, id DESC);
CREATE INDEX runs_by_updated_at ON runs(last_event_at_ms DESC, id DESC);
CREATE INDEX runs_by_status ON runs(archived_at_ms, status, last_event_at_ms DESC, id DESC);
CREATE INDEX runs_by_parent ON runs(parent_id, created_at_ms DESC, id DESC);
CREATE INDEX runs_by_automation ON runs(automation_id, created_at_ms DESC, id DESC);
