//! Browser attachment using the terminal's controller, history cells, and input paths.

use std::io::Cursor;
use std::io::Write;
use std::time::Duration;
use std::time::Instant;

use base64::Engine;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use serde_json::json;
use xedoc_app_server_protocol::ClientRequest;
use xedoc_app_server_protocol::RequestId;
use xedoc_app_server_protocol::ThreadWebUiOpenParams;
use xedoc_app_server_protocol::ThreadWebUiOpenResponse;
use xedoc_protocol::ThreadId;

use super::App;
use super::AppRunControl;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::render::renderable::Renderable;
use crate::tui::Tui;
use crate::web_ui::BrowserInput;
use crate::web_ui::CachedHistoryCell;
use crate::web_ui::WebUiSession;
use crate::web_ui_html::buffer_html;

const FRAME_INTERVAL: Duration = Duration::from_millis(/*millis*/ 80);
const HISTORY_BYTES: usize = 1_000_000;

impl App {
    pub(super) fn open_web_ui(&mut self, tui: &mut Tui, app_server: &AppServerSession) {
        if let Some(session) = &tui.web_ui
            && !session.task.is_finished()
        {
            self.open_url_in_browser(session.url.clone());
            return;
        }
        let Some(thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget
                .add_error_message("The session has not started yet.".to_string());
            return;
        };
        let handle = app_server.request_handle();
        let events = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = handle
                .request_typed(ClientRequest::ThreadWebUiOpen {
                    request_id: RequestId::String(format!("webui-{}", uuid::Uuid::new_v4())),
                    params: ThreadWebUiOpenParams {
                        thread_id: thread_id.to_string(),
                    },
                })
                .await
                .map_err(|_| "Failed to open the local session web UI.".to_string());
            events.send(AppEvent::WebUiOpened { thread_id, result });
        });
    }

    pub(super) fn web_ui_opened(
        &mut self,
        tui: &mut Tui,
        thread_id: ThreadId,
        result: Result<ThreadWebUiOpenResponse, String>,
    ) {
        match result {
            Ok(response)
                if tui
                    .web_ui
                    .as_ref()
                    .is_none_or(|session| session.task.is_finished()) =>
            {
                let url = response.url.clone();
                tui.web_ui = Some(WebUiSession::new(
                    thread_id,
                    response,
                    self.app_event_tx.clone(),
                    tui.browser_input_sender(),
                ));
                self.publish_web_ui(tui);
                self.open_url_in_browser(url);
            }
            Ok(_) => {}
            Err(error) => self.chat_widget.add_error_message(error),
        }
        tui.frame_requester().schedule_frame();
    }

    pub(super) async fn handle_web_ui_input(
        &mut self,
        tui: &mut Tui,
        thread_id: ThreadId,
        input: String,
    ) -> color_eyre::Result<AppRunControl> {
        if !tui
            .web_ui
            .as_ref()
            .is_some_and(|session| session.thread_id == thread_id)
        {
            return Ok(AppRunControl::Continue);
        }
        match serde_json::from_str::<BrowserInput>(&input) {
            Ok(BrowserInput::History { before, row_end }) => {
                if let Some(session) = &mut tui.web_ui {
                    session.history_end = (before < self.transcript_cells.len()
                        || row_end.is_some())
                    .then_some(before);
                    session.history_row_end = row_end;
                    session.last_publish = None;
                }
                self.publish_web_ui(tui);
            }
            Ok(BrowserInput::Image { data_url }) => {
                if tui
                    .web_ui
                    .as_ref()
                    .is_some_and(|session| session.images.len() < 32)
                {
                    match tokio::task::spawn_blocking(move || upload_image(data_url)).await {
                        Ok(Ok(file)) => {
                            self.chat_widget.attach_image(file.path().to_path_buf());
                            if let Some(session) = &mut tui.web_ui {
                                session.images.push(file);
                            }
                        }
                        _ => self.chat_widget.add_error_message(
                            "Could not attach image (maximum 8 MiB; PNG, JPEG, GIF or WebP)."
                                .to_string(),
                        ),
                    }
                } else {
                    self.chat_widget.add_error_message(
                        "This browser session has reached its 32-image limit.".to_string(),
                    );
                }
            }
            _ => {}
        }
        tui.frame_requester().schedule_frame();
        Ok(AppRunControl::Continue)
    }

    pub(super) fn publish_web_ui(&mut self, tui: &mut Tui) {
        let Some(session) = &mut tui.web_ui else {
            return;
        };
        if let Some(last) = session.last_publish
            && last.elapsed() < FRAME_INTERVAL
        {
            tui.frame_requester()
                .schedule_frame_in(FRAME_INTERVAL - last.elapsed().min(FRAME_INTERVAL));
            return;
        }
        let size = tui.terminal.last_known_screen_size;
        let width = size.width.clamp(1, 400);
        let active_thread = self.chat_widget.thread_id();
        if session.width != width || session.active_thread != active_thread {
            session.history.clear();
            session.history_end = None;
            session.history_row_end = None;
            session.width = width;
            session.active_thread = active_thread;
        }
        let mut history = Vec::new();
        let mut bytes: usize = 0;
        let end = session
            .history_end
            .unwrap_or(self.transcript_cells.len())
            .min(self.transcript_cells.len());
        for (index, cell) in self.transcript_cells[..end].iter().enumerate().rev() {
            let row_total = cell.desired_height(width);
            let row_end = if index + 1 == end {
                session.history_row_end.unwrap_or(row_total).min(row_total)
            } else {
                row_total
            };
            if session.history.get(&index).is_some_and(|cached| {
                !std::sync::Arc::ptr_eq(&cached.cell, cell)
                    || cached.row_end != row_end
                    || cached.row_total != row_total
            }) {
                session.history.remove(&index);
            }
            let cached = session.history.entry(index).or_insert_with(|| {
                let lines = cell.display_lines(width);
                let mut rows = row_end.min(400);
                loop {
                    let row_start = row_end.saturating_sub(rows);
                    let area = Rect::new(0, 0, width, rows);
                    let mut buffer = Buffer::empty(area);
                    if let Some(style) = cell.display_background_style() {
                        buffer.set_style(area, style);
                    }
                    Paragraph::new(Text::from(lines.clone()))
                        .wrap(Wrap { trim: false })
                        .scroll((row_start, 0))
                        .render(area, &mut buffer);
                    let html = buffer_html(&buffer, None);
                    if json!(&html).to_string().len() <= HISTORY_BYTES / 2 || rows <= 1 {
                        break CachedHistoryCell {
                            cell: cell.clone(),
                            html,
                            row_start,
                            row_end,
                            row_total,
                        };
                    }
                    rows /= 2;
                }
            });
            let fragment = json!({
                "id": format!("{}-{index}", active_thread.map(|id| id.to_string()).unwrap_or_default()),
                "html": cached.html, "rowStart": cached.row_start,
                "rowEnd": cached.row_end, "rowTotal": cached.row_total,
            });
            bytes = bytes.saturating_add(fragment.to_string().len());
            if bytes > HISTORY_BYTES {
                break;
            }
            history.push(fragment);
            if cached.row_start > 0 {
                break;
            }
        }
        let first = end.saturating_sub(history.len());
        session
            .history
            .retain(|index, _| *index >= first && *index < end);
        history.reverse();
        let renderable = self.chat_widget.as_renderable();
        let height = if self.overlay.is_some() {
            size.height
        } else {
            renderable.desired_height(width).min(size.height)
        };
        let area = Rect::new(0, 0, width, height.max(1));
        let mut buffer = Buffer::empty(area);
        let cursor = if let Some(overlay) = &mut self.overlay {
            overlay.render(area, &mut buffer);
            None
        } else {
            renderable.render(area, &mut buffer);
            renderable.cursor_pos(area)
        };
        let frame = json!({
            "type": "frame", "threadId": session.thread_id.to_string(),
            "activeThreadId": active_thread.map(|id| id.to_string()),
            "title": self.chat_widget.thread_name(), "width": width,
            "history": history, "historyStart": first, "historyEnd": end, "historyTotal": self.transcript_cells.len(),
            "historyRowEnd": session.history_row_end,
            "live": buffer_html(&buffer, cursor), "overlay": self.overlay.is_some(),
        })
        .to_string();
        session.frames.send_if_modified(|previous| {
            if previous.as_ref() == Some(&frame) {
                false
            } else {
                *previous = Some(frame);
                true
            }
        });
        session.last_publish = Some(Instant::now());
    }
}

fn upload_image(data_url: String) -> std::io::Result<tempfile::NamedTempFile> {
    let (header, data) = data_url
        .split_once(',')
        .ok_or_else(|| std::io::Error::other("invalid image"))?;
    let suffix = match header {
        "data:image/png;base64" => ".png",
        "data:image/jpeg;base64" => ".jpg",
        "data:image/gif;base64" => ".gif",
        "data:image/webp;base64" => ".webp",
        _ => return Err(std::io::Error::other("unsupported image")),
    };
    if data.len() > 11_184_812 {
        return Err(std::io::Error::other("image too large"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(std::io::Error::other)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(std::io::Error::other("image too large"));
    }
    let mut reader = image::ImageReader::new(Cursor::new(&bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().map_err(std::io::Error::other)?;
    let mut file = tempfile::Builder::new()
        .prefix("xedoc-webui-")
        .suffix(suffix)
        .tempfile()?;
    file.write_all(&bytes)?;
    Ok(file)
}
