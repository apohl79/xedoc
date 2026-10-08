//! Renders a remote session's transcript items with the regular history cells.

use xedoc_app_server_protocol::RemoteSessionItem;

use crate::chatwidget::ChatWidget;
use crate::chatwidget::ReplayKind;

/// Shows finished remote items exactly like the items of a local thread.
pub(super) fn show_remote_session_items(
    chat_widget: &mut ChatWidget,
    items: Vec<RemoteSessionItem>,
) {
    for RemoteSessionItem { turn_id, item } in items {
        chat_widget.replay_thread_item(item, turn_id, ReplayKind::ThreadSnapshot);
    }
}
