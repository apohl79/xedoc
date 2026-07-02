use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Widget;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::key_hint;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::ListKeymap;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::style::user_message_style;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::measure_rows_height;
use super::selection_popup_common::render_rows;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigSetting {
    AutoSessionName,
}

struct ConfigMenuItem {
    setting: ConfigSetting,
    name: &'static str,
    description: &'static str,
    enabled: bool,
}

pub(crate) struct ConfigSettingsView {
    items: Vec<ConfigMenuItem>,
    state: ScrollState,
    complete: bool,
    app_event_tx: AppEventSender,
    keymap: ListKeymap,
}

impl ConfigSettingsView {
    pub(crate) fn new(
        auto_session_name: bool,
        app_event_tx: AppEventSender,
        keymap: ListKeymap,
    ) -> Self {
        let mut view = Self {
            items: vec![ConfigMenuItem {
                setting: ConfigSetting::AutoSessionName,
                name: "Generate session names",
                description: "Keep session names short automatically.",
                enabled: auto_session_name,
            }],
            state: ScrollState::new(),
            complete: false,
            app_event_tx,
            keymap,
        };
        view.state.selected_idx = Some(0);
        view
    }

    fn header(&self) -> ColumnRenderable<'_> {
        let mut header = ColumnRenderable::new();
        header.push(Line::from("Config".bold()));
        header.push(Line::from(
            "Choose Codex settings. Changes are saved to config.toml".dim(),
        ));
        header
    }

    fn build_rows(&self) -> Vec<GenericDisplayRow> {
        let selected_idx = self.state.selected_idx;
        self.items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let prefix = if selected_idx == Some(idx) {
                    '›'
                } else {
                    ' '
                };
                GenericDisplayRow {
                    name: format!(
                        "{prefix} [{}] {}",
                        if item.enabled { 'x' } else { ' ' },
                        item.name
                    ),
                    description: Some(item.description.to_string()),
                    ..Default::default()
                }
            })
            .collect()
    }

    fn move_up(&mut self) {
        let len = self.items.len();
        if len == 0 {
            return;
        }
        self.state.move_up_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    fn move_down(&mut self) {
        let len = self.items.len();
        if len == 0 {
            return;
        }
        self.state.move_down_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    fn toggle_selected(&mut self) {
        let Some(selected_idx) = self.state.selected_idx else {
            return;
        };
        if let Some(item) = self.items.get_mut(selected_idx) {
            item.enabled = !item.enabled;
        }
    }

    fn current_setting(&self, setting: ConfigSetting) -> bool {
        self.items
            .iter()
            .find_map(|item| (item.setting == setting).then_some(item.enabled))
            .unwrap_or(false)
    }

    fn save(&mut self) {
        self.app_event_tx
            .send(AppEvent::UpdateAutoSessionNameSetting {
                enabled: self.current_setting(ConfigSetting::AutoSessionName),
            });
        self.complete = true;
    }

    fn cancel(&mut self) {
        self.complete = true;
    }
}

#[cfg(test)]
#[path = "config_settings_view_tests.rs"]
mod tests;

impl BottomPaneView for ConfigSettingsView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            _ if self.keymap.move_up.is_pressed(key_event) => self.move_up(),
            _ if self.keymap.move_down.is_pressed(key_event) => self.move_down(),
            KeyEvent {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.toggle_selected(),
            _ if self.keymap.accept.is_pressed(key_event) => self.save(),
            _ if self.keymap.cancel.is_pressed(key_event) => self.cancel(),
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.cancel();
        CancellationEvent::Handled
    }
}

impl Renderable for ConfigSettingsView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let [content_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);

        Block::default()
            .style(user_message_style())
            .render(content_area, buf);

        let header = self.header();
        let header_height = header.desired_height(content_area.width.saturating_sub(4));
        let rows = self.build_rows();
        let rows_width = content_area.width.saturating_sub(2);
        let rows_height = measure_rows_height(
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            rows_width.saturating_add(1),
        );
        let [header_area, _, list_area] = Layout::vertical([
            Constraint::Max(header_height),
            Constraint::Max(1),
            Constraint::Length(rows_height),
        ])
        .areas(content_area.inset(Insets::vh(/*v*/ 1, /*h*/ 2)));

        header.render(header_area, buf);
        if list_area.height > 0 {
            let render_area = Rect {
                x: list_area.x.saturating_sub(2),
                y: list_area.y,
                width: rows_width.max(1),
                height: list_area.height,
            };
            render_rows(
                render_area,
                buf,
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                "  No config settings available",
            );
        }

        Line::from(vec![
            "Press ".into(),
            key_hint::plain(KeyCode::Char(' ')).into(),
            " to toggle; ".into(),
            key_hint::plain(KeyCode::Enter).into(),
            " to save".into(),
        ])
        .render(
            Rect {
                x: footer_area.x + 2,
                y: footer_area.y,
                width: footer_area.width.saturating_sub(2),
                height: footer_area.height,
            },
            buf,
        );
    }

    fn desired_height(&self, width: u16) -> u16 {
        let header = self.header();
        let rows = self.build_rows();
        let rows_width = width.saturating_sub(2);
        let rows_height = measure_rows_height(
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            rows_width.saturating_add(1),
        );
        header
            .desired_height(width.saturating_sub(4))
            .saturating_add(rows_height + 4)
    }
}
