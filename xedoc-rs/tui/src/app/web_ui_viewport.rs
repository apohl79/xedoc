use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use serde_json::Value;
use serde_json::json;

use crate::chatwidget::ChatWidget;
use crate::pager_overlay::Overlay;
use crate::render::renderable::Renderable;
use crate::web_ui_html::buffer_html;

pub(super) fn render(
    chat_widget: &ChatWidget,
    overlay: Option<&mut Overlay>,
    width: u16,
    terminal_height: u16,
    question: bool,
) -> Value {
    let (composer_height, popup_height, dialog, _, _, status, goal_status, _, _) =
        chat_widget.browser_composer_context(width);
    let [status, goal_status] = [status, goal_status].map(|line| {
        line.map(|line| {
            let status_width = u16::try_from(line.width())
                .unwrap_or(u16::MAX)
                .clamp(1, 400);
            let area = Rect::new(0, 0, status_width, 1);
            let mut buffer = Buffer::empty(area);
            Paragraph::new(line).render(area, &mut buffer);
            buffer_html(&buffer, None)
        })
    });
    let renderable = if overlay.is_some() {
        chat_widget.as_renderable()
    } else {
        chat_widget.browser_renderable()
    };
    let height = if overlay.is_some() {
        terminal_height
    } else {
        renderable.desired_height(width).min(400)
    }
    .max(1);
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    let has_overlay = overlay.is_some();
    let cursor = if let Some(overlay) = overlay {
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
    json!({
        "width": width,
        "live": if !question && (dialog || has_overlay) { buffer_html(&buffer, cursor) } else { String::new() },
        "activity": if !question { buffer_html(&activity_buffer, None) } else { String::new() },
        "popup": if popup_height > 0 { buffer_html(&popup_buffer, None) } else { String::new() },
        "status": status,
        "goalStatus": goal_status,
    })
}
