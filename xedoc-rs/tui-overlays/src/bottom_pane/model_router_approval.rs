//! Model-router approval control for the bottom pane.

use std::cell::RefCell;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::Widget;
use xedoc_app_server_protocol::ModelRouterApprovalAction;
use xedoc_app_server_protocol::ModelRouterApprovalParams;
use xedoc_app_server_protocol::ModelRouterApprovalResponse;
use xedoc_app_server_protocol::ModelRouterRoute;
use xedoc_protocol::ThreadId;

use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::ViewCompletion;
use crate::bottom_pane::textarea::TextArea;
use crate::bottom_pane::textarea::TextAreaState;
use crate::render::renderable::Renderable;

enum ApprovalMode {
    Choice,
    Override,
}

pub struct ModelRouterApprovalView {
    request: ModelRouterApprovalParams,
    app_event_tx: AppEventSender,
    thread_id: ThreadId,
    mode: ApprovalMode,
    override_input: TextArea,
    override_input_state: RefCell<TextAreaState>,
    completion: Option<ViewCompletion>,
}

impl ModelRouterApprovalView {
    pub fn new(request: ModelRouterApprovalParams, app_event_tx: AppEventSender) -> Self {
        let thread_id = ThreadId::from_string(&request.thread_id).unwrap_or_default();
        Self {
            request,
            app_event_tx,
            thread_id,
            mode: ApprovalMode::Choice,
            override_input: TextArea::new(),
            override_input_state: RefCell::new(TextAreaState::default()),
            completion: None,
        }
    }

    fn respond(&mut self, response: ModelRouterApprovalResponse) {
        self.app_event_tx.model_router_approval(
            self.thread_id,
            self.request.approval_id.clone(),
            response,
        );
        self.completion = Some(ViewCompletion::Accepted);
    }

    fn approve(&mut self) {
        self.respond(ModelRouterApprovalResponse {
            action: ModelRouterApprovalAction::Approve,
            classification: None,
            route: None,
        });
    }

    fn reject(&mut self) {
        self.respond(ModelRouterApprovalResponse {
            action: ModelRouterApprovalAction::Reject,
            classification: None,
            route: None,
        });
    }

    fn begin_override(&mut self) {
        self.mode = ApprovalMode::Override;
        self.override_input.set_text_clearing_elements(&format!(
            "{}\n{}/{}/{}",
            self.request.predicted_classification,
            self.request.proposed_route.provider_id,
            self.request.proposed_route.model_slug,
            self.request.proposed_route.reasoning_effort
        ));
        self.override_input
            .set_cursor(self.override_input.text().len());
    }

    fn submit_override(&mut self) {
        let mut lines = self.override_input.text().lines();
        let classification = lines
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let route = lines.next().and_then(parse_route);
        self.respond(ModelRouterApprovalResponse {
            action: ModelRouterApprovalAction::Override,
            classification: classification.map(str::to_owned),
            route,
        });
    }
}

fn parse_route(value: &str) -> Option<ModelRouterRoute> {
    let mut segments = value.split('/');
    let provider_id = segments.next()?.trim();
    let model_slug = segments.next()?.trim();
    let reasoning_effort = segments.next()?.trim();
    if provider_id.is_empty()
        || model_slug.is_empty()
        || reasoning_effort.is_empty()
        || segments.next().is_some()
    {
        return None;
    }
    Some(ModelRouterRoute {
        provider_id: provider_id.to_string(),
        model_slug: model_slug.to_string(),
        reasoning_effort: reasoning_effort.to_string(),
    })
}

impl BottomPaneView for ModelRouterApprovalView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if key_event.kind == KeyEventKind::Release {
            return;
        }
        match self.mode {
            ApprovalMode::Choice => match key_event {
                KeyEvent {
                    code: KeyCode::Char('a') | KeyCode::Enter,
                    modifiers: KeyModifiers::NONE,
                    ..
                } => self.approve(),
                KeyEvent {
                    code: KeyCode::Char('r') | KeyCode::Esc,
                    modifiers: KeyModifiers::NONE,
                    ..
                } => self.reject(),
                KeyEvent {
                    code: KeyCode::Char('o'),
                    modifiers: KeyModifiers::NONE,
                    ..
                } => self.begin_override(),
                _ => {}
            },
            ApprovalMode::Override => match key_event {
                KeyEvent {
                    code: KeyCode::Enter,
                    modifiers: KeyModifiers::NONE,
                    ..
                } => self.submit_override(),
                KeyEvent {
                    code: KeyCode::Esc,
                    modifiers: KeyModifiers::NONE,
                    ..
                } => self.mode = ApprovalMode::Choice,
                other => self.override_input.input(other),
            },
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.reject();
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn terminal_title_requires_action(&self) -> bool {
        true
    }
}

impl Renderable for ModelRouterApprovalView {
    fn desired_height(&self, _width: u16) -> u16 {
        match self.mode {
            ApprovalMode::Choice => 7,
            ApprovalMode::Override => 10,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let route = &self.request.proposed_route;
        let lines: Vec<Line<'_>> = vec![
            " Model route approval".bold().into(),
            format!(" Class: {}", self.request.predicted_classification).into(),
            format!(
                " Proposed: {}/{}/{}",
                route.provider_id, route.model_slug, route.reasoning_effort
            )
            .cyan()
            .into(),
            format!(
                " Confidence: {:.2} (margin {:.2})",
                self.request.score, self.request.margin
            )
            .dim()
            .into(),
        ];
        Paragraph::new(lines).render(
            Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: 4,
            },
            buf,
        );
        match self.mode {
            ApprovalMode::Choice => {
                Paragraph::new(" [a/Enter] approve  [o] override  [r/Esc] keep current route".dim())
                    .render(
                        Rect {
                            x: area.x,
                            y: area.y.saturating_add(5),
                            width: area.width,
                            height: 1,
                        },
                        buf,
                    )
            }
            ApprovalMode::Override => {
                Paragraph::new(
                    " Edit class on line 1 and provider/model/effort on line 2. Enter applies."
                        .dim(),
                )
                .render(
                    Rect {
                        x: area.x,
                        y: area.y.saturating_add(5),
                        width: area.width,
                        height: 1,
                    },
                    buf,
                );
                let mut state = self.override_input_state.borrow_mut();
                StatefulWidgetRef::render_ref(
                    &(&self.override_input),
                    Rect {
                        x: area.x,
                        y: area.y.saturating_add(7),
                        width: area.width,
                        height: area.height.saturating_sub(7),
                    },
                    buf,
                    &mut state,
                );
            }
        }
    }
}
