//! Declarative interaction vocabulary that scripts may request.

use crate::EligibleRoute;
use crate::OpaqueId;
use crate::Route;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

/// A script-defined action selected by the user.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    /// Opaque action identifier.
    pub id: OpaqueId,
    /// Opaque nested surface identifier opened locally instead of submitted.
    pub opens: Option<OpaqueId>,
    /// Explicit host capability requested after this action is accepted.
    #[serde(default)]
    pub host_action: Option<ModelRouterSettingsHostAction>,
    /// Visible action label when the surface renders controls.
    pub label: Option<String>,
    /// Key bindings offered by the script.
    #[serde(rename = "keyBindings")]
    #[serde(default)]
    pub key_bindings: Vec<String>,
    /// Optional trailing context rendered after the label and key bindings.
    pub context: Option<String>,
    /// Optional script-defined action value.
    pub value: Option<Value>,
}

/// A host capability available to model-router settings scripts.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelRouterSettingsHostAction {
    /// Open the bounded local routing report.
    OpenReport,
    /// Arm the next routing A/B experiment.
    ArmAb,
    /// Disable a pending routing A/B experiment.
    DisableAb,
}

/// An action selected by the host.
#[derive(Debug, Serialize, Deserialize)]
pub struct SelectedAction {
    /// Opaque action identifier.
    pub id: OpaqueId,
}

/// A generic constrained interaction.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interaction {
    /// Opaque interaction identifier.
    pub id: OpaqueId,
    /// Opaque state passed back to the script unchanged.
    pub continuation: OpaqueId,
    /// Script state revision used to reject stale submissions.
    pub state_revision: Option<OpaqueId>,
    /// Optional session-scoped router state to apply after this interaction succeeds.
    #[serde(default)]
    pub session_update: Option<SessionUpdate>,
    /// Surface the host may render.
    pub surface: InteractionSurface,
}

/// A bounded router state update that lasts only for the live session.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdate {
    /// Router mode override, or `null` to resume the shared policy mode.
    pub router_mode: Option<OpaqueId>,
}

/// A renderable surface.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InteractionSurface {
    /// Selectable menu rows.
    Menu(MenuSurface),
    /// Structured field submission.
    Form(FormSurface),
    /// Approval or rejection with optional override fields.
    Confirmation(ConfirmationSurface),
    /// Non-interactive information.
    Notice(NoticeSurface),
}

/// A selectable menu.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuSurface {
    /// Menu title.
    pub title: String,
    /// Optional menu subtitle.
    pub subtitle: Option<String>,
    /// Rows the user may inspect or select.
    pub items: Vec<MenuItem>,
}

/// One menu row.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuItem {
    /// Opaque row identifier.
    pub id: OpaqueId,
    /// Visible row label.
    pub label: String,
    /// Optional row description.
    pub description: Option<String>,
    /// Action submitted when selected.
    pub action: Action,
    /// Whether this row describes the current selection.
    pub current: Option<bool>,
    /// Whether the row cannot be selected.
    pub disabled: Option<bool>,
}

/// A form with constrained field types.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormSurface {
    /// Opaque form identifier.
    pub id: OpaqueId,
    /// Form title.
    pub title: String,
    /// Optional form subtitle.
    pub subtitle: Option<String>,
    /// Fields accepted by this form.
    pub fields: Vec<FormField>,
    /// Action submitted when the form is accepted.
    pub submit: Action,
    /// Optional action submitted when the form is cancelled.
    pub cancel: Option<Action>,
}

/// A form field supported by the host renderer.
#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FormField {
    /// Choice from script-provided opaque options.
    Select {
        /// Opaque field identifier.
        id: OpaqueId,
        /// Visible field label.
        label: String,
        /// Optional field description.
        description: Option<String>,
        /// Currently selected option identifier.
        value: Option<OpaqueId>,
        /// Script-owned current option identifier, distinct from a pending selection.
        current: Option<OpaqueId>,
        /// Selectable options.
        options: Vec<SelectOption>,
    },
    /// Boolean value.
    Boolean {
        /// Opaque field identifier.
        id: OpaqueId,
        /// Visible field label.
        label: String,
        /// Optional field description.
        description: Option<String>,
        /// Current boolean value.
        value: bool,
    },
    /// Bounded plain-text input.
    Text {
        /// Opaque field identifier.
        id: OpaqueId,
        /// Visible field label.
        label: String,
        /// Optional field description.
        description: Option<String>,
        /// Current plain-text value.
        value: String,
        /// Maximum UTF-8 byte length accepted by the renderer.
        max_bytes: u32,
        /// Whether the host should mask this field's value while rendering it.
        #[serde(default)]
        sensitive: bool,
    },
    /// Host-validated selection from supplied eligible model routes.
    ModelRoute {
        /// Opaque field identifier.
        id: OpaqueId,
        /// Visible field label.
        label: String,
        /// Optional field description.
        description: Option<String>,
        /// Current selected route.
        value: Option<Route>,
        /// Eligible routes available to the user.
        eligible_routes: Vec<EligibleRoute>,
    },
}

/// One opaque selection option.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectOption {
    /// Opaque option identifier.
    pub id: OpaqueId,
    /// Visible option label.
    pub label: String,
    /// Optional option description.
    pub description: Option<String>,
    /// Whether the option is unavailable.
    pub disabled: Option<bool>,
}

/// Confirmation surface that may include an override form.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmationSurface {
    /// Confirmation title.
    pub title: String,
    /// Main confirmation copy.
    pub body: String,
    /// Supplementary label/value pairs.
    pub details: Vec<Detail>,
    /// Script-owned grouped content rendered between the body and actions.
    #[serde(default)]
    pub sections: Vec<ConfirmationSection>,
    /// Actions offered by the confirmation.
    pub actions: Vec<Action>,
    /// Optional form used to override a proposed route.
    #[serde(rename = "override")]
    pub override_form: Option<FormSurface>,
}

/// A titled group of confirmation rows.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmationSection {
    /// Optional section heading.
    pub title: Option<String>,
    /// Rows rendered in order.
    pub rows: Vec<ConfirmationRow>,
}

/// One script-owned confirmation row.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmationRow {
    /// Complete row text supplied by the script.
    pub text: String,
    /// Number of two-space indentation levels.
    #[serde(default)]
    pub indent: u8,
}

/// A visible label/value pair.
#[derive(Debug, Serialize, Deserialize)]
pub struct Detail {
    /// Visible label.
    pub label: String,
    /// Visible value.
    pub value: String,
}

/// Non-interactive information surface.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoticeSurface {
    /// Notice title.
    pub title: String,
    /// Notice body.
    pub body: String,
    /// Display severity.
    pub level: NoticeLevel,
}

/// Notice display severity.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoticeLevel {
    /// Informational notice.
    Info,
    /// Successful completion notice.
    Success,
    /// Warning notice.
    Warning,
    /// Error notice.
    Error,
}

/// User outcome supplied to `interaction.respond`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionResponse {
    /// Opaque continuation received from the script.
    pub continuation: OpaqueId,
    /// Opaque interaction identifier received from the script.
    pub interaction_id: OpaqueId,
    /// State revision rendered by the host.
    pub state_revision: Option<OpaqueId>,
    /// User outcome.
    pub outcome: InteractionOutcome,
    /// Selected action when one exists.
    pub action: Option<SelectedAction>,
    /// Values keyed by opaque field identifiers.
    pub values: Value,
}

/// How the user concluded an interaction.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InteractionOutcome {
    /// The user submitted a selected action.
    Accepted,
    /// The user cancelled the interaction.
    Cancelled,
    /// The user dismissed a notice.
    Dismissed,
}
