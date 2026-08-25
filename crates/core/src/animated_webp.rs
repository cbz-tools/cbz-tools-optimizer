//! Dedicated animated-WebP optimization path.
//!
//! Static images continue through `resize`; this module preserves animation
//! timing and ANIM metadata while decoding, resizing, and encoding stored
//! animation sequences.

use std::{io::Cursor, num::NonZeroU16, time::Duration};

use anyhow::{bail, ensure, Result};
use image::{DynamicImage, RgbaImage};
use webp_anim::{
    inspect, transcode_animated_webp, AnimationDecoder, AnimationEncoder, AnimationEncoderOptions,
    AnimationInfo, AnimationTranscodeOptions, BackgroundColor, CanvasSize, DecodeLimits,
    InspectLimits, LoopCount, ResizeOptions, ResizePlan, WebpKind,
};
use zengif::{Decoder as GifDecoder, Limits as GifLimits, Repeat as GifRepeat, Unstoppable};

use crate::{
    AnimatedWebpEncoding, AnimatedWebpKeyframePolicy, AnimatedWebpOptions, AnimatedWebpOutputPolicy,
};

const DISABLED_KEYFRAME_KMIN: i32 = i32::MAX - 1;
const DISABLED_KEYFRAME_KMAX: i32 = i32::MAX;

/// Successful result of an animated-WebP optimization attempt.
#[derive(Debug)]
pub enum AnimatedWebpOutcome {
    /// The newly encoded WebP should replace the original entry.
    Optimized {
        bytes: Vec<u8>,
        report: AnimatedWebpReport,
    },
    /// Retaining the original entry was the successful output decision.
    KeptOriginal {
        reason: AnimatedWebpKeepReason,
        report: AnimatedWebpReport,
    },
}

/// Reason an animated-WebP entry was retained without treating it as an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimatedWebpKeepReason {
    OutputLarger,
    ResizeNotRequired,
}

/// Machine-readable details of one animated-WebP optimization attempt.
#[derive(Debug, Clone)]
pub struct AnimatedWebpReport {
    pub input_bytes: usize,
    pub encoded_bytes: usize,
    pub saved_bytes: i64,
    pub saved_percent: f64,
    pub input_canvas: CanvasSize,
    pub output_canvas: CanvasSize,
    pub frame_count: u32,
    pub total_duration: Duration,
    pub loop_count: webp_anim::LoopCount,
    pub resized: bool,
}

/// Optimize one animated WebP while preserving its duration, loop count, and
/// ANIM background color. The output always remains an animated WebP.
pub fn optimize_animated_webp(
    input: &[u8],
    options: &AnimatedWebpOptions,
    max_width: u32,
    max_height: u32,
) -> Result<AnimatedWebpOutcome> {
    validate_options(options, max_width, max_height)?;

    let inspect_limits = InspectLimits {
        max_input_bytes: options.max_input_bytes,
        max_canvas_pixels: options.max_canvas_pixels,
        max_frame_count: options.max_frame_count,
        max_frame_rgba_bytes: options.max_frame_rgba_bytes,
    };
    let info = match inspect(input, inspect_limits)? {
        WebpKind::Animated(info) => info,
        WebpKind::Static(_) => bail!("input is not an animated WebP"),
    };

    let resize_options = ResizeOptions {
        maximum: CanvasSize {
            width: max_width,
            height: max_height,
        },
        allow_upscale: false,
        filter: options.resize_filter.into(),
        max_output_rgba_bytes: options.max_output_rgba_bytes,
    };
    let resize = ResizePlan::new(info.canvas, resize_options)?;

    // Do not re-encode an already in-bounds animation: it would add a lossy
    // generation without applying the requested geometric resize.
    if resize.is_noop() {
        let mut decoder = AnimationDecoder::new(input, decode_limits(options))?;
        let mut total_duration = Duration::ZERO;
        let mut decoded_frames = 0_u32;
        while let Some(frame) = decoder.next_frame()? {
            total_duration = total_duration
                .checked_add(frame.duration)
                .ok_or_else(|| anyhow::anyhow!("animation duration overflow"))?;
            decoded_frames = decoded_frames
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("animation frame count overflow"))?;
        }
        ensure!(
            decoded_frames == info.frame_count,
            "decoder produced {decoded_frames} frames; expected {}",
            info.frame_count
        );
        return Ok(AnimatedWebpOutcome::KeptOriginal {
            reason: AnimatedWebpKeepReason::ResizeNotRequired,
            report: report(
                input.len(),
                input.len(),
                info,
                resize.destination(),
                total_duration,
            ),
        });
    }

    let encoder_options = encoder_options(info, options);
    let transcoded = transcode_animated_webp(
        input,
        AnimationTranscodeOptions {
            decode_limits: decode_limits(options),
            resize: resize_options,
            encoder_config: encoder_options.config,
            animation: encoder_options.animation,
        },
    )?;
    let report = report(
        input.len(),
        transcoded.bytes.len(),
        transcoded.input,
        transcoded.output_canvas,
        transcoded.total_duration,
    );
    if should_keep_original(options.output_policy, input.len(), transcoded.bytes.len()) {
        return Ok(AnimatedWebpOutcome::KeptOriginal {
            reason: AnimatedWebpKeepReason::OutputLarger,
            report,
        });
    }

    Ok(AnimatedWebpOutcome::Optimized {
        bytes: transcoded.bytes,
        report,
    })
}

