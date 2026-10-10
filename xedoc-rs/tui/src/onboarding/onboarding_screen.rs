//! Onboarding screen orchestration and top-level keyboard routing.
//!
//! This module presents and persists the directory trust decision.

use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Color;
use ratatui::widgets::Clear;
use ratatui::widgets::WidgetRef;
use xedoc_app_server_client::AppServerRequestHandle;
use xedoc_exec_server::LOCAL_FS;
use xedoc_git_utils::resolve_root_git_project_for_trust;

use crate::config_update::format_config_error;
use crate::config_update::write_trusted_project;
use crate::key_hint::KeyBindingListExt;
use crate::legacy_core::config::Config;
use crate::onboarding::keys;
use crate::onboarding::trust_directory::TrustDirectorySelection;
use crate::onboarding::trust_directory::TrustDirectoryWidget;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use color_eyre::eyre::Result;

enum Step {
    TrustDirectory(TrustDirectoryWidget),
}

pub(crate) trait KeyboardHandler {
    fn handle_key_event(&mut self, key_event: KeyEvent);
    fn handle_paste(&mut self, _pasted: String) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepState {
    InProgress,
    Complete,
}

pub(crate) trait StepStateProvider {
    fn get_step_state(&self) -> StepState;
}

pub(crate) struct OnboardingScreen {
    request_frame: FrameRequester,
    steps: Vec<Step>,
    is_done: bool,
    should_exit: bool,
}

pub(crate) struct OnboardingScreenArgs {
    pub show_trust_screen: bool,
    pub app_server_request_handle: Option<AppServerRequestHandle>,
    pub config: Config,
}

pub(crate) struct OnboardingResult {
    pub directory_trust_persisted: bool,
    pub should_exit: bool,
}

impl OnboardingScreen {
    pub(crate) async fn new(tui: &mut Tui, args: OnboardingScreenArgs) -> Self {
        let OnboardingScreenArgs {
            show_trust_screen,
            config,
            ..
        } = args;
        let cwd = config.cwd.to_path_buf();
        let mut steps: Vec<Step> = Vec::new();
        let highlighted = TrustDirectorySelection::Trust;
        if show_trust_screen {
            let trust_target = resolve_root_git_project_for_trust(LOCAL_FS.as_ref(), &config.cwd)
                .await
                .map(Into::into)
                .unwrap_or_else(|| cwd.clone());
            steps.push(Step::TrustDirectory(TrustDirectoryWidget {
                cwd,
                trust_target,
                should_quit: false,
                selection: None,
                highlighted,
                error: None,
            }))
        }
        Self {
            request_frame: tui.frame_requester(),
            steps,
            is_done: false,
            should_exit: false,
        }
    }

    fn current_steps_mut(&mut self) -> Vec<&mut Step> {
        let mut out: Vec<&mut Step> = Vec::new();
        for step in self.steps.iter_mut() {
            match step.get_step_state() {
                StepState::Complete => out.push(step),
                StepState::InProgress => {
                    out.push(step);
                    break;
                }
            }
        }
        out
    }

    fn current_steps(&self) -> Vec<&Step> {
        let mut out: Vec<&Step> = Vec::new();
        for step in self.steps.iter() {
            match step.get_step_state() {
                StepState::Complete => out.push(step),
                StepState::InProgress => {
                    out.push(step);
                    break;
                }
            }
        }
        out
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done
            || !self
                .steps
                .iter()
                .any(|step| matches!(step.get_step_state(), StepState::InProgress))
    }

    pub fn should_exit(&self) -> bool {
        self.should_exit
    }
}

impl KeyboardHandler for OnboardingScreen {
    /// Route key events to the active onboarding step.
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        let should_quit = key_event.kind == KeyEventKind::Press && keys::QUIT.is_pressed(key_event);
        if should_quit {
            self.is_done = true;
        } else {
            if let Some(active_step) = self.current_steps_mut().into_iter().last() {
                active_step.handle_key_event(key_event);
            }
            if self
                .steps
                .iter()
                .any(|Step::TrustDirectory(widget)| widget.should_quit())
            {
                self.should_exit = true;
                self.is_done = true;
            }
        }
        self.request_frame.schedule_frame();
    }

    fn handle_paste(&mut self, pasted: String) {
        if pasted.is_empty() {
            return;
        }

        if let Some(active_step) = self.current_steps_mut().into_iter().last() {
            active_step.handle_paste(pasted);
        }
        self.request_frame.schedule_frame();
    }
}

impl WidgetRef for &OnboardingScreen {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        // Render steps top-to-bottom, measuring each step's height dynamically.
        let mut y = area.y;
        let bottom = area.y.saturating_add(area.height);
        let width = area.width;

        // Helper to scan a temporary buffer and return number of used rows.
        fn used_rows(tmp: &Buffer, width: u16, height: u16) -> u16 {
            if width == 0 || height == 0 {
                return 0;
            }
            let mut last_non_empty: Option<u16> = None;
            for yy in 0..height {
                let mut any = false;
                for xx in 0..width {
                    let cell = &tmp[(xx, yy)];
                    let has_symbol = !cell.symbol().trim().is_empty();
                    let has_style = cell.fg != Color::Reset
                        || cell.bg != Color::Reset
                        || !cell.modifier.is_empty();
                    if has_symbol || has_style {
                        any = true;
                        break;
                    }
                }
                if any {
                    last_non_empty = Some(yy);
                }
            }
            last_non_empty.map(|v| v + 2).unwrap_or(0)
        }

