//! Choosing which frames of a video to analyse.
//!
//! Pure arithmetic, kept separate from FFmpeg so the sampling policy can be
//! tested without decoding anything.

use serde::{Deserialize, Serialize};

use crate::VideoAnalysisConfig;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedFrame {
    pub at: f64,
    pub from_scene_change: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FramePlan {
    pub timestamps: Vec<PlannedFrame>,
    /// Scene changes found before thinning and capping, for the log.
    pub scene_changes_found: usize,
    /// True when `max_frames` forced frames to be dropped.
    pub truncated: bool,
}

impl FramePlan {
    pub fn len(&self) -> usize {
        self.timestamps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.timestamps.is_empty()
    }
}

/// Combines detected cuts with fixed-interval sampling, thins out timestamps
/// that sit on top of each other, and caps the total.
///
/// Scene changes are preferred over interval samples when the cap bites: a cut
/// is where new people appear, whereas an interval sample halfway through a
/// static shot usually shows the same faces again.
pub fn plan_frames(duration: Option<f64>, scene_changes: &[f64], config: &VideoAnalysisConfig) -> FramePlan {
    let duration = duration.unwrap_or(0.0);
    if duration <= 0.0 {
        // An unknown duration still deserves one look at the opening frame.
        return FramePlan {
            timestamps: vec![PlannedFrame { at: 0.0, from_scene_change: false }],
            scene_changes_found: scene_changes.len(),
            truncated: false,
        };
    }

    let mut candidates: Vec<PlannedFrame> = Vec::new();

    // Always look at the start; a talking-head clip may have no cuts at all.
    candidates.push(PlannedFrame { at: 0.0, from_scene_change: false });

    for &at in scene_changes {
        if at > 0.0 && at < duration {
            // Land just after the cut, not on it — the frame on the boundary is
            // often a dissolve or a motion-blurred transition.
            candidates.push(PlannedFrame { at: (at + 0.2).min(duration - 0.05), from_scene_change: true });
        }
    }

    if config.sample_interval > 0.0 {
        let mut at = config.sample_interval;
        while at < duration {
            candidates.push(PlannedFrame { at, from_scene_change: false });
            at += config.sample_interval;
        }
    }

    candidates.sort_by(|a, b| a.at.total_cmp(&b.at));

    // Thin: keep the first of any run closer together than `min_frame_gap`,
    // preferring a scene change if one is in the run.
    let gap = config.min_frame_gap.max(0.0);
    let mut thinned: Vec<PlannedFrame> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        match thinned.last_mut() {
            Some(last) if (candidate.at - last.at).abs() < gap => {
                if candidate.from_scene_change && !last.from_scene_change {
                    *last = candidate;
                }
            }
            _ => thinned.push(candidate),
        }
    }

    let truncated = thinned.len() > config.max_frames;
    if truncated {
        thinned = cap_frames(thinned, config.max_frames);
    }

    FramePlan {
        timestamps: thinned,
        scene_changes_found: scene_changes.len(),
        truncated,
    }
}

/// Reduces the plan to `max` frames, keeping scene changes ahead of interval
/// samples and then spreading whatever is kept evenly across the video.
fn cap_frames(frames: Vec<PlannedFrame>, max: usize) -> Vec<PlannedFrame> {
    if max == 0 {
        return Vec::new();
    }

    let (scenes, intervals): (Vec<PlannedFrame>, Vec<PlannedFrame>) =
        frames.into_iter().partition(|f| f.from_scene_change);

    let mut kept: Vec<PlannedFrame> = if scenes.len() >= max {
        evenly_spaced(scenes, max)
    } else {
        let remaining = max - scenes.len();
        let mut out = scenes;
        out.extend(evenly_spaced(intervals, remaining));
        out
    };

    kept.sort_by(|a, b| a.at.total_cmp(&b.at));
    kept
}

