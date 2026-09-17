//! Constrained scripted interaction rendering for extension requests.

use std::collections::BTreeMap;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use serde_json::Map;
use serde_json::Value;
use xedoc_app_server_protocol::ExtensionInteractionAction;
use xedoc_app_server_protocol::ExtensionInteractionEligibleRoute;
use xedoc_app_server_protocol::ExtensionInteractionField;
use xedoc_app_server_protocol::ExtensionInteractionForm;
use xedoc_app_server_protocol::ExtensionInteractionNoticeLevel;
use xedoc_app_server_protocol::ExtensionInteractionOutcome;
use xedoc_app_server_protocol::ExtensionInteractionRequestParams;
use xedoc_app_server_protocol::ExtensionInteractionRequestResponse;
use xedoc_app_server_protocol::ExtensionInteractionSelectedAction;
use xedoc_app_server_protocol::ExtensionInteractionSurface;
use xedoc_protocol::ThreadId;
use xedoc_tui_events::ResolvedAppServerRequest;

use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::ViewCompletion;
use crate::bottom_pane::selection_popup_common::render_menu_surface;
use crate::render::renderable::Renderable;

enum RenderMode {
    Surface,
    Override,
}

enum FieldValue {
    Select {
        option_index: Option<usize>,
    },
    Boolean(bool),
    Text(String),
    Action,
    ModelRoute {
        route_index: Option<usize>,
        effort_index: usize,
    },
}

struct SelectPickerState {
    field_index: usize,
    option_index: Option<usize>,
}

pub struct ScriptedInteractionView {
    request: ExtensionInteractionRequestParams,
    model_router_settings: bool,
    app_event_tx: AppEventSender,
    thread_id: ThreadId,
    model_router_settings_thread_id: Option<ThreadId>,
    mode: RenderMode,
    menu_selected: usize,
    action_selected: usize,
    field_selected: usize,
    select_picker: Option<SelectPickerState>,
    form_values: Vec<FieldValue>,
    override_values: Vec<FieldValue>,
    completion: Option<ViewCompletion>,
}

impl ScriptedInteractionView {
    pub fn new(request: ExtensionInteractionRequestParams, app_event_tx: AppEventSender) -> Self {
        Self::with_target(request, app_event_tx, /*model_router_settings*/ false)
    }

    pub fn new_model_router_settings(
        request: ExtensionInteractionRequestParams,
        app_event_tx: AppEventSender,
    ) -> Self {
        Self::with_target(request, app_event_tx, /*model_router_settings*/ true)
    }

    fn with_target(
        request: ExtensionInteractionRequestParams,
        app_event_tx: AppEventSender,
        model_router_settings: bool,
    ) -> Self {
        let thread_id = ThreadId::from_string(&request.thread_id).unwrap_or_default();
        let model_router_settings_thread_id = (!request.thread_id.is_empty())
            .then(|| ThreadId::from_string(&request.thread_id).ok())
            .flatten();
        let surface_form = Self::surface_form(&request.surface);
        let form_values = Self::form_values(surface_form.as_ref());
        let override_values = match &request.surface {
            ExtensionInteractionSurface::Confirmation { override_form, .. } => {
                Self::form_values(override_form.as_ref())
            }
            ExtensionInteractionSurface::Menu { .. }
            | ExtensionInteractionSurface::Form { .. }
            | ExtensionInteractionSurface::Notice { .. } => Vec::new(),
        };
        Self {
            request,
            model_router_settings,
            app_event_tx,
            thread_id,
            model_router_settings_thread_id,
            mode: RenderMode::Surface,
            menu_selected: 0,
            action_selected: 0,
            field_selected: 0,
            select_picker: None,
            form_values,
            override_values,
            completion: None,
        }
    }

    fn surface_form(surface: &ExtensionInteractionSurface) -> Option<ExtensionInteractionForm> {
        match surface {
            ExtensionInteractionSurface::Form {
                id,
                title,
                subtitle,
                fields,
                submit,
                cancel,
            } => Some(ExtensionInteractionForm {
                id: id.clone(),
                title: title.clone(),
                subtitle: subtitle.clone(),
                fields: fields.clone(),
                submit: submit.clone(),
                cancel: cancel.clone(),
            }),
            ExtensionInteractionSurface::Menu { .. }
            | ExtensionInteractionSurface::Confirmation { .. }
            | ExtensionInteractionSurface::Notice { .. } => None,
        }
    }

