use std::io::Cursor;

use anyhow::Result;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};
use shiguredo_svt_av1::{
    ColorFormat, EncodeOptions, Encoder as SvtEncoder, EncoderConfig, FrameData, RcMode, Tune,
};

use crate::animated_webp::{optimize_animated_webp, AnimatedWebpOutcome};
use crate::gif::{decode_static_gif, optimize_animated_gif};
use crate::{OptimizeConfig, OutputFormat, ResizeFilter};

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

    // Keep animated WebP byte-identical when convert-only requests the same
    // format, including any source Orientation metadata.
    if lower.ends_with(".webp")
        && config.convert_only
        && matches!(
            config.output_format,
            OutputFormat::Original | OutputFormat::Webp
        )
    {
        return Ok((data.to_vec(), ".webp"));
    }

    // Animated WebP has a dedicated path. It stays WebP regardless of the
    // archive-wide static output format or convert-only setting.
    if lower.ends_with(".webp") && is_animated_webp(data) {
        return match optimize_animated_webp(
            data,
            &config.animated_webp,
            config.effective_dimensions().0,
            config.effective_dimensions().1,
        )? {
            AnimatedWebpOutcome::Optimized { bytes, report } => {
                log::info!(
                    "animated WebP optimized: {entry_name} ({} -> {} bytes, {:+.1}%)",
                    report.input_bytes,
                    report.encoded_bytes,
                    report.saved_percent,
                );
                Ok((bytes, ".webp"))
            }
            AnimatedWebpOutcome::KeptOriginal { reason, report } => {
                log::info!(
                    "animated WebP kept: {entry_name} ({reason:?}; {} -> {} bytes)",
                    report.input_bytes,
                    report.encoded_bytes,
                );
                Ok((data.to_vec(), ".webp"))
            }
        };
    }

    if lower.ends_with(".gif") {
        let probe = zengif::detect::probe(data)?;
        if probe.is_animated {
            return match optimize_animated_gif(
                data,
                &config.animated_webp,
                config.effective_dimensions().0,
                config.effective_dimensions().1,
                config.convert_only,
            )? {
                AnimatedWebpOutcome::Optimized { bytes, report } => {
                    log::info!(
                        "animated GIF converted to WebP: {entry_name} ({} -> {} bytes, {:+.1}%)",
                        report.input_bytes,
                        report.encoded_bytes,
                        report.saved_percent,
                    );
                    Ok((bytes, ".webp"))
                }
                AnimatedWebpOutcome::KeptOriginal { reason, report } => {
                    log::info!(
                        "animated GIF kept: {entry_name} ({reason:?}; {} -> {} bytes)",
                        report.input_bytes,
                        report.encoded_bytes,
                    );
                    Ok((data.to_vec(), ".gif"))
                }
            };
        }

        // Original-format static GIFs remain byte-identical when no resize or
        // format conversion is requested. There is intentionally no GIF encoder
        // in this path; a requested resize under Original selects lossless PNG.
        let needs_resize = !config.convert_only
            && target_dimensions(
                u32::from(probe.width),
                u32::from(probe.height),
                config.effective_dimensions().0,
                config.effective_dimensions().1,
            )
            .is_some();
        if matches!(config.output_format, OutputFormat::Original) && !needs_resize {
            return Ok((data.to_vec(), ".gif"));
        }

        let (fmt, ext) = match config.output_format {
            OutputFormat::Jpeg => (ImageFormat::Jpeg, ".jpg"),
            OutputFormat::Png => (ImageFormat::Png, ".png"),
            OutputFormat::Webp => (ImageFormat::WebP, ".webp"),
            OutputFormat::Avif => (ImageFormat::Avif, ".avif"),
            // GIF has no static encoder in the selected output policy. PNG is
            // the lossless non-GIF representation for a resized Original GIF.
            OutputFormat::Original => (ImageFormat::Png, ".png"),
        };
        let img = decode_static_gif(data, &config.animated_webp)?;
        let processed = if config.convert_only {
            img
        } else {
            resize_image(img, config)?
        };
        let encoded = encode_image(processed, fmt, config.jpeg_quality)?;
        return Ok((encoded, ext));
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

    let img = decode_static_image(data, &lower, config)?;

    let processed = if config.convert_only {
        img // skip resize entirely
    } else {
        resize_image(img, config)?
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

fn decode_static_image(
    data: &[u8],
    entry_name: &str,
    config: &OptimizeConfig,
) -> Result<DynamicImage> {
    if entry_name.ends_with(".jpg") || entry_name.ends_with(".jpeg") {
        return decode_jpeg(data, config);
    }
    if entry_name.ends_with(".webp") {
        let mut image = webp::Decoder::new(data)
            .decode()
            .map(|image| image.to_image())
            .ok_or_else(|| anyhow::anyhow!("static WebP decode failed"))?;
        apply_input_orientation(&mut image, data);
        return Ok(image);
    }
    if entry_name.ends_with(".gif") {
        return decode_static_gif(data, &config.animated_webp);
    }
    let mut image = image::load_from_memory(data)?;
    if entry_name.ends_with(".tiff") || entry_name.ends_with(".tif") {
        apply_input_orientation(&mut image, data);
    }
    Ok(image)
}

/// Resize DynamicImage while preserving aspect ratio and its native pixel type.
fn resize_image(img: DynamicImage, config: &OptimizeConfig) -> Result<DynamicImage> {
    let (w, h) = (img.width(), img.height());
    let (max_width, max_height) = config.effective_dimensions();

    let Some((new_w, new_h)) = target_dimensions(w, h, max_width, max_height) else {
        return Ok(img);
    };

    let filter = match config.resize_filter {
        ResizeFilter::Bilinear => fast_image_resize::FilterType::Bilinear,
        ResizeFilter::CatmullRom => fast_image_resize::FilterType::CatmullRom,
        ResizeFilter::Lanczos3 => fast_image_resize::FilterType::Lanczos3,
    };
    let mut dst = blank_like(&img, new_w, new_h);
    let options = fast_image_resize::ResizeOptions::new()
        .resize_alg(fast_image_resize::ResizeAlg::Convolution(filter))
        // FIR uses MulDiv::multiply_alpha/divide_alpha for supported U8x4/U16x4
        // images, avoiding dark fringes around translucent raster content.
        .use_alpha(true);
    fast_image_resize::Resizer::new()
        .resize(&img, &mut dst, &options)
        .map_err(|error| anyhow::anyhow!("fast_image_resize: {error}"))?;
    Ok(dst)
}

fn target_dimensions(w: u32, h: u32, max_width: u32, max_height: u32) -> Option<(u32, u32)> {
    if w == 0 || h == 0 || (w <= max_width && h <= max_height) {
        return None;
    }
    let ratio = (max_width as f64 / w as f64).min(max_height as f64 / h as f64);
    Some((
        ((w as f64 * ratio).round() as u32).max(1),
        ((h as f64 * ratio).round() as u32).max(1),
    ))
}

fn blank_like(img: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    match img {
        DynamicImage::ImageLuma8(_) => {
            DynamicImage::ImageLuma8(image::GrayImage::new(width, height))
        }
        DynamicImage::ImageLumaA8(_) => {
            DynamicImage::ImageLumaA8(image::GrayAlphaImage::new(width, height))
        }
        DynamicImage::ImageRgb8(_) => DynamicImage::ImageRgb8(image::RgbImage::new(width, height)),
        DynamicImage::ImageRgba8(_) => {
            DynamicImage::ImageRgba8(image::RgbaImage::new(width, height))
        }
        DynamicImage::ImageLuma16(_) => {
            DynamicImage::ImageLuma16(image::ImageBuffer::new(width, height))
        }
        DynamicImage::ImageLumaA16(_) => {
            DynamicImage::ImageLumaA16(image::ImageBuffer::new(width, height))
        }
        DynamicImage::ImageRgb16(_) => {
            DynamicImage::ImageRgb16(image::ImageBuffer::new(width, height))
        }
        DynamicImage::ImageRgba16(_) => {
            DynamicImage::ImageRgba16(image::ImageBuffer::new(width, height))
        }
        DynamicImage::ImageRgb32F(_) => {
            DynamicImage::ImageRgb32F(image::ImageBuffer::new(width, height))
        }
        DynamicImage::ImageRgba32F(_) => {
            DynamicImage::ImageRgba32F(image::ImageBuffer::new(width, height))
        }
        _ => unreachable!("image crate DynamicImage variant is not FIR-compatible"),
    }
}

fn decode_jpeg(data: &[u8], config: &OptimizeConfig) -> Result<DynamicImage> {
    let orientation = input_orientation(data);
    let mut decompressor = match turbojpeg::Decompressor::new() {
        Ok(value) => value,
        Err(error) => {
            return jpeg_image_fallback(
                data,
                orientation,
                format!("TurboJPEG init failed: {error}"),
            )
        }
    };
    let header = match decompressor.read_header(data) {
        Ok(value) => value,
        Err(error) => {
            return jpeg_image_fallback(
                data,
                orientation,
                format!("TurboJPEG header failed: {error}"),
            )
        }
    };

    // TurboJPEG's CMYK/YCCK color conversion is intentionally not used here.
    // image's full decoder is the portable fallback for those uncommon JPEGs.
    if matches!(
        header.colorspace,
        turbojpeg::Colorspace::CMYK | turbojpeg::Colorspace::YCCK
    ) {
        return jpeg_image_fallback(data, orientation, "CMYK/YCCK JPEG".to_string());
    }

    let source = (
        u32::try_from(header.width).unwrap_or(u32::MAX),
        u32::try_from(header.height).unwrap_or(u32::MAX),
    );
    let logical_source = oriented_dimensions(source, orientation);
    let final_dimensions = target_dimensions(
        logical_source.0,
        logical_source.1,
        config.effective_dimensions().0,
        config.effective_dimensions().1,
    );
    let scale = match (config.convert_only, final_dimensions, header.is_lossless) {
        (false, Some(dimensions), false) => choose_jpeg_dct_scale(&header, dimensions, orientation),
        _ => turbojpeg::ScalingFactor::ONE,
    };

    match decode_jpeg_with_turbo(data, &mut decompressor, header, scale) {
        Ok(mut image) => {
            image.apply_orientation(orientation);
            Ok(image)
        }
        Err(error) => jpeg_image_fallback(
            data,
            orientation,
            format!("TurboJPEG decode failed: {error}"),
        ),
    }
}

fn jpeg_image_fallback(
    data: &[u8],
    orientation: Orientation,
    reason: String,
) -> Result<DynamicImage> {
    log::debug!("using image full-decode fallback for JPEG ({reason})");
    let mut image = image::load_from_memory(data)
        .map_err(|fallback| anyhow::anyhow!("{reason}; image fallback failed: {fallback}"))?;
    image.apply_orientation(orientation);
    Ok(image)
}

pub(crate) fn input_orientation(data: &[u8]) -> Orientation {
    let Ok(reader) = ImageReader::new(Cursor::new(data)).with_guessed_format() else {
        return Orientation::NoTransforms;
    };
    let Ok(mut decoder) = reader.into_decoder() else {
        return Orientation::NoTransforms;
    };
    if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP".as_slice()) {
        return decoder
            .exif_metadata()
            .ok()
            .flatten()
            .and_then(|exif| {
                Orientation::from_exif_chunk(
                    exif.strip_prefix(b"Exif\0\0").unwrap_or(exif.as_slice()),
                )
            })
            .unwrap_or(Orientation::NoTransforms);
    }
    decoder.orientation().unwrap_or(Orientation::NoTransforms)
}

fn apply_input_orientation(image: &mut DynamicImage, data: &[u8]) {
    image.apply_orientation(input_orientation(data));
}

pub(crate) fn oriented_dimensions(
    (width, height): (u32, u32),
    orientation: Orientation,
) -> (u32, u32) {
    if matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270
            | Orientation::Rotate270FlipH
    ) {
        (height, width)
    } else {
        (width, height)
    }
}

