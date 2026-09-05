CREATE TABLE tool_output_reductions (
    id INTEGER PRIMARY KEY,
    thread_id TEXT,
    turn_id TEXT,
    call_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    kind TEXT NOT NULL,
    level TEXT NOT NULL,
    reducers_applied TEXT NOT NULL,
    bytes_in INTEGER NOT NULL,
    bytes_out INTEGER NOT NULL,
    est_tokens_in INTEGER NOT NULL,
    est_tokens_out INTEGER NOT NULL,
    duration_us INTEGER NOT NULL,
    spilled INTEGER NOT NULL,
    spill_path TEXT,
    recorded_at INTEGER NOT NULL
);

CREATE INDEX tool_output_reductions_recorded_at_idx
    ON tool_output_reductions (recorded_at);
CREATE INDEX tool_output_reductions_thread_id_idx
    ON tool_output_reductions (thread_id);
