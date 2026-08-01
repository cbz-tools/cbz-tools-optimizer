//! Dedicated animated-WebP optimization path.
//!
//! Static images continue through `resize`; this module preserves animation
//! timing and ANIM metadata while decoding, resizing, and encoding stored
//! animation sequences.

use std::time::Duration;

use anyhow::{bail, ensure, Result};
use webp_anim::{
    inspect, transcode_animated_webp, AnimationDecoder, AnimationEncoderOptions, AnimationInfo,
    AnimationTranscodeOptions, CanvasSize, DecodeLimits, InspectLimits, ResizeOptions, ResizePlan,
    WebpKind,
};

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
                && options.kmin >= options.kmax / 2 + 1,
            "animated WebP keyframe intervals must satisfy kmax >= 2, 0 <= kmin < kmax, and kmin >= kmax / 2 + 1"
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