    fn form_values(form: Option<&ExtensionInteractionForm>) -> Vec<FieldValue> {
        form.map(|form| {
            form.fields
                .iter()
                .map(|field| match field {
                    ExtensionInteractionField::Select { value, options, .. } => {
                        FieldValue::Select {
                            option_index: value
                                .as_ref()
                                .and_then(|value| {
                                    options.iter().position(|option| &option.id == value)
                                })
                                .or_else(|| {
                                    options
                                        .iter()
                                        .position(|option| option.disabled != Some(true))
                                }),
                        }
                    }
                    ExtensionInteractionField::Boolean { value, .. } => FieldValue::Boolean(*value),
                    ExtensionInteractionField::Text {
                        value, max_bytes, ..
                    } => FieldValue::Text(truncate_utf8(value, *max_bytes)),
                    ExtensionInteractionField::Action { .. } => FieldValue::Action,
                    ExtensionInteractionField::ModelRoute {
                        value,
                        eligible_routes,
                        ..
                    } => {
                        let route_index = value
                            .as_ref()
                            .and_then(|value| {
                                eligible_routes.iter().position(|route| {
                                    route.provider_id == value.provider_id
                                        && route.model == value.model
                                        && route.reasoning_efforts.contains(&value.reasoning_effort)
                                })
                            })
                            .or_else(|| (!eligible_routes.is_empty()).then_some(0));
                        let effort_index = route_index
                            .and_then(|index| {
                                value.as_ref().and_then(|value| {
                                    eligible_routes[index]
                                        .reasoning_efforts
                                        .iter()
                                        .position(|effort| effort == &value.reasoning_effort)
                                })
                            })
                            .unwrap_or_default();
                        FieldValue::ModelRoute {
                            route_index,
                            effort_index,
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default()
    }

    fn active_form(&self) -> Option<ExtensionInteractionForm> {
        match (&self.request.surface, &self.mode) {
            (ExtensionInteractionSurface::Form { .. }, RenderMode::Surface) => {
                Self::surface_form(&self.request.surface)
            }
            (
                ExtensionInteractionSurface::Confirmation {
                    override_form: Some(form),
                    ..
                },
                RenderMode::Override,
            ) => Some(form.clone()),
            _ => None,
        }
    }

    fn active_form_values(&self) -> &[FieldValue] {
        match self.mode {
            RenderMode::Surface => &self.form_values,
            RenderMode::Override => &self.override_values,
        }
    }

    fn active_form_values_mut(&mut self) -> &mut [FieldValue] {
        match self.mode {
            RenderMode::Surface => &mut self.form_values,
            RenderMode::Override => &mut self.override_values,
        }
    }

    fn respond(
        &mut self,
        outcome: ExtensionInteractionOutcome,
        action: Option<&ExtensionInteractionAction>,
        values: Value,
    ) {
        let response = ExtensionInteractionRequestResponse {
            extension_id: self.request.extension_id.clone(),
            interaction_id: self.request.interaction_id.clone(),
            continuation: self.request.continuation.clone(),
            state_revision: self.request.state_revision.clone(),
            outcome,
            action: action.map(|action| ExtensionInteractionSelectedAction {
                id: action.id.clone(),
            }),
            values,
        };
        if self.model_router_settings {
            self.app_event_tx
                .send(xedoc_tui_events::AppEvent::ModelRouterSettingsResponse {
                    response,
                    host_action: (outcome == ExtensionInteractionOutcome::Accepted)
                        .then(|| action.and_then(|action| action.host_action))
                        .flatten(),
                    thread_id: self.model_router_settings_thread_id,
                });
        } else {
            self.app_event_tx.extension_interaction_response(
                self.thread_id,
                self.request.request_id.clone(),
                response,
            );
        }
        self.completion = Some(match outcome {
            ExtensionInteractionOutcome::Accepted => ViewCompletion::Accepted,
            ExtensionInteractionOutcome::Cancelled | ExtensionInteractionOutcome::Dismissed => {
                ViewCompletion::Cancelled
            }
        });
    }

    fn cancel_form(&mut self) -> bool {
        let cancel = self.active_form().and_then(|form| form.cancel.clone());
        let Some(cancel) = cancel else {
            return false;
        };
        self.respond(
            ExtensionInteractionOutcome::Cancelled,
            Some(&cancel),
            Value::Object(Map::new()),
        );
        true
    }

    fn dismiss(&mut self) {
        self.respond(
            ExtensionInteractionOutcome::Dismissed,
            None,
            Value::Object(Map::new()),
        );
    }

    fn submit_form(&mut self) {
        let Some(form) = self.active_form() else {
            return;
        };
        let values = Self::form_values_json(&form, self.active_form_values());
        self.respond(
            ExtensionInteractionOutcome::Accepted,
            Some(&form.submit),
            Value::Object(values.into_iter().collect()),
        );
    }

    fn form_values_json(
        form: &ExtensionInteractionForm,
        values: &[FieldValue],
    ) -> BTreeMap<String, Value> {
        form.fields
            .iter()
            .zip(values)
            .filter_map(|(field, value)| {
                let (id, value) = match (field, value) {
                    (
                        ExtensionInteractionField::Select { id, options, .. },
                        FieldValue::Select {
                            option_index: Some(index),
                        },
                    ) => (
                        id,
                        options
                            .get(*index)
                            .map(|option| Value::String(option.id.clone()))?,
                    ),
                    (ExtensionInteractionField::Boolean { id, .. }, FieldValue::Boolean(value)) => {
                        (id, Value::Bool(*value))
                    }
                    (ExtensionInteractionField::Text { id, .. }, FieldValue::Text(value)) => {
                        (id, Value::String(value.clone()))
                    }
                    (
                        ExtensionInteractionField::ModelRoute {
                            id,
                            eligible_routes,
                            ..
                        },
                        FieldValue::ModelRoute {
                            route_index: Some(route_index),
                            effort_index,
                        },
                    ) => {
                        let route = eligible_routes.get(*route_index)?;
                        let reasoning_effort = route.reasoning_efforts.get(*effort_index)?;
                        (
                            id,
                            serde_json::json!({
                                "providerId": route.provider_id,
                                "model": route.model,
                                "reasoningEffort": reasoning_effort,
                            }),
                        )
                    }
                    _ => return None,
                };
                Some((id.clone(), value))
            })
            .collect()
    }

    fn cycle_field(&mut self, direction: i8) {
        let field_count = self.active_form().map_or(0, |form| form.fields.len());
        if field_count == 0 {
            return;
        }
        let current = isize::try_from(self.field_selected).unwrap_or_default();
        let count = isize::try_from(field_count).unwrap_or(1);
        self.field_selected = (current + isize::from(direction)).rem_euclid(count) as usize;
    }

    fn adjust_active_field(&mut self, direction: i8) {
        let Some(form) = self.active_form() else {
            return;
        };
        let field_selected = self.field_selected;
        let Some(field) = form.fields.get(field_selected).cloned() else {
            return;
        };
        let Some(value) = self.active_form_values_mut().get_mut(field_selected) else {
            return;
        };
        match (&field, value) {
            (ExtensionInteractionField::Select { .. }, FieldValue::Select { .. }) => {}
            (ExtensionInteractionField::Boolean { .. }, FieldValue::Boolean(value)) => {
                *value = !*value;
            }
            (
                ExtensionInteractionField::ModelRoute {
                    eligible_routes, ..
                },
                FieldValue::ModelRoute {
                    route_index,
                    effort_index,
                },
            ) => {
                *route_index = cycle_route_index(*route_index, eligible_routes, direction);
                *effort_index = 0;
            }
            (ExtensionInteractionField::Text { .. }, FieldValue::Text(_)) => {}
            (ExtensionInteractionField::Action { .. }, FieldValue::Action) => {}
            _ => {}
        }
    }

    fn adjust_model_route_effort(&mut self, direction: i8) {
        let Some(form) = self.active_form() else {
            return;
        };
        let field_selected = self.field_selected;
        let Some(ExtensionInteractionField::ModelRoute {
            eligible_routes, ..
        }) = form.fields.get(field_selected).cloned()
        else {
            return;
        };
        let Some(FieldValue::ModelRoute {
            route_index: Some(route_index),
            effort_index,
        }) = self.active_form_values_mut().get_mut(field_selected)
        else {
            return;
        };
        let Some(route) = eligible_routes.get(*route_index) else {
            return;
        };
        *effort_index = cycle_index(*effort_index, route.reasoning_efforts.len(), direction);
    }

    fn cycle_single_select_field(&mut self, direction: i8) {
        let Some(form) = self.active_form() else {
            return;
        };
        let Some(ExtensionInteractionField::Select { options, .. }) = form.fields.first() else {
            return;
        };
        if let Some(FieldValue::Select { option_index }) = self.active_form_values_mut().first_mut()
        {
            *option_index = cycle_enabled_index(*option_index, options, direction);
        }
    }

    fn has_single_select_field(&self) -> bool {
        self.active_form().is_some_and(|form| {
            matches!(
                form.fields.as_slice(),
                [ExtensionInteractionField::Select { .. }]
            )
        })
    }

    fn edit_active_text(&mut self, key_event: KeyEvent) -> bool {
        let Some(form) = self.active_form() else {
            return false;
        };
        let field_selected = self.field_selected;
        let Some(ExtensionInteractionField::Text { max_bytes, .. }) =
            form.fields.get(field_selected).cloned()
        else {
            return false;
        };
        let Some(FieldValue::Text(value)) = self.active_form_values_mut().get_mut(field_selected)
        else {
            return false;
        };
        match key_event.code {
            KeyCode::Char(character) if key_event.modifiers == KeyModifiers::NONE => {
                let mut candidate = value.clone();
                candidate.push(character);
                if candidate.len() <= usize::try_from(max_bytes).unwrap_or(usize::MAX) {
                    *value = candidate;
                }
                true
            }
            KeyCode::Backspace => {
                value.pop();
                true
            }
            _ => false,
        }
    }

    fn select_menu_action(&mut self, key_event: KeyEvent) {
        let action = match &self.request.surface {
            ExtensionInteractionSurface::Menu { items, .. } => {
                let selected = items
                    .get(self.menu_selected)
                    .filter(|item| item.disabled != Some(true));
                selected
                    .filter(|item| action_matches_key(&item.action, key_event))
                    .or_else(|| {
                        items
                            .iter()
                            .filter(|item| item.disabled != Some(true))
                            .find(|item| action_matches_key(&item.action, key_event))
                    })
                    .map(|item| item.action.clone())
            }
            _ => None,
        };
        if let Some(action) = action {
            self.respond(
                ExtensionInteractionOutcome::Accepted,
                Some(&action),
                Value::Object(Map::new()),
            );
        }
    }

    fn select_confirmation_action(&mut self, key_event: KeyEvent) {
        let (action, override_form_id) = match &self.request.surface {
            ExtensionInteractionSurface::Confirmation {
                actions,
                override_form,
                ..
            } => (
                actions
                    .get(self.action_selected)
                    .filter(|action| {
                        enter_binding_matches(key_event, KeyModifiers::NONE)
                            || action_matches_key(action, key_event)
                    })
                    .or_else(|| {
                        actions
                            .iter()
                            .find(|action| action_matches_key(action, key_event))
                    })
                    .cloned(),
                override_form.as_ref().map(|form| form.id.clone()),
            ),
            ExtensionInteractionSurface::Menu { .. }
            | ExtensionInteractionSurface::Form { .. }
            | ExtensionInteractionSurface::Notice { .. } => (None, None),
        };
        let Some(action) = action else {
            return;
        };
        if action.opens.as_ref() == override_form_id.as_ref() && override_form_id.is_some() {
            self.open_override();
        } else {
            self.respond(
                ExtensionInteractionOutcome::Accepted,
                Some(&action),
                Value::Object(Map::new()),
            );
        }
    }

    fn cycle_menu_item(&mut self, direction: i8) {
        if let ExtensionInteractionSurface::Menu { items, .. } = &self.request.surface {
            self.menu_selected = cycle_menu_index(self.menu_selected, items, direction);
        }
    }

    fn return_to_parent_menu(&mut self) -> bool {
        let action = match &self.request.surface {
            ExtensionInteractionSurface::Menu { items, .. } => items
                .iter()
                .find(|item| item.label == "Back" && item.disabled != Some(true))
                .map(|item| item.action.clone()),
            ExtensionInteractionSurface::Confirmation { .. }
            | ExtensionInteractionSurface::Form { .. }
            | ExtensionInteractionSurface::Notice { .. } => None,
        };
        let action = action.or_else(|| {
            (self.model_router_settings && self.request.continuation == "settings:policy").then(
                || ExtensionInteractionAction {
                    id: "back".to_string(),
                    opens: None,
                    host_action: None,
                    label: Some("Back".to_string()),
                    key_bindings: vec!["escape".to_string()],
                    context: None,
                    value: None,
                },
            )
        });
        let Some(action) = action else {
            return false;
        };
        self.respond(
            ExtensionInteractionOutcome::Accepted,
            Some(&action),
            Value::Object(Map::new()),
        );
        true
    }

    fn cycle_confirmation_action(&mut self, direction: i8) {
        if let ExtensionInteractionSurface::Confirmation { actions, .. } = &self.request.surface {
            self.action_selected = cycle_index(self.action_selected, actions.len(), direction);
        }
    }

    fn open_override(&mut self) {
        if matches!(
            &self.request.surface,
            ExtensionInteractionSurface::Confirmation {
                override_form: Some(_),
                ..
            }
        ) {
            self.mode = RenderMode::Override;
            self.field_selected = 0;
            self.select_picker = None;
        }
    }

    fn open_active_select_picker(&mut self) -> bool {
        let Some(form) = self.active_form() else {
            return false;
        };
        let field_index = self.field_selected;
        let Some(ExtensionInteractionField::Select { options, .. }) = form.fields.get(field_index)
        else {
            return false;
        };
        let Some(FieldValue::Select { option_index }) = self.active_form_values().get(field_index)
        else {
            return false;
        };
        self.select_picker = Some(SelectPickerState {
            field_index,
            option_index: option_index.or_else(|| {
                options
                    .iter()
                    .position(|option| option.disabled != Some(true))
            }),
        });
        true
    }

    fn cycle_select_picker_option(&mut self, direction: i8) {
        let Some(field_index) = self.select_picker.as_ref().map(|picker| picker.field_index) else {
            return;
        };
        let Some(form) = self.active_form() else {
            return;
        };
        let Some(ExtensionInteractionField::Select { options, .. }) = form.fields.get(field_index)
        else {
            return;
        };
        if let Some(picker) = &mut self.select_picker {
            picker.option_index = cycle_enabled_index(picker.option_index, options, direction);
        }
    }

    fn choose_select_picker_option(&mut self) {
        let Some(picker) = self.select_picker.take() else {
            return;
        };
        let Some(form) = self.active_form() else {
            return;
        };
        let Some(ExtensionInteractionField::Select { options, .. }) =
            form.fields.get(picker.field_index)
        else {
            return;
        };
        let accepted = if picker
            .option_index
            .and_then(|index| options.get(index))
            .is_some_and(|option| option.disabled != Some(true))
        {
            if let Some(FieldValue::Select { option_index }) =
                self.active_form_values_mut().get_mut(picker.field_index)
            {
                *option_index = picker.option_index;
                true
            } else {
                false
            }
        } else {
            false
        };
        if accepted
            && form.id == "settings:policy"
            && form.fields.get(picker.field_index).is_some_and(|field| {
                matches!(
                    field,
                    ExtensionInteractionField::Select { id, .. } if id == "confidence"
                )
            })
        {
            self.submit_form();
        }
    }

    fn close_select_picker(&mut self) -> bool {
        self.select_picker.take().is_some()
    }

    fn handle_form_key_event(&mut self, key_event: KeyEvent) {
        if self.select_picker.is_some() {
            match key_event.code {
                KeyCode::Up => self.cycle_select_picker_option(-1),
                KeyCode::Down | KeyCode::Tab => self.cycle_select_picker_option(1),
                KeyCode::Right | KeyCode::Char(' ')
                    if !enter_binding_matches(key_event, KeyModifiers::NONE) =>
                {
                    self.choose_select_picker_option();
                }
                KeyCode::Esc | KeyCode::Left => {
                    self.close_select_picker();
                }
                _ if enter_binding_matches(key_event, KeyModifiers::NONE) => {
                    self.choose_select_picker_option();
                }
                _ => {}
            }
            return;
        }
        let Some(form) = self.active_form() else {
            return;
        };
        if self.has_single_select_field() {
            match key_event.code {
                KeyCode::Up => self.cycle_single_select_field(-1),
                KeyCode::Down | KeyCode::Tab => self.cycle_single_select_field(1),
                _ if action_matches_key(&form.submit, key_event) => self.submit_form(),
                _ => {}
            }
            return;
        }
        if let Some(ExtensionInteractionField::Action { action, .. }) =
            form.fields.get(self.field_selected)
        {
            if action_matches_key(action, key_event) {
                self.respond(
                    ExtensionInteractionOutcome::Accepted,
                    Some(action),
                    Value::Object(Map::new()),
                );
                return;
            }
        }
        if action_matches_key(&form.submit, key_event) {
            self.submit_form();
            return;
        }
        if form
            .cancel
            .as_ref()
            .is_some_and(|cancel| action_matches_key(cancel, key_event))
        {
            self.cancel_form();
            return;
        }
        if self.edit_active_text(key_event) {
            return;
        }
        match key_event.code {
            KeyCode::Up => self.cycle_field(-1),
            KeyCode::Down | KeyCode::Tab => self.cycle_field(1),
            KeyCode::Left => self.adjust_active_field(-1),
            KeyCode::Right | KeyCode::Char(' ') => {
                if !self.open_active_select_picker() {
                    self.adjust_active_field(1);
                }
            }
            KeyCode::Char('[') => self.adjust_model_route_effort(-1),
            KeyCode::Char(']') => self.adjust_model_route_effort(1),
            _ => {}
        }
    }
}

impl BottomPaneView for ScriptedInteractionView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if key_event.kind == KeyEventKind::Release {
            return;
        }
        if key_event.code == KeyCode::Esc {
            if self.close_select_picker() {
                return;
            }
            if matches!(self.mode, RenderMode::Override) {
                self.mode = RenderMode::Surface;
                return;
            }
            if matches!(
                &self.request.surface,
                ExtensionInteractionSurface::Confirmation { .. }
            ) {
                self.select_confirmation_action(key_event);
                if self.is_complete() || matches!(self.mode, RenderMode::Override) {
                    return;
                }
            }
            if self.return_to_parent_menu() {
                return;
            }
            if !self.cancel_form() {
                if self.model_router_settings {
                    self.completion = Some(ViewCompletion::Cancelled);
                } else {
                    self.dismiss();
                }
            }
            return;
        }
        match (&self.request.surface, &self.mode) {
            (ExtensionInteractionSurface::Menu { .. }, RenderMode::Surface) => {
                self.select_menu_action(key_event);
                if self.is_complete() {
                    return;
                }
                match key_event.code {
                    KeyCode::Up => self.cycle_menu_item(-1),
                    KeyCode::Down | KeyCode::Tab => self.cycle_menu_item(1),
                    _ => {}
                }
            }
            (ExtensionInteractionSurface::Form { .. }, RenderMode::Surface)
            | (ExtensionInteractionSurface::Confirmation { .. }, RenderMode::Override) => {
                self.handle_form_key_event(key_event);
            }
            (ExtensionInteractionSurface::Confirmation { .. }, RenderMode::Surface) => {
                self.select_confirmation_action(key_event);
                if self.is_complete() || matches!(self.mode, RenderMode::Override) {
                    return;
                }
                match key_event.code {
                    KeyCode::Up | KeyCode::Left => self.cycle_confirmation_action(-1),
                    KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
                        self.cycle_confirmation_action(1)
                    }
                    _ => {}
                }
            }
            (ExtensionInteractionSurface::Notice { .. }, RenderMode::Surface) => {
                if key_event.code == KeyCode::Enter {
                    self.dismiss();
                }
            }
            _ => {}
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        if matches!(
            &self.request.surface,
            ExtensionInteractionSurface::Notice { .. }
        ) {
            self.dismiss();
            return CancellationEvent::Handled;
        }
        if self.cancel_form() {
            CancellationEvent::Handled
        } else {
            CancellationEvent::NotHandled
        }
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }

    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn terminal_title_requires_action(&self) -> bool {
        !matches!(
            &self.request.surface,
            ExtensionInteractionSurface::Notice { .. }
        )
    }

    fn dismiss_app_server_request(&mut self, request: &ResolvedAppServerRequest) -> bool {
        let ResolvedAppServerRequest::ExtensionInteraction { request_id } = request else {
            return false;
        };
        if request_id != &self.request.request_id {
            return false;
        }
        self.completion = Some(ViewCompletion::Cancelled);
        true
    }
}

impl Renderable for ScriptedInteractionView {
    fn desired_height(&self, width: u16) -> u16 {
        u16::try_from(self.content_lines(width.saturating_sub(4)).len())
            .unwrap_or(u16::MAX)
            .saturating_add(3)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let content_lines = self.content_lines(area.width.saturating_sub(4));
        let menu_height = u16::try_from(content_lines.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(area.height.saturating_sub(1));
        let [menu_area, footer_area] =
            Layout::vertical([Constraint::Length(menu_height), Constraint::Fill(1)]).areas(area);
        let content_area = render_menu_surface(menu_area, buf);
        Paragraph::new(content_lines).render(content_area, buf);
        Paragraph::new(self.footer_line()).render(
            Rect {
                x: footer_area.x.saturating_add(2),
                y: footer_area.y,
                width: footer_area.width.saturating_sub(2),
                height: footer_area.height,
            },
            buf,
        );
    }
}

impl ScriptedInteractionView {
    fn content_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = match (&self.request.surface, &self.mode) {
            (
                ExtensionInteractionSurface::Menu {
                    title,
                    subtitle,
                    items,
                },
                RenderMode::Surface,
            ) => {
                let mut lines = titled_lines(title, subtitle.as_deref(), width);
                lines.push(Line::default());
                let label_width = items
                    .iter()
                    .map(|item| item.label.len())
                    .max()
                    .unwrap_or_default()
                    .min(usize::from(width) / 2);
                lines.extend(items.iter().enumerate().map(|(index, item)| {
                    let marker = if index == self.menu_selected {
                        ">"
                    } else {
                        " "
                    };
                    let current = if item.current == Some(true) {
                        " (current)"
                    } else {
                        ""
                    };
                    let disabled = if item.disabled == Some(true) {
                        " (unavailable)"
                    } else {
                        ""
                    };
                    let label = format!("{marker} {:label_width$}", item.label,);
                    let information = format!(
                        "  {}{current}{disabled}",
                        item.description.as_deref().unwrap_or_default(),
                    );
                    Line::from(vec![
                        if index == self.menu_selected {
                            label.cyan()
                        } else {
                            label.into()
                        },
                        Span::from(information).dim(),
                    ])
                }));
                lines
            }
            (ExtensionInteractionSurface::Form { .. }, RenderMode::Surface)
            | (
                ExtensionInteractionSurface::Confirmation {
                    override_form: Some(_),
                    ..
                },
                RenderMode::Override,
            ) => self.form_lines(width),
            (
                ExtensionInteractionSurface::Confirmation {
                    title,
                    body,
                    details,
                    sections,
                    actions,
                    override_form: _,
                },
                RenderMode::Surface,
            ) => {
                let mut lines = titled_lines(title, None, width);
                lines.push(Line::default());
                lines.extend(wrapped_lines(body, width, ""));
                lines.extend(
                    details
                        .iter()
                        .map(|detail| format!(" {}: {}", detail.label, detail.value).dim().into()),
                );
                for section in sections {
                    lines.push(Line::default());
                    if let Some(title) = section.title.as_deref() {
                        lines.push(title.to_string().bold().into());
                    }
                    lines.extend(section.rows.iter().flat_map(|row| {
                        wrapped_lines(&row.text, width, &"  ".repeat(usize::from(row.indent)))
                    }));
                }
                lines.push(Line::default());
                let action_label_width = actions
                    .iter()
                    .map(|action| action.label.as_deref().unwrap_or(action.id.as_str()).len())
                    .max()
                    .unwrap_or_default();
                lines.extend(actions.iter().enumerate().map(|(index, action)| {
                    let action_line = format!(
                        "{} {}",
                        if index == self.action_selected {
                            ">"
                        } else {
                            " "
                        },
                        action_label(
                            action,
                            action.label.as_deref().unwrap_or(action.id.as_str()),
                            action_label_width,
                        )
                    );
                    let action_text = if index == self.action_selected {
                        action_line.cyan().into()
                    } else {
                        action_line.into()
                    };
                    if let Some(context) = action.context.as_deref() {
                        Line::from(vec![action_text, format!(" {context}").dim()])
                    } else {
                        Line::from(action_text)
                    }
                }));
                lines
            }
            (ExtensionInteractionSurface::Notice { title, body, level }, RenderMode::Surface) => {
                let mut lines = vec![format!(" {}", notice_prefix(*level, title)).bold().into()];
                lines.extend(wrapped_lines(body, width, ""));
                lines
            }
            _ => Vec::new(),
        };
        if lines.is_empty() {
            lines.push(" Scripted interaction unavailable".red().into());
        }
        lines
    }

    fn form_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(form) = self.active_form() else {
            return Vec::new();
        };
        if self.has_single_select_field() {
            return self.single_select_form_lines(&form, width);
        }
        let mut lines = titled_lines(
            &form.title,
            form.subtitle
                .as_deref()
                .or(Some("Choose a setting, then apply your changes.")),
            width,
        );
        lines.push(Line::default());
        lines.extend(
            form.fields
                .iter()
                .zip(self.active_form_values())
                .enumerate()
                .flat_map(|(index, (field, value))| {
                    let marker = if index == self.field_selected {
                        ">"
                    } else {
                        " "
                    };
                    let (label, description) = field_label_and_description(field, value);
                    let field_line = format!("{marker} {label}");
                    let mut field_lines = vec![if index == self.field_selected {
                        field_line.cyan().into()
                    } else {
                        field_line.into()
                    }];
                    if let Some(description) = description {
                        field_lines.extend(wrapped_lines(description, width, "  "));
                    }
                    if self
                        .select_picker
                        .as_ref()
                        .is_some_and(|picker| picker.field_index == index)
                        && let (ExtensionInteractionField::Select { options, .. }, Some(picker)) =
                            (field, self.select_picker.as_ref())
                    {
                        field_lines.extend(options.iter().enumerate().map(
                            |(option_index, option)| {
                                let marker = if picker.option_index == Some(option_index) {
                                    ">"
                                } else {
                                    " "
                                };
                                let current = select_option_is_current(field, option_index);
                                let unavailable = if option.disabled == Some(true) {
                                    " (unavailable)"
                                } else {
                                    ""
                                };
                                let option_line =
                                    format!("    {marker} {}{current}{unavailable}", option.label);
                                if picker.option_index == Some(option_index) {
                                    option_line.cyan().into()
                                } else if option.disabled == Some(true) {
                                    option_line.dim().into()
                                } else {
                                    option_line.into()
                                }
                            },
                        ));
                    }
                    field_lines
                }),
        );
        lines
    }

    fn single_select_form_lines(
        &self,
        form: &ExtensionInteractionForm,
        width: u16,
    ) -> Vec<Line<'static>> {
        let mut lines = titled_lines(
            &form.title,
            form.subtitle
                .as_deref()
                .or(Some("Choose a setting, then apply your changes.")),
            width,
        );
        lines.push(Line::default());
        let (
            field @ ExtensionInteractionField::Select { options, .. },
            FieldValue::Select { option_index },
        ) = (&form.fields[0], &self.active_form_values()[0])
        else {
            return lines;
        };
        lines.extend(options.iter().enumerate().map(|(index, option)| {
            let marker = if *option_index == Some(index) {
                ">"
            } else {
                " "
            };
            let current = select_option_is_current(field, index);
            let unavailable = if option.disabled == Some(true) {
                " (unavailable)"
            } else {
                ""
            };
            let option_line = format!("{marker} {}{current}{unavailable}", option.label);
            if *option_index == Some(index) {
                option_line.cyan().into()
            } else if option.disabled == Some(true) {
                option_line.dim().into()
            } else {
                option_line.into()
            }
        }));
        lines
    }

    fn footer_line(&self) -> Line<'static> {
        if matches!(
            &self.request.surface,
            ExtensionInteractionSurface::Notice { .. }
        ) {
            "Press enter or esc to dismiss".dim().into()
        } else if self.active_form().is_some_and(|form| {
            form.fields.len() > 1
                && form
                    .fields
                    .iter()
                    .any(|field| matches!(field, ExtensionInteractionField::Select { .. }))
        }) {
            "↑/↓ select · → open choices · enter confirm · esc back"
                .dim()
                .into()
        } else {
            "Press enter to confirm or esc to go back".dim().into()
        }
    }
}

fn action_matches_key(action: &ExtensionInteractionAction, key_event: KeyEvent) -> bool {
    action
        .key_bindings
        .iter()
        .any(|binding| key_binding_matches(binding, key_event))
}

fn key_binding_matches(binding: &str, key_event: KeyEvent) -> bool {
    let mut parts = binding.split('-');
    let mut modifiers = KeyModifiers::NONE;
    let mut key_name = loop {
        let Some(part) = parts.next() else {
            return false;
        };
        match part {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "alt" | "option" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            key_name => break key_name.to_string(),
        }
    };
    for trailing in parts {
        key_name.push('-');
        key_name.push_str(trailing);
    }
    let code = match key_name.as_str() {
        "enter" | "return" => {
            return enter_binding_matches(key_event, modifiers);
        }
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "esc" | "escape" => KeyCode::Esc,
        "delete" | "del" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "page-up" | "pageup" | "pgup" => KeyCode::PageUp,
        "page-down" | "pagedown" | "pgdn" => KeyCode::PageDown,
        "space" | "spacebar" => KeyCode::Char(' '),
        "minus" => KeyCode::Char('-'),
        key_name if key_name.len() == 1 && key_name.is_ascii() => {
            let Some(character) = key_name.chars().next() else {
                return false;
            };
            KeyCode::Char(character)
        }
        key_name if key_name.starts_with('f') => {
            let Ok(number) = key_name[1..].parse::<u8>() else {
                return false;
            };
            if !(1..=24).contains(&number) {
                return false;
            }
            KeyCode::F(number)
        }
        _ => return false,
    };
    key_event.code == code && key_event.modifiers == modifiers
}

fn enter_binding_matches(key_event: KeyEvent, modifiers: KeyModifiers) -> bool {
    key_event.modifiers == modifiers
        && matches!(key_event.code, KeyCode::Enter | KeyCode::Char('\r'))
        || modifiers == KeyModifiers::NONE
            && key_event.code == KeyCode::Char('m')
            && key_event.modifiers == KeyModifiers::CONTROL
}

fn action_label(action: &ExtensionInteractionAction, label: &str, label_width: usize) -> String {
    let binding = if action.key_bindings.is_empty() {
        String::new()
    } else {
        format!(" [{}]", action.key_bindings.join(", "))
    };
    format!("{label:label_width$}{binding}")
}

fn select_option_is_current(
    field: &ExtensionInteractionField,
    option_index: usize,
) -> &'static str {
    match field {
        ExtensionInteractionField::Select {
            current, options, ..
        } if current.as_ref() == options.get(option_index).map(|option| &option.id) => " (current)",
        ExtensionInteractionField::Select { .. }
        | ExtensionInteractionField::Boolean { .. }
        | ExtensionInteractionField::Text { .. }
        | ExtensionInteractionField::Action { .. }
        | ExtensionInteractionField::ModelRoute { .. } => "",
    }
}

