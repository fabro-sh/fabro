CREATE TABLE workflow_runs (
    id          TEXT PRIMARY KEY NOT NULL,
    title       TEXT NOT NULL DEFAULT '',
    logs_dir    TEXT NOT NULL,
    work_dir    TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
