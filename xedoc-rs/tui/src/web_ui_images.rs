//! Bounded raster previews for image output in the browser transcript.

use std::io::Cursor;

use base64::Engine;

pub(crate) fn image_html(source: &str) -> String {
    match preview(source) {
        Some(preview) => format!(
            "<figure class=\"history-image\"><button class=\"history-image-trigger\" type=\"button\" aria-label=\"View image at full size\"><img src=\"{}\" alt=\"Generated image\" loading=\"lazy\"></button><figcaption>{} × {} · {}</figcaption></figure>",
            preview.data_url, preview.width, preview.height, preview.format
        ),
        None => "<div class=\"history-tool\">Image preview unavailable</div>".to_string(),
    }
}

struct ImagePreview {
    data_url: String,
    width: u32,
    height: u32,
    format: &'static str,
}

fn preview(source: &str) -> Option<ImagePreview> {
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
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let format = match reader.format()? {
        image::ImageFormat::Png => "PNG",
        image::ImageFormat::Jpeg => "JPEG",
        image::ImageFormat::Gif => "GIF",
        image::ImageFormat::WebP => "WebP",
        _ => return None,
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let (width, height) = (image.width(), image.height());
    for size in [1024, 512, 256, 128] {
        let mut output = Cursor::new(Vec::new());
        image
            .thumbnail(size.min(image.width()), size.min(image.height()))
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
