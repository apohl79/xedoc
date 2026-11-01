//! Bounded raster previews for image output in the browser transcript.

use std::io::Cursor;

use base64::Engine;

pub(crate) fn image_html(source: &str) -> String {
    match preview(source) {
        Some(data_url) => format!(
            "<figure class=\"history-image\"><img src=\"{data_url}\" alt=\"Image output\" loading=\"lazy\"></figure>"
        ),
        None => "<div class=\"history-tool\">Image preview unavailable</div>".to_string(),
    }
}

fn preview(source: &str) -> Option<String> {
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
    if !matches!(
        reader.format()?,
        image::ImageFormat::Png
            | image::ImageFormat::Jpeg
            | image::ImageFormat::Gif
            | image::ImageFormat::WebP
    ) {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    for size in [1024, 512, 256, 128] {
        let mut output = Cursor::new(Vec::new());
        image
            .thumbnail(size.min(image.width()), size.min(image.height()))
            .write_to(&mut output, image::ImageFormat::Png)
            .ok()?;
        let bytes = output.into_inner();
        if bytes.len() <= 256 * 1024 {
            return Some(format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ));
        }
    }
    None
}
