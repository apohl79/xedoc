//! Browser attachment using the terminal's controller, history cells, and input paths.

use std::io::Cursor;
use std::io::Write;
use std::process::Command;
use std::time::Duration;
use std::time::Instant;

use base64::Engine;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
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
use crate::chatwidget::BrowserStreamContent;
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
            self.open_session_web_ui(session.url.clone());
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
                self.open_session_web_ui(url);
            }
            Ok(_) => {}
            Err(error) => self.chat_widget.add_error_message(error),
        }
        tui.frame_requester().schedule_frame();
    }

    fn open_session_web_ui(&mut self, url: String) {
        if open_web_ui_in_cmux(&url) {
            self.chat_widget
                .add_info_message(format!("Opened {url} in your browser."), /*hint*/ None);
        } else {
            self.open_url_in_browser(url);
        }
    }

    pub(super) async fn handle_web_ui_input(
        &mut self,
        tui: &mut Tui,
        app_server: &AppServerSession,
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
            Ok(BrowserInput::Question {
                request_id,
                index,
                action,
                option,
                text,
            }) => {
                self.chat_widget.handle_browser_question(
                    &request_id,
                    index,
                    &action,
                    option,
                    text.as_deref(),
                );
            }
            Ok(BrowserInput::Resize { width }) => {
                if let Some(session) = &mut tui.web_ui {
                    session.browser_width = Some(width.clamp(20, 400));
                    session.last_publish = None;
                }
            }
            Ok(BrowserInput::Draft {
                text,
                revision,
                base_revision,
            }) => {
                let current = self.chat_widget.browser_draft().text;
                if let Some(session) = &mut tui.web_ui {
                    if session.draft_text != current {
                        session.draft_text = current;
                        session.draft_revision = session.draft_revision.saturating_add(1);
                    }
                    if revision > session.draft_ack
                        && base_revision == session.draft_revision
                        && text.len() <= 1_000_000
                    {
                        self.chat_widget.apply_browser_draft(text);
                    }
                    session.draft_ack = session.draft_ack.max(revision);
                    session.last_publish = None;
                }
            }
            Ok(BrowserInput::RemoveImage { anchor }) => {
                let draft = self.chat_widget.browser_draft();
                if draft
                    .local_images
                    .iter()
                    .any(|image| image.placeholder == anchor)
                {
                    self.chat_widget
                        .apply_browser_draft(draft.text.replace(&anchor, ""));
                }
            }
            Ok(BrowserInput::Settings {
                model,
                effort,
                router,
            }) => {
                let models = self.model_catalog.try_list_models().unwrap_or_default();
                let selected = model.as_deref().unwrap_or(self.chat_widget.current_model());
                if let Some(preset) = models.iter().find(|preset| preset.model == selected) {
                    let chosen_effort = effort.as_deref().and_then(|effort| {
                        preset
                            .supported_reasoning_efforts
                            .iter()
                            .find(|item| item.effort.to_string() == effort)
                            .map(|item| item.effort.clone())
                    });
                    if model.is_some() && (effort.is_none() || chosen_effort.is_some()) {
                        self.app_event_tx.send(AppEvent::UpdateModelAndProvider {
                            model: preset.model.clone(),
                            provider_id: preset.provider_id.clone(),
                            effort: Some(
                                chosen_effort
                                    .unwrap_or_else(|| preset.default_reasoning_effort.clone()),
                            ),
                        });
                    } else if let Some(effort) = chosen_effort {
                        self.app_event_tx
                            .send(AppEvent::UpdateReasoningEffort(Some(effort)));
                    }
                }
                if let Some(router) = router
                    && ["off", "shadow-full", "subagents", "full"].contains(&router.as_str())
                {
                    for _ in 0..4 {
                        let response = crate::config_update::cycle_model_router_mode(
                            app_server.request_handle(),
                            self.chat_widget.thread_id().unwrap_or(thread_id),
                            xedoc_app_server_protocol::ModelRouterModeCycleDirection::Next,
                        )
                        .await;
                        match response {
                            Ok(response) => {
                                let reached = response.mode == router;
                                self.chat_widget.set_model_router_mode(Some(response.mode));
                                if reached {
                                    break;
                                }
                            }
                            Err(error) => {
                                self.chat_widget.add_error_message(format!(
                                    "Could not change router mode: {error:#}"
                                ));
                                break;
                            }
                        }
                    }
                }
            }
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
            Ok(BrowserInput::Image { data_url, cursor }) => {
                if tui
                    .web_ui
                    .as_ref()
                    .is_some_and(|session| session.images.len() + session.documents.len() < 32)
                {
                    match tokio::task::spawn_blocking(move || upload_image(data_url)).await {
                        Ok(Ok(file)) => {
                            let mut text = self.chat_widget.browser_draft().text;
                            self.chat_widget.attach_image(file.path().to_path_buf());
                            if let Some(cursor) = cursor
                                && text.is_char_boundary(cursor)
                                && let Some(image) = self
                                    .chat_widget
                                    .browser_draft()
                                    .local_images
                                    .iter()
                                    .find(|image| image.path == file.path())
                            {
                                let separator = if text
                                    .get(cursor..)
                                    .and_then(|remaining| remaining.chars().next())
                                    .is_some_and(char::is_whitespace)
                                {
                                    ""
                                } else {
                                    " "
                                };
                                text.insert_str(
                                    cursor,
                                    &format!("{}{separator}", image.placeholder),
                                );
                                self.chat_widget.apply_browser_draft(text);
                            }
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
                        "This browser session has reached its 32-file limit.".to_string(),
                    );
                }
            }
            Ok(BrowserInput::File {
                name,
                data_url,
                upload_id,
            }) => {
                if tui
                    .web_ui
                    .as_ref()
                    .is_some_and(|session| session.documents.len() + session.images.len() < 32)
                {
                    match tokio::task::spawn_blocking(move || {
                        crate::web_ui_files::upload_document(name, data_url)
                    })
                    .await
                    {
                        Ok(Ok(mut document)) => {
                            if let Some(session) = &mut tui.web_ui {
                                document.anchor = format!("[Doc #{}]", session.documents.len() + 1);
                                document.upload_id = upload_id.chars().take(64).collect();
                                let reference = format!(
                                    "{}: {}",
                                    document.name,
                                    serde_json::to_string(&document.path.to_string_lossy())?
                                );
                                self.chat_widget
                                    .attach_browser_document(document.anchor.clone(), reference);
                                session.documents.push(document);
                            }
                        }
                        _ => self.chat_widget.add_error_message(
                            "Could not attach file (maximum 8 MiB).".to_string(),
                        ),
                    }
                } else {
                    self.chat_widget.add_error_message(
                        "This browser session has reached its 32-file limit.".to_string(),
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
        let width = session.browser_width.unwrap_or(size.width).clamp(1, 400);
        let active_thread = self.chat_widget.thread_id();
        if session.width != width || session.active_thread != active_thread {
            session.history.clear();
            session.history_end = None;
            session.history_row_end = None;
            session.browser_stream_kind = None;
            session.browser_stream_source_len = 0;
            session.browser_stream_prefix.clear();
            session.browser_stream_stable_pages = 0;
            session.width = width;
            session.active_thread = active_thread;
        }
        let mut history = Vec::new();
        let mut bytes: usize = 0;
        let end = session
            .history_end
            .unwrap_or(self.transcript_cells.len())
            .min(self.transcript_cells.len());
        let stream_content = self.chat_widget.browser_stream_content();
        let mut streaming_history_start = end;
        while streaming_history_start > 0 {
            let cell = &self.transcript_cells[streaming_history_start - 1];
            let is_stream_fragment = match stream_content.as_ref() {
                Some(BrowserStreamContent::Assistant(_)) => {
                    cell.as_any().is::<crate::history_cell::AgentMessageCell>()
                }
                Some(BrowserStreamContent::Plan(_)) => {
                    cell.as_any()
                        .is::<crate::history_cell::ProposedPlanStreamCell>()
                }
                None => false,
            };
            if !is_stream_fragment {
                break;
            }
            streaming_history_start -= 1;
        }
        for (index, cell) in self.transcript_cells[..end].iter().enumerate().rev() {
            let hidden = index >= streaming_history_start && stream_content.is_some();
            let row_total = if hidden {
                1
            } else {
                crate::web_ui_html::history_cell_page_count(cell.as_ref())
            };
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
            if hidden {
                session.history.remove(&index);
            }
            let cached = session.history.entry(index).or_insert_with(|| {
                if hidden {
                    return CachedHistoryCell {
                        cell: cell.clone(),
                        html: String::new(),
                        row_start: 0,
                        row_end: 1,
                        row_total: 1,
                    };
                }
                let mut rows = row_end.min(400);
                loop {
                    let row_start = row_end.saturating_sub(rows);
                    let html =
                        crate::web_ui_html::history_cell_html(cell.as_ref(), row_start, row_end);
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
                "rowEnd": cached.row_end, "rowTotal": cached.row_total, "hidden": hidden,
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
        let draft = self.chat_widget.browser_draft();
        if session.draft_text != draft.text {
            session.draft_text = draft.text.clone();
            session.draft_revision = session.draft_revision.saturating_add(1);
        }
        session
            .image_previews
            .retain(|path, _| draft.local_images.iter().any(|image| &image.path == path));
        let mut attachments: Vec<_> = draft
            .local_images
            .iter()
            .take(32)
            .map(|image| {
                let preview = session
                    .image_previews
                    .entry(image.path.clone())
                    .or_insert_with(|| image_preview(&image.path).unwrap_or_default());
                json!({"anchor": image.placeholder, "dataUrl": preview})
            })
            .collect();
        let mut preview_bytes = 0;
        for document in &session.documents {
            if draft
                .pending_pastes
                .iter()
                .any(|(anchor, _)| anchor == &document.anchor && draft.text.contains(anchor))
            {
                preview_bytes += document.preview.len();
                attachments.push(json!({
                    "anchor": document.anchor,
                    "name": document.name,
                    "mimeType": document.mime_type,
                    "uploadId": document.upload_id,
                    "dataUrl": if preview_bytes <= 512 * 1024 { &document.preview } else { "" },
                }));
            }
        }
        let models: Vec<_> = self.model_catalog.try_list_models().unwrap_or_default().iter()
            .filter(|preset| preset.show_in_picker)
            .map(|preset| json!({
                "id": preset.model, "label": preset.display_name,
                "efforts": preset.supported_reasoning_efforts.iter().map(|effort| effort.effort.to_string()).collect::<Vec<_>>(),
            })).collect();
        let (
            composer_height,
            popup_height,
            dialog,
            router,
            timer,
            status,
            display_model,
            display_effort,
        ) = self.chat_widget.browser_composer_context(width);
        let question = self
            .overlay
            .is_none()
            .then(|| self.chat_widget.browser_question())
            .flatten();
        let status = status.map(|line| {
            let status_width = u16::try_from(line.width())
                .unwrap_or(u16::MAX)
                .min(width)
                .max(1);
            let area = Rect::new(0, 0, status_width, 1);
            let mut buffer = Buffer::empty(area);
            Paragraph::new(line).render(area, &mut buffer);
            buffer_html(&buffer, None)
        });
        let stream_update = match stream_content.as_ref() {
            Some(BrowserStreamContent::Assistant(source)) => {
                let kind = crate::web_ui_html::StreamHtmlKind::Assistant;
                let kind_name = "assistant";
                let reset = session.browser_stream_kind.as_deref() != Some(kind_name)
                    || source.len() < session.browser_stream_source_len
                    || !source.starts_with(session.browser_stream_prefix.as_str());
                let pages = crate::web_ui_html::markdown_pages(source);
                let stable_page_count = pages.len().saturating_sub(1);
                let stable_start = if reset {
                    0
                } else {
                    session.browser_stream_stable_pages
                };
                let stable = (stable_start..stable_page_count)
                    .map(|index| {
                        crate::web_ui_html::stream_page_html(kind, pages[index], index == 0)
                    })
                    .collect::<Vec<_>>();
                let tail = pages.last().map(|page| {
                    crate::web_ui_html::stream_page_html(kind, page, stable_page_count == 0)
                });
                session.browser_stream_kind = Some(kind_name.to_string());
                session.browser_stream_source_len = source.len();
                session.browser_stream_prefix = source.chars().take(64).collect();
                session.browser_stream_stable_pages = stable_page_count;
                Some(json!({"reset": reset, "kind": kind_name, "stable": stable, "tail": tail}))
            }
            Some(BrowserStreamContent::Plan(source)) => {
                let kind = crate::web_ui_html::StreamHtmlKind::Plan;
                let kind_name = "plan";
                let reset = session.browser_stream_kind.as_deref() != Some(kind_name)
                    || source.len() < session.browser_stream_source_len
                    || !source.starts_with(session.browser_stream_prefix.as_str());
                let pages = crate::web_ui_html::markdown_pages(source);
                let stable_page_count = pages.len().saturating_sub(1);
                let stable_start = if reset {
                    0
                } else {
                    session.browser_stream_stable_pages
                };
                let stable = (stable_start..stable_page_count)
                    .map(|index| {
                        crate::web_ui_html::stream_page_html(kind, pages[index], index == 0)
                    })
                    .collect::<Vec<_>>();
                let tail = pages.last().map(|page| {
                    crate::web_ui_html::stream_page_html(kind, page, stable_page_count == 0)
                });
                session.browser_stream_kind = Some(kind_name.to_string());
                session.browser_stream_source_len = source.len();
                session.browser_stream_prefix = source.chars().take(64).collect();
                session.browser_stream_stable_pages = stable_page_count;
                Some(json!({"reset": reset, "kind": kind_name, "stable": stable, "tail": tail}))
            }
            None if session.browser_stream_kind.is_some() => {
                session.browser_stream_kind = None;
                session.browser_stream_source_len = 0;
                session.browser_stream_prefix.clear();
                session.browser_stream_stable_pages = 0;
                Some(json!({"reset": true, "kind": null, "stable": [], "tail": null}))
            }
            None => None,
        };
        let active_html = stream_content
            .is_none()
            .then(|| {
                self.chat_widget.browser_active_cell().map(|cell| {
                    let pages = crate::web_ui_html::history_cell_page_count(cell);
                    crate::web_ui_html::history_cell_html(cell, pages.saturating_sub(400), pages)
                })
            })
            .flatten();
        let tool_summary = (!dialog && self.overlay.is_none())
            .then(|| {
                self.chat_widget
                    .browser_tool_summary()
                    .map(|cell| crate::web_ui_html::history_cell_html(cell, 0, 1))
            })
            .flatten();
        let renderable = if dialog || self.overlay.is_some() {
            self.chat_widget.as_renderable()
        } else {
            self.chat_widget.browser_renderable()
        };
        let height = if self.overlay.is_some() {
            size.height
        } else {
            renderable.desired_height(width).min(400)
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
        let activity_area = Rect::new(0, 0, width, height.saturating_sub(composer_height));
        let activity_buffer = Buffer {
            area: activity_area,
            content: buffer
                .content
                .iter()
                .take(usize::from(activity_area.width) * usize::from(activity_area.height))
                .cloned()
                .collect(),
        };
        let popup_start = height.saturating_sub(popup_height);
        let popup_area = Rect::new(0, popup_start, width, height.saturating_sub(popup_start));
        let popup_buffer = Buffer {
            area: popup_area,
            content: buffer
                .content
                .iter()
                .skip(usize::from(popup_start) * usize::from(width))
                .take(usize::from(popup_area.width) * usize::from(popup_area.height))
                .cloned()
                .collect(),
        };
        let frame = json!({
            "type": "frame", "threadId": session.thread_id.to_string(),
            "activeThreadId": active_thread.map(|id| id.to_string()),
            "title": self.chat_widget.thread_name(), "width": width,
            "turnActive": self.chat_widget.browser_turn_active(),
            "history": history, "historyStart": first, "historyEnd": end, "historyTotal": self.transcript_cells.len(),
            "historyRowEnd": session.history_row_end,
            "live": if question.is_none() && (dialog || self.overlay.is_some()) { buffer_html(&buffer, cursor) } else { String::new() },
            "question": question,
            "overlay": self.overlay.is_some(),
            "active": active_html, "stream": stream_update, "toolSummary": tool_summary, "activity": if question.is_none() { buffer_html(&activity_buffer, None) } else { String::new() },
            "popup": if popup_height > 0 { buffer_html(&popup_buffer, None) } else { String::new() },
            "status": status,
            "composer": {
                "text": draft.text, "revision": session.draft_revision, "ackRevision": session.draft_ack,
                "attachments": attachments, "model": self.chat_widget.current_model(),
                "effort": self.chat_widget.current_reasoning_effort(),
                "displayModel": display_model, "displayEffort": display_effort,
                "router": router, "models": models,
                "routerOptions": (["off", "shadow-full", "subagents", "full"].iter().map(|id| json!({"id":id,"label":id})).collect::<Vec<_>>()),
                "sessionName": self.chat_widget.thread_name(), "timer": timer, "dialog": dialog || self.overlay.is_some(),
            },
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

fn open_web_ui_in_cmux(url: &str) -> bool {
    if std::env::var_os("CMUX_SOCKET_PATH").is_none() {
        return false;
    }
    let Ok(identity) = Command::new("cmux").arg("identify").output() else {
        return false;
    };
    let Ok(identity) = serde_json::from_slice::<serde_json::Value>(&identity.stdout) else {
        return false;
    };
    let Some(caller) = identity.get("caller") else {
        return false;
    };
    let Some(pane) = caller.get("pane_ref").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Some(workspace) = caller
        .get("workspace_ref")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    Command::new("cmux")
        .args([
            "new-surface",
            "--type",
            "browser",
            "--pane",
            pane,
            "--workspace",
            workspace,
            "--url",
            url,
            "--focus",
            "true",
        ])
        .status()
        .is_ok_and(|status| status.success())
}

fn image_preview(path: &std::path::Path) -> Option<String> {
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let preview = reader.decode().ok()?.thumbnail(96, 96).to_rgb8();
    let mut bytes = Cursor::new(Vec::new());
    preview
        .write_to(&mut bytes, image::ImageFormat::Jpeg)
        .ok()?;
    Some(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
    ))
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
