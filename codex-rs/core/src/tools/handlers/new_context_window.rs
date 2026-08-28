use crate::session::session::Session;
use crate::tools::registry::CoreToolRuntime;
use futures::future::BoxFuture;

pub use codex_core_tool_runtime::NewContextWindowHandler;

impl codex_core_tool_runtime::NewContextWindowHost for Session {
    fn request_new_context_window(&self) -> BoxFuture<'_, ()> {
        Box::pin(Session::request_new_context_window(self))
    }
}

impl CoreToolRuntime for NewContextWindowHandler {}
