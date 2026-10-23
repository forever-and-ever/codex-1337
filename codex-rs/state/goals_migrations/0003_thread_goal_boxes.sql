-- codex-1337 goal boxes: a goal's timebox and quotabox outlive its rewording and end when it
-- completes or is cleared. Writers set both timebox columns together and one quotabox column.
ALTER TABLE thread_goals ADD COLUMN timebox_set_at_ms INTEGER;
ALTER TABLE thread_goals ADD COLUMN timebox_deadline_ms INTEGER
    CHECK ((timebox_deadline_ms IS NULL) = (timebox_set_at_ms IS NULL));
ALTER TABLE thread_goals ADD COLUMN quotabox_percent INTEGER
    CHECK (quotabox_percent BETWEEN 1 AND 99);
ALTER TABLE thread_goals ADD COLUMN quotabox_usd_nanos INTEGER
    CHECK (quotabox_usd_nanos IS NULL OR quotabox_percent IS NULL);

CREATE TRIGGER thread_goal_boxes_end_with_completion
AFTER UPDATE OF status ON thread_goals
WHEN NEW.status = 'complete'
BEGIN
    UPDATE thread_goals
    SET timebox_set_at_ms = NULL,
        timebox_deadline_ms = NULL,
        quotabox_percent = NULL,
        quotabox_usd_nanos = NULL
    WHERE thread_id = NEW.thread_id;
END;