fn titled_lines(title: &str, subtitle: Option<&str>, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![title.to_string().bold().into()];
    if let Some(subtitle) = subtitle {
        lines.extend(wrapped_lines(subtitle, width, ""));
    }
    lines
}

fn field_label_and_description<'a>(
    field: &'a ExtensionInteractionField,
    value: &'a FieldValue,
) -> (String, Option<&'a str>) {
    match (field, value) {
        (
            ExtensionInteractionField::Select {
                label,
                description,
                options,
                ..
            },
            FieldValue::Select { option_index },
        ) => (
            format!(
                "{label}: {}",
                option_index
                    .and_then(|index| options.get(index))
                    .map_or("not selected", |option| option.label.as_str())
            ),
            description.as_deref(),
        ),
        (
            ExtensionInteractionField::Boolean {
                label, description, ..
            },
            FieldValue::Boolean(value),
        ) => (
            format!("{label}: {}", if *value { "on" } else { "off" }),
            description.as_deref(),
        ),
        (
            ExtensionInteractionField::Text {
                label,
                description,
                sensitive,
                ..
            },
            FieldValue::Text(value),
        ) => {
            let display_value = if *sensitive {
                "•".repeat(value.chars().count())
            } else {
                value.clone()
            };
            (format!("{label}: {display_value}"), description.as_deref())
        }
        (
            ExtensionInteractionField::Action {
                label, description, ..
            },
            FieldValue::Action,
        ) => (label.clone(), description.as_deref()),
        (
            ExtensionInteractionField::ModelRoute {
                label,
                description,
                eligible_routes,
                ..
            },
            FieldValue::ModelRoute {
                route_index,
                effort_index,
            },
        ) => (
            route_index
                .and_then(|index| eligible_routes.get(index))
                .map(|route| {
                    let effort = route
                        .reasoning_efforts
                        .get(*effort_index)
                        .map_or("default", String::as_str);
                    format!("{label}: {}/{}/{}", route.provider_id, route.model, effort)
                })
                .unwrap_or_else(|| format!("{label}: unavailable")),
            description.as_deref(),
        ),
        _ => ("Invalid field".to_string(), None),
    }
}

