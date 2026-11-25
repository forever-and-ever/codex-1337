-- Older builds marked a bounded checkpoint ready for the entire rollout. Keep the rows and
-- offsets; require an unrestricted pass before trusting them for an unrestricted read.
UPDATE thread_history_projection_state SET rebuilding = 1 WHERE rebuilding = 0;
