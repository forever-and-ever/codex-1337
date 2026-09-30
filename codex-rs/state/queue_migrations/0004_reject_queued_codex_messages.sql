-- A newer TUI can run migration 3 while an older daemon still owns delivery.
-- Remove anything that daemon added afterward, and reject stale writers at
-- the shared storage boundary instead of relying on their API validation.
DELETE FROM queued_items
WHERE CASE WHEN json_valid(payload_json) THEN
    json_extract(payload_json, '$.ResponseItem.type') = 'function_call_output'
    AND json_extract(payload_json, '$.ResponseItem.namespace') = 'codex_tui'
    AND json_extract(payload_json, '$.ResponseItem.name') = 'send_message_to_thread'
ELSE 0 END;

CREATE TRIGGER reject_queued_codex_message_insert
BEFORE INSERT ON queued_items
WHEN CASE WHEN json_valid(NEW.payload_json) THEN
    json_extract(NEW.payload_json, '$.ResponseItem.type') = 'function_call_output'
    AND json_extract(NEW.payload_json, '$.ResponseItem.namespace') = 'codex_tui'
    AND json_extract(NEW.payload_json, '$.ResponseItem.name') = 'send_message_to_thread'
ELSE 0 END
BEGIN
    SELECT RAISE(ABORT, 'Queued Codex messages are no longer supported; send immediately.');
END;

CREATE TRIGGER reject_queued_codex_message_update
BEFORE UPDATE OF payload_json ON queued_items
WHEN CASE WHEN json_valid(NEW.payload_json) THEN
    json_extract(NEW.payload_json, '$.ResponseItem.type') = 'function_call_output'
    AND json_extract(NEW.payload_json, '$.ResponseItem.namespace') = 'codex_tui'
    AND json_extract(NEW.payload_json, '$.ResponseItem.name') = 'send_message_to_thread'
ELSE 0 END
BEGIN
    SELECT RAISE(ABORT, 'Queued Codex messages are no longer supported; send immediately.');
END;
