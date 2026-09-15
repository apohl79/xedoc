//! Model-router approval control for the bottom pane.

use std::collections::BTreeMap;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use xedoc_app_server_protocol::ModelRouterApprovalAction;
use xedoc_app_server_protocol::ModelRouterApprovalParams;
use xedoc_app_server_protocol::ModelRouterApprovalResponse;
use xedoc_model_router::ApprovalRouting;
use xedoc_model_router::AxisValue;
use xedoc_model_router::ModelClass;
use xedoc_model_router::ModelRoute;
use xedoc_model_router::RankedRouteSelection;
use xedoc_model_router::RankingLadderEntry;
use xedoc_model_router::ReasoningEffort;
use xedoc_model_router::select_ranked_route_for_classifications;
use xedoc_protocol::ThreadId;

use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::ViewCompletion;
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
    selected_override_field: usize,
    selected_classifications: BTreeMap<String, usize>,
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
            selected_override_field: 0,
            selected_classifications: BTreeMap::new(),
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
            classifications: BTreeMap::new(),
        });
    }

    fn reject(&mut self) {
        self.respond(ModelRouterApprovalResponse {
            action: ModelRouterApprovalAction::Reject,
            classification: None,
            classifications: BTreeMap::new(),
        });
    }

    fn begin_override(&mut self) {
        self.mode = ApprovalMode::Override;
        self.selected_override_field = 0;
        self.selected_classifications = self
            .request
            .classification_options
            .iter()
            .map(|(axis, options)| {
                let selection = self
                    .request
                    .classifications
                    .get(axis)
                    .and_then(|selected| options.iter().position(|option| option == selected))
                    .unwrap_or_default();
                (axis.clone(), selection)
            })
            .collect();
    }

    fn submit_override(&mut self) {
        let classifications: BTreeMap<String, String> = self
            .request
            .classification_options
            .iter()
            .filter_map(|(axis, options)| {
                let selection = *self.selected_classifications.get(axis)?;
                options
                    .get(selection)
                    .map(|classification| (axis.clone(), classification.clone()))
            })
            .collect();
        self.respond(ModelRouterApprovalResponse {
            action: ModelRouterApprovalAction::Override,
            classification: classifications.get("work_type").cloned(),
            classifications,
        });
    }

    fn selected_axis(&self) -> Option<(&str, &[String])> {
        self.ordered_classification_options()
            .into_iter()
            .nth(self.selected_override_field)
            .map(|(axis, options)| (axis.as_str(), options.as_slice()))
    }

    fn ordered_classification_options(&self) -> Vec<(&String, &Vec<String>)> {
        let mut options = self
            .request
            .classification_options
            .iter()
            .collect::<Vec<_>>();
        options.sort_by_key(|(axis, _)| match axis.as_str() {
            "work_type" => 0,
            "complexity" => 1,
            "orchestration" => 2,
            "risk" => 3,
            _ => 4,
        });
        options
    }

    fn cycle_selection(&mut self, direction: isize) {
        let axis_count = self.request.classification_options.len();
        if self.selected_override_field < axis_count {
            let Some((axis, options)) = self
                .selected_axis()
                .map(|(axis, options)| (axis.to_string(), options.len()))
            else {
                return;
            };
            if options == 0 {
                return;
            }
            let selection = self.selected_classifications.entry(axis).or_default();
            *selection = (*selection as isize + direction).rem_euclid(options as isize) as usize;
        }
    }

    fn cycle_field(&mut self, direction: isize) {
        let field_count = self.request.classification_options.len();
        self.selected_override_field = (self.selected_override_field as isize + direction)
            .rem_euclid(field_count as isize) as usize;
    }

    fn preview_route(&self) -> Option<RankedRouteSelection> {
        let axes = self
            .selected_classifications
            .iter()
            .map(|(axis, _)| {
                let ratings = self.request.classification_ratings.get(axis)?;
                let values = ratings
                    .iter()
                    .map(|(id, rating)| {
                        Some((
                            id.clone(),
                            AxisValue {
                                id: id.clone(),
                                points: rating.points,
                                minimum_class: model_class(&rating.minimum_model_class)?,
                                maximum_class: model_class(&rating.maximum_model_class)?,
                            },
                        ))
                    })
                    .collect::<Option<BTreeMap<_, _>>>()?;
                Some((axis.clone(), values))
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        let ladder = self
            .request
            .ranking_ladder
            .iter()
            .map(|entry| {
                Some(RankingLadderEntry {
                    rank: entry.rank,
                    class: model_class(&entry.model_class)?,
                    route: ModelRoute {
                        provider_id: entry.route.provider_id.clone(),
                        model_slug: entry.route.model_slug.clone(),
                        reasoning_effort: reasoning_effort(&entry.route.reasoning_effort)?,
                    },
                })
            })
            .collect::<Option<Vec<_>>>()?;
        select_ranked_route_for_classifications(
            &ApprovalRouting {
                axes,
                minimum_score: self.request.ranking_minimum_score,
                maximum_score: self.request.ranking_maximum_score,
                ladder,
            },
            &self
                .selected_classifications
                .iter()
                .filter_map(|(axis, selection)| {
                    self.request
                        .classification_options
                        .get(axis)?
                        .get(*selection)
                        .map(|value| (axis.clone(), value.clone()))
                })
                .collect(),
        )
    }
}

fn model_class(model_class: &str) -> Option<ModelClass> {
    match model_class {
        "simple" => Some(ModelClass::Simple),
        "smart" => Some(ModelClass::Smart),
        "intelligent" => Some(ModelClass::Intelligent),
        _ => None,
    }
}

fn reasoning_effort(reasoning_effort: &str) -> Option<ReasoningEffort> {
    match reasoning_effort {
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "xhigh" => Some(ReasoningEffort::ExtraHigh),
        "max" => Some(ReasoningEffort::Max),
        _ => None,
    }
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
                KeyEvent {
                    code: KeyCode::Up, ..
                } => self.cycle_field(-1),
                KeyEvent {
                    code: KeyCode::Down,
                    ..
                } => self.cycle_field(1),
                KeyEvent {
                    code: KeyCode::Left,
                    ..
                } => self.cycle_selection(-1),
                KeyEvent {
                    code: KeyCode::Right,
                    ..
                } => self.cycle_selection(1),
                _ => {}
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
            ApprovalMode::Override => {
                u16::try_from(self.request.classification_options.len()).unwrap_or(u16::MAX) + 9
            }
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let route = &self.request.proposed_route;
        let lines: Vec<Line<'_>> = vec![
            " Model route approval".bold().into(),
            format!(" Classes: {:?}", self.request.classifications).into(),
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
                    " Use ↑/↓ to select and ←/→ to change a class. The route is derived. Enter applies."
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
                let mut selectors = self
                    .ordered_classification_options()
                    .into_iter()
                    .enumerate()
                    .map(|(index, (axis, options))| {
                        let selected = self
                            .selected_classifications
                            .get(axis)
                            .and_then(|selection| options.get(*selection))
                            .map_or("", String::as_str);
                        Line::from(format!(
                            "{} {axis}: {selected}",
                            if index == self.selected_override_field {
                                ">"
                            } else {
                                " "
                            }
                        ))
                    })
                    .collect::<Vec<_>>();
                if let Some(selection) = self.preview_route() {
                    let bounded_score = selection.score.clamp(
                        self.request.ranking_minimum_score,
                        self.request.ranking_maximum_score,
                    );
                    selectors.push(
                        format!(
                            "  score {} (bounded {}; domain {}–{}) · classes {}–{} · ranks {}–{} · target {} · selected {}",
                            selection.score,
                            bounded_score,
                            self.request.ranking_minimum_score,
                            self.request.ranking_maximum_score,
                            model_class_label(selection.minimum_class),
                            model_class_label(selection.maximum_class),
                            selection.minimum_rank,
                            selection.maximum_rank,
                            selection.target_rank,
                            selection.selected_rank,
                        )
                        .dim()
                        .into(),
                    );
                    selectors.push(
                        format!(
                            "  route: {}/{}/{} (derived)",
                            selection.route.provider_id,
                            selection.route.model_slug,
                            reasoning_effort_label(selection.route.reasoning_effort),
                        )
                        .dim()
                        .into(),
                    );
                } else {
                    selectors.push(
                        "  route: unavailable (incomplete approval metadata)"
                            .dim()
                            .into(),
                    );
                }
                Paragraph::new(selectors).render(
                    Rect {
                        x: area.x,
                        y: area.y.saturating_add(7),
                        width: area.width,
                        height: area.height.saturating_sub(7),
                    },
                    buf,
                );
            }
        }
    }
}

fn model_class_label(model_class: ModelClass) -> &'static str {
    match model_class {
        ModelClass::Simple => "simple",
        ModelClass::Smart => "smart",
        ModelClass::Intelligent => "intelligent",
    }
}

fn reasoning_effort_label(reasoning_effort: ReasoningEffort) -> &'static str {
    match reasoning_effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::ExtraHigh => "xhigh",
        ReasoningEffort::Max => "max",
    }
}
