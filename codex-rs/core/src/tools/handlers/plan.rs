use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::registry::CoreToolRuntime;
use codex_protocol::plan_tool::UpdatePlanArgs;
use codex_protocol::protocol::EventMsg;
use futures::future::BoxFuture;

pub use codex_core_tool_runtime::PlanHandler;

impl codex_core_tool_runtime::PlanHost for Session {
    fn send_plan_update<'a>(
        &'a self,
        turn: &'a TurnContext,
        args: UpdatePlanArgs,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.send_event(turn, EventMsg::PlanUpdate(args)).await;
        })
    }
}

impl CoreToolRuntime for PlanHandler {}
