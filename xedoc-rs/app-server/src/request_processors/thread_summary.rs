use super::*;
use xedoc_protocol::config_types::MultiAgentMode;

pub(super) fn with_thread_spawn_agent_metadata(
    source: xedoc_protocol::protocol::SessionSource,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
) -> xedoc_protocol::protocol::SessionSource {
    if agent_nickname.is_none() && agent_role.is_none() {
        return source;
    }

    match source {
        xedoc_protocol::protocol::SessionSource::SubAgent(
            xedoc_protocol::protocol::SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth,
                agent_path,
                agent_nickname: existing_agent_nickname,
                agent_role: existing_agent_role,
            },
        ) => xedoc_protocol::protocol::SessionSource::SubAgent(
            xedoc_protocol::protocol::SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth,
                agent_path,
                agent_nickname: agent_nickname.or(existing_agent_nickname),
                agent_role: agent_role.or(existing_agent_role),
            },
        ),
        _ => source,
    }
}

pub(crate) fn thread_response_active_permission_profile(
    active_permission_profile: Option<xedoc_protocol::models::ActivePermissionProfile>,
) -> Option<xedoc_app_server_protocol::ActivePermissionProfile> {
    active_permission_profile.map(Into::into)
}

pub(crate) fn thread_settings_from_config_snapshot(
    config_snapshot: &ThreadConfigSnapshot,
) -> ThreadSettings {
    ThreadSettings {
        cwd: config_snapshot.cwd().clone(),
        approval_policy: config_snapshot.approval_policy.into(),
        sandbox_policy: config_snapshot.sandbox_policy().into(),
        active_permission_profile: thread_response_active_permission_profile(
            config_snapshot.active_permission_profile.clone(),
        ),
        model: config_snapshot.model.clone(),
        model_provider: config_snapshot.model_provider_id.clone(),
        service_tier: config_snapshot.service_tier.clone(),
        effort: config_snapshot.reasoning_effort.clone(),
        summary: config_snapshot.reasoning_summary,
        collaboration_mode: config_snapshot.collaboration_mode.clone(),
        multi_agent_mode: MultiAgentMode::ExplicitRequestOnly,
        personality: config_snapshot.personality,
    }
}

pub(crate) fn thread_settings_from_core_snapshot(
    snapshot: xedoc_protocol::protocol::ThreadSettingsSnapshot,
) -> ThreadSettings {
    let xedoc_protocol::protocol::ThreadSettingsSnapshot {
        model,
        model_provider_id,
        service_tier,
        approval_policy,
        permission_profile,
        active_permission_profile,
        cwd,
        reasoning_effort,
        reasoning_summary,
        personality,
        collaboration_mode,
    } = snapshot;
    let sandbox_policy = xedoc_sandboxing::compatibility_sandbox_policy_for_permission_profile(
        &permission_profile,
        cwd.as_path(),
    )
    .into();
    ThreadSettings {
        sandbox_policy,
        cwd,
        approval_policy: approval_policy.into(),
        active_permission_profile: thread_response_active_permission_profile(
            active_permission_profile,
        ),
        model,
        model_provider: model_provider_id,
        service_tier,
        effort: reasoning_effort,
        summary: reasoning_summary,
        collaboration_mode,
        multi_agent_mode: MultiAgentMode::ExplicitRequestOnly,
        personality,
    }
}

pub(super) fn thread_started_notification(mut thread: Thread) -> ThreadStartedNotification {
    thread.turns.clear();
    ThreadStartedNotification { thread }
}
