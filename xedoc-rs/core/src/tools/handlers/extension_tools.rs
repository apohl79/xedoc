use std::sync::Arc;
use std::sync::Weak;

use xedoc_protocol::items::TurnItem;
use xedoc_protocol::protocol::Event;
use xedoc_protocol::protocol::EventMsg;
use xedoc_tools::ConversationHistory;
use xedoc_tools::ExtensionTurnItem;
use xedoc_tools::ToolCall as ExtensionToolCall;
use xedoc_tools::ToolEnvironment;
use xedoc_tools::ToolName;
use xedoc_tools::ToolSearchInfo;
use xedoc_tools::ToolSpec;
use xedoc_tools::TurnItemEmissionFuture;
use xedoc_tools::TurnItemEmitter;
use xedoc_utils_string::to_ascii_json_string;

use crate::sandboxing::SandboxPermissions;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::apply_granted_turn_permissions;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use crate::turn_metadata::McpTurnMetadataContext;

pub(crate) struct ExtensionToolAdapter(Arc<dyn xedoc_tools::ToolExecutor<ExtensionToolCall>>);

impl ExtensionToolAdapter {
    pub(crate) fn new(executor: Arc<dyn xedoc_tools::ToolExecutor<ExtensionToolCall>>) -> Self {
        Self(executor)
    }
}

impl ToolExecutor<ToolInvocation> for ExtensionToolAdapter {
    fn tool_name(&self) -> ToolName {
        self.0.tool_name()
    }

    fn spec(&self) -> ToolSpec {
        self.0.spec()
    }

    fn exposure(&self) -> crate::tools::registry::ToolExposure {
        self.0.exposure()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.0.supports_parallel_tool_calls()
    }

    fn search_info(&self) -> Option<ToolSearchInfo> {
        self.0.search_info()
    }

    fn handle(&self, invocation: ToolInvocation) -> xedoc_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { self.0.handle(to_extension_call(&invocation).await).await })
    }
}

impl CoreToolRuntime for ExtensionToolAdapter {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

struct CoreTurnItemEmitter {
    session: Weak<Session>,
    turn: Weak<TurnContext>,
}

async fn emit_legacy_events(session: &Session, turn: &TurnContext, legacy_events: Vec<EventMsg>) {
    for msg in legacy_events {
        session
            .send_event_raw(Event {
                id: turn.sub_id.clone(),
                msg,
            })
            .await;
    }
}

impl TurnItemEmitter for CoreTurnItemEmitter {
    fn emit_started<'a>(&'a self, item: ExtensionTurnItem) -> TurnItemEmissionFuture<'a> {
        Box::pin(async move {
            let (Some(session), Some(turn)) = (self.session.upgrade(), self.turn.upgrade()) else {
                return;
            };
            let ExtensionTurnItem {
                item,
                legacy_events,
            } = item;
            let item = TurnItem::Extension(item);
            session.emit_turn_item_started(turn.as_ref(), &item).await;
            emit_legacy_events(session.as_ref(), turn.as_ref(), legacy_events).await;
        })
    }

    fn emit_completed<'a>(&'a self, item: ExtensionTurnItem) -> TurnItemEmissionFuture<'a> {
        Box::pin(async move {
            let (Some(session), Some(turn)) = (self.session.upgrade(), self.turn.upgrade()) else {
                return;
            };
            let ExtensionTurnItem {
                item,
                legacy_events,
            } = item;
            let item = TurnItem::Extension(item);
            session.emit_turn_item_completed(turn.as_ref(), item).await;
            emit_legacy_events(session.as_ref(), turn.as_ref(), legacy_events).await;
        })
    }
}

async fn to_extension_call(invocation: &ToolInvocation) -> ExtensionToolCall {
    let conversation_history =
        ConversationHistory::new(invocation.session.clone_history().await.into_raw_items());
    let xedoc_turn_metadata = invocation
        .turn
        .turn_metadata_state
        .current_meta_value_for_mcp_request(McpTurnMetadataContext {
            model: invocation.turn.model_info.slug.as_str(),
            reasoning_effort: invocation.turn.effective_reasoning_effort(),
        })
        .and_then(|metadata| to_ascii_json_string(&metadata).ok());
    let mut environments = Vec::new();
    for environment in invocation.step_context.environments.turn_environments() {
        // TODO(anp): Migrate extension ToolEnvironment and granted-permission lookup to PathUri
        // so extensions can receive foreign environment cwd values.
        let Ok(native_cwd) = environment.cwd().to_abs_path() else {
            continue;
        };
        let additional_permissions = apply_granted_turn_permissions(
            invocation.session.as_ref(),
            &environment.environment_id,
            native_cwd.as_path(),
            SandboxPermissions::UseDefault,
            /*additional_permissions*/ None,
        )
        .await
        .additional_permissions;
        let file_system_sandbox_context = invocation
            .turn
            .file_system_sandbox_context(additional_permissions, environment);
        environments.push(ToolEnvironment {
            environment_id: environment.environment_id.clone(),
            cwd: native_cwd,
            file_system: environment.environment.get_filesystem(),
            file_system_sandbox_context,
        });
    }
    ExtensionToolCall {
        turn_id: invocation.turn.sub_id.clone(),
        call_id: invocation.call_id.clone(),
        tool_name: invocation.tool_name.clone(),
        model: invocation.turn.model_info.slug.clone(),
        xedoc_turn_metadata,
        truncation_policy: invocation.turn.model_info.truncation_policy.into(),
        conversation_history,
        turn_item_emitter: Arc::new(CoreTurnItemEmitter {
            session: Arc::downgrade(&invocation.session),
            turn: Arc::downgrade(&invocation.turn),
        }),
        environments,
        payload: invocation.payload.clone(),
    }
}
