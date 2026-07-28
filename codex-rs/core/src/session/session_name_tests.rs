use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;

#[test]
fn normalize_generated_session_name_trims_quotes_and_caps_length() {
    let name =
        normalize_generated_session_name("\"Refactor generated session names with overrides\"")
            .expect("generated name");

    assert!(name.chars().count() <= MAX_SESSION_NAME_CHARS);
    assert_eq!(name, "Refactor generated session names");
}

#[test]
fn normalize_generated_session_name_caps_length_at_word_boundary() {
    let name = normalize_generated_session_name("Run reviewer team on main-fork diff")
        .expect("generated name");

    assert_eq!(name, "Run reviewer team on main-fork");
}

#[test]
fn normalize_generated_session_name_removes_repeated_title_phrase() {
    let name = normalize_generated_session_name("Merge and Push Changes Merge and Push Changes")
        .expect("generated name");

    assert_eq!(name, "Merge and Push Changes");
}

#[test]
fn normalize_generated_session_name_removes_partial_repeated_title_prefix() {
    let name = normalize_generated_session_name("Merge and Push Changes Merge and")
        .expect("generated name");

    assert_eq!(name, "Merge and Push Changes");
}

#[test]
fn normalize_generated_session_name_uses_explicit_label_from_agent_response() {
    let name = normalize_generated_session_name(
        "I'll do both: name the session and write the report.\n\
         <function_calls>\n\
         <invoke name=\"bash\"></invoke>\n\
         </function_calls>\n\
         **Session name:** Reconciliation code review\n\
         **Report written to:** `review-codex.md`",
    )
    .expect("generated name");

    assert_eq!(name, "Reconciliation code review");
}

#[test]
fn normalize_generated_session_name_rejects_agent_action_without_label() {
    let name =
        normalize_generated_session_name("I'll do both: name the session and write the report.");

    assert_eq!(name, None);
}

#[test]
fn normalize_generated_session_name_strips_unsafe_display_characters() {
    let name = normalize_generated_session_name(
        "  Project\tNames\x1b\x07\u{009D}\u{202E} \u{200B}Refresh  ",
    )
    .expect("generated name");

    assert_eq!(name, "Project Names Refresh");
}

#[test]
fn normalize_generated_session_name_redacts_sensitive_shapes() {
    let name =
        normalize_generated_session_name("sk-proj-1234567890abcdef").expect("generated name");

    assert_eq!(name, SENSITIVE_SESSION_NAME_FALLBACK);
}

#[test]
fn normalize_generated_session_name_keeps_non_secret_aws_prefix_words() {
    let name = normalize_generated_session_name("Asia travel planning").expect("generated name");

    assert_eq!(name, "Asia travel planning");
}

#[test]
fn normalize_generated_session_name_caps_words() {
    let name = normalize_generated_session_name("aa bb cc dd ee ff gg hh").expect("generated name");

    assert_eq!(name, "aa bb cc dd ee ff gg");
}

#[test]
fn transcript_excerpt_uses_recent_user_and_assistant_text() {
    let items = vec![
        message("developer", "ignore this"),
        message("user", "set up config"),
        message("assistant", "updated the config path"),
    ];

    assert_eq!(
        transcript_excerpt_with_partial_response(&items, None),
        Some("User: set up config\nAssistant: updated the config path".to_string())
    );
}

#[test]
fn transcript_excerpt_keeps_latest_forty_eight_messages() {
    let items = (0..50)
        .map(|index| message("user", &format!("m{index:02}")))
        .collect::<Vec<_>>();

    let transcript =
        transcript_excerpt_with_partial_response(&items, None).expect("transcript excerpt");
    let lines = transcript
        .split('\n')
        .map(str::to_string)
        .collect::<Vec<_>>();
    let expected = (2..50)
        .map(|index| format!("User: m{index:02}"))
        .collect::<Vec<_>>();

    assert_eq!(lines, expected);
}

#[test]
fn transcript_excerpt_includes_partial_response_after_history() {
    let items = vec![message("user", "set up automatic names")];

    assert_eq!(
        transcript_excerpt_with_partial_response(
            &items,
            Some("working through the mid turn title update")
        ),
        Some(
            "User: set up automatic names\nAssistant: working through the mid turn title update"
                .to_string()
        )
    );
}

