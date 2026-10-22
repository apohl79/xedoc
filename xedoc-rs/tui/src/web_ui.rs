//! State and bounded transport for a browser sharing the live terminal controller.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use xedoc_app_server_client::web_ui::run_web_ui_bridge;
use xedoc_app_server_protocol::ThreadWebUiOpenResponse;
use xedoc_protocol::ThreadId;
use xedoc_tui_transcript::history_cell::HistoryCell;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::tui::TuiEvent;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum BrowserInput {
    Key {
        key: String,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        alt: bool,
        #[serde(default)]
        shift: bool,
    },
    Paste {
        text: String,
    },
    Image {
        #[serde(rename = "dataUrl")]
        data_url: String,
    },
    File {
        name: String,
        #[serde(rename = "dataUrl")]
        data_url: String,
        #[serde(default, rename = "uploadId")]
        upload_id: String,
    },
    Draft {
        text: String,
        revision: u64,
        #[serde(rename = "baseRevision")]
        base_revision: u64,
    },
    RemoveImage {
        anchor: String,
    },
    Settings {
        model: Option<String>,
        effort: Option<String>,
        router: Option<String>,
    },
    Resize {
        width: u16,
    },
    History {
        before: usize,
        #[serde(default, rename = "rowEnd")]
        row_end: Option<u16>,
    },
}

impl BrowserInput {
    fn terminal_event(&self) -> Option<TuiEvent> {
        match self {
            Self::Key {
                key,
                ctrl,
                alt,
                shift,
            } => browser_key(key, *ctrl, *alt, *shift).map(TuiEvent::Key),
            Self::Paste { text } if text.len() <= 1_000_000 => Some(TuiEvent::Paste(text.clone())),
            Self::Paste { .. }
            | Self::Image { .. }
            | Self::File { .. }
            | Self::History { .. }
            | Self::Draft { .. }
            | Self::RemoveImage { .. }
            | Self::Settings { .. }
            | Self::Resize { .. } => None,
        }
    }
}

pub(crate) struct WebUiSession {
    pub(crate) thread_id: ThreadId,
    pub(crate) url: String,
    pub(crate) frames: watch::Sender<Option<String>>,
    pub(crate) task: JoinHandle<()>,
    pub(crate) images: Vec<tempfile::NamedTempFile>,
    pub(crate) documents: Vec<crate::web_ui_files::UploadedDocument>,
    pub(crate) history: HashMap<usize, CachedHistoryCell>,
    pub(crate) width: u16,
    pub(crate) active_thread: Option<ThreadId>,
    pub(crate) last_publish: Option<Instant>,
    pub(crate) history_end: Option<usize>,
    pub(crate) history_row_end: Option<u16>,
    pub(crate) browser_width: Option<u16>,
    pub(crate) draft_text: String,
    pub(crate) draft_revision: u64,
    pub(crate) draft_ack: u64,
    pub(crate) image_previews: HashMap<std::path::PathBuf, String>,
}

pub(crate) struct CachedHistoryCell {
    pub(crate) cell: Arc<dyn HistoryCell>,
    pub(crate) html: String,
    pub(crate) row_start: u16,
    pub(crate) row_end: u16,
    pub(crate) row_total: u16,
}

impl WebUiSession {
    pub(crate) fn new(
        thread_id: ThreadId,
        response: ThreadWebUiOpenResponse,
        events: AppEventSender,
        terminal_inputs: mpsc::Sender<TuiEvent>,
    ) -> Self {
        let (frames, frame_rx) = watch::channel(None);
        let task = tokio::spawn(async move {
            let (inputs, mut input_rx) = mpsc::channel(4);
            let bridge = run_web_ui_bridge(response.bridge_url, frame_rx, inputs);
            tokio::pin!(bridge);
            loop {
                tokio::select! {
                    _ = &mut bridge => break,
                    input = input_rx.recv() => {
                        let Some(input) = input else { break };
                        if let Some(event) = serde_json::from_str::<BrowserInput>(&input).ok().and_then(|input| input.terminal_event()) {
                            if terminal_inputs.send(event).await.is_err() { break; }
                            continue;
                        }
                        let (complete, done) = oneshot::channel();
                        events.send(AppEvent::WebUiInput { thread_id, input, complete });
                        if done.await.is_err() { break; }
                    }
                }
            }
        });
        Self {
            thread_id,
            url: response.url,
            frames,
            task,
            images: Vec::new(),
            documents: Vec::new(),
            history: HashMap::new(),
            width: 0,
            active_thread: None,
            last_publish: None,
            history_end: None,
            history_row_end: None,
            browser_width: None,
            draft_text: String::new(),
            draft_revision: 0,
            draft_ack: 0,
            image_previews: HashMap::new(),
        }
    }

    pub(crate) fn publish_overlay(&self, buffer: &ratatui::buffer::Buffer) {
        let previous = self.frames.borrow().clone();
        if let Some(mut frame) =
            previous.and_then(|frame| serde_json::from_str::<serde_json::Value>(&frame).ok())
        {
            frame["live"] = crate::web_ui_html::buffer_html(buffer, None).into();
            frame["overlay"] = true.into();
            frame["composer"]["dialog"] = true.into();
            self.frames.send_replace(Some(frame.to_string()));
        }
    }
}

impl Drop for WebUiSession {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn browser_key(key: &str, ctrl: bool, alt: bool, shift: bool) -> Option<KeyEvent> {
    let code = match key {
        "Enter" => KeyCode::Enter,
        "Escape" => KeyCode::Esc,
        "Backspace" => KeyCode::Backspace,
        "Delete" => KeyCode::Delete,
        "ArrowUp" => KeyCode::Up,
        "ArrowDown" => KeyCode::Down,
        "ArrowLeft" => KeyCode::Left,
        "ArrowRight" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Tab" if shift => KeyCode::BackTab,
        "Tab" => KeyCode::Tab,
        key if key.chars().count() == 1 => {
            let character = key.chars().next()?;
            KeyCode::Char(if ctrl && !shift {
                character.to_ascii_lowercase()
            } else {
                character
            })
        }
        key => KeyCode::F(
            key.strip_prefix('F')?
                .parse::<u8>()
                .ok()
                .filter(|number| (1..=24).contains(number))?,
        ),
    };
    let mut modifiers = KeyModifiers::empty();
    modifiers.set(KeyModifiers::CONTROL, ctrl);
    modifiers.set(KeyModifiers::ALT, alt);
    modifiers.set(KeyModifiers::SHIFT, shift);
    Some(KeyEvent::new(code, modifiers))
}
