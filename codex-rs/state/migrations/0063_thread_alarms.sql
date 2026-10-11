CREATE TABLE thread_alarms (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL CHECK (owner IN ('production', 'staging')),
    thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    note TEXT NOT NULL,
    armed_at INTEGER NOT NULL,
    clock_at INTEGER,
    session_id INTEGER,
    command TEXT,
    pattern TEXT,
    deadline INTEGER,
    rendered_body TEXT,
    CHECK (
        (rendered_body IS NOT NULL AND clock_at IS NULL AND session_id IS NULL
            AND command IS NULL AND pattern IS NULL AND deadline IS NULL)
        OR
        (rendered_body IS NULL AND (
            (clock_at IS NOT NULL AND session_id IS NULL AND command IS NULL
                AND pattern IS NULL AND deadline IS NULL)
            OR
            (clock_at IS NULL AND session_id IS NOT NULL AND command IS NOT NULL)
        ))
    )
);

CREATE INDEX idx_thread_alarms_thread_owner_id ON thread_alarms(thread_id, owner, id);

CREATE TRIGGER thread_alarms_delete_on_archive
AFTER UPDATE OF archived ON threads
WHEN NEW.archived = 1
BEGIN
    DELETE FROM thread_alarms WHERE thread_id = NEW.id;
END;
