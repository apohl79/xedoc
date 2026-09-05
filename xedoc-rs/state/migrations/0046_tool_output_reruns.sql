ALTER TABLE tool_output_reductions ADD COLUMN command_hash TEXT;

CREATE INDEX tool_output_reductions_command_hash_idx
    ON tool_output_reductions (command_hash);
