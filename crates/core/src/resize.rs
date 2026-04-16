use anyhow::Result;
use image::{DynamicImage, ImageFormat};

use crate::{OptimizeConfig, OutputFormat};

/// Supported image extensions
pub fn is_image(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".png")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
        || lower.ends_with(".tiff")
        || lower.ends_with(".tif")
        || lower.ends_with(".gif")
}

/// Determine output format from file extension
pub fn output_format(name: &str) -> ImageFormat {
    let lower = name.to_lowercase();
    if lower.ends_with(".png") {
        ImageFormat::Png
    } else if lower.ends_with(".webp") {
        ImageFormat::WebP
    } else if lower.ends_with(".bmp") {
        ImageFormat::Bmp
    } else if lower.ends_with(".tiff") || lower.ends_with(".tif") {
        ImageFormat::Tiff
    } else if lower.ends_with(".gif") {
        ImageFormat::Gif
    } else {
        // .jpg / .jpeg → JPEG
        ImageFormat::Jpeg
    }
}

/// Resize image bytes and return (encoded data, output extension).
pub fn resize_image_bytes(
    data: &[u8],
    entry_name: &str,
    config: &OptimizeConfig,
) -> Result<(Vec<u8>, &'static str)> {
    let lower = entry_name.to_lowercase();

    // Skip animated WebP (image crate does not support animated WebP encoding)
    if lower.ends_with(".webp") && is_animated_webp(data) {
        log::info!("animated WebP skipped: {entry_name}");
        return Ok((data.to_vec(), original_ext(entry_name)));
    }

    // Always skip GIF (may be animated)
    if lower.ends_with(".gif") {
        log::info!("GIF skipped (animation not supported): {entry_name}");
        return Ok((data.to_vec(), original_ext(entry_name)));
    }

    let img = image::load_from_memory(data)?;
    let resized = resize_image(img, config);

    let (fmt, ext) = match config.output_format {
        OutputFormat::Jpeg     => (ImageFormat::Jpeg, ".jpg"),
        OutputFormat::Png      => (ImageFormat::Png,  ".png"),
        OutputFormat::Webp     => (ImageFormat::WebP, ".webp"),
        OutputFormat::Original => {
            let f = output_format(entry_name);
            let e = original_ext(entry_name);
            (f, e)
        }
    };

    let encoded = encode_image(resized, fmt, config.jpeg_quality)?;
    Ok((encoded, ext))
}

/// Return the original extension of an entry name (lowercase, with dot)
fn original_ext(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        ".jpg"
    } else if lower.ends_with(".png") {
        ".png"
    } else if lower.ends_with(".webp") {
        ".webp"
    } else if lower.ends_with(".bmp") {
        ".bmp"
    } else if lower.ends_with(".tiff") || lower.ends_with(".tif") {
        ".tiff"
    } else if lower.ends_with(".gif") {
        ".gif"
    } else {
        ".jpg"
    }
}

/// Resize DynamicImage while preserving aspect ratio
fn resize_image(img: DynamicImage, config: &OptimizeConfig) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    let (max_width, max_height) = config.effective_dimensions();

    // Already within limits, skip resize
    if w <= max_width && h <= max_height {
        return img;
    }

    let ratio_w = max_width as f64 / w as f64;
    let ratio_h = max_height as f64 / h as f64;
    let ratio = ratio_w.min(ratio_h);

    let new_w = ((w as f64 * ratio).round() as u32).max(1);
    let new_h = ((h as f64 * ratio).round() as u32).max(1);

    img.resize_exact(new_w, new_h, image::imageops::FilterType::CatmullRom)
}

/// Detect animated WebP from raw bytes (also used by processor.rs)
///
/// WebP format: RIFF????WEBPVP8X + flags byte
/// bit1 (0x02) of flags is the animation flag
pub fn is_animated_webp(data: &[u8]) -> bool {
    data.len() >= 21
        && &data[0..4] == b"RIFF"
        && &data[8..12] == b"WEBP"
        && &data[12..16] == b"VP8X"
        && data[20] & 0x02 != 0
}

/// Encode image to bytes
fn encode_image(img: DynamicImage, fmt: ImageFormat, jpeg_quality: u8) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match fmt {
        ImageFormat::Jpeg => {
            let mut encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, jpeg_quality);
            encoder.encode_image(&img)?;
        }
        _ => {
            img.write_to(&mut std::io::Cursor::new(&mut buf), fmt)?;
        }
    }
    Ok(buf)
}
