#[cfg(test)]
pub(crate) use codex_tui_status::status::StatusAccountDisplay;
#[cfg(test)]
pub(crate) use codex_tui_status::status::new_status_output_with_rate_limits_handle;

#[cfg(test)]
pub(crate) use codex_tui_status::status::new_status_output;
#[cfg(test)]
pub(crate) use codex_tui_status::status::new_status_output_with_rate_limits;
#[cfg(test)]
pub(crate) use codex_tui_status::status::rate_limit_snapshot_display;

#[cfg(test)]
pub(crate) mod rate_limits {
    pub(crate) use codex_tui_status::status::rate_limits::*;
}

pub(crate) mod remote_connection;

#[cfg(test)]
mod tests;