/// Picks `count` items spread across `items`, always including the first.
fn evenly_spaced(items: Vec<PlannedFrame>, count: usize) -> Vec<PlannedFrame> {
    if count == 0 || items.is_empty() {
        return Vec::new();
    }
    if items.len() <= count {
        return items;
    }
    let step = items.len() as f64 / count as f64;
    (0..count)
        .map(|i| items[((i as f64 * step) as usize).min(items.len() - 1)])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> VideoAnalysisConfig {
        VideoAnalysisConfig::default()
    }

    #[test]
    fn a_clip_with_no_cuts_is_sampled_on_interval() {
        let plan = plan_frames(Some(30.0), &[], &config());
        // 0, 5, 10, 15, 20, 25
        assert_eq!(plan.len(), 6);
        assert_eq!(plan.timestamps[0].at, 0.0);
        assert_eq!(plan.timestamps[5].at, 25.0);
        assert!(plan.timestamps.iter().all(|f| !f.from_scene_change));
    }

    #[test]
    fn scene_changes_are_sampled_just_after_the_cut() {
        let plan = plan_frames(Some(60.0), &[12.0], &config());
        let cut = plan
            .timestamps
            .iter()
            .find(|f| f.from_scene_change)
            .expect("the cut should be planned");
        assert!((cut.at - 12.2).abs() < 1e-9);
        assert_eq!(plan.scene_changes_found, 1);
    }

    #[test]
    fn nearby_timestamps_collapse_and_prefer_the_cut() {
        // A cut at 4.9 sits within min_frame_gap of the 5.0 interval sample.
        let plan = plan_frames(Some(20.0), &[4.9], &config());
        let near_five: Vec<&PlannedFrame> =
            plan.timestamps.iter().filter(|f| (f.at - 5.0).abs() < 1.0).collect();
        assert_eq!(near_five.len(), 1, "the pair should collapse to one frame");
        assert!(near_five[0].from_scene_change, "the scene change wins the slot");
    }

    #[test]
    fn always_looks_at_the_first_frame() {
        assert_eq!(plan_frames(Some(3.0), &[], &config()).timestamps[0].at, 0.0);
    }

    #[test]
    fn a_short_clip_produces_a_single_frame() {
        let plan = plan_frames(Some(2.0), &[], &config());
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn unknown_duration_still_yields_one_frame() {
        let plan = plan_frames(None, &[], &config());
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.timestamps[0].at, 0.0);
        assert!(!plan.truncated);
    }

    #[test]
    fn the_frame_cap_is_enforced() {
        // A 90-minute video at a 5s interval would be over a thousand frames.
        let plan = plan_frames(Some(5400.0), &[], &config());
        assert!(plan.truncated);
        assert_eq!(plan.len(), config().max_frames);

        // The kept frames must still span the whole video, not just the start.
        let last = plan.timestamps.last().unwrap().at;
        assert!(last > 4000.0, "sampling should reach the end of the video, got {last}");
    }

    #[test]
    fn scene_changes_survive_the_cap_ahead_of_interval_samples() {
        let scenes: Vec<f64> = (1..=40).map(|i| i as f64 * 30.0).collect();
        let config = VideoAnalysisConfig { max_frames: 45, ..config() };
        let plan = plan_frames(Some(1800.0), &scenes, &config);

        assert!(plan.truncated);
        assert_eq!(plan.len(), 45);
        let kept_scenes = plan.timestamps.iter().filter(|f| f.from_scene_change).count();
        assert_eq!(kept_scenes, 40, "every cut should be kept before any interval sample");
    }

    #[test]
    fn cuts_outside_the_duration_are_ignored() {
        let plan = plan_frames(Some(10.0), &[-3.0, 99.0], &config());
        assert!(plan.timestamps.iter().all(|f| f.at >= 0.0 && f.at < 10.0));
    }

    #[test]
    fn timestamps_come_back_in_order() {
        let plan = plan_frames(Some(120.0), &[77.0, 3.5, 44.2, 100.0], &config());
        let times: Vec<f64> = plan.timestamps.iter().map(|f| f.at).collect();
        let mut sorted = times.clone();
        sorted.sort_by(f64::total_cmp);
        assert_eq!(times, sorted);
    }

    #[test]
    fn disabling_the_interval_leaves_only_cuts_and_the_opening_frame() {
        let config = VideoAnalysisConfig { sample_interval: 0.0, ..config() };
        let plan = plan_frames(Some(300.0), &[60.0, 120.0], &config);
        assert_eq!(plan.len(), 3);
    }
}