fn choose_jpeg_dct_scale(
    header: &turbojpeg::DecompressHeader,
    (target_width, target_height): (u32, u32),
    orientation: Orientation,
) -> turbojpeg::ScalingFactor {
    let (source_width, source_height) = if matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270
            | Orientation::Rotate270FlipH
    ) {
        (header.height, header.width)
    } else {
        (header.width, header.height)
    };
    let guarded_width = (u64::from(target_width) * 6).div_ceil(5);
    let guarded_height = (u64::from(target_height) * 6).div_ceil(5);
    let meets_guard = |scale: turbojpeg::ScalingFactor| {
        u64::try_from(scale.scale(source_width)).unwrap_or(u64::MAX) >= guarded_width
            && u64::try_from(scale.scale(source_height)).unwrap_or(u64::MAX) >= guarded_height
    };

    if meets_guard(turbojpeg::ScalingFactor::ONE_QUARTER) {
        turbojpeg::ScalingFactor::ONE_QUARTER
    } else if meets_guard(turbojpeg::ScalingFactor::ONE_HALF) {
        turbojpeg::ScalingFactor::ONE_HALF
    } else {
        turbojpeg::ScalingFactor::ONE
    }
}

fn decode_jpeg_with_turbo(
    data: &[u8],
    decompressor: &mut turbojpeg::Decompressor,
    header: turbojpeg::DecompressHeader,
    scale: turbojpeg::ScalingFactor,
) -> Result<DynamicImage> {
    let scaled = header.scaled(scale);
    let width = u32::try_from(scaled.width)?;
    let height = u32::try_from(scaled.height)?;
    let pixel_len = scaled
        .width
        .checked_mul(scaled.height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| anyhow::anyhow!("JPEG pixel buffer size overflow"))?;
    let mut pixels = vec![0u8; pixel_len];
    decompressor.set_scaling_factor(scale)?;
    let image = turbojpeg::Image {
        pixels: pixels.as_mut_slice(),
        width: scaled.width,
        pitch: scaled.width * 3,
        height: scaled.height,
        format: turbojpeg::PixelFormat::RGB,
    };
    decompressor.decompress(data, image)?;
    let image = image::RgbImage::from_raw(width, height, pixels)
        .ok_or_else(|| anyhow::anyhow!("invalid JPEG RGB buffer"))?;
    Ok(DynamicImage::ImageRgb8(image))
}

