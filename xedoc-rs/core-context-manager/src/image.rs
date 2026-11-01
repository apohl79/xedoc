use base64::Engine;
use std::io::Cursor;
use tracing::warn;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::FunctionCallOutputContentItem;
use xedoc_protocol::models::ImageDetail;
use xedoc_protocol::models::ResponseItem;
use xedoc_utils_image::ImageProcessingError;
use xedoc_utils_image::PromptImageMode;
use xedoc_utils_image::PromptImageResizeLimits;
use xedoc_utils_image::load_data_url_for_prompt;

pub(crate) const IMAGE_PROCESSING_ERROR_PLACEHOLDER: &str =
    "image content omitted because it could not be processed";
const IMAGE_TOO_LARGE_PLACEHOLDER: &str =
    "image content omitted because it exceeded the supported size limit; use a smaller image";
const UNSUPPORTED_LOW_DETAIL_PLACEHOLDER: &str = "image content omitted because detail 'low' is not supported; use 'high', 'original', or 'auto'";
const REMOTE_IMAGE_URL_PLACEHOLDER: &str =
    "image content omitted because remote image URLs are not supported";

const HIGH_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 2048,
    max_patches: 2_500,
};
const ORIGINAL_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 6000,
    max_patches: 10_000,
};

/// Validates sampled assistant images without rewriting signed provider content.
pub fn validate_assistant_image_message(item: &ResponseItem) -> Result<(), String> {
    let ResponseItem::Message { role, content, .. } = item else {
        return Ok(());
    };
    if role != "assistant" {
        return Ok(());
    }
    let images: Vec<_> = content
        .iter()
        .filter_map(|part| match part {
            ContentItem::OutputImage { image_url, .. }
            | ContentItem::InputImage { image_url, .. } => Some(image_url),
            _ => None,
        })
        .take(33)
        .collect();
    if images.is_empty() {
        return Ok(());
    }
    if images.len() > 32
        || images.iter().any(|image| image.len() > 12 * 1024 * 1024)
        || images.iter().map(|image| image.len()).sum::<usize>() > 32 * 1024 * 1024
    {
        return Err("Assistant image output exceeds the image size limit".to_string());
    }
    if crate::estimate_item_token_count(item) > 10_000 {
        return Err(
            "Assistant image output exceeds the 10,000-token context item limit".to_string(),
        );
    }
    for source in images {
        let (header, encoded) = source
            .split_once(',')
            .ok_or("Assistant image output must be an inline raster data URL")?;
        let format = match header {
            "data:image/png;base64" => image::ImageFormat::Png,
            "data:image/jpeg;base64" => image::ImageFormat::Jpeg,
            "data:image/gif;base64" => image::ImageFormat::Gif,
            "data:image/webp;base64" => image::ImageFormat::WebP,
            _ => return Err("Assistant image output must be an inline raster data URL".to_string()),
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "Assistant image output contains invalid base64")?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("Assistant image output exceeds the image size limit".to_string());
        }
        let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        reader
            .decode()
            .map_err(|error| format!("Assistant image output is invalid or oversized: {error}"))?;
    }
    Ok(())
}
#[derive(Debug, thiserror::Error)]
enum ImagePreparationError {
    #[error("remote image URLs are not supported")]
    RemoteUrlUnsupported,
    #[error("image detail `low` is not supported")]
    UnsupportedLowDetail,
    #[error(transparent)]
    Processing(#[from] ImageProcessingError),
}

impl ImagePreparationError {
    fn placeholder(&self) -> &'static str {
        match self {
            ImagePreparationError::RemoteUrlUnsupported => REMOTE_IMAGE_URL_PLACEHOLDER,
            ImagePreparationError::UnsupportedLowDetail => UNSUPPORTED_LOW_DETAIL_PLACEHOLDER,
            ImagePreparationError::Processing(ImageProcessingError::ImageTooLarge { .. }) => {
                IMAGE_TOO_LARGE_PLACEHOLDER
            }
            ImagePreparationError::Processing(_) => IMAGE_PROCESSING_ERROR_PLACEHOLDER,
        }
    }
}

pub fn prepare_image_response_items(items: &mut [ResponseItem]) {
    for item in items {
        match item {
            ResponseItem::Message { content, .. } => prepare_message_content(content),
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                if let Some(content) = output.content_items_mut() {
                    prepare_tool_output_content(content);
                }
            }
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::AgentMessage { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::FunctionCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::CompactionTrigger { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other => {}
        }
    }
}

fn prepare_message_content(items: &mut [ContentItem]) {
    for item in items {
        if let ContentItem::InputImage { image_url, detail } = item
            && let Err(error) = prepare_image(image_url, *detail)
        {
            warn!(%error, "failed to prepare message image");
            *item = ContentItem::InputText {
                text: error.placeholder().to_string(),
            };
        }
    }
}

fn prepare_tool_output_content(items: &mut [FunctionCallOutputContentItem]) {
    for item in items {
        if let FunctionCallOutputContentItem::InputImage { image_url, detail } = item
            && let Err(error) = prepare_image(image_url, *detail)
        {
            warn!(%error, "failed to prepare tool output image");
            *item = FunctionCallOutputContentItem::InputText {
                text: error.placeholder().to_string(),
            };
        }
    }
}

fn is_remote_image_url(image_url: &str) -> bool {
    image_url.split_once(':').is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    })
}

fn is_data_url(image_url: &str) -> bool {
    image_url
        .get(.."data:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
}

fn prepare_image(
    image_url: &mut String,
    detail: Option<ImageDetail>,
) -> Result<(), ImagePreparationError> {
    if is_remote_image_url(image_url) {
        return Err(ImagePreparationError::RemoteUrlUnsupported);
    }
    if !is_data_url(image_url) {
        return Ok(());
    }

    let limits = match detail {
        None | Some(ImageDetail::Auto | ImageDetail::High) => HIGH_DETAIL_LIMITS,
        Some(ImageDetail::Original) => ORIGINAL_DETAIL_LIMITS,
        Some(ImageDetail::Low) => return Err(ImagePreparationError::UnsupportedLowDetail),
    };
    let image = load_data_url_for_prompt(image_url, PromptImageMode::ResizeWithLimits(limits))?;
    *image_url = image.into_data_url();
    Ok(())
}
