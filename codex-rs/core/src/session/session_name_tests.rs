use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
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
