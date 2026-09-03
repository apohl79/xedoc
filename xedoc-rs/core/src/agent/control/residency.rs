use super::AgentControl;
use crate::agent::AgentStatus;
use crate::config::Config;
use crate::thread_manager::ThreadManagerState;
use crate::xedoc_thread::XedocThread;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use tracing::warn;
use xedoc_protocol::ThreadId;
use xedoc_protocol::error::Result as XedocResult;
use xedoc_protocol::error::XedocErr;
use xedoc_protocol::protocol::MultiAgentVersion;
use xedoc_protocol::protocol::SessionSource;

#[derive(Default)]
pub(crate) struct V2Residency {
    state: Mutex<V2ResidencyState>,
}

#[derive(Default)]
struct V2ResidencyState {
    residents: VecDeque<ThreadId>,
    pending_slots: usize,
}

pub(super) struct V2ResidencySlot {
    residency: Arc<V2Residency>,
    active: bool,
}

impl V2ResidencySlot {
    pub(super) fn commit(mut self, thread_id: ThreadId) {
        self.residency.commit_slot(thread_id);
        self.active = false;
    }
}

impl Drop for V2ResidencySlot {
    fn drop(&mut self) {
        if self.active {
            self.residency.release_pending_slot();
        }
    }
}

impl AgentControl {
    pub(super) async fn reserve_v2_residency_slot(
        &self,
        state: &Arc<ThreadManagerState>,
        config: &Config,
        protected_thread_id: Option<ThreadId>,
    ) -> XedocResult<V2ResidencySlot> {
        Arc::clone(&self.v2_residency)
            .reserve_slot(
                state,
                config.multi_agent_v2.max_loaded_threads_per_session,
                protected_thread_id,
            )
            .await
    }

    pub(super) async fn touch_loaded_v2_residency(
        &self,
        state: &Arc<ThreadManagerState>,
        thread_id: ThreadId,
    ) {
        if let Ok(thread) = state.get_thread(thread_id).await
            && is_resident_candidate(thread.as_ref())
        {
            self.v2_residency.touch(thread_id);
        }
    }

    pub(super) fn forget_v2_residency(&self, thread_id: ThreadId) {
        self.v2_residency.remove(thread_id);
    }

    pub(crate) fn schedule_terminal_v2_unload(&self, thread_id: ThreadId, status: AgentStatus) {
        self.state.update_last_status(thread_id, status);
        let expected_io_generation = self.v2_agent_io_generation(thread_id);
        let control = self.clone();
        tokio::spawn(async move {
            let Ok(state) = control.upgrade() else {
                return;
            };
            if !control
                .try_unload_v2_agent(&state, thread_id, Some(expected_io_generation))
                .await
            {
                control.touch_loaded_v2_residency(&state, thread_id).await;
            }
        });
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "the per-agent I/O lock serializes the complete unload lifecycle operation"
    )]
    async fn try_unload_v2_agent(
        &self,
        manager: &Arc<ThreadManagerState>,
        thread_id: ThreadId,
        expected_io_generation: Option<u64>,
    ) -> bool {
        let io_lock = self.v2_agent_io_lock(thread_id);
        let _guard = io_lock.lock().await;
        if expected_io_generation
            .is_some_and(|expected| self.v2_agent_io_generation(thread_id) != expected)
        {
            return false;
        }
        let Some(thread) = manager
            .get_thread(thread_id)
            .await
            .ok()
            .filter(|thread| is_resident_candidate(thread))
        else {
            return false;
        };
        if !is_unloadable(thread.as_ref()).await {
            return false;
        }
        self.state
            .update_last_status(thread_id, thread.agent_status().await);
        thread.ensure_rollout_materialized().await;
        if let Err(err) = thread.shutdown_and_wait().await {
            warn!("failed to shut down v2 resident thread before unloading {thread_id}: {err}");
            return false;
        }
        let _ = manager.remove_thread(&thread_id).await;
        self.forget_v2_residency(thread_id);
        thread.session.release_resident_memory().await;
        drop(thread);
        release_unused_allocator_memory();
        true
    }
}

