//! Video face recognition (§9).
//!
//! Running the detector on every frame of a 4K clip would cost more than the
//! entire photo shoot around it and tell us almost nothing new — consecutive
//! frames are the same picture. Instead: find where the picture actually
//! changes, sample on a fixed cadence between those points, and analyse that
//! handful of frames.

pub mod sampling;
pub mod tracking;

use std::path::Path;

use image::RgbImage;
use serde::{Deserialize, Serialize};

use teo_media_core::{Ffmpeg, MediaError};

pub use sampling::{plan_frames, FramePlan, PlannedFrame};

/// A full scene-score pass over high-bitrate 4K originals is disproportionately
/// expensive. At and below 1440p it remains useful; above this pixel count the
/// fixed-interval samples provide predictable coverage without reading the
/// entire source once before sampling begins.
const MAX_SCENE_SCAN_PIXELS: u64 = 2560 * 1440;

#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("FFmpeg is required for video analysis")]
    MissingFfmpeg,
    #[error(transparent)]
    Media(#[from] MediaError),
}

pub type Result<T> = std::result::Result<T, VideoError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoAnalysisConfig {
    /// How much the picture must change to count as a cut. FFmpeg's scene
    /// score runs 0–1; 0.3 catches hard cuts without firing on camera moves.
    pub scene_threshold: f64,
    /// Fallback cadence, in seconds, for stretches with no cuts — a long
    /// single-take interview needs sampling too.
    pub sample_interval: f64,
    /// Hard cap per video, so one 90-minute recording cannot monopolise the
    /// worker pool.
    pub max_frames: usize,
    /// Frame rate the scene detector decodes at. Lower is cheaper (§19).
    pub probe_fps: f64,
    /// Frames are downscaled to this before detection.
    pub frame_max_dim: u32,
    /// Cuts closer together than this collapse into one sample.
    pub min_frame_gap: f64,
}

impl Default for VideoAnalysisConfig {
    fn default() -> Self {
        Self {
            scene_threshold: 0.3,
            sample_interval: 5.0,
            max_frames: 60,
            probe_fps: 4.0,
            frame_max_dim: 1280,
            min_frame_gap: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SampledFrame {
    pub timestamp: f64,
    pub image: RgbImage,
    /// True when this frame came from a detected scene change rather than the
    /// fixed cadence.
    pub from_scene_change: bool,
}

/// Decides which timestamps are worth looking at, without decoding them yet.
pub fn plan_video(
    ffmpeg: &Ffmpeg,
    path: &Path,
    duration: Option<f64>,
    dimensions: Option<(u32, u32)>,
    config: &VideoAnalysisConfig,
) -> FramePlan {
    let scan_scenes = dimensions.is_none_or(|(width, height)| scene_detection_allowed(width, height));
    let scene_times = if !scan_scenes {
        tracing::info!(video = %path.display(), ?dimensions, "skipping full scene scan for high-resolution video");
        Vec::new()
    } else {
        match ffmpeg.scene_changes(path, config.scene_threshold, config.probe_fps) {
            Ok(times) => times,
            Err(e) => {
                // Scene detection is an optimisation, not a requirement: fall back
                // to plain interval sampling rather than failing the whole video.
                tracing::warn!(video = %path.display(), error = %e, "scene detection failed; sampling on interval only");
                Vec::new()
            }
        }
    };
    plan_frames(duration, &scene_times, config)
}

fn scene_detection_allowed(width: u32, height: u32) -> bool {
    u64::from(width).saturating_mul(u64::from(height)) <= MAX_SCENE_SCAN_PIXELS
}

/// Decodes one planned frame. Keeping this operation singular lets callers run
/// inference and release its RGB buffer before the next 4K source frame is
/// requested.
pub fn sample_frame(
    ffmpeg: &Ffmpeg,
    path: &Path,
    entry: &PlannedFrame,
    orientation: u16,
    config: &VideoAnalysisConfig,
) -> Result<SampledFrame> {
    let image =
        teo_media_core::decode::load_video_frame(path, entry.at, orientation, Some(config.frame_max_dim), ffmpeg)?;
    Ok(SampledFrame {
        timestamp: entry.at,
        image,
        from_scene_change: entry.from_scene_change,
    })
}

/// Decodes the planned frames. Returns whatever succeeded — a single
/// unreadable timestamp should not abandon the rest of the video.
pub fn sample_frames(
    ffmpeg: &Ffmpeg,
    path: &Path,
    plan: &FramePlan,
    orientation: u16,
    config: &VideoAnalysisConfig,
) -> Vec<SampledFrame> {
    let mut out = Vec::with_capacity(plan.timestamps.len());
    for entry in &plan.timestamps {
        match sample_frame(ffmpeg, path, entry, orientation, config) {
            Ok(frame) => out.push(frame),
            Err(e) => tracing::debug!(video = %path.display(), at = entry.at, error = %e, "frame decode failed"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_scan_policy_keeps_normal_video_and_skips_4k() {
        assert!(scene_detection_allowed(1920, 1080));
        assert!(scene_detection_allowed(2560, 1440));
        assert!(!scene_detection_allowed(3840, 2160));
        assert!(!scene_detection_allowed(2160, 3840));
    }
}
