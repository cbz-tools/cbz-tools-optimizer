use anyhow::Result;
use image::{DynamicImage, ImageFormat, RgbaImage};
use shiguredo_svt_av1::{
    ColorFormat, EncodeOptions, Encoder as SvtEncoder, EncoderConfig, FrameData, RcMode, Tune,
};

use crate::{OptimizeConfig, OutputFormat};

/// Supported image extensions for input.
/// AVIF decoding uses the native libdav1d library via `image/avif-native`.
pub fn is_image(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".png")
        || lower.ends_with(".webp")
        || lower.ends_with(".avif")
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
    } else if lower.ends_with(".avif") {
        ImageFormat::Avif
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

    let (fmt, ext) = match config.output_format {
        OutputFormat::Jpeg => (ImageFormat::Jpeg, ".jpg"),
        OutputFormat::Png => (ImageFormat::Png, ".png"),
        OutputFormat::Webp => (ImageFormat::WebP, ".webp"),
        OutputFormat::Avif => (ImageFormat::Avif, ".avif"),
        OutputFormat::Original => {
            let f = output_format(entry_name);
            let e = original_ext(entry_name);
            (f, e)
        }
    };

    // convert_only + same format → pass through bytes as-is (zero re-encoding, zero degradation)
    if config.convert_only && original_ext(entry_name) == ext {
        return Ok((data.to_vec(), ext));
    }

    let img = image::load_from_memory(data)?;

    let processed = if config.convert_only {
        img // skip resize entirely
    } else {
        resize_image(img, config)
    };

    let encoded = encode_image(processed, fmt, config.jpeg_quality)?;
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
    } else if lower.ends_with(".avif") {
        ".avif"
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
        ImageFormat::Avif => {
            buf = encode_avif_svt(img)?;
        }
        _ => {
            img.write_to(&mut std::io::Cursor::new(&mut buf), fmt)?;
        }
    }
    Ok(buf)
}

/// Encode an 8-bit image as AVIF using SVT-AV1 for the color planes.
///
/// SVT-AV1's still-image encoder accepts I420 only. The conversion uses full-range BT.709,
/// which is also written into the AVIF color metadata. Transparent images use a separate
/// monochrome AV1 alpha item; SVT has no monochrome input mode, so that item is encoded with
/// ravif while the color item remains SVT-AV1.
fn encode_avif_svt(img: DynamicImage) -> Result<Vec<u8>> {
    const QUALITY: u8 = 80;
    const SPEED: u8 = 6;

    // SVT-AV1 rejects dimensions below 4 pixels. Preserve support for tiny images with the
    // existing encoder; normal CBZ pages use the SVT path below.
    if img.width() < 4 || img.height() < 4 {
        let mut output = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut output), ImageFormat::Avif)?;
        return Ok(output);
    }

    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let (y, u, v) = rgba_to_yuv420_bt709(&rgba);
    let color = encode_svt_i420(width, height, &y, &u, &v, QUALITY, SPEED)?;
    let alpha = has_transparency(&rgba)
        .then(|| encode_alpha_item(&rgba, QUALITY, SPEED))
        .transpose()?;

    let mut avif = avif_serialize::Aviffy::new();
    avif.set_color_primaries(avif_serialize::constants::ColorPrimaries::Bt709)
        .set_transfer_characteristics(avif_serialize::constants::TransferCharacteristics::Srgb)
        .set_matrix_coefficients(avif_serialize::constants::MatrixCoefficients::Bt709)
        .set_full_color_range(true)
        .set_chroma_subsampling((true, true));

    Ok(avif.to_vec(&color, alpha.as_deref(), width, height, 8))
}

fn encode_svt_i420(
    width: u32,
    height: u32,
    y: &[u8],
    u: &[u8],
    v: &[u8],
    quality: u8,
    speed: u8,
) -> Result<Vec<u8>> {
    let mut config = EncoderConfig::new(width as usize, height as usize, ColorFormat::I420);
    config.fps_numerator = 1;
    config.fps_denominator = 1;
    config.rate_control_mode = RcMode::CqpOrCrf;
    config.target_bit_rate = 0;
    config.qp = Some(quality_to_svt_qp(quality));
    config.enc_mode = speed;
    config.tune = Some(Tune::Vq);
    config.avif = Some(true);
    // Let SVT choose its internal parallelism level. The CLI's default outer Rayon scheduling
    // (`--threads 0`) remains enabled and is validated together with this setting.
    config.level_of_parallelism = Some(0);

    let mut encoder = SvtEncoder::new(config)?;
    let frame = FrameData::I420 { y, u, v };
    encoder.encode(&frame, &EncodeOptions::default())?;

    let mut encoded = drain_svt_frames(&mut encoder);
    encoder.finish()?;
    encoded.extend(drain_svt_frames(&mut encoder));
    anyhow::ensure!(!encoded.is_empty(), "SVT-AV1 produced no encoded frame");
    Ok(encoded)
}

fn drain_svt_frames(encoder: &mut SvtEncoder) -> Vec<u8> {
    let mut encoded = Vec::new();
    while let Some(frame) = encoder.next_frame() {
        encoded.extend_from_slice(frame.data());
    }
    encoded
}

fn quality_to_svt_qp(quality: u8) -> u8 {
    // Match ravif's non-linear quality curve, which is the existing image::AvifEncoder
    // behavior. AV1's 0..63 QP scale is a downscaled version of ravif's 0..255 quantizer.
    let quality = f32::from(quality.min(100)) / 100.0;
    let ravif_quantizer = if quality >= 0.82 {
        (1.0 - quality) * 2.6
    } else if quality > 0.25 {
        quality.mul_add(-0.5, 0.875)
    } else {
        1.0 - quality
    } * 255.0;
    (ravif_quantizer * 63.0 / 255.0).round().clamp(0.0, 63.0) as u8
}

