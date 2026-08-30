/*
Module: runtimes

Concrete ToolRuntime implementations for specific tools. Shared execution
helpers live in `xedoc-core-exec` so they can be reused without depending on
the session host.
*/
pub(crate) use xedoc_core_exec::tool_runtime::*;

pub(crate) mod apply_patch;
pub(crate) mod shell;
pub(crate) mod unified_exec;
