//! Shared, bounded decoding for long videos.
//!
//! Each segment performs the existing accurate timestamp seeks in order. A
//! process-wide pool lets a long video use idle decode capacity without
//! multiplying concurrent FFmpeg processes beyond a fixed bound.

use std::{path::Path, sync::OnceLock};

use rayon::prelude::*;
use skwad_media_core::Ffmpeg;

use crate::{sample_frame, FramePlan, PlannedFrame, Result, SampledFrame, VideoAnalysisConfig};

const LONG_VIDEO_SECONDS: f64 = 60.0;
const MAX_SEGMENTS_PER_VIDEO: usize = 4;
const MAX_SHARED_DECODERS: usize = 10;

static DECODER_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

#[derive(Debug)]
pub struct DecodedPlan {
    pub frames: Vec<(f64, Result<SampledFrame>)>,
    pub segments: usize,
}

/// Uses parallel targeted-seek segments only when a video is long enough to
/// benefit. Results always retain plan order so tracking and database writes
/// remain deterministic.
pub fn decode_plan(
    ffmpeg: &Ffmpeg,
    path: &Path,
    plan: &FramePlan,
    duration: Option<f64>,
    orientation: u16,
    config: &VideoAnalysisConfig,
) -> DecodedPlan {
    let segments = segment_count(duration, plan.timestamps.len());
    if segments <= 1 {
        return DecodedPlan {
            frames: plan
                .timestamps
                .iter()
                .map(|entry| (entry.at, sample_frame(ffmpeg, path, entry, orientation, config)))
                .collect(),
            segments: 1,
        };
    }

    let chunk_size = plan.timestamps.len().div_ceil(segments);
    let chunks: Vec<&[PlannedFrame]> = plan.timestamps.chunks(chunk_size).collect();
    let pool = DECODER_POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(MAX_SHARED_DECODERS)
            .thread_name(|index| format!("skwad-video-decoder-{index}"))
            .build()
            .expect("failed to create bounded video decoder pool")
    });
    let batches: Vec<Vec<(f64, Result<SampledFrame>)>> = pool.install(|| {
        chunks
            .par_iter()
            .map(|entries| decode_segment(ffmpeg, path, entries, orientation, config))
            .collect()
    });

    DecodedPlan {
        frames: batches.into_iter().flatten().collect(),
        segments: chunks.len(),
    }
}

fn decode_segment(
    ffmpeg: &Ffmpeg,
    path: &Path,
    entries: &[PlannedFrame],
    orientation: u16,
    config: &VideoAnalysisConfig,
) -> Vec<(f64, Result<SampledFrame>)> {
    entries
        .iter()
        .map(|entry| (entry.at, sample_frame(ffmpeg, path, entry, orientation, config)))
        .collect()
}

fn segment_count(duration: Option<f64>, frames: usize) -> usize {
    let duration = duration.unwrap_or(0.0);
    if duration <= LONG_VIDEO_SECONDS || frames < 2 {
        return 1;
    }
    ((duration / LONG_VIDEO_SECONDS).ceil() as usize)
        .clamp(2, MAX_SEGMENTS_PER_VIDEO)
        .min(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_videos_keep_one_decoder() {
        assert_eq!(segment_count(Some(60.0), 12), 1);
        assert_eq!(segment_count(Some(12.0), 3), 1);
    }

    #[test]
    fn long_videos_scale_to_four_bounded_segments() {
        assert_eq!(segment_count(Some(61.0), 13), 2);
        assert_eq!(segment_count(Some(121.0), 25), 3);
        assert_eq!(segment_count(Some(241.0), 49), 4);
        assert_eq!(segment_count(Some(3600.0), 60), 4);
    }
}
