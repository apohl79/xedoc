use std::path::Path;
use std::path::PathBuf;

use xedoc_protocol::openai_models::ModelInfo;
use xedoc_protocol::openai_models::ModelInstructionsVariables;
use xedoc_protocol::openai_models::ModelMessages;

use crate::config::ModelsManagerConfig;
use crate::model_info::clear_instruction_messages;
use crate::model_info::with_config_overrides;

pub const BASE_INSTRUCTIONS: &str = include_str!("../prompts/base.md");
const FRIENDLY_PERSONALITY: &str = include_str!("../prompts/personality_friendly.md");
const PRAGMATIC_PERSONALITY: &str = include_str!("../prompts/personality_pragmatic.md");
const PERSONALITY_PLACEHOLDER: &str = "{{ personality }}";

const _: () = assert!(BASE_INSTRUCTIONS.len() <= 4_096);
const _: () = assert!(FRIENDLY_PERSONALITY.len() <= 512);
const _: () = assert!(PRAGMATIC_PERSONALITY.len() <= 512);

/// Resolves instructions from configuration, explicit overrides, catalog metadata, or Xedoc's built-in prompt.
pub fn resolve_instructions(
    mut model: ModelInfo,
    catalog_model: Option<&ModelInfo>,
    config: &ModelsManagerConfig,
    provider_display_name: Option<&str>,
    prompt_override: Option<&str>,
) -> ModelInfo {
    model = with_config_overrides(model, config);
    if config.base_instructions.is_some() {
        return model;
    }

    if let Some(prompt_override) = prompt_override.filter(|prompt| !prompt.trim().is_empty()) {
        model.base_instructions = prompt_override.to_string();
        clear_instruction_messages(&mut model);
        return model;
    }

    if config.model_remote_instructions
        && let Some(catalog_model) =
            catalog_model.filter(|catalog_model| !catalog_model.base_instructions.trim().is_empty())
    {
        apply_catalog_instructions(&mut model, catalog_model);
        if !config.personality_enabled {
            clear_instruction_messages(&mut model);
        }
        return model;
    }

    apply_built_in_instructions(&mut model, provider_display_name);
    if !config.personality_enabled {
        clear_instruction_messages(&mut model);
    }
    model
}

/// Returns the provider-specific file used for a model's explicit prompt override.
pub fn prompt_override_path(xedoc_home: &Path, provider_id: &str, model_id: &str) -> PathBuf {
    xedoc_home
        .join("prompts")
        .join(provider_id)
        .join(format!("{model_id}.md"))
}

/// Loads the first non-empty explicit prompt override for a model.
pub fn load_prompt_override(
    xedoc_home: &Path,
    provider_id: &str,
    model_id: &str,
) -> Option<String> {
    [
        prompt_override_path(xedoc_home, provider_id, model_id),
        xedoc_home.join("prompts").join(format!("{model_id}.md")),
    ]
    .into_iter()
    .find_map(|path| {
        std::fs::read_to_string(path)
            .ok()
            .filter(|contents| !contents.trim().is_empty())
    })
}

/// Removes prompt-specific metadata before it is persisted in the model registry.
pub fn clear_persisted_instructions(model: &mut ModelInfo) {
    model.base_instructions.clear();
    clear_instruction_messages(model);
}

fn apply_catalog_instructions(model: &mut ModelInfo, catalog_model: &ModelInfo) {
    model
        .base_instructions
        .clone_from(&catalog_model.base_instructions);
    replace_instruction_messages(model, catalog_model.model_messages.as_ref());
}

fn apply_built_in_instructions(model: &mut ModelInfo, provider_display_name: Option<&str>) {
    let template = built_in_template(&model.slug, provider_display_name);
    model.base_instructions = template.replace(PERSONALITY_PLACEHOLDER, "");
    replace_instruction_messages(
        model,
        Some(&ModelMessages {
            instructions_template: Some(template),
            instructions_variables: Some(ModelInstructionsVariables {
                personality_default: Some(String::new()),
                personality_friendly: Some(FRIENDLY_PERSONALITY.to_string()),
                personality_pragmatic: Some(PRAGMATIC_PERSONALITY.to_string()),
            }),
            approvals: None,
            auto_review: None,
            permissions: None,
        }),
    );
}

fn built_in_template(slug: &str, provider_display_name: Option<&str>) -> String {
    let Some(provider_display_name) = provider_display_name.filter(|name| !name.trim().is_empty())
    else {
        return BASE_INSTRUCTIONS.to_string();
    };

    format!(
        "{BASE_INSTRUCTIONS}\n\n# Model identity\nYou are running as {slug} from {provider_display_name}."
    )
}

fn replace_instruction_messages(model: &mut ModelInfo, instructions: Option<&ModelMessages>) {
    let (approvals, auto_review, permissions) = model
        .model_messages
        .take()
        .map(|messages| {
            (
                messages.approvals,
                messages.auto_review,
                messages.permissions,
            )
        })
        .unwrap_or((None, None, None));
    let (instructions_template, instructions_variables) = instructions
        .map(|messages| {
            (
                messages.instructions_template.clone(),
                messages.instructions_variables.clone(),
            )
        })
        .unwrap_or((None, None));

    if instructions_template.is_some()
        || instructions_variables.is_some()
        || approvals.is_some()
        || auto_review.is_some()
        || permissions.is_some()
    {
        model.model_messages = Some(ModelMessages {
            instructions_template,
            instructions_variables,
            approvals,
            auto_review,
            permissions,
        });
    }
}
