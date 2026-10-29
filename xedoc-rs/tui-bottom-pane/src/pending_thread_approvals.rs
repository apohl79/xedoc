use crate::city_lights::CityLightsStylize;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::render::renderable::Renderable;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;

/// Widget that lists inactive threads with outstanding approval requests.
pub struct PendingThreadApprovals {
    threads: Vec<String>,
}

impl PendingThreadApprovals {
    pub fn new() -> Self {
        Self {
            threads: Vec::new(),
        }
    }

    pub fn set_threads(&mut self, threads: Vec<String>) -> bool {
        if self.threads == threads {
            return false;
        }
        self.threads = threads;
        true
    }

    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn threads(&self) -> &[String] {
        &self.threads
    }

    fn as_renderable(&self, width: u16) -> Box<dyn Renderable> {
        if self.threads.is_empty() || width < 4 {
            return Box::new(());
        }

        let mut lines = Vec::new();
        for thread in self.threads.iter().take(3) {
            let wrapped = adaptive_wrap_lines(
                std::iter::once(Line::from(format!("Approval needed in {thread}"))),
                RtOptions::new(width as usize)
                    .initial_indent(Line::from(vec![
                        "  ".into(),
                        "!".cl_red().bold(),
                        " ".into(),
                    ]))
                    .subsequent_indent(Line::from("    ")),
            );
            lines.extend(wrapped);
        }

        if self.threads.len() > 3 {
            lines.push(Line::from("    ...".dim().italic()));
        }

        lines.push(
            Line::from(vec![
                "    ".into(),
                "/agent".cl_cyan().bold(),
                " to switch threads".dim(),
            ])
            .dim(),
        );

        Paragraph::new(lines).into()
    }
}

impl Renderable for PendingThreadApprovals {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        self.as_renderable(area.width).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.as_renderable(width).desired_height(width)
    }
}