fn cycle_enabled_index(
    selected: Option<usize>,
    options: &[xedoc_app_server_protocol::ExtensionInteractionOption],
    direction: i8,
) -> Option<usize> {
    if options.is_empty() || options.iter().all(|option| option.disabled == Some(true)) {
        return None;
    }
    let mut index = selected.unwrap_or_default();
    for _ in 0..options.len() {
        index = cycle_index(index, options.len(), direction);
        if options[index].disabled != Some(true) {
            return Some(index);
        }
    }
    None
}

fn cycle_route_index(
    selected: Option<usize>,
    routes: &[ExtensionInteractionEligibleRoute],
    direction: i8,
) -> Option<usize> {
    (!routes.is_empty()).then(|| cycle_index(selected.unwrap_or_default(), routes.len(), direction))
}

fn cycle_menu_index(
    selected: usize,
    items: &[xedoc_app_server_protocol::ExtensionInteractionMenuItem],
    direction: i8,
) -> usize {
    if items.is_empty() || items.iter().all(|item| item.disabled == Some(true)) {
        return 0;
    }
    let mut index = selected;
    for _ in 0..items.len() {
        index = cycle_index(index, items.len(), direction);
        if items[index].disabled != Some(true) {
            return index;
        }
    }
    0
}

fn cycle_index(index: usize, len: usize, direction: i8) -> usize {
    if len == 0 {
        return 0;
    }
    let index = isize::try_from(index).unwrap_or_default();
    let len = isize::try_from(len).unwrap_or(1);
    (index + isize::from(direction)).rem_euclid(len) as usize
}

fn truncate_utf8(value: &str, max_bytes: u32) -> String {
    let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    if value.len() <= max_bytes {
        return value.to_string();
    }
    value
        .char_indices()
        .take_while(|(index, _)| *index < max_bytes)
        .map(|(_, character)| character)
        .collect()
}

fn wrapped_lines(value: &str, width: u16, indent: &str) -> Vec<Line<'static>> {
    let available = usize::from(width).saturating_sub(indent.len()).max(1);
    textwrap::wrap(value, available)
        .into_iter()
        .map(|line| format!("{indent}{line}").dim().into())
        .collect()
}

fn notice_prefix(level: ExtensionInteractionNoticeLevel, title: &str) -> String {
    match level {
        ExtensionInteractionNoticeLevel::Info => format!("Info: {title}"),
        ExtensionInteractionNoticeLevel::Success => format!("Success: {title}"),
        ExtensionInteractionNoticeLevel::Warning => format!("Warning: {title}"),
        ExtensionInteractionNoticeLevel::Error => format!("Error: {title}"),
    }
}
