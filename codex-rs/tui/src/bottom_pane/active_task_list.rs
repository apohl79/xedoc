use crate::city_lights::CityLightsStylize;
use codex_protocol::plan_tool::PlanItemArg;
use codex_protocol::plan_tool::StepStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Styled;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;

use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::render::line_utils::prefix_lines;
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
        let header_style = crate::city_lights::active_list_header_style();
        lines.push(
            vec![
                "• ".dim(),
                "Tasks ".set_style(header_style.bold()),
                format!("{completed}/{total}").into(),
            ]
            .into(),
        );

        let task_lines = self.visible_task_lines();
        lines.extend(prefix_lines(task_lines, "  └ ".dim(), "    ".into()));

        lines
            .into_iter()
            .map(|line| truncate_line_with_ellipsis_if_overflow(line, max_width))
            .collect()
    }

    fn visible_task_lines(&self) -> Vec<Line<'static>> {
        let total = self.tasks.len();
        let Some(current_index) = self
            .tasks
            .iter()
            .position(|item| matches!(&item.status, StepStatus::InProgress))
        else {
            return self.leading_task_lines();
        };

        if total <= MAX_VISIBLE_TASKS || current_index < MAX_VISIBLE_TASKS {
            return self.leading_task_lines();
        }

        let start = current_index + 1 - MAX_VISIBLE_TASKS;
        let end = total.min(start + MAX_VISIBLE_TASKS);
        let hidden = total.saturating_sub(end);
        let mut lines: Vec<Line<'static>> = self
            .tasks
            .iter()
            .skip(start)
            .take(end - start)
            .map(Self::task_line)
            .collect();
        if hidden > 0 {
            lines.push(format!("... {hidden} more").dim().into());
        }
        lines
    }

    fn leading_task_lines(&self) -> Vec<Line<'static>> {
        let total = self.tasks.len();
        let mut lines: Vec<Line<'static>> = self
            .tasks
            .iter()
            .take(MAX_VISIBLE_TASKS)
            .map(Self::task_line)
            .collect();
        let hidden = total.saturating_sub(MAX_VISIBLE_TASKS);
        if hidden > 0 {
            lines.push(format!("... {hidden} more").dim().into());
        }
        lines
    }

    fn task_line(item: &PlanItemArg) -> Line<'static> {
        let (marker, step_style) = match &item.status {
            StepStatus::Completed => ("✔ ".dim(), Style::default().crossed_out().dim()),
            StepStatus::InProgress => ("□ ".cl_cyan().bold(), Style::default().cl_cyan().bold()),
            StepStatus::Pending => ("□ ".dim(), Style::default().dim()),
        };
        vec![marker, Span::styled(item.step.clone(), step_style)].into()
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

#[cfg(test)]
#[path = "active_task_list_tests.rs"]
mod tests;