impl V2Residency {
    async fn reserve_slot(
        self: Arc<Self>,
        manager: &Arc<ThreadManagerState>,
        capacity: usize,
        protected_thread_id: Option<ThreadId>,
    ) -> XedocResult<V2ResidencySlot> {
        loop {
            if self.try_reserve_pending_slot(capacity) {
                return Ok(V2ResidencySlot {
                    residency: self,
                    active: true,
                });
            }
            if !self
                .try_unload_one_resident(manager, protected_thread_id)
                .await
            {
                return Err(XedocErr::AgentLimitReached {
                    max_threads: capacity,
                });
            }
        }
    }

    fn try_reserve_pending_slot(&self, capacity: usize) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.residents.len().saturating_add(state.pending_slots) >= capacity {
            return false;
        }
        state.pending_slots += 1;
        true
    }

    async fn try_unload_one_resident(
        &self,
        manager: &Arc<ThreadManagerState>,
        protected_thread_id: Option<ThreadId>,
    ) -> bool {
        let candidates_to_scan = self.resident_count();
        for _ in 0..candidates_to_scan {
            let Some(candidate_thread_id) = self.pop_lru_candidate(protected_thread_id) else {
                return false;
            };
            let Some(candidate_control) = manager
                .get_thread(candidate_thread_id)
                .await
                .ok()
                .filter(|thread| is_resident_candidate(thread))
                .map(|thread| thread.session.services.agent_control.clone())
            else {
                return true;
            };
            let expected_io_generation =
                candidate_control.v2_agent_io_generation(candidate_thread_id);
            if !candidate_control
                .try_unload_v2_agent(manager, candidate_thread_id, Some(expected_io_generation))
                .await
            {
                self.touch(candidate_thread_id);
                continue;
            }
            return true;
        }
        false
    }

    fn resident_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .residents
            .len()
    }

    fn pop_lru_candidate(&self, protected_thread_id: Option<ThreadId>) -> Option<ThreadId> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let candidates_to_scan = state.residents.len();
        for _ in 0..candidates_to_scan {
            let candidate_thread_id = state.residents.pop_front()?;
            if Some(candidate_thread_id) == protected_thread_id {
                state.residents.push_back(candidate_thread_id);
                continue;
            }
            return Some(candidate_thread_id);
        }
        None
    }

    fn touch(&self, thread_id: ThreadId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        touch_resident(&mut state.residents, thread_id);
    }

    fn remove(&self, thread_id: ThreadId) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .residents
            .retain(|resident_thread_id| *resident_thread_id != thread_id);
    }

    fn commit_slot(&self, thread_id: ThreadId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending_slots = state.pending_slots.saturating_sub(1);
        touch_resident(&mut state.residents, thread_id);
    }

    fn release_pending_slot(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending_slots = state.pending_slots.saturating_sub(1);
    }
}

fn touch_resident(residents: &mut VecDeque<ThreadId>, thread_id: ThreadId) {
    residents.retain(|resident_thread_id| *resident_thread_id != thread_id);
    residents.push_back(thread_id);
}

fn is_resident_candidate(thread: &XedocThread) -> bool {
    thread.multi_agent_version() == Some(MultiAgentVersion::V2)
        && is_v2_resident_session_source(&thread.session_source)
}

pub(super) fn is_v2_resident_session_source(session_source: &SessionSource) -> bool {
    matches!(session_source, SessionSource::SubAgent(_))
}

async fn is_unloadable(thread: &XedocThread) -> bool {
    matches!(
        thread.agent_status().await,
        AgentStatus::Completed(_) | AgentStatus::Errored(_) | AgentStatus::Interrupted
    ) && thread.session.active_turn.lock().await.is_none()
        && !thread.session.input_queue.has_pending_mailbox_items().await
}

#[cfg(target_os = "macos")]
fn release_unused_allocator_memory() {
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }

    // SAFETY: A null zone asks the system allocator to inspect every registered zone. A zero
    // goal is the documented request to release as many unused pages as practical.
    unsafe {
        malloc_zone_pressure_relief(/*zone*/ std::ptr::null_mut(), /*goal*/ 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn release_unused_allocator_memory() {}

#[cfg(test)]
#[path = "residency_tests.rs"]
mod tests;