/// Cheap animated-WebP classification used for routing. Full validation and
/// resource limits are applied by the dedicated animation path.
pub fn is_animated_webp(data: &[u8]) -> bool {
    webp_anim::is_animated_webp_fast(data)
}

/// Encode image to bytes
fn encode_image(img: DynamicImage, fmt: ImageFormat, jpeg_quality: u8) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match fmt {
        ImageFormat::Jpeg => {
            buf = encode_jpeg_turbo(&img, jpeg_quality)?;
        }
        ImageFormat::WebP => {
            buf = encode_webp_lossless(&img)?;
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

fn encode_jpeg_turbo(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let rgb = img.to_rgb8();
    let (width, height) = rgb.dimensions();
    let mut compressor = turbojpeg::Compressor::new()?;
    compressor.set_quality(i32::from(quality.clamp(1, 100)))?;
    // Match image::codecs::jpeg::JpegEncoder's existing 4:2:2 output
    // characteristic instead of changing chroma detail/size as a side effect
    // of the backend switch.
    compressor.set_subsamp(turbojpeg::Subsamp::Sub2x1)?;
    let image = turbojpeg::Image {
        pixels: rgb.as_raw().as_slice(),
        width: usize::try_from(width)?,
        pitch: usize::try_from(width)? * 3,
        height: usize::try_from(height)?,
        format: turbojpeg::PixelFormat::RGB,
    };
    Ok(compressor.compress_to_vec(image)?)
}

fn encode_webp_lossless(img: &DynamicImage) -> Result<Vec<u8>> {
    let encoded = match img {
        DynamicImage::ImageRgb8(image) => {
            webp::Encoder::from_rgb(image.as_raw(), image.width(), image.height()).encode_lossless()
        }
        DynamicImage::ImageRgba8(image) => {
            webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height())
                .encode_lossless()
        }
        _ if img.color().has_alpha() => {
            let image = img.to_rgba8();
            webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height())
                .encode_lossless()
        }
        _ => {
            let image = img.to_rgb8();
            webp::Encoder::from_rgb(image.as_raw(), image.width(), image.height()).encode_lossless()
        }
    };
    Ok(encoded.to_vec())
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
        return encode_avif_fallback(img);
    }

    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let (y, u, v) = rgba_to_yuv420_bt709(&rgba);
    let color = match encode_svt_i420(width, height, &y, &u, &v, QUALITY, SPEED) {
        Ok(color) => color,
        // Some SVT builds also reject small-but-formally-valid dimensions while
        // allocating their internal resources. The binding does not expose the
        // native error code, so its Display form is the available way to
        // distinguish this recoverable initialization failure.
        Err(error) if is_svt_initialization_resource_error(&error) => {
            log::warn!(
                "SVT-AV1 could not allocate encoder resources for {}x{}; using the fallback AVIF encoder",
                img.width(),
                img.height(),
            );
            return encode_avif_fallback(img);
        }
        Err(error) => return Err(error),
    };
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

fn encode_avif_fallback(img: DynamicImage) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut output), ImageFormat::Avif)?;
    Ok(output)
}

fn is_svt_initialization_resource_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<shiguredo_svt_av1::Error>()
        .is_some_and(|error| is_svt_initialization_resource_message(&error.to_string()))
}

fn is_svt_initialization_resource_message(message: &str) -> bool {
    message == "svt_av1_enc_init() failed: code=-2147479552"
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
