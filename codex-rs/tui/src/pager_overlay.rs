use crate::tui::Tui;
use crate::tui::TuiEvent;
use std::io::Result;

pub(crate) use codex_tui_overlays::pager_overlay::Overlay;
#[cfg(test)]
pub(crate) use codex_tui_overlays::pager_overlay::TranscriptOverlay;

pub(crate) fn handle_event(overlay: &mut Overlay, tui: &mut Tui, event: TuiEvent) -> Result<()> {
    match event {
        TuiEvent::Key(key_event) => {
            let viewport_area = tui.terminal.viewport_area;
            let frame_requester = tui.frame_requester();
            overlay.handle_key_event(viewport_area, &frame_requester, key_event);
            Ok(())
        }
        TuiEvent::Draw | TuiEvent::Resize => tui.draw(u16::MAX, |frame| {
            overlay.render(frame.area(), frame.buffer);
        }),
        TuiEvent::Paste(_) => Ok(()),
    }
}
