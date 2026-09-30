-- Retire only the delegated Codex message class. User input and other named
-- tool outputs remain queued; the existing DELETE trigger publishes revisions.
DELETE FROM queued_items
WHERE CASE WHEN json_valid(payload_json) THEN
    json_extract(payload_json, '$.ResponseItem.type') = 'function_call_output'
    AND json_extract(payload_json, '$.ResponseItem.namespace') = 'codex_tui'
    AND json_extract(payload_json, '$.ResponseItem.name') = 'send_message_to_thread'
ELSE 0 END;