/// Decode one static GIF through zengif into the existing static image pipeline.
pub(crate) fn decode_static_gif(
    input: &[u8],
    options: &AnimatedWebpOptions,
) -> Result<DynamicImage> {
    let probe = zengif::detect::probe(input)?;
    ensure!(
        !probe.is_animated,
        "GIF has {} frames; expected a static GIF",
        probe.frame_count
    );
    ensure!(probe.frame_count == 1, "GIF contains no decodable frame");
    validate_gif_input(input, probe.width, probe.height, options)?;

    let mut decoder = GifDecoder::new(Cursor::new(input), gif_limits(options), &Unstoppable)?;
    let frame = decoder
        .next_frame()?
        .ok_or_else(|| anyhow::anyhow!("static GIF contains no frame"))?;
    let pixels = frame.as_bytes().to_vec();
    let image = RgbaImage::from_raw(u32::from(probe.width), u32::from(probe.height), pixels)
        .ok_or_else(|| anyhow::anyhow!("invalid static GIF RGBA buffer"))?;
    Ok(DynamicImage::ImageRgba8(image))
}

/// Encode a GIF with two or more frames through the existing animated-WebP path.
/// Frames are decoded, optionally resized, and handed to the encoder one at a time.
pub(crate) fn optimize_animated_gif(
    input: &[u8],
    options: &AnimatedWebpOptions,
    max_width: u32,
    max_height: u32,
    convert_only: bool,
) -> Result<AnimatedWebpOutcome> {
    validate_options(options, max_width, max_height)?;
    let probe = zengif::detect::probe(input)?;
    ensure!(
        probe.is_animated && probe.frame_count >= 2,
        "input is not an animated GIF"
    );
    validate_gif_input(input, probe.width, probe.height, options)?;

    let input_canvas = CanvasSize {
        width: u32::from(probe.width),
        height: u32::from(probe.height),
    };
    let maximum = if convert_only {
        input_canvas
    } else {
        CanvasSize {
            width: max_width,
            height: max_height,
        }
    };
    let resize_options = ResizeOptions {
        maximum,
        allow_upscale: false,
        filter: options.resize_filter.into(),
        max_output_rgba_bytes: options.max_output_rgba_bytes,
    };
    let resize = ResizePlan::new(input_canvas, resize_options)?;
    let mut workspace = resize.workspace()?;
    let mut decoder = GifDecoder::new(Cursor::new(input), gif_limits(options), &Unstoppable)?;

    let first = decoder
        .next_frame()?
        .ok_or_else(|| anyhow::anyhow!("animated GIF contains no frame"))?;
    let repeat = if probe.repeat.is_none() {
        GifRepeat::Once
    } else {
        decoder.repeat()
    };
    let loop_count = gif_loop_count(repeat)?;
    let background = decoder.metadata().background_color();
    let info = AnimationInfo {
        canvas: input_canvas,
        frame_count: probe.frame_count,
        loop_count,
        background_color: BackgroundColor {
            // WebP's raw ANIM color is stored as 0xAABBGGRR; retain the GIF
            // background components without imposing a new public color type.
            raw: u32::from_le_bytes([background.r, background.g, background.b, background.a]),
        },
    };
    let mut encoder = AnimationEncoder::new(resize.destination(), encoder_options(info, options))?;
    let mut total_duration = Duration::ZERO;
    let mut decoded_frames = 0_u32;
    let mut frame = Some(first);

    while let Some(current) = frame.take() {
        let delay_ms = u64::from(current.delay)
            .checked_mul(10)
            .ok_or_else(|| anyhow::anyhow!("GIF frame delay overflow"))?;
        let duration = Duration::from_millis(delay_ms);
        total_duration = total_duration
            .checked_add(duration)
            .ok_or_else(|| anyhow::anyhow!("GIF animation duration overflow"))?;

        // zengif owns the composited frame. Copy its packed RGBA view into the
        // one mutable source buffer required by webp-anim's reusable workspace,
        // then release the zengif frame before decoding the next one.
        let mut source_rgba = current.as_bytes().to_vec();
        drop(current);
        workspace.transform_rgba(&mut source_rgba)?;
        encoder.add_rgba(workspace.pixels(), duration)?;
        drop(source_rgba);
        decoded_frames = decoded_frames
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("GIF frame count overflow"))?;

        if decoded_frames < probe.frame_count {
            frame = decoder.next_frame()?;
        }
    }

    ensure!(
        decoded_frames == probe.frame_count,
        "GIF decoder produced {decoded_frames} frames; expected {}",
        probe.frame_count
    );
    let encoded = encoder.finish()?;
    let report = report(
        input.len(),
        encoded.len(),
        info,
        resize.destination(),
        total_duration,
    );
    if should_keep_original(options.output_policy, input.len(), encoded.len()) {
        return Ok(AnimatedWebpOutcome::KeptOriginal {
            reason: AnimatedWebpKeepReason::OutputLarger,
            report,
        });
    }

    Ok(AnimatedWebpOutcome::Optimized {
        bytes: encoded,
        report,
    })
}

