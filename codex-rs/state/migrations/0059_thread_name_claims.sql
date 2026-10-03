-- Names, unlike rollout-derived titles, are durable claims in both history modes.
WITH whitespace(chars) AS (
    VALUES (char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288))
)
UPDATE threads
SET name = trim(title, (SELECT chars FROM whitespace))
WHERE history_mode = 'legacy'
    AND trim(COALESCE(name, ''), (SELECT chars FROM whitespace)) = ''
    AND trim(title, (SELECT chars FROM whitespace)) != ''
    AND trim(title, (SELECT chars FROM whitespace))
        != trim(COALESCE(first_user_message, ''), (SELECT chars FROM whitespace));

UPDATE threads SET name = NULLIF(trim(name,
    char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)), '');

-- A duplicate aborts migration. Never hide an existing owner by archiving it.
CREATE UNIQUE INDEX threads_active_name_claim
ON threads (name COLLATE NOCASE)
WHERE archived = 0
    AND name IS NOT NULL
    AND CASE WHEN json_valid(source)
        THEN json_type(source, '$.subagent') IS NULL
            AND json_type(source, '$.internal') IS NULL
        ELSE 1 END;

-- Local storage imports the legacy name index once, before names can be claimed.
CREATE TABLE thread_name_claims_import (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1)
);
