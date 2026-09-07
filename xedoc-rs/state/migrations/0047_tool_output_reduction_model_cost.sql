ALTER TABLE tool_output_reductions ADD COLUMN model_slug TEXT;
ALTER TABLE tool_output_reductions ADD COLUMN input_price_per_1m REAL;

CREATE INDEX tool_output_reductions_model_slug_idx
    ON tool_output_reductions (model_slug);
