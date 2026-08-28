use super::*;

// Return value of `Session::reconstruct_history_from_rollout`, bundling rebuilt history with the
// resume/fork hydration metadata derived from the same replay.
#[derive(Debug)]
pub(super) struct RolloutReconstruction {
    pub(super) history: Vec<ResponseItem>,
    pub(super) previous_turn_settings: Option<PreviousTurnSettings>,
    pub(super) reference_context_item: Option<TurnContextItem>,
    pub(super) world_state_baseline: Option<crate::context::world_state::WorldStateSnapshot>,
    pub(super) window_number: u64,
    pub(super) first_window_id: Option<Uuid>,
    pub(super) previous_window_id: Option<Uuid>,
    pub(super) window_id: Option<Uuid>,
}

impl From<codex_core_rollout_reconstruction::ReconstructedTurnSettings> for PreviousTurnSettings {
    fn from(settings: codex_core_rollout_reconstruction::ReconstructedTurnSettings) -> Self {
        Self {
            model: settings.model,
            comp_hash: settings.comp_hash,
            realtime_active: settings.realtime_active,
        }
    }
}

impl Session {
    pub(super) async fn reconstruct_history_from_rollout(
        &self,
        turn_context: &TurnContext,
        rollout_items: &[RolloutItem],
    ) -> RolloutReconstruction {
        let reconstruction = codex_core_rollout_reconstruction::reconstruct_history_from_rollout(
            turn_context.model_info.truncation_policy.into(),
            rollout_items,
            &|history, summary| {
                let user_messages = compact::collect_user_messages(history);
                compact::build_compacted_history(Vec::new(), &user_messages, summary)
            },
        );
        RolloutReconstruction {
            history: reconstruction.history,
            previous_turn_settings: reconstruction.previous_turn_settings.map(Into::into),
            reference_context_item: reconstruction.reference_context_item,
            world_state_baseline: reconstruction.world_state_baseline,
            window_number: reconstruction.window_number,
            first_window_id: reconstruction.first_window_id,
            previous_window_id: reconstruction.previous_window_id,
            window_id: reconstruction.window_id,
        }
    }
}