        let mut i = 0usize;
        let current_steps = self.current_steps();

        while i < current_steps.len() && y < bottom {
            let step = &current_steps[i];
            let max_h = bottom.saturating_sub(y);
            if max_h == 0 || width == 0 {
                break;
            }
            let scratch_area = Rect::new(0, 0, width, max_h);
            let mut scratch = Buffer::empty(scratch_area);
            step.render_ref(scratch_area, &mut scratch);
            let h = used_rows(&scratch, width, max_h).min(max_h);
            if h > 0 {
                let target = Rect {
                    x: area.x,
                    y,
                    width,
                    height: h,
                };
                Clear.render(target, buf);
                step.render_ref(target, buf);
                y = y.saturating_add(h);
            }
            i += 1;
        }
    }
}

impl KeyboardHandler for Step {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match self {
            Step::TrustDirectory(widget) => widget.handle_key_event(key_event),
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        match self {
            Step::TrustDirectory(widget) => widget.handle_paste(pasted),
        }
    }
}

impl StepStateProvider for Step {
    fn get_step_state(&self) -> StepState {
        match self {
            Step::TrustDirectory(w) => w.get_step_state(),
        }
    }
}

impl WidgetRef for Step {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Step::TrustDirectory(widget) => {
                widget.render_ref(area, buf);
            }
        }
    }
}

pub(crate) async fn run_onboarding_app(
    args: OnboardingScreenArgs,
    tui: &mut Tui,
) -> Result<OnboardingResult> {
    use tokio_stream::StreamExt;

    let app_server_request_handle = args.app_server_request_handle.clone();
    let mut onboarding_screen = OnboardingScreen::new(tui, args).await;
    let mut directory_trust_persisted = false;

    tui.draw(u16::MAX, |frame| {
        frame.render_widget_ref(&onboarding_screen, frame.area());
    })?;

    let mut tui_events = tui.event_stream();

    while !onboarding_screen.is_done() {
        if let Some(event) = tui_events.next().await {
            match event {
                TuiEvent::Key(key_event) => {
                    onboarding_screen.handle_key_event(key_event);
                    if !directory_trust_persisted {
                        directory_trust_persisted = persist_selected_trust(
                            &mut onboarding_screen,
                            app_server_request_handle.clone(),
                        )
                        .await;
                    }
                }
                TuiEvent::Paste(text) => {
                    onboarding_screen.handle_paste(text);
                }
                TuiEvent::Draw | TuiEvent::Resize => {
                    let _ = tui.draw(u16::MAX, |frame| {
                        frame.render_widget_ref(&onboarding_screen, frame.area());
                    });
                }
            }
        }
    }
    Ok(OnboardingResult {
        directory_trust_persisted,
        should_exit: onboarding_screen.should_exit(),
    })
}

async fn persist_selected_trust(
    onboarding_screen: &mut OnboardingScreen,
    request_handle: Option<AppServerRequestHandle>,
) -> bool {
    let Some((trust_step_index, trust_target)) =
        onboarding_screen.steps.iter().enumerate().find_map(
            |(index, Step::TrustDirectory(widget))| {
                (widget.selection == Some(TrustDirectorySelection::Trust))
                    .then(|| (index, widget.trust_target.clone()))
            },
        )
    else {
        return false;
    };

    let result = match request_handle {
        Some(request_handle) => write_trusted_project(request_handle, &trust_target)
            .await
            .map(|_| ()),
        None => Err(color_eyre::eyre::eyre!("app server unavailable")),
    };

    match result {
        Ok(()) => true,
        Err(error) => {
            let error = format_config_error(&error);
            tracing::error!(
                "failed to persist trusted project state for {}: {error}",
                trust_target.display()
            );
            let Step::TrustDirectory(widget) = &mut onboarding_screen.steps[trust_step_index];
            widget.selection = None;
            widget.error = Some(format!(
                "Failed to set trust for {}: {error}",
                trust_target.display()
            ));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OnboardingScreen;
    use super::Step;
    use super::StepStateProvider;
    use super::persist_selected_trust;
    use crate::onboarding::trust_directory::TrustDirectorySelection;
    use crate::onboarding::trust_directory::TrustDirectoryWidget;
    use crate::tui::FrameRequester;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    #[tokio::test]
    async fn trust_persistence_failure_keeps_trust_step_in_progress() {
        let mut onboarding_screen = OnboardingScreen {
            request_frame: FrameRequester::test_dummy(),
            steps: vec![Step::TrustDirectory(TrustDirectoryWidget {
                cwd: PathBuf::from("/workspace/project"),
                trust_target: PathBuf::from("/workspace/project"),
                should_quit: false,
                selection: Some(TrustDirectorySelection::Trust),
                highlighted: TrustDirectorySelection::Trust,
                error: None,
            })],
            is_done: false,
            should_exit: false,
        };

        let persisted =
            persist_selected_trust(&mut onboarding_screen, /*request_handle*/ None).await;

        assert!(!persisted);
        let Step::TrustDirectory(widget) = &onboarding_screen.steps[0] else {
            panic!("trust step should remain present");
        };
        assert_eq!(widget.selection, None);
        assert_eq!(widget.get_step_state(), super::StepState::InProgress);
        assert!(
            widget
                .error
                .as_deref()
                .is_some_and(|error| error.contains("app server unavailable"))
        );
    }
}
