PRAGMA foreign_keys = ON;

CREATE TABLE schema_version (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    version INTEGER NOT NULL,
    applied_at TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT INTO schema_version (id, version) VALUES (1, 1);

CREATE TABLE work_item (
    id INTEGER PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'done', 'canceled')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT INTO work_item (slug, title, description)
VALUES ('preserved-old-work', 'Preserved old work', 'Synthetic fixture; contains no user data');

-- This represents an adapter using an old Core database as an uncoordinated
-- schema host. The unified contract must reject it before altering either table.
CREATE TABLE adapter_unregistered_record (
    id INTEGER PRIMARY KEY,
    source_version INTEGER NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json))
);
INSERT INTO adapter_unregistered_record (source_version, payload_json)
VALUES (2, '{"fixture":true}');
