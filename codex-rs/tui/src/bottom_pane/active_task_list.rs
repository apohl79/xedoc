use codex_protocol::plan_tool::PlanItemArg;
use codex_protocol::plan_tool::StepStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;

use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::render::renderable::Renderable;

const MAX_VISIBLE_TASKS: usize = 7;

#[derive(Default)]
pub(crate) struct ActiveTaskList {
    tasks: Vec<PlanItemArg>,
}

impl ActiveTaskList {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_tasks(&mut self, tasks: Vec<PlanItemArg>) {
        self.tasks = tasks
            .into_iter()
            .filter_map(|mut item| {
                item.step = item.step.trim().to_string();
                (!item.step.is_empty()).then_some(item)
            })
            .collect();
    }

    pub(crate) fn clear(&mut self) {
        self.tasks.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    fn completed_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|item| matches!(&item.status, StepStatus::Completed))
            .count()
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.tasks.is_empty() || width == 0 {
            return Vec::new();
        }

        let max_width = usize::from(width);
        let total = self.tasks.len();
        let completed = self.completed_count();
        let visible_tasks = total.min(MAX_VISIBLE_TASKS);
        let mut lines = Vec::with_capacity(1 + visible_tasks + usize::from(total > visible_tasks));
        lines.push(vec!["Tasks ".bold(), format!("{completed}/{total}").dim()].into());
        lines.extend(
            self.tasks
                .iter()
                .take(MAX_VISIBLE_TASKS)
                .map(Self::task_line),
        );
        let hidden = total.saturating_sub(MAX_VISIBLE_TASKS);
        if hidden > 0 {
            lines.push(format!("... {hidden} more").dim().into());
        }

        lines
            .into_iter()
            .map(|line| truncate_line_with_ellipsis_if_overflow(line, max_width))
            .collect()
    }

    fn task_line(item: &PlanItemArg) -> Line<'static> {
        let (marker, step_style) = match &item.status {
            StepStatus::Completed => ("✔".dim(), Style::default().crossed_out().dim()),
            StepStatus::InProgress => ("□".cyan().bold(), Style::default().cyan().bold()),
            StepStatus::Pending => ("□".dim(), Style::default().dim()),
        };
        vec![
            "  ".into(),
            marker,
            " ".into(),
            Span::styled(item.step.clone(), step_style),
        ]
        .into()
    }
}

impl Renderable for ActiveTaskList {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        Paragraph::new(self.display_lines(area.width)).render_ref(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.display_lines(width).len() as u16
    }
}
