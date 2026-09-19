-- The events of an Ask Fabro session: the turns and their messages, tool
-- calls and endings, numbered per session from 1. They stream a turn live
-- and project the session's metadata; the conversation itself is the
-- session's record in `run_session_records`.
CREATE TABLE run_session_events (
    session_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    turn_id TEXT,
    event_name TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL,
    properties_json TEXT NOT NULL,
    PRIMARY KEY (session_id, seq),
    CHECK (seq >= 1),
    CHECK (json_valid(properties_json))
) WITHOUT ROWID;

CREATE INDEX run_session_events_by_run
ON run_session_events(run_id, session_id, seq);
