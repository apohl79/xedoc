use crate::session::session::Session;
use crate::tools::registry::CoreToolRuntime;
use futures::future::BoxFuture;

pub use codex_core_tool_runtime::CurrentTimeHandler;

impl codex_core_tool_runtime::CurrentTimeHost for Session {
    fn current_time(&self) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            self.services
                .time_provider
                .current_time(self.thread_id)
                .await
                .map(|current_time| current_time.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                .map_err(|err| err.to_string())
        })
    }
}

impl CoreToolRuntime for CurrentTimeHandler {}
