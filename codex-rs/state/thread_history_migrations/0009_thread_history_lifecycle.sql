-- Fork (history-pruning): when each projection was last used, and whether a build from offset 0
-- is still running. Rows written by older binaries default to never used and fully built.
ALTER TABLE thread_history_projection_state ADD COLUMN accessed_at_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE thread_history_projection_state ADD COLUMN rebuilding INTEGER NOT NULL DEFAULT 0;

-- No index on accessed_at_ms: pruning scans this one-row-per-rollout table a few times a day,
-- while an index would add a page to every projected write.

-- One row per maintenance step, recording its last attempt so failures retry at most daily.
CREATE TABLE thread_history_maintenance (
    step TEXT PRIMARY KEY,
    attempted_at_ms INTEGER NOT NULL
);
