//! Renders a remote session's transcript items with the regular history cells.

use xedoc_app_server_protocol::RemoteSessionItem;
use xedoc_app_server_protocol::RemoteSessionStatus;
use xedoc_app_server_protocol::Turn;
use xedoc_app_server_protocol::TurnItemsView;
use xedoc_app_server_protocol::TurnStatus;

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

/// Replays a paginated remote transcript through the same turn lifecycle as a local thread.
pub(super) fn replay_remote_session_transcript(
    chat_widget: &mut ChatWidget,
    items: Vec<RemoteSessionItem>,
    status: RemoteSessionStatus,
    active_turn_id: Option<&str>,
) {
    let mut turns: Vec<(String, Vec<_>)> = Vec::new();
    for RemoteSessionItem { turn_id, item } in items {
        if let Some((previous_turn_id, turn_items)) = turns.last_mut()
            && previous_turn_id == &turn_id
        {
            turn_items.push(item);
        } else {
            turns.push((turn_id, vec![item]));
        }
    }

    let turns = turns
        .into_iter()
        .map(|(id, items)| Turn {
            status: if status == RemoteSessionStatus::Running && active_turn_id == Some(id.as_str())
            {
                TurnStatus::InProgress
            } else {
                TurnStatus::Completed
            },
            id,
            items,
            items_view: TurnItemsView::Full,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        })
        .collect();
    chat_widget.replay_thread_turns(turns, ReplayKind::ThreadSnapshot);
}
