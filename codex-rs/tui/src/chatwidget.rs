pub(crate) use codex_tui_chatwidget::*;

#[cfg(test)]
pub(crate) mod tests {
    pub(crate) use codex_tui_chatwidget::dependency_test_support::make_chatwidget_manual_with_sender;
    pub(crate) use codex_tui_chatwidget::dependency_test_support::set_chatgpt_auth;
    pub(crate) use codex_tui_chatwidget::dependency_test_support::set_fast_mode_test_catalog;

    pub(crate) mod helpers {
        pub(crate) use codex_tui_chatwidget::dependency_test_support::render_bottom_popup;
        pub(crate) use codex_tui_chatwidget::dependency_test_support::set_active_cell;
    }
}
