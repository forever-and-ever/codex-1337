-- Unlike history projections, accepted mail is authoritative until canonical admission.
CREATE TABLE agent_mail (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    root_thread_id TEXT NOT NULL,
    sender_thread_id TEXT NOT NULL,
    recipient_thread_id TEXT NOT NULL,
    origin TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL CHECK (length(payload_sha256) = 64),
    state TEXT NOT NULL CHECK (state IN ('pending', 'admitted', 'cancelled')),
    payload_json TEXT,
    CHECK ((state = 'pending' AND payload_json IS NOT NULL)
        OR (state <> 'pending' AND payload_json IS NULL)),
    UNIQUE (sender_thread_id, recipient_thread_id, origin)
);
CREATE INDEX agent_mail_pending_recipient
    ON agent_mail(recipient_thread_id, sequence) WHERE state = 'pending';
