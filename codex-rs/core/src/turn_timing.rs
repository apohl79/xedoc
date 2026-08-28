use crate::ResponseEvent;
use crate::session::turn_context::TurnContext;
use codex_otel::TURN_TTFM_DURATION_METRIC;

pub(crate) use codex_core_turn_timing::TurnTimingState;
pub(crate) use codex_core_turn_timing::now_unix_timestamp_ms;

pub(crate) async fn record_turn_ttft_metric(turn_context: &TurnContext, event: &ResponseEvent) {
    let Some(duration) = turn_context
        .turn_timing_state
        .record_ttft_for_response_event(event)
        .await
    else {
        return;
    };
    turn_context.session_telemetry.record_turn_ttft(duration);
}

pub(crate) async fn record_turn_ttfm_metric(
    turn_context: &TurnContext,
    item: &codex_protocol::items::TurnItem,
) {
    let Some(duration) = turn_context
        .turn_timing_state
        .record_ttfm_for_turn_item(item)
        .await
    else {
        return;
    };
    turn_context
        .session_telemetry
        .record_duration(TURN_TTFM_DURATION_METRIC, duration, &[]);
}
