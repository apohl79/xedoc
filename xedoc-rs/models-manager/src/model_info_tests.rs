use super::*;
use crate::ModelsManagerConfig;
use crate::instructions::resolve_instructions;
use pretty_assertions::assert_eq;
use xedoc_protocol::config_types::Personality;
use xedoc_protocol::openai_models::ApprovalMessages;
use xedoc_protocol::openai_models::AutoReviewMessages;
use xedoc_protocol::openai_models::ModelInstructionsVariables;
use xedoc_protocol::openai_models::ModelMessages;
use xedoc_protocol::openai_models::ModelPreset;
use xedoc_protocol::openai_models::PermissionMessages;

fn config_with_personality(personality: Option<Personality>) -> ModelsManagerConfig {
    ModelsManagerConfig {
        personality_enabled: true,
        personality,
        ..Default::default()
    }
}

#[test]
fn provider_catalog_metadata_identifies_the_model_provider_and_reasoning_levels() {
    let model = model_info_from_provider_catalog_slug("gemma4:e2b", "ollama");

    assert_eq!(
        model.description,
        Some("ollama model gemma4:e2b".to_string())
    );
    assert!(model.base_instructions.is_empty());
    assert!(model.model_messages.is_none());
    assert!(model.include_skills_usage_instructions);
    assert_eq!(
        model.apply_patch_tool_type,
        Some(ApplyPatchToolType::Freeform)
    );
    assert_eq!(model.visibility, ModelVisibility::List);
    assert_eq!(model.default_reasoning_level, Some(ReasoningEffort::Medium));
    assert_eq!(
        model
            .supported_reasoning_levels
            .iter()
            .map(|preset| preset.effort.clone())
            .collect::<Vec<_>>(),
        vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
        ]
    );
    assert!(ModelPreset::from(model).show_in_picker);
}

#[test]
fn base_instruction_override_preserves_catalog_approval_messages() {
    let mut model = model_info_from_slug("unknown-model");
    let approvals = ApprovalMessages {
        on_request: Some("user approvals".to_string()),
        on_request_auto_review: Some("auto approvals".to_string()),
        never: None,
        unless_trusted: None,
    };
    model.model_messages = Some(ModelMessages {
        instructions_template: Some("template".to_string()),
        instructions_variables: Some(ModelInstructionsVariables {
            personality_default: Some("default".to_string()),
            personality_friendly: Some("friendly".to_string()),
            personality_pragmatic: Some("pragmatic".to_string()),
        }),
        approvals: Some(approvals.clone()),
        auto_review: None,
        permissions: None,
    });
    let config = ModelsManagerConfig {
        base_instructions: Some("override".to_string()),
        ..Default::default()
    };

    let updated = with_config_overrides(model, &config);

    assert_eq!(
        updated.model_messages,
        Some(ModelMessages {
            instructions_template: None,
            instructions_variables: None,
            approvals: Some(approvals),
            auto_review: None,
            permissions: None,
        })
    );
}

#[test]
fn base_instruction_override_preserves_catalog_auto_review_messages() {
    let mut model = model_info_from_slug("unknown-model");
    let auto_review = AutoReviewMessages {
        policy: Some("review policy".to_string()),
        policy_template: Some("review policy template".to_string()),
    };
    model.model_messages = Some(ModelMessages {
        instructions_template: Some("template".to_string()),
        instructions_variables: None,
        approvals: None,
        auto_review: Some(auto_review.clone()),
        permissions: None,
    });
    let config = ModelsManagerConfig {
        base_instructions: Some("override".to_string()),
        ..Default::default()
    };

    let updated = with_config_overrides(model, &config);

    assert_eq!(
        updated.model_messages,
        Some(ModelMessages {
            instructions_template: None,
            instructions_variables: None,
            approvals: None,
            auto_review: Some(auto_review),
            permissions: None,
        })
    );
}

#[test]
fn base_instruction_override_preserves_catalog_permission_messages() {
    let mut model = model_info_from_slug("unknown-model");
    let permissions = PermissionMessages {
        danger_full_access: Some("danger".to_string()),
        workspace_write: Some(String::new()),
        read_only: None,
    };
    model.model_messages = Some(ModelMessages {
        instructions_template: Some("template".to_string()),
        instructions_variables: None,
        approvals: None,
        auto_review: None,
        permissions: Some(permissions.clone()),
    });
    let config = ModelsManagerConfig {
        base_instructions: Some("override".to_string()),
        ..Default::default()
    };

    let updated = with_config_overrides(model, &config);

    assert_eq!(
        updated.model_messages,
        Some(ModelMessages {
            instructions_template: None,
            instructions_variables: None,
            approvals: None,
            auto_review: None,
            permissions: Some(permissions),
        })
    );
}

