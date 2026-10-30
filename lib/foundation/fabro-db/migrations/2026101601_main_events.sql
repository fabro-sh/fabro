-- The global lifecycle mirror outlives individual runs. Deliberately no
-- foreign key to runs: deleting a run must keep its events and source keys.
CREATE TABLE main_events (
    seq INTEGER PRIMARY KEY CHECK (seq > 0 AND seq <= 4294967295),
    source_key TEXT UNIQUE,
    event_json TEXT NOT NULL
);
