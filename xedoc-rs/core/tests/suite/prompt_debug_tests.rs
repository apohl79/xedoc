use std::sync::Arc;

use anyhow::Result;
use core_test_support::responses::strip_metadata;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use xedoc_core::build_prompt_input;
use xedoc_core::config::ConfigBuilder;
use xedoc_core::config::ConfigOverrides;
use xedoc_home::XedocHomeUserInstructionsProvider;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::user_input::UserInput;

const TEST_INSTRUCTIONS: &str = "Global test instructions";

#[tokio::test]
async fn build_prompt_input_includes_context_and_user_message() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::write(xedoc_home.path().join("AGENTS.md"), TEST_INSTRUCTIONS)?;
    let config = ConfigBuilder::default()
        .xedoc_home(xedoc_home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            xedoc_self_exe: Some(std::env::current_exe()?),
            ..ConfigOverrides::default()
        })
        .build()
        .await?;
    let user_instructions_provider = Arc::new(XedocHomeUserInstructionsProvider::new(
        config.xedoc_home.clone(),
    ));

    let input = build_prompt_input(
        config,
        vec![UserInput::Text {
            text: "hello from debug prompt".to_string(),
            text_elements: Vec::new(),
        }],
        /*state_db*/ None,
        user_instructions_provider,
    )
    .await?;

    let expected_user_message = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "hello from debug prompt".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    assert_eq!(
        input.last().cloned().map(strip_metadata),
        Some(expected_user_message)
    );
    assert!(input.iter().any(|item| {
        let ResponseItem::Message { content, .. } = item else {
            return false;
        };

        content.iter().any(|content_item| {
            let (ContentItem::InputText { text } | ContentItem::OutputText { text }) = content_item
            else {
                return false;
            };
            text.contains(TEST_INSTRUCTIONS)
        })
    }));

    Ok(())
}
