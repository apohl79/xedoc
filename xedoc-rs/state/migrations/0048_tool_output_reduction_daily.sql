CREATE TABLE tool_output_reduction_daily (
    day INTEGER NOT NULL,
    model_slug TEXT,
    reductions INTEGER NOT NULL,
    tokens_saved INTEGER NOT NULL,
    cost_saved_usd REAL NOT NULL,
    PRIMARY KEY (day, model_slug)
);

CREATE TABLE tool_output_reduction_rollup_state (
    id INTEGER PRIMARY KEY CHECK (id = 0),
    last_folded_day INTEGER NOT NULL DEFAULT 0
);

INSERT INTO tool_output_reduction_rollup_state (id, last_folded_day)
VALUES (0, 0);
