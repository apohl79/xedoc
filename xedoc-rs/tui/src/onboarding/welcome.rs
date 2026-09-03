use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;

use crate::onboarding::onboarding_screen::StepStateProvider;

use super::onboarding_screen::StepState;

pub(crate) struct WelcomeWidget {
    pub is_logged_in: bool,
}

impl WelcomeWidget {
    pub(crate) fn new(is_logged_in: bool) -> Self {
        Self { is_logged_in }
    }
}

impl WidgetRef for &WelcomeWidget {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let lines = vec![Line::from(vec![
            "  ".into(),
            "Welcome to ".into(),
            "Xedoc".bold(),
            ", your command-line coding agent.".into(),
        ])];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

impl StepStateProvider for WelcomeWidget {
    fn get_step_state(&self) -> StepState {
        match self.is_logged_in {
            true => StepState::Hidden,
            false => StepState::Complete,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_backend::VT100Backend;
    use ratatui::Terminal;

    #[test]
    fn renders_welcome_snapshot() {
        let widget = WelcomeWidget::new(/*is_logged_in*/ false);
        let mut terminal =
            Terminal::new(VT100Backend::new(/*width*/ 60, /*height*/ 3)).expect("terminal");
        terminal
            .draw(|frame| (&widget).render_ref(frame.area(), frame.buffer_mut()))
            .expect("draw");

        insta::assert_snapshot!(terminal.backend());
    }
}
