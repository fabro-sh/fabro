CREATE TABLE legacy_imports (
    import_name TEXT PRIMARY KEY NOT NULL,
    source_path TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('imported', 'skipped_existing_target')),
    imported_rows INTEGER NOT NULL CHECK (imported_rows >= 0),
    skipped_rows INTEGER NOT NULL CHECK (skipped_rows >= 0),
    imported_at TEXT NOT NULL CHECK (length(imported_at) > 0)
);

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
