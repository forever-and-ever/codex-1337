CREATE TABLE thread_farm_requests (
    parent_thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL CHECK (length(payload_sha256) = 64),
    child_thread_id TEXT UNIQUE,
    agent_path TEXT,
    instructions TEXT,
    accepted INTEGER NOT NULL DEFAULT 0 CHECK (accepted IN (0, 1)),
    PRIMARY KEY (parent_thread_id, request_id),
    UNIQUE (parent_thread_id, agent_path)
);
