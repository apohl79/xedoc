ALTER TABLE model_router_daily
ADD COLUMN classified_decisions INTEGER NOT NULL DEFAULT 0;

ALTER TABLE model_router_daily
ADD COLUMN fallback_decisions INTEGER NOT NULL DEFAULT 0;

ALTER TABLE model_router_daily
ADD COLUMN average_score REAL;

ALTER TABLE model_router_daily
ADD COLUMN average_margin REAL;
