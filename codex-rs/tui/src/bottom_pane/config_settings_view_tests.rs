use super::*;
use crate::keymap::RuntimeKeymap;
use ratatui::buffer::Buffer;
use tokio::sync::mpsc::unbounded_channel;

fn render_snapshot(view: &ConfigSettingsView, width: u16) -> String {
    let area = Rect::new(0, 0, width, view.desired_height(width));
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    format!("{buf:?}")
}

fn view(auto_session_name: bool) -> ConfigSettingsView {
    let (app_event_tx, _app_event_rx) = unbounded_channel();
    ConfigSettingsView::new(
        auto_session_name,
        AppEventSender::new(app_event_tx),
        RuntimeKeymap::defaults().list,
    )
}

#[test]
fn config_settings_view_enabled_snapshot() {
    insta::assert_snapshot!(
        "config_settings_view_enabled",
        render_snapshot(&view(/*auto_session_name*/ true), /*width*/ 76)
    );
}
