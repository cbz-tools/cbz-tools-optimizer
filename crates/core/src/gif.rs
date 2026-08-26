//! GIF decoding and conversion through the shared animated-WebP pipeline.
//!
//! GIF frames remain zengif-streamed and use the reusable WebP resize workspace.

use std::{io::Cursor, num::NonZeroU16, time::Duration};

use anyhow::{bail, ensure, Result};
use image::{DynamicImage, RgbaImage};
use webp_anim::{
    AnimationEncoder, AnimationInfo, BackgroundColor, CanvasSize, LoopCount, ResizeOptions,
    ResizePlan,
};
use zengif::{Decoder as GifDecoder, Limits as GifLimits, Repeat as GifRepeat, Unstoppable};

use crate::animated_webp::{self, AnimatedWebpKeepReason, AnimatedWebpOutcome};
use crate::AnimatedWebpOptions;

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
    animated_webp::validate_options(options, max_width, max_height)?;
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
    let mut encoder = AnimationEncoder::new(
        resize.destination(),
        animated_webp::encoder_options(info, options),
    )?;
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
    let report = animated_webp::report(
        input.len(),
        encoded.len(),
        info,
        resize.destination(),
        total_duration,
    );
    if animated_webp::should_keep_original(options.output_policy, input.len(), encoded.len()) {
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
