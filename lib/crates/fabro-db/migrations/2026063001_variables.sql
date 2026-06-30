CREATE TABLE variables (
    name TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    description TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (length(name) > 0),
    CHECK (substr(name, 1, 1) GLOB '[A-Za-z_]'),
    CHECK (name NOT GLOB '*[^A-Za-z0-9_]*'),
    CHECK (length(created_at) > 0),
    CHECK (length(updated_at) > 0)
);