fn validate_options(options: &AnimatedWebpOptions, max_width: u32, max_height: u32) -> Result<()> {
    ensure!(
        max_width > 0 && max_height > 0,
        "animated WebP dimensions must be non-zero"
    );
    ensure!(
        options.max_input_bytes > 0,
        "max_input_bytes must be non-zero"
    );
    ensure!(
        options.max_canvas_pixels > 0,
        "max_canvas_pixels must be non-zero"
    );
    ensure!(
        options.max_frame_count > 0,
        "max_frame_count must be non-zero"
    );
    ensure!(
        options.max_total_duration_ms > 0,
        "max_total_duration_ms must be non-zero"
    );
    ensure!(
        options.max_frame_rgba_bytes > 0 && options.max_output_rgba_bytes > 0,
        "RGBA byte limits must be non-zero"
    );
    ensure!(
        options.preprocessing <= 2,
        "animated WebP preprocessing must be between 0 and 2"
    );
    ensure!(
        (0..=100).contains(&options.filter_strength),
        "animated WebP filter strength must be between 0 and 100"
    );
    ensure!(
        (0..=7).contains(&options.filter_sharpness),
        "animated WebP filter sharpness must be between 0 and 7"
    );
    ensure!(
        (0..=1).contains(&options.filter_type),
        "animated WebP filter type must be 0 or 1"
    );
    if matches!(options.keyframe_policy, AnimatedWebpKeyframePolicy::Bounded) {
        ensure!(
            options.kmin >= 0
                && options.kmax >= 2
                && options.kmin < options.kmax
                && options.kmin > options.kmax / 2,
            "animated WebP keyframe intervals must satisfy kmax >= 2, 0 <= kmin < kmax, and kmin > kmax / 2"
        );
    }
    match options.encoding {
        AnimatedWebpEncoding::Lossy { quality, method } => {
            ensure!(
                quality.is_finite() && (0.0..=100.0).contains(&quality),
                "animated WebP quality must be between 0 and 100"
            );
            ensure!(method <= 6, "animated WebP method must be between 0 and 6");
        }
        AnimatedWebpEncoding::Lossless { method } => {
            ensure!(method <= 6, "animated WebP method must be between 0 and 6");
        }
    }
    ensure!(
        options.alpha_quality <= 100,
        "animated WebP alpha quality must be between 0 and 100"
    );
    Ok(())
}

fn decode_limits(options: &AnimatedWebpOptions) -> DecodeLimits {
    DecodeLimits {
        max_input_bytes: options.max_input_bytes,
        max_canvas_pixels: options.max_canvas_pixels,
        max_frame_count: options.max_frame_count,
        max_total_duration: Duration::from_millis(options.max_total_duration_ms),
        max_frame_rgba_bytes: options.max_frame_rgba_bytes,
    }
}

fn gif_limits(options: &AnimatedWebpOptions) -> GifLimits {
    let max_frame_rgba_bytes = u64::try_from(options.max_frame_rgba_bytes).unwrap_or(u64::MAX);
    // zengif may retain the compositing canvas, one composed frame, and a
    // disposal-previous snapshot at the same time. Keep that internal budget
    // finite and derived from the existing animated-WebP frame limit.
    let max_memory = max_frame_rgba_bytes
        .saturating_mul(4)
        .saturating_add(64 * 1024);
    GifLimits::none()
        .max_file_size(u64::try_from(options.max_input_bytes).unwrap_or(u64::MAX))
        .max_total_pixels(options.max_canvas_pixels)
        .max_frame_count(u64::from(options.max_frame_count))
        .max_animation_ms(options.max_total_duration_ms)
        .max_memory(max_memory)
        .max_decompression_ratio(gif_decompression_ratio(options))
}

