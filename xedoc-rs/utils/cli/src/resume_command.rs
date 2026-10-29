//! Shared formatting for user-facing `xedoc resume` command hints.

use xedoc_protocol::ThreadId;
use xedoc_shell_command::parse_command::shlex_join;

pub fn resume_command(thread_name: Option<&str>, thread_id: Option<ThreadId>) -> Option<String> {
    let resume_target = thread_name
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| thread_id.map(|thread_id| thread_id.to_string()));
    resume_target.map(|target| {
        let needs_double_dash = target.starts_with('-');
        let escaped = shlex_join(&[target]);
        if needs_double_dash {
            format!("xedoc resume -- {escaped}")
        } else {
            format!("xedoc resume {escaped}")
        }
    })
}

pub fn resume_hint(thread_name: Option<&str>, thread_id: Option<ThreadId>) -> Option<String> {
    let thread_id = thread_id?;
    match thread_name.filter(|name| !name.is_empty()) {
        Some(thread_name) => Some(format!(
            "xedoc resume, then select {thread_name} ({thread_id})"
        )),
        None => resume_command(/*thread_name*/ None, Some(thread_id)),
    }
}
