use std::pin::Pin;

use codex_tui_session_picker::SessionPickerEvent;
use codex_tui_session_picker::SessionPickerTui;
use tokio_stream::StreamExt as _;

use crate::custom_terminal;
use crate::tui::Tui;
use crate::tui::TuiEvent;

pub(crate) use codex_tui_session_picker::SessionSelection;
pub(crate) use codex_tui_session_picker::SessionTarget;
pub(crate) use codex_tui_session_picker::resume_source_kinds;
pub(crate) use codex_tui_session_picker::run_fork_picker_with_app_server;
pub(crate) use codex_tui_session_picker::run_resume_picker_from_existing_session_with_app_server;
pub(crate) use codex_tui_session_picker::run_resume_picker_with_app_server;

impl SessionPickerTui for Tui {
    fn enter_alt_screen(&mut self) -> std::io::Result<()> {
        Tui::enter_alt_screen(self)
    }

    fn leave_alt_screen(&mut self) -> std::io::Result<()> {
        Tui::leave_alt_screen(self)
    }

    fn frame_requester(&self) -> codex_tui_frame::FrameRequester {
        Tui::frame_requester(self)
    }

    fn event_stream(
        &self,
    ) -> Pin<Box<dyn tokio_stream::Stream<Item = SessionPickerEvent> + Send + 'static>> {
        Box::pin(Tui::event_stream(self).map(|event| match event {
            TuiEvent::Key(key_event) => SessionPickerEvent::Key(key_event),
            TuiEvent::Paste(pasted) => SessionPickerEvent::Paste(pasted),
            TuiEvent::Resize => SessionPickerEvent::Resize,
            TuiEvent::Draw => SessionPickerEvent::Draw,
        }))
    }

    fn terminal_size(&self) -> std::io::Result<ratatui::layout::Size> {
        self.terminal.size()
    }

    fn viewport_area(&self) -> ratatui::layout::Rect {
        self.terminal.viewport_area
    }

    fn draw(
        &mut self,
        height: u16,
        draw_fn: impl FnOnce(&mut custom_terminal::Frame),
    ) -> std::io::Result<()> {
        Tui::draw(self, height, draw_fn)
    }
}
