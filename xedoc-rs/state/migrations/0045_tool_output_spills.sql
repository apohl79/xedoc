CREATE TABLE tool_output_retrievals (
    id INTEGER PRIMARY KEY,
    call_id TEXT NOT NULL,
    spill_path TEXT NOT NULL,
    retrieved_at INTEGER NOT NULL
);

CREATE INDEX tool_output_retrievals_call_id_idx
    ON tool_output_retrievals (call_id);
