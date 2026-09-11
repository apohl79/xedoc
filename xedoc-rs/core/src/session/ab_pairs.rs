//! Ephemeral root-thread state for one-turn model-router A/B experiments.

use xedoc_model_router::ModelRoute;

/// A bounded preference recorded by the root after inspecting a completed pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AbPairPreference {
    Routed,
    Orchestrator,
    Tie,
    Unusable,
}

impl AbPairPreference {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Routed => "routed",
            Self::Orchestrator => "orchestrator",
            Self::Tie => "tie",
            Self::Unusable => "unusable",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveAbPair {
    pub(crate) pair_id: String,
    pub(crate) orchestrator_route: ModelRoute,
    pub(crate) router_decision_id: Option<String>,
    pub(crate) preference: Option<AbPairPreference>,
    pub(crate) turn_id: Option<String>,
    pub(crate) routed_decision_id: Option<String>,
    spawned: bool,
}

#[derive(Default)]
pub(crate) struct AbPairRuntime {
    armed_for_next_root_turn: bool,
    pub(crate) active: Option<ActiveAbPair>,
}

impl AbPairRuntime {
    pub(crate) fn arm_next(&mut self) {
        self.armed_for_next_root_turn = true;
    }

    pub(crate) fn disable(&mut self) {
        self.armed_for_next_root_turn = false;
        self.active = None;
    }

    pub(crate) fn begin_root_turn(&mut self, orchestrator_route: ModelRoute) {
        self.active = self.armed_for_next_root_turn.then(|| ActiveAbPair {
            pair_id: uuid::Uuid::now_v7().to_string(),
            orchestrator_route,
            router_decision_id: None,
            preference: None,
            turn_id: None,
            routed_decision_id: None,
            spawned: false,
        });
        self.armed_for_next_root_turn = false;
    }

    pub(crate) fn take_for_spawn(&mut self) -> Option<ActiveAbPair> {
        let active = self.active.as_mut()?;
        if active.spawned {
            return None;
        }
        active.spawned = true;
        Some(active.clone())
    }

    pub(crate) fn set_router_decision_id(&mut self, pair_id: &str, router_decision_id: String) {
        if let Some(active) = self.active.as_mut()
            && active.pair_id == pair_id
        {
            active.router_decision_id = Some(router_decision_id);
        }
    }
}
