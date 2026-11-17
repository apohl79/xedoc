//! Bounded raster previews for image output in the browser transcript.

use std::io::Cursor;
use std::io::Read;

use base64::Engine;

pub(crate) fn markdown_image_html(destination: &str, alt: &str, budget: &mut usize) -> String {
    let mut html = String::new();
    let source = if destination.starts_with("data:") {
        Some(destination.to_owned())
    } else {
        let path = if destination.starts_with("file:") {
            url::Url::parse(destination)
                .ok()
                .filter(|url| url.host_str().is_none_or(|host| host == "localhost"))
                .and_then(|url| url.to_file_path().ok())
        } else {
            let path = std::path::Path::new(destination);
            (path.is_absolute()
                && !destination.starts_with("//")
                && !destination.starts_with("\\\\"))
            .then(|| path.to_path_buf())
        };
        path.and_then(|path| file_source(&path))
    };
    let preview = source.as_deref().and_then(preview);
    if let Some(preview) = preview
        && preview.data_url.len() <= *budget
    {
        *budget -= preview.data_url.len();
        html.push_str("<span class=\"history-markdown-image\"><button class=\"history-image-trigger\" type=\"button\" aria-label=\"View image preview\"><img loading=\"lazy\" src=\"");
        html.push_str(&preview.data_url);
        html.push_str("\" alt=\"");
        crate::web_ui_html::escape_html(&mut html, alt);
        html.push_str("\"></button><span class=\"muted\">");
        crate::web_ui_html::escape_html(&mut html, alt);
        html.push_str("</span></span>");
    } else {
        html.push_str("<span class=\"history-tool\">Image preview unavailable");
        if !alt.is_empty() {
            html.push_str(": ");
            crate::web_ui_html::escape_html(&mut html, alt);
        }
        html.push_str("</span>");
    }
    html
}

pub(crate) fn history_image_source(
    cell: &dyn crate::history_cell::HistoryCell,
    image_index: usize,
) -> Option<std::borrow::Cow<'_, str>> {
    use crate::history_cell::ImageOutputCell;
    use crate::history_cell::UserHistoryCell;

    if let Some(images) = cell.as_any().downcast_ref::<ImageOutputCell>() {
        return images
            .images
            .get(image_index)
            .map(|source| std::borrow::Cow::Borrowed(source.as_str()));
    }
    let user = cell.as_any().downcast_ref::<UserHistoryCell>()?;
    if let Some(source) = user.remote_image_urls.get(image_index) {
        return Some(std::borrow::Cow::Borrowed(source.as_str()));
    }
    let path = user
        .local_image_paths
        .get(image_index.checked_sub(user.remote_image_urls.len())?)?;
    file_source(path).map(std::borrow::Cow::Owned)
}

fn file_source(path: &std::path::Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 8 * 1024 * 1024 {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 8 * 1024 * 1024 {
        return None;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
}

pub(crate) fn image_html(source: &str, cell_index: Option<usize>, image_index: usize) -> String {
    let image_reference = cell_index.map_or_else(String::new, |cell_index| {
        format!(" data-image-cell=\"{cell_index}\" data-image-index=\"{image_index}\"")
    });
    match preview(source) {
        Some(preview) => format!(
            "<figure class=\"history-image\"><button class=\"history-image-trigger\" type=\"button\" aria-label=\"View image at full size\"{image_reference}><img src=\"{}\" alt=\"Image\" loading=\"lazy\"></button><figcaption>{} × {} · {}</figcaption></figure>",
            preview.data_url, preview.width, preview.height, preview.format
        ),
        None => "<div class=\"history-tool\">Image preview unavailable</div>".to_string(),
    }
}

pub(crate) fn full_image_data_url(source: &str) -> Option<String> {
    let (bytes, _, mime_type) = decode_source(source)?;
    Some(format!(
        "data:{mime_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

struct ImagePreview {
    data_url: String,
    width: u32,
    height: u32,
    format: &'static str,
}

fn preview(source: &str) -> Option<ImagePreview> {
    let (bytes, format, _) = decode_source(source)?;
    let mut reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let (width, height) = (image.width(), image.height());
    for size in [800, 512, 256, 128] {
        let mut output = Cursor::new(Vec::new());
        image
            .thumbnail(size.min(width), size.min(height))
            .write_to(&mut output, image::ImageFormat::Png)
            .ok()?;
        let bytes = output.into_inner();
        if bytes.len() <= 256 * 1024 {
            return Some(ImagePreview {
                data_url: format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                ),
                width,
                height,
                format,
            });
        }
    }
    None
}

fn decode_source(source: &str) -> Option<(Vec<u8>, &'static str, &'static str)> {
    if source.len() > 12 * 1024 * 1024 {
        return None;
    }
    let encoded = if source.starts_with("data:") {
        let (header, data) = source.split_once(',')?;
        if !matches!(
            header,
            "data:image/png;base64"
                | "data:image/jpeg;base64"
                | "data:image/gif;base64"
                | "data:image/webp;base64"
        ) {
            return None;
        }
        data
    } else {
        source
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    if bytes.len() > 8 * 1024 * 1024 {
        return None;
    }
    let reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?;
    let (format, mime_type) = match reader.format()? {
        image::ImageFormat::Png => ("PNG", "image/png"),
        image::ImageFormat::Jpeg => ("JPEG", "image/jpeg"),
        image::ImageFormat::Gif => ("GIF", "image/gif"),
        image::ImageFormat::WebP => ("WebP", "image/webp"),
        _ => return None,
    };
    Some((bytes, format, mime_type))
}