fn gif_decompression_ratio(options: &AnimatedWebpOptions) -> f64 {
    // zengif measures decompressed indexed pixels, while the product limit is
    // expressed as RGBA bytes. Derive a finite ratio from the existing limits
    // so custom resource budgets remain the source of truth.
    let max_indexed_bytes = u64::try_from(options.max_frame_rgba_bytes)
        .unwrap_or(u64::MAX)
        .saturating_div(4)
        .saturating_mul(u64::from(options.max_frame_count));
    let max_input_bytes = u64::try_from(options.max_input_bytes)
        .unwrap_or(u64::MAX)
        .max(1);
    (max_indexed_bytes as f64 / max_input_bytes as f64).max(1.0)
}

fn validate_gif_input(
    input: &[u8],
    width: u16,
    height: u16,
    options: &AnimatedWebpOptions,
) -> Result<()> {
    ensure!(
        input.len() <= options.max_input_bytes,
        "GIF input exceeds the configured byte limit"
    );
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| anyhow::anyhow!("GIF canvas pixel count overflow"))?;
    ensure!(
        pixels <= options.max_canvas_pixels,
        "GIF canvas exceeds the configured pixel limit"
    );
    let rgba_bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("GIF RGBA buffer size overflow"))?;
    ensure!(
        rgba_bytes <= u64::try_from(options.max_frame_rgba_bytes).unwrap_or(u64::MAX),
        "GIF RGBA frame exceeds the configured byte limit"
    );
    Ok(())
}

fn gif_loop_count(repeat: GifRepeat) -> Result<LoopCount> {
    match repeat {
        GifRepeat::Infinite => Ok(LoopCount::Infinite),
        GifRepeat::Once => Ok(LoopCount::Finite(
            NonZeroU16::new(1).expect("one is non-zero"),
        )),
        GifRepeat::Count(count) => {
            let total_iterations = count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("GIF loop count overflow"))?;
            Ok(LoopCount::Finite(
                NonZeroU16::new(total_iterations).expect("GIF loop count is non-zero"),
            ))
        }
        _ => bail!("unsupported GIF repeat mode"),
    }
}

fn encoder_options(info: AnimationInfo, options: &AnimatedWebpOptions) -> AnimationEncoderOptions {
    let mut result = AnimationEncoderOptions::from_animation_info(info);
    match options.encoding {
        AnimatedWebpEncoding::Lossy { quality, method } => {
            result.config.quality = Some(quality);
            result.config.lossless = Some(false);
            result.config.method = Some(method);
        }
        AnimatedWebpEncoding::Lossless { method } => {
            result.config.lossless = Some(true);
            result.config.method = Some(method);
        }
    }
    result.config.use_sharp_yuv = Some(options.use_sharp_yuv);
    result.config.autofilter = Some(options.autofilter);
    result.config.filter_strength = Some(options.filter_strength);
    result.config.filter_sharpness = Some(options.filter_sharpness);
    result.config.filter_type = Some(options.filter_type);
    result.config.alpha_quality = Some(options.alpha_quality);
    result.config.preprocessing = Some(options.preprocessing);
    result.config.thread_level = Some(options.thread_level);
    result.animation.allow_mixed = Some(options.allow_mixed);
    let (kmin, kmax) = effective_keyframe_intervals(options);
    result.animation.kmin = Some(kmin);
    result.animation.kmax = Some(kmax);
    result
}

fn effective_keyframe_intervals(options: &AnimatedWebpOptions) -> (i32, i32) {
    match options.keyframe_policy {
        AnimatedWebpKeyframePolicy::Bounded => (options.kmin, options.kmax),
        AnimatedWebpKeyframePolicy::Disabled => (DISABLED_KEYFRAME_KMIN, DISABLED_KEYFRAME_KMAX),
    }
}

fn report(
    input_bytes: usize,
    encoded_bytes: usize,
    info: AnimationInfo,
    output_canvas: CanvasSize,
    total_duration: Duration,
) -> AnimatedWebpReport {
    let saved_bytes = i64::try_from(input_bytes)
        .unwrap_or(i64::MAX)
        .saturating_sub(i64::try_from(encoded_bytes).unwrap_or(i64::MAX));
    let saved_percent = if input_bytes == 0 {
        0.0
    } else {
        saved_bytes as f64 * 100.0 / input_bytes as f64
    };
    AnimatedWebpReport {
        input_bytes,
        encoded_bytes,
        saved_bytes,
        saved_percent,
        input_canvas: info.canvas,
        output_canvas,
        frame_count: info.frame_count,
        total_duration,
        loop_count: info.loop_count,
        resized: info.canvas != output_canvas,
    }
}

fn should_keep_original(
    policy: AnimatedWebpOutputPolicy,
    input_bytes: usize,
    encoded_bytes: usize,
) -> bool {
    matches!(policy, AnimatedWebpOutputPolicy::KeepOriginalIfLarger) && encoded_bytes > input_bytes
}
