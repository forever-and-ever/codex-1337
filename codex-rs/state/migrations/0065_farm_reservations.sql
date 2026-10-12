CREATE TABLE thread_farm_reservations (
    parent_thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL CHECK (length(payload_sha256) = 64),
    state TEXT NOT NULL CHECK (state IN ('legacy', 'reserved', 'allocated', 'accepted', 'rejected')),
    child_thread_id TEXT UNIQUE,
    agent_path TEXT,
    instructions TEXT,
    rejection_kind TEXT CHECK (rejection_kind IN ('invalid_request', 'unsupported_operation', 'fatal')),
    rejection_message TEXT,
    PRIMARY KEY (parent_thread_id, request_id),
    UNIQUE (parent_thread_id, agent_path),
    CHECK (
        (state = 'legacy' AND child_thread_id IS NULL AND agent_path IS NULL AND rejection_kind IS NULL AND rejection_message IS NULL)
        OR (state = 'reserved' AND child_thread_id IS NULL AND agent_path IS NOT NULL AND rejection_kind IS NULL AND rejection_message IS NULL)
        OR (state IN ('allocated', 'accepted') AND child_thread_id IS NOT NULL AND agent_path IS NOT NULL AND rejection_kind IS NULL AND rejection_message IS NULL)
        OR (state = 'rejected' AND child_thread_id IS NULL AND agent_path IS NULL AND instructions IS NULL AND rejection_kind IS NOT NULL AND rejection_message IS NOT NULL)
    )
);

INSERT INTO thread_farm_reservations
    (parent_thread_id, request_id, payload_sha256, state, child_thread_id, agent_path, instructions)
SELECT parent_thread_id, request_id, payload_sha256,
    CASE WHEN accepted = 1 THEN 'accepted'
         WHEN child_thread_id IS NOT NULL THEN 'allocated'
         WHEN agent_path IS NOT NULL THEN 'reserved'
         ELSE 'legacy' END,
    child_thread_id, agent_path, instructions
FROM thread_farm_requests;

DROP TABLE thread_farm_requests;
ALTER TABLE thread_farm_reservations RENAME TO thread_farm_requests;
