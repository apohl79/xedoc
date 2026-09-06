use xedoc_model_provider_info::WireApi;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::provider_item_metadata::ProviderItemMetadata;

#[derive(Clone)]
pub(crate) struct ProviderProvenance {
    pub(crate) provider_id: String,
    pub(crate) model: String,
    pub(crate) wire_api: WireApi,
}

impl ProviderProvenance {
    pub(crate) fn project(&self, input: &mut Vec<ResponseItem>) {
        input.retain_mut(|item| match item {
            ResponseItem::Reasoning {
                encrypted_content,
                provider_metadata,
                ..
            } => self.project_reasoning(encrypted_content, provider_metadata),
            ResponseItem::FunctionCall {
                provider_metadata, ..
            }
            | ResponseItem::CustomToolCall {
                provider_metadata, ..
            } => {
                if self.wire_api != WireApi::Gemini
                    || provider_metadata.as_ref().is_some_and(|metadata| {
                        !metadata.belongs_to_gemini_provider(&self.provider_id)
                    })
                {
                    *provider_metadata = None;
                }
                true
            }
            ResponseItem::Compaction {
                provider_metadata, ..
            }
            | ResponseItem::ContextCompaction {
                provider_metadata, ..
            } => provider_metadata
                .as_ref()
                .is_none_or(|metadata| metadata.belongs_to_responses_provider(&self.provider_id)),
            _ => true,
        });
    }

    pub(crate) fn tag_output_item(&self, item: &mut ResponseItem) {
        match item {
            ResponseItem::Reasoning {
                encrypted_content,
                provider_metadata,
                ..
            } => match self.wire_api {
                WireApi::Anthropic => {
                    let thinking_signature = encrypted_content.take();
                    *provider_metadata = Some(ProviderItemMetadata::Anthropic {
                        provider_id: self.provider_id.clone(),
                        model: self.model.clone(),
                        thinking_signature,
                    });
                }
                WireApi::Responses => {
                    *provider_metadata = Some(ProviderItemMetadata::Responses {
                        provider_id: self.provider_id.clone(),
                    });
                }
                WireApi::Gemini => {}
            },
            ResponseItem::Compaction {
                provider_metadata, ..
            }
            | ResponseItem::ContextCompaction {
                provider_metadata, ..
            } if self.wire_api == WireApi::Responses => {
                *provider_metadata = Some(ProviderItemMetadata::Responses {
                    provider_id: self.provider_id.clone(),
                });
            }
            ResponseItem::FunctionCall {
                provider_metadata: Some(ProviderItemMetadata::Gemini { provider_id, .. }),
                ..
            }
            | ResponseItem::CustomToolCall {
                provider_metadata: Some(ProviderItemMetadata::Gemini { provider_id, .. }),
                ..
            } if self.wire_api == WireApi::Gemini => {
                provider_id.clone_from(&self.provider_id);
            }
            _ => {}
        }
    }

    fn project_reasoning(
        &self,
        encrypted_content: &mut Option<String>,
        provider_metadata: &mut Option<ProviderItemMetadata>,
    ) -> bool {
        let Some(metadata) = provider_metadata.as_ref() else {
            return self.project_legacy_reasoning(encrypted_content.as_deref());
        };

        match self.wire_api {
            WireApi::Anthropic => {
                let Some(signature) =
                    metadata.anthropic_thinking_signature(&self.provider_id, &self.model)
                else {
                    return false;
                };
                *encrypted_content = signature.map(ToString::to_string);
                true
            }
            WireApi::Responses => metadata.belongs_to_responses_provider(&self.provider_id),
            WireApi::Gemini => false,
        }
    }

    fn project_legacy_reasoning(&self, encrypted_content: Option<&str>) -> bool {
        match (self.wire_api, encrypted_content) {
            (_, None) => true,
            (WireApi::Responses, Some(content)) => is_fernet_token(content),
            (WireApi::Anthropic, Some(content)) => !is_fernet_token(content),
            (WireApi::Gemini, Some(_)) => false,
        }
    }
}

fn is_fernet_token(value: &str) -> bool {
    let unpadded = value.trim_end_matches('=');
    let padding = value.len() - unpadded.len();
    value.starts_with("gAAAA")
        && padding <= 2
        && !unpadded.contains('=')
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '=')
        })
}

#[cfg(test)]
#[path = "history_projection_tests.rs"]
mod tests;
