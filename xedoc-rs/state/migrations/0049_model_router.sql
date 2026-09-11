CREATE TABLE model_router_decisions (
    decision_id TEXT PRIMARY KEY NOT NULL,
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    parent_decision_id TEXT,
    classifier_revision TEXT,
    policy_revision TEXT NOT NULL,
    artifact_revision TEXT,
    artifact_sha256 TEXT,
    class_id TEXT,
    score REAL,
    margin REAL,
    proposed_provider_id TEXT,
    proposed_model_slug TEXT,
    proposed_reasoning_effort TEXT,
    effective_provider_id TEXT,
    effective_model_slug TEXT,
    effective_reasoning_effort TEXT,
    disposition TEXT NOT NULL,
    reason TEXT NOT NULL,
    prompt_sha256 TEXT NOT NULL,
    prompt_original_bytes INTEGER NOT NULL,
    prompt_truncated INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX model_router_decisions_recent
ON model_router_decisions (created_at DESC);

CREATE TABLE model_router_invocations (
    invocation_id TEXT PRIMARY KEY NOT NULL,
    decision_id TEXT REFERENCES model_router_decisions (decision_id),
    ab_pair_id TEXT REFERENCES model_router_ab_outcomes (pair_id),
    ab_branch TEXT,
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    response_id TEXT,
    invocation_kind TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    model_slug TEXT NOT NULL,
    reasoning_effort TEXT,
    input_tokens INTEGER,
    cached_input_tokens INTEGER,
    output_tokens INTEGER,
    actual_input_price_usd_per_token REAL,
    actual_cached_input_price_usd_per_token REAL,
    actual_output_price_usd_per_token REAL,
    actual_price_revision TEXT,
    input_cost_usd REAL,
    cached_input_cost_usd REAL,
    output_cost_usd REAL,
    total_cost_usd REAL,
    baseline_provider_id TEXT,
    baseline_model_slug TEXT,
    baseline_reasoning_effort TEXT,
    baseline_input_price_usd_per_token REAL,
    baseline_cached_input_price_usd_per_token REAL,
    baseline_output_price_usd_per_token REAL,
    baseline_price_revision TEXT,
    normalized_baseline_usd REAL,
    estimated_savings_usd REAL,
    ab_experiment_overhead_usd REAL,
    created_at INTEGER NOT NULL
);

CREATE INDEX model_router_invocations_decision
ON model_router_invocations (decision_id, created_at DESC);

CREATE UNIQUE INDEX model_router_invocations_idempotency
ON model_router_invocations (
    thread_id,
    turn_id,
    COALESCE(response_id, ''),
    invocation_kind
);

CREATE TABLE model_router_ab_outcomes (
    pair_id TEXT PRIMARY KEY NOT NULL,
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    routed_decision_id TEXT REFERENCES model_router_decisions (decision_id),
    orchestrator_decision_id TEXT REFERENCES model_router_decisions (decision_id),
    outcome TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE model_router_daily (
    day INTEGER NOT NULL,
    provider_id TEXT NOT NULL,
    model_slug TEXT NOT NULL,
    scope TEXT NOT NULL,
    reasoning_effort TEXT,
    decisions INTEGER NOT NULL,
    invocations INTEGER NOT NULL,
    input_tokens INTEGER,
    cached_input_tokens INTEGER,
    output_tokens INTEGER,
    total_cost_usd REAL,
    normalized_baseline_usd REAL,
    estimated_savings_usd REAL,
    ab_experiment_overhead_usd REAL,
    attributed_invocations INTEGER NOT NULL,
    unattributed_invocations INTEGER NOT NULL,
    missing_usage_invocations INTEGER NOT NULL,
    unknown_price_invocations INTEGER NOT NULL,
    PRIMARY KEY (day, provider_id, model_slug, scope, reasoning_effort)
);