fn has_transparency(img: &RgbaImage) -> bool {
    img.pixels().any(|pixel| pixel[3] != u8::MAX)
}

fn encode_alpha_item(img: &RgbaImage, quality: u8, speed: u8) -> Result<Vec<u8>> {
    let (width, height) = img.dimensions();
    let color = img.pixels().map(|pixel| [pixel[0], pixel[1], pixel[2]]);
    let alpha = img.pixels().map(|pixel| pixel[3]);
    let encoded = ravif::Encoder::new()
        .with_quality(f32::from(quality))
        .with_alpha_quality(f32::from(quality))
        .with_speed(speed.clamp(1, 10))
        .encode_raw_planes_8_bit(
            width as usize,
            height as usize,
            color,
            Some(alpha),
            ravif::PixelRange::Full,
            ravif::MatrixCoefficients::BT709,
        )?;
    let parsed = avif_parse::read_avif(&mut encoded.avif_file.as_slice())?;
    parsed
        .alpha_item
        .map(|item| item.as_slice().to_vec())
        .ok_or_else(|| anyhow::anyhow!("ravif did not produce an alpha AV1 item"))
}

fn rgba_to_yuv420_bt709(img: &RgbaImage) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (width, height) = img.dimensions();
    let mut y = Vec::with_capacity((width * height) as usize);
    let chroma_len = width.div_ceil(2) as usize * height.div_ceil(2) as usize;
    let mut u = Vec::with_capacity(chroma_len);
    let mut v = Vec::with_capacity(chroma_len);

    for pixel in img.pixels() {
        y.push(rgb_to_yuv_bt709(pixel[0], pixel[1], pixel[2]).0);
    }

    for block_y in (0..height).step_by(2) {
        for block_x in (0..width).step_by(2) {
            let mut u_sum = 0_u32;
            let mut v_sum = 0_u32;
            let mut count = 0_u32;
            for sample_y in block_y..(block_y + 2).min(height) {
                for sample_x in block_x..(block_x + 2).min(width) {
                    let pixel = img.get_pixel(sample_x, sample_y);
                    let (_, sample_u, sample_v) = rgb_to_yuv_bt709(pixel[0], pixel[1], pixel[2]);
                    u_sum += u32::from(sample_u);
                    v_sum += u32::from(sample_v);
                    count += 1;
                }
            }
            u.push(((u_sum + count / 2) / count) as u8);
            v.push(((v_sum + count / 2) / count) as u8);
        }
    }

    (y, u, v)
}

fn rgb_to_yuv_bt709(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let r = f32::from(r);
    let g = f32::from(g);
    let b = f32::from(b);
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let u = -0.114_572 * r - 0.385_428 * g + 0.5 * b + 128.0;
    let v = 0.5 * r - 0.454_153 * g - 0.045_847 * b + 128.0;
    (
        y.round().clamp(0.0, 255.0) as u8,
        u.round().clamp(0.0, 255.0) as u8,
        v.round().clamp(0.0, 255.0) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SizePreset;
    use image::GenericImageView;

    #[test]
    fn avif_input_can_be_resized_and_reencoded() {
        let source = DynamicImage::new_rgb8(8, 8);
        let mut input = Vec::new();
        source
            .write_to(&mut std::io::Cursor::new(&mut input), ImageFormat::Avif)
            .unwrap();

        let config = OptimizeConfig {
            preset: SizePreset::Custom,
            max_width: 4,
            max_height: 4,
            output_format: OutputFormat::Avif,
            ..OptimizeConfig::default()
        };
        let (output, extension) = resize_image_bytes(&input, "page.avif", &config).unwrap();
        let decoded = image::load_from_memory(&output).unwrap();

        assert_eq!(extension, ".avif");
        assert_eq!(decoded.dimensions(), (4, 4));
    }

    #[test]
    fn svt_avif_keeps_odd_dimensions() {
        let source = DynamicImage::new_rgb8(9, 7);
        let mut input = Vec::new();
        source
            .write_to(&mut std::io::Cursor::new(&mut input), ImageFormat::Png)
            .unwrap();

        let config = OptimizeConfig {
            output_format: OutputFormat::Avif,
            convert_only: true,
            ..OptimizeConfig::default()
        };
        let (output, extension) = resize_image_bytes(&input, "odd.png", &config).unwrap();
        let decoded = image::load_from_memory(&output).unwrap();

        assert_eq!(extension, ".avif");
        assert_eq!(decoded.dimensions(), (9, 7));
    }

    #[test]
    fn svt_avif_keeps_transparency() {
        let mut source = RgbaImage::new(8, 8);
        for (x, y, pixel) in source.enumerate_pixels_mut() {
            *pixel = image::Rgba([x as u8 * 20, y as u8 * 20, 160, if x < 4 { 0 } else { 255 }]);
        }
        let mut input = Vec::new();
        DynamicImage::ImageRgba8(source)
            .write_to(&mut std::io::Cursor::new(&mut input), ImageFormat::Png)
            .unwrap();

        let config = OptimizeConfig {
            output_format: OutputFormat::Avif,
            convert_only: true,
            ..OptimizeConfig::default()
        };
        let (output, extension) = resize_image_bytes(&input, "alpha.png", &config).unwrap();
        let decoded = image::load_from_memory(&output).unwrap().to_rgba8();

        assert_eq!(extension, ".avif");
        assert!(decoded.get_pixel(1, 1)[3] < 10);
        assert!(decoded.get_pixel(6, 1)[3] > 245);
    }
}