#[test]
fn transcript_excerpt_uses_partial_response_without_committed_history() {
    assert_eq!(
        transcript_excerpt_with_partial_response(
            &[],
            Some("streaming the first assistant response")
        ),
        Some("Assistant: streaming the first assistant response".to_string())
    );
}

#[test]
fn select_session_name_model_uses_openai_mini_when_available() {
    let models = vec![
        model_preset("gpt-5.5", "GPT-5.5", /*show_in_picker*/ true),
        model_preset("gpt-5.4-mini", "GPT-5.4 Mini", /*show_in_picker*/ true),
    ];

    let selection = select_session_name_model("openai", Some("gpt-fast"), "gpt-5.5", &models);

    assert_eq!(
        (selection.model, selection.reason),
        ("gpt-5.4-mini", SessionNameModelSelectionReason::OpenAiMini)
    );
}

#[test]
fn select_session_name_model_uses_configured_fast_model_for_custom_provider() {
    let models = vec![
        model_preset(
            "claude-opus-4-1",
            "Claude Opus",
            /*show_in_picker*/ true,
        ),
        model_preset(
            "claude-3-5-haiku-latest",
            "Claude 3.5 Haiku",
            /*show_in_picker*/ true,
        ),
    ];

    let selection = select_session_name_model(
        "custom-claude",
        Some("claude-3-5-haiku-latest"),
        "claude-opus-4-1",
        &models,
    );

    assert_eq!(
        (selection.model, selection.reason),
        (
            "claude-3-5-haiku-latest",
            SessionNameModelSelectionReason::ConfiguredFastModel
        )
    );
}

#[test]
fn select_session_name_model_uses_default_for_custom_provider_without_fast_model() {
    let models = vec![
        model_preset(
            "claude-sonnet-4",
            "Claude Sonnet",
            /*show_in_picker*/ true,
        ),
        model_preset(
            "claude-haiku-4",
            "Claude Haiku",
            /*show_in_picker*/ true,
        ),
    ];

    let selection = select_session_name_model("custom-claude", None, "claude-sonnet-4", &models);

    assert_eq!(
        (selection.model, selection.reason),
        (
            "claude-sonnet-4",
            SessionNameModelSelectionReason::DefaultModel
        )
    );
}

#[test]
fn select_session_name_model_uses_default_when_openai_mini_is_missing() {
    let models = vec![model_preset(
        "gpt-5.5", "GPT-5.5", /*show_in_picker*/ true,
    )];

    let selection = select_session_name_model("openai", Some("gpt-fast"), "gpt-5.5", &models);

    assert_eq!(
        (selection.model, selection.reason),
        ("gpt-5.5", SessionNameModelSelectionReason::DefaultModel)
    );
}

#[test]
fn select_session_name_model_ignores_hidden_preferred_models() {
    let models = vec![
        model_preset(
            "gpt-5.4-mini",
            "GPT-5.4 Mini",
            /*show_in_picker*/ false,
        ),
        model_preset("gpt-5.5", "GPT-5.5", /*show_in_picker*/ true),
    ];

    let selection = select_session_name_model("openai", None, "gpt-5.5", &models);

    assert_eq!(
        (selection.model, selection.reason),
        ("gpt-5.5", SessionNameModelSelectionReason::DefaultModel)
    );
}

#[test]
fn select_session_name_model_uses_default_for_blank_fast_model() {
    let selection = select_session_name_model("custom-claude", Some("  "), "claude-opus-4-1", &[]);

    assert_eq!(
        (selection.model, selection.reason),
        (
            "claude-opus-4-1",
            SessionNameModelSelectionReason::DefaultModel
        )
    );
}

fn message(role: &str, text: &str) -> ResponseItem {
    let content = match role {
        "assistant" => vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        _ => vec![ContentItem::InputText {
            text: text.to_string(),
        }],
    };
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content,
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn model_preset(model: &str, display_name: &str, show_in_picker: bool) -> ModelPreset {
    ModelPreset {
        id: model.to_string(),
        model: model.to_string(),
        display_name: display_name.to_string(),
        description: String::new(),
        default_reasoning_effort: ReasoningEffort::None,
        supported_reasoning_efforts: Vec::new(),
        supports_personality: false,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        is_default: false,
        upgrade: None,
        show_in_picker,
        multi_agent_version: None,
        availability_nux: None,
        supported_in_api: true,
        input_modalities: Vec::new(),
    }
}