#[test]
fn baked_personality_section_is_preserved_without_enabled_explicit_none() {
    let instructions = "Intro\n# Personality\nKeep me\n# General\nKeep me too";
    let configs = [
        config_with_personality(/*personality*/ None),
        config_with_personality(Some(Personality::Friendly)),
        config_with_personality(Some(Personality::Pragmatic)),
        ModelsManagerConfig {
            personality: Some(Personality::None),
            ..Default::default()
        },
    ];

    for config in configs {
        let mut model = model_info_from_slug("unknown-model");
        model.base_instructions = instructions.to_string();

        assert_eq!(
            with_config_overrides(model, &config).base_instructions,
            instructions
        );
    }
}

#[test]
fn model_context_window_override_clamps_to_max_context_window() {
    let mut model = model_info_from_slug("unknown-model");
    model.context_window = Some(273_000);
    model.max_context_window = Some(400_000);
    let config = ModelsManagerConfig {
        model_context_window: Some(500_000),
        ..Default::default()
    };

    let updated = with_config_overrides(model.clone(), &config);
    let mut expected = model;
    expected.context_window = Some(400_000);

    assert_eq!(updated, expected);
}

#[test]
fn model_context_window_uses_model_value_without_override() {
    let mut model = model_info_from_slug("unknown-model");
    model.context_window = Some(273_000);
    model.max_context_window = Some(400_000);
    let config = ModelsManagerConfig::default();

    let updated = with_config_overrides(model.clone(), &config);

    assert_eq!(updated, model);
}

#[test]
fn built_in_instructions_render_personality_and_model_identity() {
    let model = resolve_instructions(
        model_info_from_slug("example-model"),
        None,
        &config_with_personality(/*personality*/ None),
        Some("Example Provider"),
        None,
    );

    assert!(model.base_instructions.contains("You are Xedoc"));
    assert!(
        model
            .base_instructions
            .contains("You are running as example-model from Example Provider.")
    );
    assert!(
        model
            .get_model_instructions(Some(Personality::Friendly))
            .contains("warm, encouraging")
    );
    assert!(
        model
            .get_model_instructions(Some(Personality::Pragmatic))
            .contains("terse, pragmatic senior engineer")
    );
    assert_eq!(
        model.get_model_instructions(Some(Personality::None)),
        model.base_instructions
    );
}

#[test]
fn remote_catalog_instructions_require_opt_in_and_yield_to_file_overrides() {
    let model = model_info_from_slug("example-model");
    let mut catalog_model = model_info_from_slug("example-model");
    catalog_model.base_instructions = "remote instructions".to_string();
    catalog_model.model_messages = Some(ModelMessages {
        instructions_template: Some("remote {{ personality }} instructions".to_string()),
        instructions_variables: Some(ModelInstructionsVariables {
            personality_default: Some(String::new()),
            personality_friendly: Some("friendly".to_string()),
            personality_pragmatic: Some("pragmatic".to_string()),
        }),
        approvals: None,
        auto_review: None,
        permissions: None,
    });

    let built_in = resolve_instructions(
        model.clone(),
        Some(&catalog_model),
        &ModelsManagerConfig::default(),
        Some("Example Provider"),
        None,
    );
    assert_ne!(built_in.base_instructions, catalog_model.base_instructions);

    let remote = resolve_instructions(
        model.clone(),
        Some(&catalog_model),
        &ModelsManagerConfig {
            model_remote_instructions: true,
            personality_enabled: true,
            ..Default::default()
        },
        Some("Example Provider"),
        None,
    );
    assert_eq!(
        remote.get_model_instructions(Some(Personality::Friendly)),
        "remote friendly instructions"
    );

    let remote_without_personality = resolve_instructions(
        model.clone(),
        Some(&catalog_model),
        &ModelsManagerConfig {
            model_remote_instructions: true,
            ..Default::default()
        },
        Some("Example Provider"),
        None,
    );
    assert_eq!(
        remote_without_personality.base_instructions,
        catalog_model.base_instructions
    );
    assert_eq!(remote_without_personality.model_messages, None);

    let prompt_override = resolve_instructions(
        model,
        Some(&catalog_model),
        &ModelsManagerConfig {
            model_remote_instructions: true,
            personality_enabled: true,
            ..Default::default()
        },
        Some("Example Provider"),
        Some("file override"),
    );
    assert_eq!(prompt_override.base_instructions, "file override");
    assert_eq!(prompt_override.model_messages, None);
}
